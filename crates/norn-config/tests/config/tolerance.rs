//! The tolerant reader, on both files.
//!
//! Three behaviours, and every one of them exists because two binaries of
//! different ages read the same machine: a version ahead is refused, a version
//! behind is handled in memory without touching the file, and a field this
//! build does not model survives a rewrite by it.
//!
//! The symlink refusals are here too, because they are the same class of
//! question — what a reader does with a file it cannot make sense of — and the
//! answer is the same shape: a typed refusal, never a guess. A link to
//! nothing, a link to something, and a link one directory up are three
//! readings of a path, and each of them says which it was.

use norn_config::machine::tokens;
use norn_config::{ConfigDirs, ConfigError};

use crate::common::{Scratch, entry, label, name, registry};

/// Both files, so a case is never written for one and forgotten for the other.
fn both_files(scratch: &Scratch) -> [(&'static str, std::path::PathBuf); 2] {
    [
        ("registry", scratch.registry_file()),
        ("tokens", scratch.tokens_file()),
    ]
}

/// Reading a file written by a newer build and keeping only the parts this one
/// understands would make the next write a silent deletion of the newer
/// build's state.
#[test]
fn a_version_ahead_of_this_build_is_refused_on_both_files() {
    let scratch = Scratch::new("ahead");
    let dirs = scratch.dirs();
    for (which, path) in both_files(&scratch) {
        place_either(&scratch, which, &path, "version = 2\n");
        let error = read_either(which, dirs).expect_err("a file from a newer build");
        let ConfigError::VersionAhead {
            found, supported, ..
        } = error
        else {
            panic!("the {which} file was refused as something other than a version ahead");
        };
        assert_eq!((found, supported), (2, 1));
    }
}

/// A mutation refuses the same way. A writer that opened a newer file and
/// wrote it back at this build's version would destroy exactly what the read
/// refusal exists to protect.
#[test]
fn a_version_ahead_refuses_a_mutation_too() {
    let scratch = Scratch::new("ahead-mutate");
    let dirs = scratch.dirs();
    scratch.place(&scratch.registry_file(), "version = 2\n");
    let error = registry::mutate(dirs, |registry| {
        registry.insert(entry("notes", "/home/person/notes"));
        Ok(())
    })
    .expect_err("a mutation of a newer file");
    assert!(matches!(error, ConfigError::VersionAhead { .. }), "{error}");
    assert_eq!(scratch.text_at(&scratch.registry_file()), "version = 2\n");
}

/// A version below the first one names a format no release ever wrote, so
/// there is nothing to migrate it from. What matters here is that the
/// version-behind branch is a branch: it is reached, and it refuses with the
/// version it was given rather than falling through to a parse error.
#[test]
fn a_version_below_the_first_one_is_refused_with_the_version() {
    let scratch = Scratch::new("behind");
    let dirs = scratch.dirs();
    scratch.place(&scratch.registry_file(), "version = 0\n");
    let error = registry::read(dirs).expect_err("a version nobody wrote");
    let ConfigError::Corrupt { reason, .. } = error else {
        panic!("a version below the first one was refused as something other than corrupt");
    };
    assert!(reason.contains("version 0"), "{reason}");
}

/// A file with no bytes in it is not a machine with no vaults: an absent file
/// is a first run, and a file that was truncated is a file something went
/// wrong with. It is refused, and the recovery is a person editing it or
/// removing it — a mutation reads before it writes, so nothing here repairs it
/// silently.
#[test]
fn a_zero_byte_file_is_refused_rather_than_read_as_empty() {
    let scratch = Scratch::new("truncated");
    let dirs = scratch.dirs();
    scratch.place(&scratch.registry_file(), "");

    let error = registry::read(dirs).expect_err("a truncated registry");
    let ConfigError::Corrupt { reason, .. } = error else {
        panic!("a zero-byte file was refused as something other than corrupt");
    };
    assert!(reason.contains("version"), "{reason}");

    let error = registry::mutate(dirs, |registry| {
        registry.insert(entry("notes", "/home/person/notes"));
        Ok(())
    })
    .expect_err("a mutation of a truncated registry");
    assert!(matches!(error, ConfigError::Corrupt { .. }), "{error}");
    assert_eq!(scratch.text_at(&scratch.registry_file()), "");
}

/// A field this build does not model survives a rewrite by it — at the top
/// level and inside an entry alike. A reader that projected the file onto its
/// own struct and serialized the struct back would drop both.
#[test]
fn a_field_this_build_does_not_model_survives_a_rewrite() {
    let scratch = Scratch::new("unknown-fields");
    let dirs = scratch.dirs();
    scratch.place(
        &scratch.registry_file(),
        "version = 1\nfuture_top_level = \"kept\"\n\n\
         [vaults.notes]\nroot = \"/home/person/notes\"\nfuture_entry_field = 7\n\n\
         [future_section]\nsomething = true\n",
    );

    registry::mutate(dirs, |registry| {
        registry.insert(entry("work", "/home/person/work"));
        Ok(())
    })
    .expect("a registration beside fields this build does not model");

    let text = scratch.text_at(&scratch.registry_file());
    assert!(text.contains("future_top_level"), "{text}");
    assert!(text.contains("future_entry_field"), "{text}");
    assert!(text.contains("[future_section]"), "{text}");

    // And the entry the fields hang off is still readable, with its own known
    // fields intact.
    let read = registry::read(dirs).expect("the registry");
    assert_eq!(read.entries().count(), 2);
    assert_eq!(
        read.get(&name("notes")).expect("the entry").root.as_path(),
        std::path::Path::new("/home/person/notes")
    );
}

/// The same on the token file, where the stakes are a credential a newer build
/// scoped or dated in a way this one cannot read.
#[test]
fn a_token_field_this_build_does_not_model_survives_a_rewrite() {
    let scratch = Scratch::new("unknown-token-fields");
    let dirs = scratch.dirs();
    scratch.place_private(
        &scratch.tokens_file(),
        "version = 1\n\n[tokens.laptop]\nsecret = \"0a0b\"\ncreated = 10\nscope = \"read\"\n",
    );

    tokens::mutate(dirs, |tokens| tokens.add(&label("desktop"), b"\x01\x02"))
        .expect("a second token");

    let text = scratch.text_at(&scratch.tokens_file());
    assert!(text.contains("scope = \"read\""), "{text}");
    assert!(text.contains("0a0b"), "{text}");
    assert!(text.contains("0102"), "{text}");
}

/// A read is a read. A file written by another build is not rewritten by
/// looking at it: a plain listing on a machine that also runs an older service
/// must leave the bytes under it exactly as they were.
#[test]
fn a_read_never_rewrites_the_file() {
    let scratch = Scratch::new("read-only");
    let dirs = scratch.dirs();
    let original =
        "version = 1\nfuture = 1\n\n[vaults.notes]\nroot = \"/home/person/notes\"\nfuture = 2\n";
    scratch.place(&scratch.registry_file(), original);

    registry::read(dirs).expect("the registry");
    registry::read(dirs).expect("the registry again");

    assert_eq!(scratch.text_at(&scratch.registry_file()), original);
    assert_eq!(
        scratch.names_in(dirs.config_dir()),
        vec!["registry.toml".to_string()],
        "a read left something beside the file"
    );
}

/// A symlink pointing at nothing and a file that is not there fail an open
/// identically, and they mean opposite things: nothing there is a first run,
/// and a link to nothing is state that moved or was half removed. Reading the
/// second as the first would answer "no vaults are registered" to a machine
/// whose registry is one restored symlink away.
#[test]
fn a_dangling_symlink_refuses_rather_than_reading_as_absent() {
    let scratch = Scratch::new("dangling");
    let dirs = scratch.dirs();
    for (which, path) in both_files(&scratch) {
        let config = scratch.make_config_dir();
        link(&config.join("gone"), &path);
        let error = read_either(which, dirs).expect_err("a link to nothing");
        let ConfigError::DanglingSymlink { path: refused } = error else {
            panic!("the {which} file was refused as something other than a dangling symlink");
        };
        assert_eq!(refused, path);

        let error = mutate_either(which, dirs).expect_err("a mutation through a link to nothing");
        assert!(
            matches!(error, ConfigError::DanglingSymlink { .. }),
            "the {which} file: {error}"
        );
        unlink(&path);
    }
}

/// A link that resolves is refused too, and says so in its own terms. These
/// files are machine state written by replacement: a read would take bytes
/// from a file nothing here governs, and the next write would rename over the
/// link rather than through it — so the two directions would disagree about
/// where the state is. Refusing both is the only reading that stays true.
#[test]
fn a_symlinked_data_file_refuses_in_both_directions() {
    let scratch = Scratch::new("linked");
    let dirs = scratch.dirs();
    for (which, path) in both_files(&scratch) {
        let config = scratch.make_config_dir();
        let elsewhere = config.join("elsewhere");
        place_either(&scratch, which, &elsewhere, "version = 1\n");
        link(&elsewhere, &path);

        let error = read_either(which, dirs).expect_err("a link to a real file");
        let ConfigError::SymlinkedFile { path: refused } = error else {
            panic!("the {which} file was refused as something other than a symlinked file");
        };
        assert_eq!(refused, path);

        let error = mutate_either(which, dirs).expect_err("a mutation through a link");
        assert!(
            matches!(error, ConfigError::SymlinkedFile { .. }),
            "the {which} file: {error}"
        );

        unlink(&path);
        unlink(&elsewhere);
    }
}

/// The refusal is about the path, not only about its last component. A
/// directory symlink pointing at nothing fails the open of everything under it
/// exactly the way an empty machine does, and answering "no vaults are
/// registered" to a machine whose config directory has moved is the same
/// mistake one level up.
#[test]
fn a_dangling_ancestor_refuses_rather_than_reading_as_absent() {
    let scratch = Scratch::new("dangling-ancestor");
    let dirs = scratch.dirs();
    link(&scratch.config_base().join("gone"), dirs.config_dir());

    for which in ["registry", "tokens"] {
        let error = read_either(which, dirs).expect_err("a config directory that moved");
        let ConfigError::DanglingSymlink { path: refused } = error else {
            panic!("the {which} file was refused as something other than a dangling symlink");
        };
        assert_eq!(refused, dirs.config_dir());

        // The write direction says the same thing. Building the directory
        // would fail with "file exists", which is true and useless.
        let error =
            mutate_either(which, dirs).expect_err("a mutation under a directory that moved");
        assert!(
            matches!(error, ConfigError::DanglingSymlink { .. }),
            "the {which} file: {error}"
        );
    }
    unlink(dirs.config_dir());
}

fn read_either(which: &str, dirs: &ConfigDirs) -> Result<(), ConfigError> {
    match which {
        "registry" => registry::read(dirs).map(|_| ()),
        _ => tokens::read(dirs).map(|_| ()),
    }
}

fn mutate_either(which: &str, dirs: &ConfigDirs) -> Result<(), ConfigError> {
    match which {
        "registry" => registry::mutate(dirs, |registry| {
            registry.insert(entry("notes", "/home/person/notes"));
            Ok(())
        }),
        _ => tokens::mutate(dirs, |tokens| tokens.add(&label("laptop"), b"secret")),
    }
}

/// The token file's permission check runs ahead of everything else it could
/// refuse, so a case about the version has to place it at the mode a token
/// file is read at.
fn place_either(scratch: &Scratch, which: &str, path: &std::path::Path, text: &str) {
    match which {
        "registry" => scratch.place(path, text),
        _ => scratch.place_private(path, text),
    }
}

#[allow(clippy::disallowed_methods)] // Harness scaffolding: arranging a link the API never writes.
fn link(target: &std::path::Path, at: &std::path::Path) {
    std::os::unix::fs::symlink(target, at).expect("a symlink");
}

#[allow(clippy::disallowed_methods)] // Harness scaffolding: clearing what the case arranged.
fn unlink(path: &std::path::Path) {
    std::fs::remove_file(path).expect("removing a link");
}
