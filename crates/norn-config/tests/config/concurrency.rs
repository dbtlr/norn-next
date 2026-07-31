//! What two writers and a reader do to each other, which is nothing.
//!
//! Two claims are made here, and they are the two the write protocol exists
//! for:
//!
//! - **A lost update is structurally impossible.** The exclusive lock is held
//!   from before the read to after the replacement, so no writer's
//!   read-modify-write can begin inside another's. Without it, a race of `n`
//!   writers over one file produces somewhere between one and `n` results; with
//!   it, it produces `n`.
//! - **A reader never sees a half-written file.** Bytes land in a temporary
//!   file and a rename puts them at the real name, so a reader arriving at any
//!   moment sees a whole file — the previous one or the new one, never a
//!   prefix and never the temporary name.
//!
//! The two claims are tested differently on purpose. The lock is what a race
//! decides, so it is raced for: `n` writers each adding one entry produce `n`
//! entries, which they would not if a lock were dropped between the read and
//! the write. Atomicity is **not** something a race decides — an in-place
//! rewrite is observably partial only for the microseconds its bytes are going
//! down, and a reader that never lands in that window proves nothing — so it is
//! asserted structurally instead: a replaced file has a new inode, an edited
//! one does not, and that difference holds on every run rather than on the
//! lucky ones. The concurrent reader is kept beside it as a soak, for the
//! refusals it would surface rather than for the claim it cannot make.
//!
//! The racers are threads rather than processes. An `flock` follows the open
//! file description rather than the process, and each writer here opens the
//! lock file for itself, so two threads contend on exactly the terms two
//! processes do — and the harness gets to fail loudly rather than through an
//! exit code.
//!
//! What the writer leaves behind is here for the same reason, because it is
//! the same protocol seen from the other side: a temporary file a dead writer
//! left, a lock name somebody planted a link at, a mutation with nothing to
//! say, and a mutation inside a mutation.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;

use norn_config::ConfigError;
use norn_config::machine::tokens;
use norn_config::registry;

use crate::common::{Scratch, entry, label, name};

const WRITERS: usize = 4;
const PER_WRITER: usize = 10;

/// Every writer's addition survives. A read-modify-write that dropped its lock
/// between the read and the write would lose most of these: each writer reads a
/// file, adds one label, and writes the whole file back.
#[test]
fn concurrent_token_additions_all_land() {
    let scratch = Scratch::new("token-race");
    let dirs = scratch.dirs().clone();

    thread::scope(|scope| {
        for writer in 0..WRITERS {
            let dirs = dirs.clone();
            scope.spawn(move || {
                for round in 0..PER_WRITER {
                    tokens::mutate(&dirs, |tokens| {
                        tokens.add(
                            &label(&format!("w{writer}-{round}")),
                            &[writer as u8, round as u8],
                        )
                    })
                    .expect("a token");
                }
            });
        }
    });

    let read = tokens::read(&dirs).expect("the tokens");
    assert_eq!(
        read.all().count(),
        WRITERS * PER_WRITER,
        "labels were lost between a read and the write that followed it"
    );
    for writer in 0..WRITERS {
        for round in 0..PER_WRITER {
            let wanted = format!("w{writer}-{round}");
            let token = read
                .all()
                .find(|token| token.label().as_str() == wanted)
                .unwrap_or_else(|| panic!("`{wanted}` was lost"));
            assert_eq!(token.secret(), &[writer as u8, round as u8]);
        }
    }
}

/// The same over the registry, whose entries are a different table in a
/// different file under a different mode — and the same lock.
#[test]
fn concurrent_registrations_all_land() {
    let scratch = Scratch::new("registry-race");
    let dirs = scratch.dirs().clone();

    thread::scope(|scope| {
        for writer in 0..WRITERS {
            let dirs = dirs.clone();
            scope.spawn(move || {
                for round in 0..PER_WRITER {
                    registry::mutate(&dirs, |registry| {
                        registry.insert(entry(
                            &format!("w{writer}-{round}"),
                            &format!("/vaults/w{writer}/{round}"),
                        ));
                        Ok(())
                    })
                    .expect("a registration");
                }
            });
        }
    });

    let read = registry::read(&dirs).expect("the registry");
    assert_eq!(
        read.entries().count(),
        WRITERS * PER_WRITER,
        "entries were lost between a read and the write that followed it"
    );
    assert!(read.get(&name("w0-0")).is_some());
    assert!(
        read.get(&name(&format!("w{}-{}", WRITERS - 1, PER_WRITER - 1)))
            .is_some()
    );
}

/// The number of entries the registry is seeded with before the reader soak
/// starts writing.
///
/// Enough that a rewrite is a document rather than a line — the refusals this
/// soak is here to catch are met while a file is being replaced, and a file of
/// one line is replaced too quickly to be caught doing anything. It is not
/// sized to win a race: the structural claim is
/// [`every_write_replaces_the_file_rather_than_editing_it_in_place`], and no
/// seed count would make this case prove it.
const SEEDED: usize = 32;

/// A reader running flat out beside a writer never gets a refusal. Every
/// reading is a whole registry — the previous one or the new one — and the
/// count it reports is one the writer actually wrote.
///
/// A soak rather than a proof: what it would catch is a refusal a reader has
/// no business meeting, and what it cannot catch is a window too small to land
/// in. The structural claim is
/// [`every_write_replaces_the_file_rather_than_editing_it_in_place`].
#[test]
fn a_reader_beside_a_writer_never_sees_a_half_written_file() {
    let scratch = Scratch::new("crash-window");
    let dirs = scratch.dirs().clone();

    registry::mutate(&dirs, |registry| {
        for seed in 0..SEEDED {
            registry.insert(entry(&format!("s{seed}"), &format!("/vaults/seed/{seed}")));
        }
        Ok(())
    })
    .expect("a seeded registry");

    let writing = Arc::new(AtomicBool::new(true));
    let reader_dirs = dirs.clone();
    let reader_flag = Arc::clone(&writing);
    let reader = thread::spawn(move || {
        let mut seen = 0usize;
        let mut counts = Vec::new();
        while reader_flag.load(Ordering::Relaxed) {
            let registry = registry::read(&reader_dirs)
                .expect("a reading beside a writer is always a whole file");
            counts.push(registry.entries().count());
            seen += 1;
        }
        (seen, counts)
    });

    let rounds = 100;
    for round in 0..rounds {
        registry::mutate(&dirs, |registry| {
            registry.insert(entry(&format!("v{round}"), &format!("/vaults/{round}")));
            Ok(())
        })
        .expect("a registration");
    }
    writing.store(false, Ordering::Relaxed);

    let (seen, counts) = reader.join().expect("the reader");
    assert!(seen > 0, "the reader never got a reading in");
    for count in counts {
        assert!(
            (SEEDED..=SEEDED + rounds).contains(&count),
            "the reader saw {count} entries, which no write ever left behind"
        );
    }
    assert_eq!(
        registry::read(&dirs)
            .expect("the registry")
            .entries()
            .count(),
        SEEDED + rounds
    );
}

/// Every write **replaces** the file rather than editing it: the inode behind
/// the name is a new one each time.
///
/// This is the crash-window claim stated as something decidable rather than
/// raced for. A file edited in place is truncated before its new bytes go
/// down, so there is a moment at which the real name holds a partial document
/// and a reader holding it open sees one; a file renamed into place has no
/// such moment, because the bytes were complete and on the disk before the
/// name ever pointed at them, and a reader that opened the old one keeps a
/// whole file for as long as it holds the handle. A changed inode is what
/// distinguishes the two, and it distinguishes them on every run rather than
/// on the runs where a reader gets lucky.
#[test]
fn every_write_replaces_the_file_rather_than_editing_it_in_place() {
    let scratch = Scratch::new("replacement");
    let dirs = scratch.dirs();

    let mut inodes = Vec::new();
    for round in 0..4 {
        registry::mutate(dirs, |registry| {
            registry.insert(entry(&format!("v{round}"), &format!("/vaults/{round}")));
            Ok(())
        })
        .expect("a registration");
        inodes.push(inode_of(&scratch.registry_file()));
        // And what is at the name is always a whole document.
        assert_eq!(
            registry::read(dirs)
                .expect("a whole registry")
                .entries()
                .count(),
            round + 1
        );
    }

    for pair in inodes.windows(2) {
        assert_ne!(
            pair[0], pair[1],
            "the file was edited in place, so there is a moment at which its name holds a \
             partial document"
        );
    }
}

/// The same for the token file, whose in-place rewrite would expose a
/// truncated credential rather than a truncated list of vaults.
#[test]
fn every_token_write_replaces_the_file_too() {
    let scratch = Scratch::new("token-replacement");
    let dirs = scratch.dirs();

    tokens::mutate(dirs, |tokens| tokens.add(&label("first"), b"a")).expect("a token");
    let first = inode_of(&scratch.tokens_file());
    tokens::mutate(dirs, |tokens| tokens.add(&label("second"), b"b")).expect("a second token");
    assert_ne!(first, inode_of(&scratch.tokens_file()));
}

#[allow(clippy::disallowed_methods)] // Harness scaffolding: judging what the write protocol did to the name.
fn inode_of(path: &std::path::Path) -> u64 {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata(path)
        .unwrap_or_else(|e| panic!("reading {}: {e}", path.display()))
        .ino()
}

/// The temporary file is never at the real name and never left behind. Its
/// name is distinct, so no reader can open it by asking for the registry, and
/// a completed write leaves the directory holding the file and the lock and
/// nothing else.
#[test]
fn the_write_protocol_leaves_no_temporary_behind() {
    let scratch = Scratch::new("no-temporaries");
    let dirs = scratch.dirs();

    for round in 0..5 {
        registry::mutate(dirs, |registry| {
            registry.insert(entry(&format!("v{round}"), &format!("/vaults/{round}")));
            Ok(())
        })
        .expect("a registration");
    }
    tokens::mutate(dirs, |tokens| tokens.add(&label("laptop"), b"secret")).expect("a token");

    let mut names = scratch.names_in(dirs.config_dir());
    names.sort();
    assert_eq!(
        names,
        vec![
            ".registry.toml.lock".to_string(),
            ".tokens.toml.lock".to_string(),
            "registry.toml".to_string(),
            "tokens.toml".to_string(),
        ],
        "the write protocol left something behind"
    );
}

/// A writer that died before its rename left a whole secret in a temporary
/// file. The next writer holds the lock — so there is no live writer to
/// confuse it with — and clears it.
#[test]
fn a_dead_writers_temporary_is_swept_by_the_next_one() {
    let scratch = Scratch::new("orphan-sweep");
    let dirs = scratch.dirs();
    let orphan = scratch.make_config_dir().join(".tokens.toml.tmp-1234-0");
    scratch.place_private(&orphan, "version = 1\n\n[tokens.ghost]\nsecret = \"0a\"\n");

    tokens::mutate(dirs, |tokens| tokens.add(&label("laptop"), b"secret")).expect("a token");

    assert!(
        !scratch
            .names_in(dirs.config_dir())
            .iter()
            .any(|name| name.starts_with(".tokens.toml.tmp-")),
        "a secret was left behind in a temporary file"
    );
}

/// The lock file is opened without following a link. A link planted at the
/// lock name would otherwise hand this writer's lock — and the `0600` file it
/// creates — to a path nothing here guards, and every writer would then be
/// excluding each other somewhere else.
#[test]
fn a_symlink_at_the_lock_name_is_refused_rather_than_followed() {
    let scratch = Scratch::new("lock-link");
    let dirs = scratch.dirs();
    let config = scratch.make_config_dir();
    let elsewhere = config.join("elsewhere");
    scratch.place(&elsewhere, "");
    link(&elsewhere, &config.join(".registry.toml.lock"));

    let error = registry::mutate(dirs, |registry| {
        registry.insert(entry("notes", "/home/person/notes"));
        Ok(())
    })
    .expect_err("a link at the lock name");
    let ConfigError::Io { operation, .. } = error else {
        panic!("a link at the lock name was refused as something other than an i/o failure");
    };
    assert_eq!(operation, "opening");
}

#[allow(clippy::disallowed_methods)] // Harness scaffolding: planting a link the API never writes.
fn link(target: &std::path::Path, at: &std::path::Path) {
    std::os::unix::fs::symlink(target, at).expect("a symlink");
}

/// A temporary belonging to another file is not this file's residue, and a
/// sweep keyed by the target's name never touches it. The registry and the
/// token file share a directory.
#[test]
fn a_sweep_leaves_the_other_files_temporaries_alone() {
    let scratch = Scratch::new("orphan-neighbour");
    let dirs = scratch.dirs();
    let neighbour = scratch.make_config_dir().join(".tokens.toml.tmp-1234-0");
    scratch.place_private(&neighbour, "version = 1\n");

    registry::mutate(dirs, |registry| {
        registry.insert(entry("notes", "/home/person/notes"));
        Ok(())
    })
    .expect("a registration");

    assert!(
        scratch
            .names_in(dirs.config_dir())
            .contains(&".tokens.toml.tmp-1234-0".to_string()),
        "the registry's sweep removed the token file's temporary"
    );
}

/// A mutation that changes nothing writes nothing. The file keeps its inode,
/// which is the same fact the replacement cases read the other way: no
/// temporary is taken, no window is opened, and nothing that was tightened by
/// hand is undone — for a mutation that had nothing to say.
#[test]
fn a_mutation_that_changes_nothing_leaves_the_file_untouched() {
    let scratch = Scratch::new("no-op");
    let dirs = scratch.dirs();

    registry::mutate(dirs, |registry| {
        registry.insert(entry("notes", "/home/person/notes"));
        Ok(())
    })
    .expect("a registration");
    let before = inode_of(&scratch.registry_file());
    let text = scratch.text_at(&scratch.registry_file());

    registry::mutate(dirs, |_| Ok(())).expect("a mutation that says nothing");
    registry::mutate(dirs, |registry| {
        // Removing an entry that is not there, and re-inserting one that is.
        registry.remove(&name("absent"));
        registry.insert(entry("notes", "/home/person/notes"));
        Ok(())
    })
    .expect("a mutation that changes nothing");

    assert_eq!(inode_of(&scratch.registry_file()), before);
    assert_eq!(scratch.text_at(&scratch.registry_file()), text);
}

/// A mutation inside a mutation of the same file would wait on its own
/// caller's exclusive lock for as long as the process lives. It is refused
/// with the path it would have waited on.
#[test]
fn a_mutation_inside_a_mutation_of_the_same_file_is_refused() {
    let scratch = Scratch::new("re-entry");
    let dirs = scratch.dirs();

    let nested = registry::mutate(dirs, |registry| {
        registry.insert(entry("notes", "/home/person/notes"));
        Ok(registry::mutate(dirs, |inner| {
            inner.insert(entry("work", "/home/person/work"));
            Ok(())
        }))
    })
    .expect("the outer mutation");
    let error = nested.expect_err("the inner mutation");
    assert!(
        matches!(error, ConfigError::NestedMutation { .. }),
        "{error}"
    );

    // The outer mutation still landed, and the guard is released with it.
    assert_eq!(
        registry::read(dirs)
            .expect("the registry")
            .entries()
            .count(),
        1
    );
    registry::mutate(dirs, |registry| {
        registry.insert(entry("work", "/home/person/work"));
        Ok(())
    })
    .expect("a mutation after the guard was released");
}

/// The guard is one file's, not the whole crate's: the token file has its own
/// lock, and a mutation of one inside a mutation of the other waits on
/// nothing.
#[test]
fn a_mutation_of_the_other_file_inside_one_is_allowed() {
    let scratch = Scratch::new("re-entry-other");
    let dirs = scratch.dirs();

    registry::mutate(dirs, |registry| {
        registry.insert(entry("notes", "/home/person/notes"));
        tokens::mutate(dirs, |tokens| tokens.add(&label("laptop"), b"secret"))
    })
    .expect("a token written inside a registration");

    assert_eq!(tokens::read(dirs).expect("the tokens").all().count(), 1);
    assert_eq!(
        registry::read(dirs)
            .expect("the registry")
            .entries()
            .count(),
        1
    );
}
