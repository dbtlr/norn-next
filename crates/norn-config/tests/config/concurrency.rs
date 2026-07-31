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

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;

use norn_config::registry;

use crate::common::{Scratch, entry, name};

const WRITERS: usize = 8;
const PER_WRITER: usize = 25;

/// Every writer's registration survives. A read-modify-write that dropped its
/// lock between the read and the write would lose most of these: each writer
/// reads a file, adds one entry, and writes the whole file back.
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
        read.len(),
        WRITERS * PER_WRITER,
        "entries were lost between a read and the write that followed it"
    );
    assert!(read.get(&name("w0-0")).is_some());
    assert!(
        read.get(&name(&format!("w{}-{}", WRITERS - 1, PER_WRITER - 1)))
            .is_some()
    );
}

/// The number of entries the registry is seeded with before the crash-window
/// case starts writing.
///
/// Large enough that each rewrite is tens of kilobytes rather than a line or
/// two: a file replaced in place rather than renamed into place is only
/// observably partial while its bytes are going down, and a one-line file
/// closes that window before a reader can land in it.
const SEEDED: usize = 400;

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
            counts.push(registry.len());
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
        registry::read(&dirs).expect("the registry").len(),
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
        inodes.push(inode_of(&dirs.registry_file()));
        // And what is at the name is always a whole document.
        assert_eq!(
            registry::read(dirs).expect("a whole registry").len(),
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
    let mut names = scratch.names_in(dirs.config_dir());
    names.sort();
    assert_eq!(
        names,
        vec![
            ".registry.toml.lock".to_string(),
            "registry.toml".to_string()
        ],
        "the write protocol left something behind"
    );
}
