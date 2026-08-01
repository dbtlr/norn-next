//! How a write becomes visible: the swap, and what it carries with it.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use norn_fs::{Precondition, Refusal, write};

use crate::common::{
    Scratch, bytes_at, demand_unwritable, exists, hash, identity_at, mode_at, names_in, set_mode,
};

/// **The bar on the swap.** A replacement publishes a new file rather than
/// editing the old one, so there is no moment at which the destination holds a
/// mixture.
///
/// Asserted structurally rather than raced for, because atomicity is not
/// something a race decides: an in-place rewrite is observably partial only for
/// the microseconds its bytes are going down, and a reader that never lands in
/// that window proves nothing. A replaced file has a new inode and an edited one
/// does not, and that difference holds on every run rather than on the lucky
/// ones.
#[test]
fn a_replacement_publishes_a_new_file_rather_than_editing_the_old_one() {
    let scratch = Scratch::new("swap-identity");
    let path = scratch.place("note.md", b"old");
    let before = identity_at(&path);

    write(
        &path,
        b"new bytes, and more of them",
        Precondition::Replace(hash(b"old")),
        scratch.shadows(),
    )
    .expect("a replacement");

    assert_ne!(
        identity_at(&path),
        before,
        "the destination kept its inode, so the write edited it in place"
    );
    assert_eq!(bytes_at(&path), b"new bytes, and more of them");
    assert_eq!(
        names_in(&scratch.vault()),
        vec!["note.md".to_string()],
        "the swap left something in the vault"
    );
}

/// A reader running beside a stream of replacements never sees anything but a
/// whole document.
///
/// This is a soak rather than a proof — see the structural bar above for the
/// claim itself. It is here for what it would surface: a read that found a
/// prefix, an absent path, or a shadow's name in the vault would fail loudly,
/// and none of those is something the structural assertion can observe.
///
/// The reader looks up what it saw in a set and pauses between reads, and the
/// round count is deliberately modest. A soak that spins at full speed over a
/// linear scan spends its wall time proving nothing new — the interleavings it
/// samples repeat long before the rounds do.
#[test]
fn a_reader_beside_a_stream_of_replacements_never_sees_a_partial_document() {
    const ROUNDS: usize = 50;
    let scratch = Scratch::new("swap-reader");
    let path = scratch.place("note.md", b"round 0");
    let contents: Vec<Vec<u8>> = (0..=ROUNDS)
        .map(|round| format!("round {round}").into_bytes())
        .collect();

    let writing = Arc::new(AtomicBool::new(true));
    let reader = {
        let path = path.clone();
        let writing = Arc::clone(&writing);
        let known: BTreeSet<Vec<u8>> = contents.iter().cloned().collect();
        thread::spawn(move || {
            let mut reads = 0usize;
            while writing.load(Ordering::Relaxed) {
                #[allow(clippy::disallowed_methods)] // Harness scaffolding: the concurrent reader.
                let seen = std::fs::read(&path).expect("the destination is always there");
                assert!(
                    known.contains(&seen),
                    "a reader saw {:?}, which is neither the previous document nor the new one",
                    String::from_utf8_lossy(&seen)
                );
                reads += 1;
                thread::sleep(Duration::from_micros(200));
            }
            reads
        })
    };

    for round in 0..ROUNDS {
        write(
            &path,
            &contents[round + 1],
            Precondition::Replace(hash(&contents[round])),
            scratch.shadows(),
        )
        .unwrap_or_else(|e| panic!("round {round}: {e}"));
    }
    writing.store(false, Ordering::Relaxed);
    let reads = reader.join().expect("the reader");

    assert!(reads > 0, "the reader never read");
    assert_eq!(bytes_at(&path), contents[ROUNDS]);
}

/// **The bar on mode preservation.** A replacement carries the replaced file's
/// permission mode forward; a create takes the umask's defaults, because there is
/// nothing to preserve.
///
/// The forbidden shape is a replacement that takes fresh defaults: a document
/// somebody deliberately made group-readable, or deliberately made private, comes
/// back with whatever the process's umask says. Replacement is the mechanism and
/// a surgical edit is the contract, and the mode is part of what "surgical"
/// means.
#[test]
fn a_replacement_carries_the_mode_forward_and_a_create_does_not() {
    let scratch = Scratch::new("mode");
    let path = scratch.place("note.md", b"old");
    set_mode(&path, 0o640);

    write(
        &path,
        b"new",
        Precondition::Replace(hash(b"old")),
        scratch.shadows(),
    )
    .expect("a replacement");
    assert_eq!(
        mode_at(&path),
        0o640,
        "the replacement took fresh defaults instead of the mode it replaced"
    );

    // And an unusual mode is carried just as faithfully, so the case above is
    // not passing because 0640 happens to be what a default would give.
    set_mode(&path, 0o600);
    write(
        &path,
        b"newer",
        Precondition::Replace(hash(b"new")),
        scratch.shadows(),
    )
    .expect("a second replacement");
    assert_eq!(mode_at(&path), 0o600);

    // A create has nothing to preserve, so it takes what an ordinary create
    // takes. The control is a plain write in the same directory under the same
    // umask, which is what "the defaults" means without naming a number.
    let created = scratch.at("fresh.md");
    write(&created, b"fresh", Precondition::Create, scratch.shadows())
        .expect("a create onto nothing");
    let control = scratch.place("control.md", b"control");
    assert_eq!(
        mode_at(&created),
        mode_at(&control),
        "a create did not take the mode an ordinary create takes"
    );
}

/// **The bar on the bits that are not carried.** A document with the
/// set-user-id or set-group-id bit set comes back with its permission bits and
/// without those, because the file the write publishes is owned by whoever ran
/// the write.
///
/// The forbidden shape is carrying the whole mode. A `04755` document owned by
/// one user is replaced by a `04755` file owned by another, and "run as the
/// file's owner" now names somebody who never asked for it — a privilege the
/// document did not have before Norn touched it. Ownership itself cannot be
/// carried at all, which is why dropping the bits is the only correct answer
/// rather than a cautious one.
#[test]
fn a_replacement_carries_permission_bits_without_the_setuid_bits() {
    let scratch = Scratch::new("mode-setuid");
    let path = scratch.place("note.md", b"old");
    set_mode(&path, 0o6755);
    assert_eq!(
        mode_at(&path),
        0o6755,
        "the filesystem did not keep the bits this case is about"
    );

    write(
        &path,
        b"new",
        Precondition::Replace(hash(b"old")),
        scratch.shadows(),
    )
    .expect("a replacement");

    assert_eq!(
        mode_at(&path),
        0o755,
        "the replacement carried a set-user-id or set-group-id bit onto a file it owns"
    );
}

/// A destination nothing may write to is still replaced, and its mode comes
/// forward with it.
///
/// Ordinary atomic-replace semantics: the rename needs the *directory*, and the
/// file's own bits have nothing to say about it. Stated as a case so that a
/// `chmod` on a document is not mistaken for protection against Norn — the
/// protection a caller has is the hash precondition, which is the whole point of
/// there being one.
#[test]
fn a_read_only_destination_is_still_replaced_with_its_mode_carried() {
    let scratch = Scratch::new("mode-read-only");
    let path = scratch.place("note.md", b"old");
    set_mode(&path, 0o444);

    write(
        &path,
        b"new",
        Precondition::Replace(hash(b"old")),
        scratch.shadows(),
    )
    .expect("a replacement of a read-only document");

    assert_eq!(bytes_at(&path), b"new");
    assert_eq!(mode_at(&path), 0o444);
}

/// A replacement into a directory nothing may write into refuses, and neither
/// the destination nor the shadow home is left holding anything.
///
/// The refusal is reached after the shadow exists — the rename is what the
/// directory's mode stops — so this is also the bar on cleanup after staging: a
/// failure path that returned without cleaning up would leave a copy of the
/// document in the shadow home on every refused write.
///
/// `ENOSPC` reaches the same arm; a permission is what a test can arrange.
#[test]
fn a_destination_in_an_unwritable_directory_is_an_environmental_refusal() {
    let scratch = Scratch::new("eacces-replace");
    let folder = scratch.directory("folder");
    let path = scratch.place("folder/note.md", b"old");
    set_mode(&folder, 0o500);
    demand_unwritable(&folder);

    let refusal = write(
        &path,
        b"new",
        Precondition::Replace(hash(b"old")),
        scratch.shadows(),
    )
    .expect_err("a rename into a directory nothing may write");

    assert!(
        matches!(
            &refusal,
            Refusal::Environment {
                operation: "renaming onto",
                kind: std::io::ErrorKind::PermissionDenied,
                ..
            }
        ),
        "{refusal}"
    );
    assert!(!refusal.is_os_error(libc::ENOSPC));

    set_mode(&folder, 0o755);
    assert_eq!(bytes_at(&path), b"old");
    assert!(scratch.shadow_names().is_empty());
}

/// A create into a directory nothing may write into refuses the same way, and
/// leaves no name behind.
#[test]
fn a_create_in_an_unwritable_directory_is_an_environmental_refusal() {
    let scratch = Scratch::new("eacces-create");
    let folder = scratch.directory("folder");
    set_mode(&folder, 0o500);
    demand_unwritable(&folder);
    let path = folder.join("fresh.md");

    let refusal = write(&path, b"fresh", Precondition::Create, scratch.shadows())
        .expect_err("a create into a directory nothing may write");

    assert!(
        matches!(
            &refusal,
            Refusal::Environment {
                operation: "creating",
                kind: std::io::ErrorKind::PermissionDenied,
                ..
            }
        ),
        "{refusal}"
    );
    set_mode(&folder, 0o755);
    assert!(!exists(&path));
}

/// A create whose parent directory is not there refuses as a missing path, which
/// is distinguishable from a denied one and from a taken name.
///
/// The kernel makes the shadow home and the lock's directory, both of which are
/// Norn's own. A vault directory is not: a document whose parent is missing is a
/// plan against a tree that is not the tree, and inventing the directory would be
/// this crate deciding what a vault's shape should be.
#[test]
fn a_destination_whose_parent_is_missing_refuses_as_a_missing_path() {
    let scratch = Scratch::new("missing-parent");
    let path = scratch.at("absent/fresh.md");
    let refusal = write(&path, b"fresh", Precondition::Create, scratch.shadows())
        .expect_err("a create under a directory that is not there");
    assert!(
        matches!(
            &refusal,
            Refusal::Environment {
                kind: std::io::ErrorKind::NotFound,
                ..
            }
        ),
        "{refusal}"
    );
    assert!(!exists(&scratch.at("absent")));
}
