//! The token file: what it stores, what it refuses, and the modes it insists
//! on in both directions.

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use norn_config::ConfigError;
use norn_config::machine::tokens::{self, Token};

use crate::common::{Scratch, label};

/// The token labelled `wanted`, or a panic.
fn token<'a>(tokens: &'a tokens::Tokens, wanted: &str) -> &'a Token {
    tokens
        .all()
        .find(|token| token.label().as_str() == wanted)
        .unwrap_or_else(|| panic!("`{wanted}` was not stored"))
}

#[test]
fn a_token_file_that_is_not_there_reads_as_no_tokens() {
    let scratch = Scratch::new("no-tokens");
    let tokens = tokens::read(scratch.dirs()).expect("no tokens");
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

    tokens::mutate(dirs, |tokens| tokens.add(&label("laptop"), &secret)).expect("a token");

    let read = tokens::read(dirs).expect("the tokens");
    assert_eq!(token(&read, "laptop").secret(), &secret[..]);
    assert_eq!(
        read.labels()
            .map(|label| label.as_str())
            .collect::<Vec<_>>(),
        ["laptop"]
    );
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

    tokens::mutate(dirs, |tokens| tokens.add(&label("laptop"), b"secret")).expect("a token");

    let read = tokens::read(dirs).expect("the tokens");
    let created = token(&read, "laptop")
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
    tokens::mutate(dirs, |tokens| tokens.add(&label("laptop"), b"first")).expect("a token");
    let before = scratch.text_at(&scratch.tokens_file());

    let error = tokens::mutate(dirs, |tokens| tokens.add(&label("laptop"), b"second"))
        .expect_err("a duplicate label");
    assert_eq!(
        error,
        ConfigError::DuplicateLabel {
            label: "laptop".to_string()
        }
    );

    assert_eq!(scratch.text_at(&scratch.tokens_file()), before);
    let read = tokens::read(dirs).expect("the tokens");
    assert_eq!(token(&read, "laptop").secret(), b"first");
    assert_eq!(read.all().count(), 1);
}

/// An empty secret is matched by a request that presents nothing at all, so a
/// token holding one authorizes everybody.
#[test]
fn a_token_with_no_secret_is_refused() {
    let scratch = Scratch::new("empty-secret");
    let dirs = scratch.dirs();
    let error =
        tokens::mutate(dirs, |tokens| tokens.add(&label("laptop"), b"")).expect_err("no secret");
    assert_eq!(
        error,
        ConfigError::EmptySecret {
            label: "laptop".to_string()
        }
    );
    assert_eq!(
        tokens::read(dirs).expect("the tokens").all().count(),
        0,
        "the refused token was written down"
    );
}

/// The refusal is per label, not per file: a different label beside an
/// existing one is a second token, which is what rotation is made of.
#[test]
fn a_second_label_is_a_second_token() {
    let scratch = Scratch::new("two-labels");
    let dirs = scratch.dirs();
    tokens::mutate(dirs, |tokens| {
        tokens.add(&label("laptop"), b"first")?;
        tokens.add(&label("service"), b"second")
    })
    .expect("two tokens");

    let read = tokens::read(dirs).expect("the tokens");
    assert_eq!(
        read.labels()
            .map(|label| label.as_str())
            .collect::<Vec<_>>(),
        ["laptop", "service"]
    );
}

#[test]
fn a_token_is_removed_by_label() {
    let scratch = Scratch::new("remove");
    let dirs = scratch.dirs();
    tokens::mutate(dirs, |tokens| {
        tokens.add(&label("laptop"), b"first")?;
        tokens.add(&label("service"), b"second")
    })
    .expect("two tokens");

    let removed =
        tokens::mutate(dirs, |tokens| Ok(tokens.remove(&label("laptop")))).expect("a removal");
    assert!(removed);
    let missing = tokens::mutate(dirs, |tokens| Ok(tokens.remove(&label("laptop"))))
        .expect("a removal of nothing");
    assert!(!missing);

    let read = tokens::read(dirs).expect("the tokens");
    assert_eq!(
        read.labels()
            .map(|label| label.as_str())
            .collect::<Vec<_>>(),
        ["service"]
    );
}

/// Rotation through one label is a new token, not an edit of the old one. A
/// field a newer build wrote about the removed secret — a revocation, a scope,
/// an expiry — must not come back attached to the secret that replaced it.
#[test]
fn a_label_used_again_carries_nothing_of_the_token_that_had_it() {
    let scratch = Scratch::new("rotation");
    let dirs = scratch.dirs();
    scratch.place_private(
        &scratch.tokens_file(),
        "version = 1\n\n[tokens.laptop]\nsecret = \"0a0b\"\ncreated = 10\nrevoked = true\n",
    );

    tokens::mutate(dirs, |tokens| {
        tokens.remove(&label("laptop"));
        tokens.add(&label("laptop"), b"\x02\x03")
    })
    .expect("a rotation");

    let text = scratch.text_at(&scratch.tokens_file());
    assert!(
        !text.contains("revoked"),
        "the removed token's fields were grafted onto the new secret: {text}"
    );
    assert!(text.contains("0203"), "{text}");
}

/// The other direction: a token moved to a different label leaves the fields
/// of the label it had behind with it.
#[test]
fn a_token_moved_to_another_label_leaves_the_old_labels_fields_behind() {
    let scratch = Scratch::new("relabel");
    let dirs = scratch.dirs();
    scratch.place_private(
        &scratch.tokens_file(),
        "version = 1\n\n[tokens.laptop]\nsecret = \"0a0b\"\ncreated = 10\nscope = \"read\"\n",
    );

    tokens::mutate(dirs, |tokens| {
        tokens.remove(&label("laptop"));
        tokens.add(&label("desktop"), b"\x0a\x0b")
    })
    .expect("a relabelling");

    let text = scratch.text_at(&scratch.tokens_file());
    assert!(!text.contains("scope"), "{text}");
    assert!(text.contains("[tokens.desktop]"), "{text}");
}

/// A label in the file is held to the grammar the API holds one to. A label
/// outside it is a token nobody could address for removal.
#[test]
fn a_file_labelling_a_token_illegally_is_refused_on_read() {
    let scratch = Scratch::new("illegal-label");
    let dirs = scratch.dirs();
    scratch.place_private(
        &scratch.tokens_file(),
        "version = 1\n\n[tokens.\"my laptop\"]\nsecret = \"0a\"\ncreated = 1\n",
    );
    let error = tokens::read(dirs).expect_err("an illegal label in the file");
    assert!(matches!(error, ConfigError::IllegalLabel { .. }), "{error}");
}

/// The mode is set when the file is created, not adjusted afterwards, so there
/// is no window in which a token is readable beyond its owner.
#[test]
fn the_token_file_is_written_readable_only_by_its_owner() {
    let scratch = Scratch::new("mode-out");
    let dirs = scratch.dirs();
    tokens::mutate(dirs, |tokens| tokens.add(&label("laptop"), b"secret")).expect("a token");
    assert_eq!(mode_of(&scratch.tokens_file()), 0o600);

    // And a rewrite does not loosen it.
    tokens::mutate(dirs, |tokens| tokens.add(&label("service"), b"secret"))
        .expect("a second token");
    assert_eq!(mode_of(&scratch.tokens_file()), 0o600);
}

/// The directory the token file sits in is owner-only too: a directory the
/// group can traverse is a token file the group can reach whatever the file's
/// own mode says.
#[test]
fn the_config_directory_is_owner_only() {
    let scratch = Scratch::new("directory-mode");
    let dirs = scratch.dirs();
    tokens::mutate(dirs, |tokens| tokens.add(&label("laptop"), b"secret")).expect("a token");
    assert_eq!(mode_of(dirs.config_dir()), 0o700);
}

/// A directory somebody else made first is not a directory this crate created,
/// and a directory builder no-ops on one that is already there. A token
/// operation over a loose directory refuses in both directions rather than
/// writing a `0600` file into a room the world can walk into — where it could
/// be replaced by a planted one.
#[test]
fn a_token_operation_under_a_loose_directory_is_refused() {
    for loosened in [0o755, 0o770, 0o707, 0o777] {
        let scratch = Scratch::new("loose-directory");
        let dirs = scratch.dirs();
        scratch.place(&scratch.tokens_file(), "version = 1\n");
        set_mode(&scratch.tokens_file(), 0o600);
        set_mode(dirs.config_dir(), loosened);

        let error = tokens::read(dirs).expect_err("a loose config directory");
        let ConfigError::InsecurePermissions { path, mode } = error else {
            panic!("mode {loosened:04o} was refused as something other than insecure permissions");
        };
        assert_eq!(path, dirs.config_dir());
        assert_eq!(mode, loosened);

        let error = tokens::mutate(dirs, |tokens| tokens.add(&label("laptop"), b"secret"))
            .expect_err("a mutation under a loose config directory");
        assert!(
            matches!(error, ConfigError::InsecurePermissions { .. }),
            "{error}"
        );
    }
}

/// The other direction. A file the group or the world can read has already
/// exposed its bytes for as long as the mode has stood; tightening it quietly
/// would hide that from the person who has to rotate them.
#[test]
fn a_token_file_readable_beyond_its_owner_is_refused() {
    for loosened in [0o640, 0o604, 0o644, 0o666] {
        let scratch = Scratch::new("mode-in");
        let dirs = scratch.dirs();
        tokens::mutate(dirs, |tokens| tokens.add(&label("laptop"), b"secret")).expect("a token");
        set_mode(&scratch.tokens_file(), loosened);

        let error = tokens::read(dirs).expect_err("a loosened token file");
        let ConfigError::InsecurePermissions { path, mode } = error else {
            panic!("mode {loosened:04o} was refused as something other than insecure permissions");
        };
        assert_eq!(path, scratch.tokens_file());
        assert_eq!(mode, loosened);

        // A mutation refuses on the same terms: it reads before it writes, and
        // rewriting the file at a tight mode would erase the evidence.
        let error = tokens::mutate(dirs, |tokens| tokens.add(&label("service"), b"secret"))
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
    scratch.place_private(&scratch.tokens_file(), "version = 2\n");
    set_mode(&scratch.tokens_file(), 0o644);
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
    tokens::mutate(dirs, |tokens| tokens.add(&label("laptop"), b"secret")).expect("a token");
    set_mode(&scratch.tokens_file(), 0o400);
    let read = tokens::read(dirs).expect("a read-only token file");
    assert_eq!(read.all().count(), 1);
}

/// The registry is not a secret and is not held to the token file's mode: it
/// names machine layout, not a credential.
#[test]
#[allow(clippy::disallowed_methods)] // The registry surface is this crate's own; here it is what the token file's mode is compared against.
fn the_registry_file_is_not_held_to_the_token_mode() {
    let scratch = Scratch::new("registry-mode");
    let dirs = scratch.dirs();
    norn_config::registry::mutate(dirs, |registry| {
        registry.insert(crate::common::entry("notes", "/home/person/notes"));
        Ok(())
    })
    .expect("a registration");
    assert_eq!(mode_of(&scratch.registry_file()), 0o644);
    norn_config::registry::read(dirs).expect("the registry");
}

/// A stored secret that is not the encoding this crate writes is corrupt with
/// the reason, not a token with surprising bytes.
#[test]
fn a_secret_that_is_not_hexadecimal_is_refused_with_the_reason() {
    let scratch = Scratch::new("bad-hex");
    let dirs = scratch.dirs();
    scratch.place_private(
        &scratch.tokens_file(),
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
        (
            "version = 1\n\n[tokens.laptop]\nsecret = \"0a\"\ncreated = -1\n",
            "before the Unix epoch",
        ),
    ] {
        let scratch = Scratch::new("token-malformed");
        let dirs = scratch.dirs();
        scratch.place_private(&scratch.tokens_file(), body);
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

/// The most bytes a token file may hold. Spelled here rather than asked of the
/// crate: the bound is a contract the serving side states, and this is the
/// third spelling that makes the crate and the contract say the same number.
const TOKEN_FILE_CAP: usize = 1_048_576;

/// A valid token file padded with a comment to exactly `length` bytes.
fn token_file_of_length(length: usize) -> String {
    let head = "version = 1\n\n[tokens.laptop]\nsecret = \"0a\"\ncreated = 1\n#";
    let mut text = String::from(head);
    text.push_str(&"x".repeat(length - head.len() - 1));
    text.push('\n');
    assert_eq!(text.len(), length);
    text
}

/// A token file is read on every changed request, inline, so its size is
/// bounded the way `config.toml`'s is: past 1 MiB it is unreadable rather than
/// a read that grows with whatever was put at the name.
#[test]
fn a_token_file_over_one_mebibyte_is_refused_as_unreadable() {
    let scratch = Scratch::new("tokens-over-cap");
    let dirs = scratch.dirs();
    scratch.place_private(
        &scratch.tokens_file(),
        &token_file_of_length(TOKEN_FILE_CAP + 1),
    );

    let error = tokens::read(dirs).expect_err("a token file over the cap");
    let ConfigError::Io { path, message, .. } = &error else {
        panic!("refused as something other than unreadable: {error}");
    };
    assert_eq!(path, &scratch.tokens_file());
    assert!(message.contains("exceeds 1 MiB"), "{message}");

    // The read inside a mutation is the same read.
    let error = tokens::mutate(dirs, |tokens| tokens.add(&label("service"), b"secret"))
        .expect_err("a mutation of a token file over the cap");
    assert!(
        matches!(&error, ConfigError::Io { message, .. } if message.contains("exceeds 1 MiB")),
        "{error}"
    );
}

/// The bound is inclusive: a file of exactly 1 MiB is a file, and it reads.
#[test]
fn a_token_file_of_exactly_one_mebibyte_is_read() {
    let scratch = Scratch::new("tokens-at-cap");
    let dirs = scratch.dirs();
    scratch.place_private(
        &scratch.tokens_file(),
        &token_file_of_length(TOKEN_FILE_CAP),
    );
    let read = tokens::read(dirs).expect("a token file at the cap");
    assert_eq!(read.all().count(), 1);
}

/// The cap is the token file's, not every machine-local file's: the registry
/// names vault roots and is read at whatever size it has.
#[test]
#[allow(clippy::disallowed_methods)] // The registry surface is this crate's own; here it is what the token file's cap is compared against.
fn the_registry_file_is_not_held_to_the_token_cap() {
    let scratch = Scratch::new("registry-uncapped");
    let dirs = scratch.dirs();
    let mut text = String::from("version = 1\n#");
    text.push_str(&"x".repeat(TOKEN_FILE_CAP));
    text.push('\n');
    scratch.place(&scratch.registry_file(), &text);
    norn_config::registry::read(dirs).expect("a registry over the token cap");
}

/// A FIFO at the token file's name is an unreadable file, answered at once: a
/// read that waited for a writer would hang the request that asked, and on
/// the serving side that request runs on the I/O thread.
#[test]
fn a_fifo_at_the_token_file_is_refused_promptly() {
    let scratch = Scratch::new("tokens-fifo");
    scratch.make_config_dir();
    make_fifo(&scratch.tokens_file());
    let dirs = scratch.dirs().clone();

    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let read = tokens::read(&dirs).map(|_| ());
        let mutation =
            tokens::mutate(&dirs, |tokens| tokens.add(&label("laptop"), b"secret")).map(|_| ());
        let _ = sender.send((read, mutation));
    });
    let (read, mutation) = receiver
        .recv_timeout(std::time::Duration::from_secs(10))
        .expect("a FIFO read answers rather than waiting for a writer");
    for (what, outcome) in [("a read", read), ("a mutation", mutation)] {
        let error = outcome.expect_err("a FIFO is not a token file");
        assert!(
            matches!(&error, ConfigError::Io { message, .. } if message.contains("not a regular file")),
            "{what}: {error}"
        );
    }
}

/// Put a FIFO at `path`. Through the system's `mkfifo`, because the suite
/// carries no `unsafe` and the standard library has no name for the call.
pub(crate) fn make_fifo(path: &Path) {
    let status = std::process::Command::new("mkfifo")
        .arg(path)
        .status()
        .expect("running mkfifo");
    assert!(status.success(), "mkfifo {} failed", path.display());
}

/// The set as a whole is printed without its secrets too: it was read from a
/// document that holds every secret as text, and printing the document would
/// print them all.
#[test]
fn a_token_sets_debug_does_not_carry_its_secrets() {
    let scratch = Scratch::new("tokens-debug");
    let dirs = scratch.dirs();
    tokens::mutate(dirs, |tokens| {
        tokens.add(&label("laptop"), b"the-secret-bytes")
    })
    .expect("a token");
    let read = tokens::read(dirs).expect("the tokens");
    let printed = format!("{read:?}");
    assert!(printed.contains("laptop"), "{printed}");
    assert!(
        !printed.contains(&norn_wire::hex::encode(b"the-secret-bytes")),
        "{printed}"
    );
    assert!(!printed.contains("secret-bytes"), "{printed}");
}

/// A token file that does not parse says where and why, and never what the
/// line held: the line is a secret's line, and a refusal reaches a log the
/// moment anything holding it is formatted.
#[test]
fn a_token_file_that_does_not_read_never_quotes_its_secret() {
    let secret = "5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e";
    let head = "version = 1\n\n[tokens.laptop]\n";
    for (case, body, needle) in [
        (
            "trailing text",
            format!("{head}secret = \"{secret}\" oops\ncreated = 1\n"),
            "line 4, column",
        ),
        (
            "an unterminated string",
            format!("{head}secret = \"{secret}\ncreated = 1\n"),
            "line 4, column",
        ),
        (
            "a duplicate key",
            format!("{head}secret = \"{secret}\"\nsecret = \"{secret}\"\ncreated = 1\n"),
            "line 5, column",
        ),
        (
            "a secret of the wrong type",
            format!("{head}secret = 12\ncreated = 1\n# {secret}\n"),
            "`secret` is integer",
        ),
    ] {
        let scratch = Scratch::new("tokens-no-echo");
        let dirs = scratch.dirs();
        scratch.place_private(&scratch.tokens_file(), &body);

        let error = tokens::read(dirs).expect_err("a token file that does not read");
        let watched = tokens::WatchedTokens::read(dirs);
        for printed in [
            error.to_string(),
            format!("{error:?}"),
            format!("{watched:?}"),
        ] {
            assert!(!printed.contains(secret), "{case}: {printed}");
            assert!(!printed.contains("5e5e5e"), "{case}: {printed}");
        }
        assert!(
            error.to_string().contains(needle),
            "{case}: `{error}` does not say `{needle}`"
        );
    }
}

/// The read direction agrees with [`tokens::Tokens::add`]: an empty secret is
/// matched by a request that presents nothing, so a file holding one is
/// corrupt rather than a token set that authenticates everybody.
#[test]
fn a_stored_empty_secret_is_refused_on_read() {
    let scratch = Scratch::new("tokens-empty-secret");
    let dirs = scratch.dirs();
    scratch.place_private(
        &scratch.tokens_file(),
        "version = 1\n\n[tokens.laptop]\nsecret = \"\"\ncreated = 1\n",
    );
    let error = tokens::read(dirs).expect_err("an empty secret");
    let ConfigError::Corrupt { reason, .. } = &error else {
        panic!("refused as something other than corrupt: {error}");
    };
    assert!(reason.contains("token `laptop`"), "{reason}");
    assert!(reason.contains("`secret` is empty"), "{reason}");
    assert!(tokens::WatchedTokens::read(dirs).current().is_err());
}

/// The write direction agrees with the read direction: a mutation whose
/// result would be over 1 MiB is refused before anything is written, rather
/// than leaving a file every later read refuses.
#[test]
fn a_mutation_that_would_write_past_one_mebibyte_writes_nothing() {
    let scratch = Scratch::new("tokens-write-over-cap");
    let dirs = scratch.dirs();
    tokens::mutate(dirs, |tokens| tokens.add(&label("laptop"), b"secret")).expect("a token");
    let before = scratch.text_at(&scratch.tokens_file());

    // Hex doubles a secret, so this one alone renders past the cap.
    let secret = vec![0x5a; TOKEN_FILE_CAP / 2];
    let error = tokens::mutate(dirs, |tokens| tokens.add(&label("service"), &secret))
        .expect_err("a token set over the cap");
    assert!(
        matches!(&error, ConfigError::Io { message, .. } if message.contains("exceed 1 MiB")),
        "{error}"
    );
    assert_eq!(scratch.text_at(&scratch.tokens_file()), before);
    let read = tokens::read(dirs).expect("the file as it was");
    assert_eq!(read.labels().count(), 1);
}
