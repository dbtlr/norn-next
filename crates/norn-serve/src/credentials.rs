//! The credential state the hello and the bearer middleware share: the token
//! set held fresh against the file, the index a bearer is looked up by, and
//! the counters a bar reads them through.
//!
//! **One check per request, inline on the I/O thread.** Each request that
//! reaches the hello or the bearer middleware calls [`Credentials::check`]
//! once, before it reads anything from the set. A settled, unchanged token
//! file costs one fingerprint through `norn-config` and no read. A racy,
//! changed or refused one costs two fingerprints and a re-read (at most
//! 1 MiB): racy while the held fingerprint was taken less than 3 s after the
//! file's later modification or change time, refused while a refusal is
//! held. A refusal whose fingerprint itself fails (a parent directory that
//! cannot be searched) costs the two fingerprints and no read, since there is
//! nothing the read could open. Neither waits on a blocking pool. The index
//! is rebuilt only when the check replaced the held set.
//!
//! **One lookup, never a walk.** A hello's label is looked up in the set
//! itself; a bearer is looked up by the SHA-256 of its decoded bytes in the
//! digest index, then compared with the stored secret in constant time. Both
//! lookups are the private `held` module's, which alone holds the set and the
//! index, and each counts itself.
//!
//! **A changed file that no longer reads fails closed**: while the held
//! outcome is a refusal, every request is refused, and the log says so once,
//! on entering that state, naming the file and the fix.

use std::path::Path;

use hmac::{Hmac, KeyInit, Mac};
use norn_config::machine::tokens::{TokenLabel, WatchedTokens};
use norn_config::{ConfigDirs, ConfigError};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

use crate::log::log;

/// What the bearer middleware decided about one request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Admission {
    /// The bearer names a stored token.
    Admitted,
    /// No bearer, a malformed one, or one that names no stored token.
    Unauthorized,
    /// The token file changed and no longer reads.
    FailedClosed,
}

/// What the hello decided about one request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Keyed {
    /// The MAC over the input, keyed by the labelled token.
    Mac([u8; 32]),
    /// The label names no stored token, or is not a label at all.
    Refused,
    /// The token file changed and no longer reads.
    FailedClosed,
}

/// The `serve.token_*` counters, cumulative over the host's life.
///
/// Kept in every build, since keeping them costs three additions per
/// request; only the `counters` feature serves them.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[cfg_attr(not(any(test, feature = "counters")), allow(dead_code))]
pub(crate) struct TokenCounters {
    /// Fingerprints of the token file taken, `serve.token_fingerprints`.
    pub(crate) fingerprints: u64,
    /// Reads of the token file, `serve.token_file_reads`.
    pub(crate) file_reads: u64,
    /// Lookups by label or by digest, `serve.token_lookups`.
    pub(crate) lookups: u64,
}

pub(crate) struct Credentials {
    /// The token set and the index over it: the only way to a secret.
    held: Held,
    /// Fingerprints of the token file taken, `serve.token_fingerprints`.
    fingerprints: u64,
    /// Reads of the token file, `serve.token_file_reads`.
    file_reads: u64,
    /// Whether the held outcome was a refusal at the last check, so the log
    /// line is written on entering the state rather than on every request.
    failed_closed: bool,
    tokens_file: String,
}

impl Credentials {
    /// The state over a token set already read.
    pub(crate) fn new(dirs: &ConfigDirs, watched: WatchedTokens) -> Self {
        let mut credentials = Credentials {
            held: Held::new(watched),
            fingerprints: 0,
            file_reads: 0,
            failed_closed: false,
            tokens_file: tokens_file(dirs),
        };
        credentials.held_replaced();
        credentials
    }

    /// Bring the held set up to date with the file: the one check a request
    /// makes.
    pub(crate) fn check(&mut self) {
        let check = self.held.check();
        self.fingerprints += u64::from(check.fingerprints());
        self.file_reads += u64::from(check.read());
        if check.replaced() {
            self.held_replaced();
        }
    }

    /// Whether the held outcome is a refusal: the fail-closed state, in
    /// which every request is refused until the file reads again.
    pub(crate) fn fails_closed(&self) -> bool {
        self.held.refusal().is_some()
    }

    /// Log the transitions into and out of the fail-closed state.
    fn held_replaced(&mut self) {
        match self.held.refusal() {
            None => {
                if self.failed_closed {
                    log(format_args!(
                        "the token file {} reads again; requests are served",
                        self.tokens_file
                    ));
                }
                self.failed_closed = false;
            }
            Some(error) => {
                if !self.failed_closed {
                    log(format_args!(
                        "warning: the token file no longer reads, so every request is refused \
                         until it does: {}",
                        unreadable(&self.tokens_file, error)
                    ));
                }
                self.failed_closed = true;
            }
        }
    }

    /// The bearer middleware's decision over the `Authorization` values a
    /// request carried. [`check`](Self::check) is the caller's, first.
    pub(crate) fn admit<'a>(&mut self, authorization: impl Iterator<Item = &'a [u8]>) -> Admission {
        if self.fails_closed() {
            return Admission::FailedClosed;
        }
        let Some(presented) = bearer(authorization) else {
            return Admission::Unauthorized;
        };
        match self.held.by_digest(&presented) {
            Some(secret) if bool::from(secret.ct_eq(&presented)) => Admission::Admitted,
            _ => Admission::Unauthorized,
        }
    }

    /// The hello's MAC, keyed by the token `label` names, over what `input`
    /// builds — `None` when the hello's nonce does not read, which is refused
    /// like an unknown label and costs no lookup. [`check`](Self::check) is
    /// the caller's, first.
    pub(crate) fn key(&mut self, label: &str, input: impl FnOnce() -> Option<Vec<u8>>) -> Keyed {
        if self.fails_closed() {
            return Keyed::FailedClosed;
        }
        let (Ok(label), Some(input)) = (TokenLabel::new(label), input()) else {
            return Keyed::Refused;
        };
        match self.held.by_label(&label) {
            Some(secret) => Keyed::Mac(mac(secret, &input)),
            None => Keyed::Refused,
        }
    }

    #[cfg_attr(not(any(test, feature = "counters")), allow(dead_code))]
    pub(crate) fn counters(&self) -> TokenCounters {
        TokenCounters {
            fingerprints: self.fingerprints,
            file_reads: self.file_reads,
            lookups: self.held.lookups(),
        }
    }
}

/// The held token set, private to this module so the only way a request
/// reaches a stored secret is one of [`Held`]'s two lookups: by digest for a
/// bearer, by label for the hello. Each counts itself, so
/// `serve.token_lookups` is the number of lookups actually made through them,
/// not a number a call site claims.
///
/// Nothing outside this module can walk the stored tokens without editing
/// it: the set and the digest index are its fields alone.
///
/// Rebuilding the digest index on every check would cost O(tokens) per
/// request, and the counter bar cannot see it. The guard is the unit test
/// `a_bearer_is_admitted_only_when_the_secret_its_digest_finds_is_equal`,
/// by side effect: it poisons the index after the set is held, so a rebuild
/// would wipe the poison. It must keep poisoning after the set is held.
mod held {
    use std::collections::HashMap;

    use norn_config::ConfigError;
    use norn_config::machine::tokens::{Check, Token, TokenLabel, WatchedTokens};

    use super::digest;

    pub(super) struct Held {
        watched: WatchedTokens,
        /// The SHA-256 of each held secret, to the secret it is the digest
        /// of. Rebuilt only when a check replaces the held set.
        by_digest: HashMap<[u8; 32], Vec<u8>>,
        /// Lookups by digest or by label, `serve.token_lookups`.
        lookups: u64,
    }

    impl Held {
        pub(super) fn new(watched: WatchedTokens) -> Self {
            let mut held = Held {
                watched,
                by_digest: HashMap::new(),
                lookups: 0,
            };
            held.index();
            held
        }

        /// The token file's check, rebuilding the index when it replaced the
        /// held set.
        pub(super) fn check(&mut self) -> Check {
            let check = self.watched.check();
            if check.replaced() {
                self.index();
            }
            check
        }

        fn index(&mut self) {
            self.by_digest = match self.watched.current() {
                Ok(tokens) => tokens
                    .all()
                    .map(|token| (digest(token.secret()), token.secret().to_vec()))
                    .collect(),
                Err(_) => HashMap::new(),
            };
        }

        /// Why the held outcome is a refusal, if it is one. It names no
        /// secret.
        pub(super) fn refusal(&self) -> Option<&ConfigError> {
            self.watched.current().err()
        }

        /// The one bearer lookup: the secret whose digest is the presented
        /// bytes' digest. A hit is not an admission; the caller compares.
        pub(super) fn by_digest(&mut self, presented: &[u8]) -> Option<&[u8]> {
            self.lookups += 1;
            self.by_digest.get(&digest(presented)).map(Vec::as_slice)
        }

        /// The one hello lookup: the secret of the token `label` names,
        /// through `Tokens::get`.
        pub(super) fn by_label(&mut self, label: &TokenLabel) -> Option<&[u8]> {
            self.lookups += 1;
            self.watched.current().ok()?.get(label).map(Token::secret)
        }

        pub(super) fn lookups(&self) -> u64 {
            self.lookups
        }

        /// Point the index entry for `presented`'s digest at `secret`, or
        /// drop it: the unit suite's poisoned index.
        #[cfg(test)]
        pub(super) fn point(&mut self, presented: &[u8], secret: Option<&[u8]>) {
            match secret {
                Some(secret) => {
                    self.by_digest.insert(digest(presented), secret.to_vec());
                }
                None => {
                    self.by_digest.remove(&digest(presented));
                }
            }
        }
    }
}

use held::Held;

/// The bytes a request's bearer spells: exactly one `Authorization` value,
/// `Bearer ` and lowercase hex. The scheme is matched without regard to case,
/// as HTTP's authentication schemes are; the hex is the one strict codec.
pub(crate) fn bearer<'a>(mut authorization: impl Iterator<Item = &'a [u8]>) -> Option<Vec<u8>> {
    let value = authorization.next()?;
    if authorization.next().is_some() {
        return None;
    }
    let value = std::str::from_utf8(value).ok()?;
    let (scheme, credential) = value.split_once(' ')?;
    if !scheme.eq_ignore_ascii_case("bearer") {
        return None;
    }
    norn_wire::hex::decode(credential)
}

/// HMAC-SHA256 of `input` under `key`, whatever the key's length.
pub(crate) fn mac(key: &[u8], input: &[u8]) -> [u8; 32] {
    let mut mac =
        <Hmac<Sha256> as KeyInit>::new_from_slice(key).expect("HMAC takes a key of any length");
    mac.update(input);
    mac.finalize().into_bytes().into()
}

/// Whether `tag` is HMAC-SHA256 of `input` under `key`, compared in constant
/// time.
pub(crate) fn mac_verifies(key: &[u8], input: &[u8], tag: &[u8; 32]) -> bool {
    let mut mac =
        <Hmac<Sha256> as KeyInit>::new_from_slice(key).expect("HMAC takes a key of any length");
    mac.update(input);
    mac.verify_slice(tag).is_ok()
}

fn digest(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

/// The token file as a person finds it. The name is `norn-config`'s; the
/// path is spelled here only to be logged, never opened.
pub(crate) fn tokens_file(dirs: &ConfigDirs) -> String {
    dirs.config_dir().join("tokens.toml").display().to_string()
}

/// A token file that does not read, with the fix, as one log line: the file
/// named once, whether or not the error already names it, and an error that
/// spans lines folded onto one.
pub(crate) fn unreadable(file: &str, error: &ConfigError) -> String {
    let text = error
        .to_string()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let named = if text.contains(file) {
        text
    } else {
        format!("{file}: {text}")
    };
    let fix = match error {
        ConfigError::InsecurePermissions { path, .. } if is_file(path, file) => {
            format!("restrict it to its owner with `chmod 600 {file}`")
        }
        ConfigError::InsecurePermissions { path, .. } => format!(
            "restrict its directory to its owner with `chmod 700 {}`",
            path.display()
        ),
        _ => "repair the token file, or move it aside so the next start mints a new `default` \
              token (every client then reads the new one)"
            .to_string(),
    };
    format!("{named}; {fix}")
}

fn is_file(path: &Path, file: &str) -> bool {
    path.display().to_string() == file
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// RFC 4231's first test case, so the MAC is HMAC-SHA256 and not
    /// something shaped like it.
    #[test]
    fn the_mac_is_hmac_sha256() {
        let key = [0x0b; 20];
        let expected = norn_wire::hex::decode(
            "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7",
        )
        .unwrap();
        assert_eq!(mac(&key, b"Hi There").as_slice(), expected.as_slice());
        let tag: [u8; 32] = expected.try_into().unwrap();
        assert!(mac_verifies(&key, b"Hi There", &tag));
        let mut other = tag;
        other[31] ^= 1;
        assert!(!mac_verifies(&key, b"Hi There", &other));
        assert!(!mac_verifies(&[0x0c; 20], b"Hi There", &tag));
    }

    #[allow(clippy::disallowed_methods)] // Harness scaffolding: the bases a case's state lives under.
    fn held(scratch: &norn_testkit::scratch::Scratch, count: usize) -> (ConfigDirs, Credentials) {
        std::fs::create_dir_all(scratch.join("config")).expect("a config base");
        let dirs = ConfigDirs::new(scratch.join("config"), scratch.join("data")).expect("bases");
        norn_config::machine::tokens::mutate(&dirs, |tokens| {
            for n in 0..count {
                tokens.add(
                    &TokenLabel::new(format!("t{n}")).unwrap(),
                    format!("s{n}").as_bytes(),
                )?;
            }
            Ok(())
        })
        .expect("tokens");
        let credentials = Credentials::new(&dirs, settled(&dirs));
        (dirs, credentials)
    }

    /// The token set read under a clock a minute ahead, so every fingerprint
    /// is settled: an unchanged file then costs a check one fingerprint and
    /// no read, and the index is rebuilt only when the file changes. A
    /// file's change time cannot be set back, so the clock is moved instead.
    pub(crate) fn settled(dirs: &ConfigDirs) -> WatchedTokens {
        WatchedTokens::read_with_clock(dirs, || {
            std::time::SystemTime::now() + std::time::Duration::from_secs(60)
        })
    }

    fn admit(credentials: &mut Credentials, header: &str) -> Admission {
        credentials.check();
        credentials.admit(std::iter::once(header.as_bytes()))
    }

    /// A digest hit is not an admission: the stored secret is compared with
    /// the presented bytes, so a digest index that answered wrongly would
    /// still refuse.
    #[test]
    fn a_bearer_is_admitted_only_when_the_secret_its_digest_finds_is_equal() {
        let scratch = norn_testkit::scratch::Scratch::new("norn-serve-bearer-compare");
        let (_dirs, mut credentials) = held(&scratch, 2);
        let s0 = norn_wire::hex::encode(b"s0");
        assert_eq!(
            admit(&mut credentials, &format!("Bearer {s0}")),
            Admission::Admitted
        );
        assert_eq!(
            admit(
                &mut credentials,
                &format!("Bearer {}", norn_wire::hex::encode(b"s9"))
            ),
            Admission::Unauthorized
        );
        // Point s0's digest at another secret: the lookup hits, the compare
        // refuses.
        credentials.held.point(b"s0", Some(b"s1"));
        assert_eq!(
            admit(&mut credentials, &format!("Bearer {s0}")),
            Admission::Unauthorized
        );
        // Drop s0's digest from the index: the lookup misses, and a stored
        // secret the index does not find is refused. A walk over the tokens
        // beside the index would admit it here.
        credentials.held.point(b"s0", None);
        assert_eq!(
            admit(&mut credentials, &format!("Bearer {s0}")),
            Admission::Unauthorized
        );
    }

    /// `serve.token_lookups` counts each lookup made through the held set's
    /// two lookup methods, hit or miss, and only those.
    #[test]
    fn every_lookup_through_the_held_set_is_counted_hit_or_miss() {
        let scratch = norn_testkit::scratch::Scratch::new("norn-serve-held-lookups");
        let (_dirs, mut credentials) = held(&scratch, 3);
        assert_eq!(credentials.counters().lookups, 0);
        assert_eq!(credentials.held.by_digest(b"s1"), Some(&b"s1"[..]));
        assert_eq!(credentials.held.by_digest(b"s9"), None);
        let t2 = TokenLabel::new("t2").unwrap();
        assert_eq!(credentials.held.by_label(&t2), Some(&b"s2"[..]));
        let t9 = TokenLabel::new("t9").unwrap();
        assert_eq!(credentials.held.by_label(&t9), None);
        assert_eq!(credentials.counters().lookups, 4);
    }

    /// The token counter bar at the unit seam: one fingerprint and one lookup
    /// per request over an unchanged file, whatever the number of tokens; no
    /// lookup for a request that presents nothing.
    ///
    /// The lookups are counted inside the held set's two lookup methods, so
    /// the bar measures the lookups made through the index. A walk over the
    /// tokens added beside the index would make no counted lookup, so this
    /// bar cannot see it: the poisoned-index test above catches one, by
    /// refusing a bearer whose digest the index misdirects or misses, and
    /// review catches the rest. The same test is the only guard against a
    /// per-request index rebuild, which this bar cannot see either: a rebuild
    /// would wipe the poison, so that test must keep poisoning the index
    /// after the set is held.
    #[test]
    fn an_unchanged_file_costs_one_fingerprint_and_one_lookup_per_request() {
        for count in [1, 64] {
            let scratch = norn_testkit::scratch::Scratch::new("norn-serve-bearer-counters");
            let (_dirs, mut credentials) = held(&scratch, count);
            let bearer = format!("Bearer {}", norn_wire::hex::encode(b"s0"));
            for _ in 0..10 {
                assert_eq!(admit(&mut credentials, &bearer), Admission::Admitted);
            }
            credentials.check();
            assert_eq!(
                credentials.admit(std::iter::empty()),
                Admission::Unauthorized
            );
            credentials.check();
            assert!(matches!(
                credentials.key("t0", || Some(b"input".to_vec())),
                Keyed::Mac(mac_bytes) if mac_bytes == mac(b"s0", b"input")
            ));
            credentials.check();
            assert_eq!(credentials.key("t0", || None), Keyed::Refused);
            assert_eq!(
                credentials.counters(),
                TokenCounters {
                    fingerprints: 13,
                    file_reads: 0,
                    lookups: 11,
                },
                "{count} tokens"
            );
        }
    }

    #[test]
    fn a_bearer_is_one_authorization_value_of_lowercase_hex() {
        let read = |values: &[&str]| bearer(values.iter().map(|value| value.as_bytes()));
        assert_eq!(read(&["Bearer 00ff"]), Some(vec![0x00, 0xff]));
        assert_eq!(read(&["bearer 00ff"]), Some(vec![0x00, 0xff]));
        assert_eq!(read(&[]), None);
        assert_eq!(
            read(&["Bearer 00FF"]),
            None,
            "uppercase is not the spelling"
        );
        assert_eq!(read(&["Bearer 0"]), None);
        assert_eq!(read(&["Bearer  00ff"]), None);
        assert_eq!(read(&["Basic 00ff"]), None);
        assert_eq!(read(&["00ff"]), None);
        assert_eq!(read(&["Bearer 00ff", "Bearer 00ff"]), None);
    }
}
