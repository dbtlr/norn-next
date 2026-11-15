//! The token file's freshness: a holder of the parsed set sees every change to
//! the file on its next check, pays one fingerprint and no read while nothing
//! changed, and refuses everything while a changed file no longer reads.

use std::io::{Seek, SeekFrom, Write};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::Path;
use std::time::Duration;

use norn_config::ConfigError;
use norn_config::machine::tokens::{self, Check, WatchedTokens};

use crate::common::{Scratch, label};
use crate::tokens::make_fifo;

/// A token file holding one token, `laptop`, whose secret is `secret_hex`.
fn one_token(secret_hex: &str) -> String {
    format!("version = 1\n\n[tokens.laptop]\nsecret = \"{secret_hex}\"\ncreated = 1\n")
}

/// The labels the holder holds, or a panic when it is failing closed.
fn labels(watched: &WatchedTokens) -> Vec<String> {
    watched
        .current()
        .unwrap_or_else(|error| panic!("the holder is failing closed: {error}"))
        .labels()
        .map(|label| label.as_str().to_string())
        .collect()
}

/// The secret the holder holds for `laptop`.
fn laptop_secret(watched: &WatchedTokens) -> Vec<u8> {
    watched
        .current()
        .unwrap_or_else(|error| panic!("the holder is failing closed: {error}"))
        .get(&label("laptop"))
        .expect("a token labelled `laptop`")
        .secret()
        .to_vec()
}

/// The cost of a check that found nothing changed.
fn assert_unchanged(check: Check) {
    assert_eq!(check.fingerprints(), 1, "{check:?}");
    assert!(!check.read(), "{check:?}");
    assert!(!check.replaced(), "{check:?}");
}

/// The cost of a check that found a change and read the file again.
fn assert_reread(check: Check) {
    assert_eq!(check.fingerprints(), 2, "{check:?}");
    assert!(check.read(), "{check:?}");
    assert!(check.replaced(), "{check:?}");
}

/// Wait out the filesystem's timestamp tick.
///
/// Linux stamps a write with a coarse clock, one scheduler tick wide (at most
/// 10 ms), so two writes inside one tick can carry one modification and one
/// change time — the declared limit of the fingerprint. A case about an
/// in-place edit or a mode change, which keep the inode and the size, waits
/// the tick out first so that what it pins is the fingerprint, not the clock.
fn past_a_tick() {
    std::thread::sleep(Duration::from_millis(30));
}

/// Overwrite bytes of the file at `path` in place, from `offset`, without
/// truncating it: the inode and the size are kept, so only the times change.
#[allow(clippy::disallowed_methods, clippy::disallowed_types)] // Harness scaffolding: the in-place edit the API never makes.
fn overwrite_in_place(path: &Path, offset: u64, bytes: &[u8]) {
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .open(path)
        .expect("opening the token file for writing");
    file.seek(SeekFrom::Start(offset)).expect("seeking");
    file.write_all(bytes).expect("overwriting");
    file.sync_all().expect("syncing");
}

/// Where the secret's digits start in [`one_token`]'s bytes.
fn secret_offset(text: &str) -> u64 {
    let needle = "secret = \"";
    (text.find(needle).expect("a secret") + needle.len()) as u64
}

#[allow(clippy::disallowed_methods)] // Harness scaffolding: judging what an edit kept.
fn inode_and_size(path: &Path) -> (u64, u64) {
    let metadata = std::fs::metadata(path).expect("the token file's metadata");
    (metadata.ino(), metadata.size())
}

#[allow(clippy::disallowed_methods)] // Harness scaffolding: arranging a mode the API never writes.
fn set_mode(path: &Path, mode: u32) {
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
        .unwrap_or_else(|e| panic!("setting the mode of {}: {e}", path.display()));
}

#[allow(clippy::disallowed_methods)] // Harness scaffolding: removing what a case put at the name.
fn remove(path: &Path) {
    std::fs::remove_file(path).unwrap_or_else(|e| panic!("removing {}: {e}", path.display()));
}

/// The cost the serving side pays on every request: while the file is as it
/// was, one fingerprint and nothing else, check after check.
#[test]
fn an_unchanged_token_file_costs_one_fingerprint_and_no_read() {
    let scratch = Scratch::new("fresh-unchanged");
    let dirs = scratch.dirs();
    tokens::mutate(dirs, |tokens| tokens.add(&label("laptop"), b"secret")).expect("a token");

    let mut watched = WatchedTokens::read(dirs);
    assert_eq!(labels(&watched), ["laptop"]);
    for _ in 0..3 {
        assert_unchanged(watched.check());
    }
    assert_eq!(labels(&watched), ["laptop"]);
}

/// No file is a file that holds no tokens, and it stays one cheaply.
#[test]
fn an_absent_token_file_is_held_as_no_tokens_and_costs_one_fingerprint() {
    let scratch = Scratch::new("fresh-absent");
    let mut watched = WatchedTokens::read(scratch.dirs());
    assert_eq!(labels(&watched), Vec::<String>::new());
    assert_unchanged(watched.check());
}

/// A revocation takes effect on the next check, with no restart and no reload:
/// keeping a revoked token working is the failure this type exists to prevent.
#[test]
fn a_revocation_by_rewrite_is_seen_on_the_next_check() {
    let scratch = Scratch::new("fresh-revoked");
    let dirs = scratch.dirs();
    tokens::mutate(dirs, |tokens| {
        tokens.add(&label("laptop"), b"first")?;
        tokens.add(&label("service"), b"second")
    })
    .expect("two tokens");
    let mut watched = WatchedTokens::read(dirs);
    assert_eq!(labels(&watched), ["laptop", "service"]);

    tokens::mutate(dirs, |tokens| Ok(tokens.remove(&label("laptop")))).expect("a revocation");

    assert_reread(watched.check());
    assert_eq!(labels(&watched), ["service"]);
    assert_unchanged(watched.check());
}

/// A hand edit that overwrites bytes in place keeps the inode and the size.
/// The kernel still sets the modification and change times, so it is seen.
#[test]
fn an_in_place_same_size_edit_is_seen() {
    let scratch = Scratch::new("fresh-in-place");
    let path = scratch.tokens_file();
    let text = one_token("0a0a");
    scratch.place_private(&path, &text);
    let mut watched = WatchedTokens::read(scratch.dirs());
    assert_eq!(laptop_secret(&watched), [0x0a, 0x0a]);
    let before = inode_and_size(&path);

    past_a_tick();
    overwrite_in_place(&path, secret_offset(&text), b"0b0b");
    assert_eq!(
        inode_and_size(&path),
        before,
        "the edit kept the inode and the size"
    );

    assert_reread(watched.check());
    assert_eq!(laptop_secret(&watched), [0x0b, 0x0b]);
}

/// Restoring the modification time after an edit is itself a change the
/// kernel stamps on the change time, which no caller can set: the edit is seen
/// whatever the modification time says.
#[test]
#[allow(clippy::disallowed_methods, clippy::disallowed_types)] // Harness scaffolding: restoring a modification time, as `touch -d` would.
fn an_edit_whose_modification_time_is_restored_is_seen_by_the_change_time() {
    let scratch = Scratch::new("fresh-mtime-restored");
    let path = scratch.tokens_file();
    let text = one_token("0a0a");
    scratch.place_private(&path, &text);
    let mut watched = WatchedTokens::read(scratch.dirs());
    let original = std::fs::metadata(&path)
        .and_then(|metadata| metadata.modified())
        .expect("the modification time");

    past_a_tick();
    overwrite_in_place(&path, secret_offset(&text), b"0c0c");
    std::fs::File::options()
        .write(true)
        .open(&path)
        .and_then(|file| file.set_modified(original))
        .expect("restoring the modification time");
    assert_eq!(
        std::fs::metadata(&path)
            .and_then(|metadata| metadata.modified())
            .expect("the modification time"),
        original,
        "the modification time reads as it did before the edit"
    );
    assert_eq!(inode_and_size(&path).1, text.len() as u64);

    assert_reread(watched.check());
    assert_eq!(laptop_secret(&watched), [0x0c, 0x0c]);
}

/// Every way a changed file can stop reading, and the repair each one takes.
/// Each breaks the file after a good read; the holder must then fail closed
/// rather than keep the old set, stay failed closed at one fingerprint per
/// check while the file stays broken, and recover once the file reads again.
#[test]
#[allow(clippy::disallowed_methods)] // Harness scaffolding: planting a link at the token file's name.
fn a_changed_file_that_no_longer_reads_fails_closed_until_it_reads_again() {
    type Break = fn(&Scratch, &Path);
    let cases: [(&str, Break); 6] = [
        ("a TOML error", |scratch, path| {
            scratch.place_private(path, "version = 1\n[tokens\n");
        }),
        ("an illegal label", |scratch, path| {
            scratch.place_private(
                path,
                "version = 1\n\n[tokens.\"My Laptop\"]\nsecret = \"0a\"\ncreated = 1\n",
            );
        }),
        ("a group-readable mode", |_, path| set_mode(path, 0o640)),
        ("a file over 1 MiB", |scratch, path| {
            let mut text = one_token("0a");
            text.push('#');
            text.push_str(&"x".repeat(1_048_576));
            scratch.place_private(path, &text);
        }),
        ("a FIFO", |_, path| {
            remove(path);
            make_fifo(path);
        }),
        ("a symlink", |scratch, path| {
            let elsewhere = scratch.config_base().join("elsewhere.toml");
            scratch.place(&elsewhere, &one_token("0a"));
            remove(path);
            std::os::unix::fs::symlink(&elsewhere, path).expect("planting a link");
        }),
    ];

    for (name, break_file) in cases {
        let scratch = Scratch::new("fresh-fail-closed");
        let path = scratch.tokens_file();
        scratch.place_private(&path, &one_token("0a"));
        let mut watched = WatchedTokens::read(scratch.dirs());
        assert_eq!(labels(&watched), ["laptop"], "{name}");

        past_a_tick();
        break_file(&scratch, &path);

        let check = watched.check();
        assert_eq!(check.fingerprints(), 2, "{name}: {check:?}");
        assert!(check.replaced(), "{name}: {check:?}");
        assert!(
            watched.current().is_err(),
            "{name}: the old set was kept after the file stopped reading"
        );

        // Unchanged since it stopped reading: failing closed costs what
        // serving does, and it does not quietly recover.
        assert_unchanged(watched.check());
        assert!(watched.current().is_err(), "{name}: recovered unchanged");

        past_a_tick();
        remove(&path);
        scratch.place_private(&path, &one_token("0d"));
        assert_reread(watched.check());
        assert_eq!(laptop_secret(&watched), [0x0d], "{name}");
    }
}

/// The mode case alone, named: a file loosened to `0640` has exposed its bytes,
/// and a holder that kept serving from them would hide that.
#[test]
fn a_token_file_loosened_to_group_readable_fails_closed() {
    let scratch = Scratch::new("fresh-loosened");
    let path = scratch.tokens_file();
    scratch.place_private(&path, &one_token("0a"));
    let mut watched = WatchedTokens::read(scratch.dirs());

    past_a_tick();
    set_mode(&path, 0o640);

    assert_reread(watched.check());
    let error = watched.current().expect_err("a loosened file fails closed");
    assert!(
        matches!(error, ConfigError::InsecurePermissions { mode: 0o640, .. }),
        "{error}"
    );
}

/// A removed file is a file holding no tokens: every token it held is revoked.
#[test]
fn a_removed_token_file_reads_as_no_tokens() {
    let scratch = Scratch::new("fresh-removed");
    let dirs = scratch.dirs();
    tokens::mutate(dirs, |tokens| tokens.add(&label("laptop"), b"secret")).expect("a token");
    let mut watched = WatchedTokens::read(dirs);

    remove(&scratch.tokens_file());

    assert_reread(watched.check());
    assert_eq!(labels(&watched), Vec::<String>::new());
}

/// A first read that refuses is held like a later one: the holder fails
/// closed from the start, and recovers on the change that repairs the file.
#[test]
fn a_first_read_that_refuses_fails_closed_until_the_file_changes() {
    let scratch = Scratch::new("fresh-first-refused");
    let path = scratch.tokens_file();
    scratch.place_private(&path, "version = 1\n[tokens\n");
    let mut watched = WatchedTokens::read(scratch.dirs());
    assert!(watched.current().is_err());
    assert_unchanged(watched.check());
    assert!(watched.current().is_err());

    scratch.place_private(&path, &one_token("0e"));
    assert_reread(watched.check());
    assert_eq!(laptop_secret(&watched), [0x0e]);
}

/// A fingerprint that cannot be taken is a change, and while it cannot be
/// taken the holder fails closed: nothing says the file is the one it read.
#[test]
#[allow(clippy::disallowed_methods)] // Harness scaffolding: locking the config directory so a stat cannot answer.
fn a_fingerprint_that_cannot_be_taken_fails_closed() {
    let scratch = Scratch::new("fresh-unstattable");
    let path = scratch.tokens_file();
    scratch.place_private(&path, &one_token("0a"));
    let mut watched = WatchedTokens::read(scratch.dirs());
    let directory = scratch.dirs().config_dir().to_path_buf();

    set_mode(&directory, 0o000);
    // A credential that bypasses directory modes (root, most commonly) can
    // still stat the file, so there is no failure for this case to observe.
    if std::fs::symlink_metadata(&path).is_ok() {
        set_mode(&directory, 0o700);
        return;
    }

    let check = watched.check();
    set_mode(&directory, 0o700);
    assert_eq!(check.fingerprints(), 2, "{check:?}");
    assert!(!check.read(), "no read follows a fingerprint that failed");
    assert!(check.replaced(), "{check:?}");
    assert!(watched.current().is_err());

    // The stat answers again: the file is read again, since nothing said it
    // was unchanged in between.
    assert_reread(watched.check());
    assert_eq!(laptop_secret(&watched), [0x0a]);
}

/// The declared limit, pinned so it is changed on purpose: the fingerprint is
/// the file's, so loosening the config directory alone is not a change to it.
/// The next change to the file itself is read under the directory's new mode,
/// and refused.
#[test]
fn a_mode_change_on_the_directory_alone_is_not_a_change_to_the_file() {
    let scratch = Scratch::new("fresh-directory-mode");
    let path = scratch.tokens_file();
    scratch.place_private(&path, &one_token("0a"));
    let mut watched = WatchedTokens::read(scratch.dirs());

    set_mode(scratch.dirs().config_dir(), 0o750);
    assert_unchanged(watched.check());
    assert_eq!(labels(&watched), ["laptop"]);

    past_a_tick();
    overwrite_in_place(&path, secret_offset(&one_token("0a")), b"0f");
    assert_reread(watched.check());
    assert!(
        matches!(
            watched.current(),
            Err(ConfigError::InsecurePermissions { .. })
        ),
        "{:?}",
        watched.current().map(|_| ())
    );
    set_mode(scratch.dirs().config_dir(), 0o700);
}

/// A fingerprint names what is at the name, a link included, and nothing at
/// all is `None`.
#[test]
#[allow(clippy::disallowed_methods)] // Harness scaffolding: planting a link at the token file's name.
fn a_fingerprint_names_what_is_at_the_name() {
    let scratch = Scratch::new("fingerprint");
    let dirs = scratch.dirs();
    let path = scratch.tokens_file();
    assert_eq!(tokens::fingerprint(dirs).expect("a fingerprint"), None);

    scratch.place_private(&path, &one_token("0a"));
    let file = tokens::fingerprint(dirs)
        .expect("a fingerprint")
        .expect("a file");
    assert_eq!(
        tokens::fingerprint(dirs).expect("a fingerprint"),
        Some(file),
        "an untouched file fingerprints the same"
    );

    remove(&path);
    std::os::unix::fs::symlink(scratch.config_base().join("elsewhere"), &path)
        .expect("planting a link");
    let link = tokens::fingerprint(dirs)
        .expect("a fingerprint of a dangling link")
        .expect("a link is something at the name");
    assert_ne!(link, file);
}
