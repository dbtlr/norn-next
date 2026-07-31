//! The token file: what it stores, what it refuses, and the mode it insists
//! on in both directions.

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use norn_config::ConfigError;
use norn_config::machine::{Endpoint, default_endpoint, tokens};

use crate::common::Scratch;

#[test]
fn a_token_file_that_is_not_there_reads_as_no_tokens() {
    let scratch = Scratch::new("no-tokens");
    let tokens = tokens::read(scratch.dirs()).expect("no tokens");
    assert!(tokens.is_empty());
    assert_eq!(tokens.len(), 0);
    assert_eq!(tokens.labels().count(), 0);
    assert_eq!(
        scratch.names_in(&scratch.config_base()),
        Vec::<String>::new()
    );
}

#[test]
fn a_secret_round_trips_byte_for_byte() {
    let scratch = Scratch::new("secret-round-trip");
    let dirs = scratch.dirs();
    let secret: Vec<u8> = (0..=255u8).collect();

    tokens::mutate(dirs, |tokens| tokens.add("laptop", &secret)).expect("a token");

    let read = tokens::read(dirs).expect("the tokens");
    assert_eq!(read.get("laptop").expect("the token").secret(), &secret[..]);
    assert_eq!(read.labels().collect::<Vec<_>>(), ["laptop"]);
    assert_eq!(read.all().count(), 1);
}

/// The stamp is the moment the token was written down, and it is written down
/// by this crate rather than by the caller — there is one writer of the file,
/// and the field is part of what it writes.
#[test]
fn a_token_carries_the_moment_it_was_written() {
    let scratch = Scratch::new("created");
    let dirs = scratch.dirs();
    let before = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("a clock past the epoch")
        .as_secs();

    tokens::mutate(dirs, |tokens| tokens.add("laptop", b"secret")).expect("a token");

    let read = tokens::read(dirs).expect("the tokens");
    let created = read
        .get("laptop")
        .expect("the token")
        .created()
        .duration_since(UNIX_EPOCH)
        .expect("a stamp past the epoch")
        .as_secs();
    assert!(created >= before, "{created} is before the mutation began");
    assert!(
        created
            <= SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("a clock past the epoch")
                .as_secs(),
        "{created} is after the mutation ended"
    );
}

/// A label is how a token is addressed for removal, so a second entry under
/// one label would be a token nobody could name. Replacing the first silently
/// would be worse: a credential would stop working without anybody asking.
#[test]
fn a_duplicate_label_is_refused_and_leaves_the_first_token_alone() {
    let scratch = Scratch::new("duplicate");
    let dirs = scratch.dirs();
    tokens::mutate(dirs, |tokens| tokens.add("laptop", b"first")).expect("a token");
    let before = scratch.text_at(&dirs.tokens_file());

    let error = tokens::mutate(dirs, |tokens| tokens.add("laptop", b"second"))
        .expect_err("a duplicate label");
    assert_eq!(
        error,
        ConfigError::DuplicateLabel {
            label: "laptop".to_string()
        }
    );

    assert_eq!(scratch.text_at(&dirs.tokens_file()), before);
    let read = tokens::read(dirs).expect("the tokens");
    assert_eq!(read.get("laptop").expect("the token").secret(), b"first");
    assert_eq!(read.len(), 1);
}

/// The refusal is per label, not per file: a different label beside an
/// existing one is a second token, which is what rotation is made of.
#[test]
fn a_second_label_is_a_second_token() {
    let scratch = Scratch::new("two-labels");
    let dirs = scratch.dirs();
    tokens::mutate(dirs, |tokens| {
        tokens.add("laptop", b"first")?;
        tokens.add("service", b"second")
    })
    .expect("two tokens");

    let read = tokens::read(dirs).expect("the tokens");
    assert_eq!(read.labels().collect::<Vec<_>>(), ["laptop", "service"]);
}

#[test]
fn a_token_is_removed_by_label() {
    let scratch = Scratch::new("remove");
    let dirs = scratch.dirs();
    tokens::mutate(dirs, |tokens| {
        tokens.add("laptop", b"first")?;
        tokens.add("service", b"second")
    })
    .expect("two tokens");

    let removed = tokens::mutate(dirs, |tokens| Ok(tokens.remove("laptop"))).expect("a removal");
    assert!(removed);
    let missing =
        tokens::mutate(dirs, |tokens| Ok(tokens.remove("laptop"))).expect("a removal of nothing");
    assert!(!missing);

    let read = tokens::read(dirs).expect("the tokens");
    assert_eq!(read.labels().collect::<Vec<_>>(), ["service"]);
    assert!(read.get("laptop").is_none());
}

/// The mode is set when the file is created, not adjusted afterwards, so there
/// is no window in which a token is readable beyond its owner.
#[test]
fn the_token_file_is_written_readable_only_by_its_owner() {
    let scratch = Scratch::new("mode-out");
    let dirs = scratch.dirs();
    tokens::mutate(dirs, |tokens| tokens.add("laptop", b"secret")).expect("a token");
    assert_eq!(mode_of(&dirs.tokens_file()), 0o600);

    // And a rewrite does not loosen it.
    tokens::mutate(dirs, |tokens| tokens.add("service", b"secret")).expect("a second token");
    assert_eq!(mode_of(&dirs.tokens_file()), 0o600);
}

/// The directory the token file sits in is owner-only too: a directory the
/// group can traverse is a token file the group can reach whatever the file's
/// own mode says.
#[test]
fn the_config_directory_is_owner_only() {
    let scratch = Scratch::new("directory-mode");
    let dirs = scratch.dirs();
    tokens::mutate(dirs, |tokens| tokens.add("laptop", b"secret")).expect("a token");
    assert_eq!(mode_of(dirs.config_dir()), 0o700);
}

/// The other direction. A file the group or the world can read has already
/// exposed its bytes for as long as the mode has stood; tightening it quietly
/// would hide that from the person who has to rotate them.
#[test]
fn a_token_file_readable_beyond_its_owner_is_refused() {
    for loosened in [0o640, 0o604, 0o644, 0o666] {
        let scratch = Scratch::new("mode-in");
        let dirs = scratch.dirs();
        tokens::mutate(dirs, |tokens| tokens.add("laptop", b"secret")).expect("a token");
        set_mode(&dirs.tokens_file(), loosened);

        let error = tokens::read(dirs).expect_err("a loosened token file");
        let ConfigError::InsecurePermissions { path, mode } = error else {
            panic!("mode {loosened:04o} was refused as something other than insecure permissions");
        };
        assert_eq!(path, dirs.tokens_file());
        assert_eq!(mode, loosened);

        // A mutation refuses on the same terms: it reads before it writes, and
        // rewriting the file at a tight mode would erase the evidence.
        let error = tokens::mutate(dirs, |tokens| tokens.add("service", b"secret"))
            .expect_err("a mutation of a loosened token file");
        assert!(
            matches!(error, ConfigError::InsecurePermissions { .. }),
            "{error}"
        );
    }
}

/// The mode is judged before a byte is parsed. A file that is both exposed and
/// unreadable by this build says the exposure first: the credential has
/// already been read by whoever could read it, and that is the fact worth
/// acting on.
#[test]
fn an_exposed_token_file_says_so_ahead_of_anything_else_wrong_with_it() {
    let scratch = Scratch::new("mode-first");
    let dirs = scratch.dirs();
    scratch.place(&dirs.tokens_file(), "version = 2\n");
    set_mode(&dirs.tokens_file(), 0o644);
    let error = tokens::read(dirs).expect_err("an exposed file from a newer build");
    assert!(
        matches!(error, ConfigError::InsecurePermissions { .. }),
        "{error}"
    );
}

/// Owner-execute is not a read, and the crate's demand is about readability.
#[test]
fn a_mode_tighter_than_the_written_one_is_read() {
    let scratch = Scratch::new("mode-tight");
    let dirs = scratch.dirs();
    tokens::mutate(dirs, |tokens| tokens.add("laptop", b"secret")).expect("a token");
    set_mode(&dirs.tokens_file(), 0o400);
    let read = tokens::read(dirs).expect("a read-only token file");
    assert_eq!(read.len(), 1);
}

/// The registry is not a secret and is not held to the token file's mode: it
/// names machine layout, not a credential.
#[test]
fn the_registry_file_is_not_held_to_the_token_mode() {
    let scratch = Scratch::new("registry-mode");
    let dirs = scratch.dirs();
    norn_config::registry::mutate(dirs, |registry| {
        registry.insert(crate::common::entry("notes", "/home/person/notes"));
        Ok(())
    })
    .expect("a registration");
    assert_eq!(mode_of(&dirs.registry_file()), 0o644);
    norn_config::registry::read(dirs).expect("the registry");
}

/// A stored secret that is not the encoding this crate writes is corrupt with
/// the reason, not a token with surprising bytes.
#[test]
fn a_secret_that_is_not_hexadecimal_is_refused_with_the_reason() {
    let scratch = Scratch::new("bad-hex");
    let dirs = scratch.dirs();
    scratch.place_private(
        &dirs.tokens_file(),
        "version = 1\n\n[tokens.laptop]\nsecret = \"nothex\"\ncreated = 1\n",
    );
    let error = tokens::read(dirs).expect_err("a secret that is not hexadecimal");
    let ConfigError::Corrupt { reason, .. } = error else {
        panic!("refused as something other than corrupt");
    };
    assert!(reason.contains("hexadecimal"), "{reason}");
}

#[test]
fn a_token_missing_a_required_field_is_refused_with_the_reason() {
    for (body, needle) in [
        (
            "version = 1\n\n[tokens.laptop]\ncreated = 1\n",
            "`secret` is required",
        ),
        (
            "version = 1\n\n[tokens.laptop]\nsecret = \"0a\"\n",
            "`created` is required",
        ),
        (
            "version = 1\n\n[tokens.laptop]\nsecret = \"0a\"\ncreated = \"now\"\n",
            "`created` is string",
        ),
    ] {
        let scratch = Scratch::new("token-malformed");
        let dirs = scratch.dirs();
        scratch.place_private(&dirs.tokens_file(), body);
        let error = tokens::read(dirs).expect_err("a malformed token file");
        let ConfigError::Corrupt { reason, .. } = error else {
            panic!("`{body}` was refused as something other than corrupt");
        };
        assert!(
            reason.contains(needle),
            "`{body}` was refused with `{reason}`, which does not mention `{needle}`"
        );
    }
}

/// The endpoint is a convention rather than a discovery: no file is read, and
/// the channel decides the port.
#[test]
fn the_endpoint_is_loopback_at_the_channels_port() {
    let endpoint = default_endpoint();
    assert!(endpoint.address().ip().is_loopback());
    assert_eq!(
        endpoint.origin(),
        format!("http://127.0.0.1:{}", endpoint.port())
    );
    assert_eq!(
        endpoint.to_string(),
        format!("127.0.0.1:{}", endpoint.port())
    );
    assert_eq!(Endpoint::at(endpoint.port()), endpoint);
}

#[allow(clippy::disallowed_methods)] // Harness scaffolding: judging the mode the API wrote.
fn mode_of(path: &Path) -> u32 {
    std::fs::metadata(path)
        .unwrap_or_else(|e| panic!("reading the mode of {}: {e}", path.display()))
        .permissions()
        .mode()
        & 0o7777
}

#[allow(clippy::disallowed_methods)] // Harness scaffolding: arranging a mode the API never writes.
fn set_mode(path: &Path, mode: u32) {
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
        .unwrap_or_else(|e| panic!("setting the mode of {}: {e}", path.display()));
}
