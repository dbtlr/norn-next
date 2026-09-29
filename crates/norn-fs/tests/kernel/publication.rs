//! How a staged target becomes visible: publication, what it checks again,
//! and what it carries with it.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use norn_fs::{AfterState, Durability, Refusal, Staging, Transition, confirm_landed};

use crate::common::{
    Scratch, bytes_at, demand_unwritable, exists, found, hash, identity_at, mode_at, mtime_at,
    names_in, set_mode, staged,
};

/// A create publishes its content at a name that had nothing at it, reports
/// the identity of what it published, and reports the folder synced.
#[test]
fn a_create_lands_and_reports_its_identity() {
    let scratch = Scratch::new("create-lands");
    let published = scratch
        .stage_and_publish(
            "fresh.md",
            Transition::Create {
                content: b"fresh bytes",
            },
        )
        .expect("a create onto nothing");

    let path = scratch.at("fresh.md");
    let AfterState::Present(state) = published.after else {
        panic!("a create reported {:?}", published.after);
    };
    assert_eq!(bytes_at(&path), b"fresh bytes");
    assert_eq!(state.content_hash, hash(b"fresh bytes"));
    assert_eq!(state.len, b"fresh bytes".len() as u64);
    assert_eq!((state.dev, state.ino), identity_at(&path));
    assert_eq!(
        state.mtime,
        mtime_at(&path),
        "the reported mtime is not the file's"
    );
    assert!(
        matches!(published.durability, Durability::Synced),
        "{:?}",
        published.durability
    );
    assert!(
        scratch.shadow_names().is_empty(),
        "the shadow was not consumed"
    );
}

/// A replacement lands, publishes exactly the composed bytes, and reports an
/// identity that matches what is at the path.
///
/// Every field of the reported post-state is judged against a fresh look at the
/// file. A field that is a constant, or a copy of what the caller passed, is a
/// field a suppression path cannot use.
#[test]
fn a_replacement_lands_and_reports_what_it_published() {
    let scratch = Scratch::new("replace-lands");
    let path = scratch.place("note.md", b"old");
    let published = scratch
        .stage_and_publish(
            "note.md",
            Transition::Replace {
                before: hash(b"old"),
                content: b"new bytes",
            },
        )
        .expect("a replacement");

    let AfterState::Present(state) = published.after else {
        panic!("a replacement reported {:?}", published.after);
    };
    assert_eq!(bytes_at(&path), b"new bytes");
    assert_eq!(state.content_hash, hash(b"new bytes"));
    assert_eq!(state.len, b"new bytes".len() as u64);
    assert_eq!((state.dev, state.ino), identity_at(&path));
    assert_eq!(state.mtime, mtime_at(&path));
    assert!(matches!(published.durability, Durability::Synced));
}

/// A removal under its before-state takes the document and reports absence.
#[test]
fn a_removal_takes_the_document_and_reports_absence() {
    let scratch = Scratch::new("remove-lands");
    let path = scratch.place("folder/note.md", b"to be removed");

    let published = scratch
        .stage_and_publish(
            "folder/note.md",
            Transition::Remove {
                before: hash(b"to be removed"),
            },
        )
        .expect("a removal");

    assert!(!exists(&path));
    assert!(
        matches!(published.after, AfterState::Absent),
        "{published:?}"
    );
    assert!(matches!(published.durability, Durability::Synced));
    assert!(exists(&scratch.at("folder")), "a removal took its folder");
}

/// **The bar on the swap.** A replacement publishes a new file rather than
/// editing the old one, so there is no moment at which the target holds a
/// mixture.
///
/// Asserted structurally rather than raced for: a replaced file has a new inode
/// and an edited one does not, and that difference holds on every run.
#[test]
fn a_replacement_publishes_a_new_file_rather_than_editing_the_old_one() {
    let scratch = Scratch::new("swap-identity");
    let path = scratch.place("note.md", b"old");
    let before = identity_at(&path);

    let _ = scratch
        .stage_and_publish(
            "note.md",
            Transition::Replace {
                before: hash(b"old"),
                content: b"new bytes, and more of them",
            },
        )
        .expect("a replacement");

    assert_ne!(
        identity_at(&path),
        before,
        "the target kept its inode, so the write edited it in place"
    );
    assert_eq!(bytes_at(&path), b"new bytes, and more of them");
    assert_eq!(
        names_in(&scratch.vault()),
        vec!["note.md".to_string()],
        "publication left something in the vault"
    );
}

/// A reader running beside a stream of replacements never sees anything but a
/// whole document.
///
/// A soak rather than a proof — the structural bar above is the claim. It is
/// here for what it would surface: a read that found a prefix, an absent path,
/// or a shadow's name in the vault.
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
            loop {
                #[allow(clippy::disallowed_methods)] // Harness scaffolding: the concurrent reader.
                let seen = std::fs::read(&path).expect("the target is always there");
                assert!(
                    known.contains(&seen),
                    "a reader saw {:?}, which is neither the previous document nor the new one",
                    String::from_utf8_lossy(&seen)
                );
                reads += 1;
                if !writing.load(Ordering::Relaxed) {
                    break;
                }
                thread::sleep(Duration::from_micros(200));
            }
            reads
        })
    };

    for round in 0..ROUNDS {
        let _ = scratch
            .stage_and_publish(
                "note.md",
                Transition::Replace {
                    before: hash(&contents[round]),
                    content: &contents[round + 1],
                },
            )
            .unwrap_or_else(|e| panic!("round {round}: {e}"));
    }
    writing.store(false, Ordering::Relaxed);
    let reads = reader.join().expect("the reader");

    assert!(reads > 0, "the reader never read");
    assert_eq!(bytes_at(&path), contents[ROUNDS]);
}

/// **The bar on mode preservation.** A replacement carries the replaced file's
/// permission mode forward; a create takes the umask's defaults, because there
/// is nothing to preserve.
#[test]
fn a_replacement_carries_the_mode_forward_and_a_create_does_not() {
    let scratch = Scratch::new("mode");
    let path = scratch.place("note.md", b"old");
    set_mode(&path, 0o640);

    let _ = scratch
        .stage_and_publish(
            "note.md",
            Transition::Replace {
                before: hash(b"old"),
                content: b"new",
            },
        )
        .expect("a replacement");
    assert_eq!(
        mode_at(&path),
        0o640,
        "the replacement took fresh defaults instead of the mode it replaced"
    );

    set_mode(&path, 0o600);
    let _ = scratch
        .stage_and_publish(
            "note.md",
            Transition::Replace {
                before: hash(b"new"),
                content: b"newer",
            },
        )
        .expect("a second replacement");
    assert_eq!(mode_at(&path), 0o600);

    // A create takes what an ordinary create takes: the control is a plain
    // write in the same directory under the same umask.
    let _ = scratch
        .stage_and_publish("fresh.md", Transition::Create { content: b"fresh" })
        .expect("a create onto nothing");
    let control = scratch.place("control.md", b"control");
    assert_eq!(
        mode_at(&scratch.at("fresh.md")),
        mode_at(&control),
        "a create did not take the mode an ordinary create takes"
    );
}

/// **The bar on the bits that are not carried.** A document with the
/// set-user-id or set-group-id bit set comes back with its permission bits and
/// without those, because the file publication puts there is owned by whoever
/// published it.
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

    let _ = scratch
        .stage_and_publish(
            "note.md",
            Transition::Replace {
                before: hash(b"old"),
                content: b"new",
            },
        )
        .expect("a replacement");

    assert_eq!(mode_at(&path), 0o755);
}

/// A target nothing may write to is still replaced, and its mode comes forward
/// with it: the rename needs the *directory*, and the file's own bits have
/// nothing to say about it.
#[test]
fn a_read_only_target_is_still_replaced_with_its_mode_carried() {
    let scratch = Scratch::new("mode-read-only");
    let path = scratch.place("note.md", b"old");
    set_mode(&path, 0o444);

    let _ = scratch
        .stage_and_publish(
            "note.md",
            Transition::Replace {
                before: hash(b"old"),
                content: b"new",
            },
        )
        .expect("a replacement of a read-only document");

    assert_eq!(bytes_at(&path), b"new");
    assert_eq!(mode_at(&path), 0o444);
}

/// A publication into a folder nothing may write into refuses for every kind,
/// leaves the target as it was, and takes its shadow with it.
///
/// The refusal is reached after the shadow exists — the rename or the unlink
/// is what the folder's mode stops — so this is also the bar on cleanup after
/// staging.
#[test]
fn a_publication_into_an_unwritable_folder_is_an_environmental_refusal() {
    let scratch = Scratch::new("eacces");
    let folder = scratch.directory("folder");
    let document = scratch.place("folder/note.md", b"old");
    let staged = [
        staged(
            scratch
                .stage(
                    "folder/note.md",
                    Transition::Replace {
                        before: hash(b"old"),
                        content: b"new",
                    },
                )
                .expect("a replacement stages"),
        ),
        staged(
            scratch
                .stage("folder/fresh.md", Transition::Create { content: b"fresh" })
                .expect("a create stages"),
        ),
        staged(
            scratch
                .stage(
                    "folder/note.md",
                    Transition::Remove {
                        before: hash(b"old"),
                    },
                )
                .expect("a removal stages"),
        ),
    ];
    set_mode(&folder, 0o500);
    demand_unwritable(&folder);

    for (staged, operation) in
        staged
            .into_iter()
            .zip(["renaming onto", "renaming onto", "removing"])
    {
        let refusal = scratch
            .publish(staged)
            .expect_err("a publication into a folder nothing may write");
        assert!(
            matches!(
                &refusal,
                Refusal::Environment {
                    operation: named,
                    kind: std::io::ErrorKind::PermissionDenied,
                    ..
                } if *named == operation
            ),
            "{refusal}"
        );
    }

    set_mode(&folder, 0o755);
    assert_eq!(bytes_at(&document), b"old");
    assert!(!exists(&scratch.at("folder/fresh.md")));
    assert!(
        scratch.shadow_names().is_empty(),
        "a refused publication left its shadow"
    );
}

/// **The bar on re-verification.** A foreign edit that lands between staging
/// and publication refuses at publication, for every kind that read the
/// target, and the foreign bytes are what remain.
///
/// No handle is held between the phases, so the check at publication is a
/// fresh one. The forbidden shape is a publication that trusts what staging
/// saw: the foreign edit is silently overwritten or removed.
#[test]
fn a_foreign_edit_after_staging_refuses_at_publication() {
    let scratch = Scratch::new("foreign-edit");
    for transition in [
        Transition::Replace {
            before: hash(b"old"),
            content: b"ours",
        },
        Transition::Remove {
            before: hash(b"old"),
        },
    ] {
        let path = scratch.place("note.md", b"old");
        let staged = staged(scratch.stage("note.md", transition).expect("staged"));
        scratch.place("note.md", b"theirs");

        let refusal = scratch
            .publish(staged)
            .expect_err("a foreign edit after staging");

        let Refusal::Drifted {
            expected, observed, ..
        } = &refusal
        else {
            panic!("a foreign edit was not reported as drift: {refusal}");
        };
        assert_eq!(*expected, hash(b"old"));
        assert_eq!(
            observed.as_ref().expect("the observed state").content_hash,
            hash(b"theirs")
        );
        assert_eq!(
            bytes_at(&path),
            b"theirs",
            "publication ran over the foreign edit"
        );
        assert!(
            scratch.shadow_names().is_empty(),
            "a refused publication left its shadow"
        );
    }
}

/// A document removed between staging and publication is drift onto nothing,
/// and a replacement does not resurrect it.
#[test]
fn a_document_removed_after_staging_is_not_resurrected() {
    let scratch = Scratch::new("foreign-remove");
    let path = scratch.place("note.md", b"old");
    let staged = staged(
        scratch
            .stage(
                "note.md",
                Transition::Replace {
                    before: hash(b"old"),
                    content: b"ours",
                },
            )
            .expect("staged"),
    );
    #[allow(clippy::disallowed_methods)] // Harness scaffolding: playing the foreign writer.
    std::fs::remove_file(&path).expect("a foreign removal");

    let refusal = scratch
        .publish(staged)
        .expect_err("a removal after staging");

    assert!(
        matches!(&refusal, Refusal::Drifted { observed: None, .. }),
        "{refusal}"
    );
    assert!(!exists(&path), "publication resurrected a removed document");
    assert!(scratch.shadow_names().is_empty());
}

/// **The bar on a landing another writer made.** A target that reaches its
/// after-state between staging and publication is found landed, for every
/// kind: not refused, and not written again.
///
/// A resolved plan's after-state is what it promised, whoever put it there
/// (ADR 0031). The forbidden shapes are a refusal — a re-sent plan would never
/// finish over a target somebody else finished for it — and a report that this
/// call wrote it, which would prime the own-write ledger for an event this call
/// did not cause. The shadow a found create or replace staged is discarded.
#[test]
fn a_target_another_writer_landed_after_staging_is_found() {
    let scratch = Scratch::new("found-landed");
    scratch.place("note.md", b"old");
    scratch.place("gone.md", b"going");
    let staged = [
        staged(
            scratch
                .stage("fresh.md", Transition::Create { content: b"fresh" })
                .expect("a create stages"),
        ),
        staged(
            scratch
                .stage(
                    "note.md",
                    Transition::Replace {
                        before: hash(b"old"),
                        content: b"new",
                    },
                )
                .expect("a replacement stages"),
        ),
        staged(
            scratch
                .stage(
                    "gone.md",
                    Transition::Remove {
                        before: hash(b"going"),
                    },
                )
                .expect("a removal stages"),
        ),
    ];
    // Another writer lands every one of them first.
    scratch.place("fresh.md", b"fresh");
    scratch.place("note.md", b"new");
    #[allow(clippy::disallowed_methods)] // Harness scaffolding: playing the foreign writer.
    std::fs::remove_file(scratch.at("gone.md")).expect("a foreign removal");
    let theirs = [
        identity_at(&scratch.at("fresh.md")),
        identity_at(&scratch.at("note.md")),
    ];

    let [create, replace, remove] = staged.map(|staged| {
        found(
            scratch
                .publish(staged)
                .expect("a landing another writer made"),
        )
    });

    for (confirmed, path, identity) in [
        (&create, "fresh.md", theirs[0]),
        (&replace, "note.md", theirs[1]),
    ] {
        let AfterState::Present(state) = confirmed.after else {
            panic!("{path}: {:?}", confirmed.after);
        };
        assert_eq!((state.dev, state.ino), identity, "{path} was written again");
        assert!(confirmed.durability.is_synced());
    }
    assert!(matches!(remove.after, AfterState::Absent), "{remove:?}");
    assert!(remove.durability.is_synced());
    assert!(
        scratch.shadow_names().is_empty(),
        "a found landing kept its shadow: {:?}",
        scratch.shadow_names()
    );
}

/// A removal whose folder is gone by publication has landed: absence is its
/// after-state, whatever took the folder with it.
#[test]
fn a_removal_whose_folder_is_gone_at_publication_is_found() {
    let scratch = Scratch::new("found-folder-gone");
    scratch.place("folder/deeper/note.md", b"going");
    let staged = staged(
        scratch
            .stage(
                "folder/deeper/note.md",
                Transition::Remove {
                    before: hash(b"going"),
                },
            )
            .expect("a removal stages"),
    );
    #[allow(clippy::disallowed_methods)] // Harness scaffolding: playing the foreign writer.
    std::fs::remove_dir_all(scratch.at("folder")).expect("a foreign removal of the folder");

    let confirmed = found(scratch.publish(staged).expect("a landed removal"));

    assert!(matches!(confirmed.after, AfterState::Absent));
    assert!(confirmed.durability.is_synced());
}

/// A replacement whose folder is gone by publication is still drift: its
/// after-state is content, and nothing is there.
#[test]
fn a_replacement_whose_folder_is_gone_at_publication_is_drift() {
    let scratch = Scratch::new("drift-folder-gone");
    scratch.place("folder/note.md", b"old");
    let staged = staged(
        scratch
            .stage(
                "folder/note.md",
                Transition::Replace {
                    before: hash(b"old"),
                    content: b"new",
                },
            )
            .expect("a replacement stages"),
    );
    #[allow(clippy::disallowed_methods)] // Harness scaffolding: playing the foreign writer.
    std::fs::remove_dir_all(scratch.at("folder")).expect("a foreign removal of the folder");

    let refusal = scratch
        .publish(staged)
        .expect_err("a replacement onto nothing");

    assert!(
        matches!(&refusal, Refusal::Drifted { observed: None, .. }),
        "{refusal}"
    );
    assert!(
        !exists(&scratch.at("folder")),
        "a replacement made the folder again"
    );
    assert!(scratch.shadow_names().is_empty());
}

/// **The bar on exclusive create.** A name taken after staging refuses the
/// create at publication, and the racer's bytes survive.
///
/// The forbidden shape is trusting staging's look: an existence test followed
/// by a rename is two moments, and anything that arrives between them is
/// overwritten.
#[test]
fn a_create_whose_name_is_taken_after_staging_refuses_and_leaves_it() {
    let scratch = Scratch::new("create-race");
    let staged = staged(
        scratch
            .stage("note.md", Transition::Create { content: b"ours" })
            .expect("staged"),
    );
    let path = scratch.place("note.md", b"the racer's bytes");

    let refusal = scratch
        .publish(staged)
        .expect_err("a create onto a taken name");

    assert_eq!(refusal, Refusal::DestinationExists { path: path.clone() });
    assert_eq!(
        bytes_at(&path),
        b"the racer's bytes",
        "the create overwrote the racer"
    );
    assert!(scratch.shadow_names().is_empty());
}

/// **The bar on the root's identity.** A vault root replaced between staging
/// and publication refuses every kind, and neither root is written.
///
/// The forbidden shape is a publication that trusts the root's spelling: a
/// root path that now names another directory would receive a plan checked
/// against a different tree.
#[test]
#[allow(clippy::disallowed_methods)] // Harness scaffolding: replacing the vault root.
fn a_root_replaced_between_the_phases_refuses() {
    let scratch = Scratch::new("root-swapped");
    scratch.place("note.md", b"old");
    let staged = [
        staged(
            scratch
                .stage("fresh.md", Transition::Create { content: b"fresh" })
                .expect("a create stages"),
        ),
        staged(
            scratch
                .stage(
                    "note.md",
                    Transition::Replace {
                        before: hash(b"old"),
                        content: b"new",
                    },
                )
                .expect("a replacement stages"),
        ),
        staged(
            scratch
                .stage(
                    "note.md",
                    Transition::Remove {
                        before: hash(b"old"),
                    },
                )
                .expect("a removal stages"),
        ),
    ];
    let staged_root = staged[0].root();
    let aside = scratch.vault().with_extension("aside");
    std::fs::rename(scratch.vault(), &aside).expect("moving the root aside");
    std::fs::create_dir(scratch.vault()).expect("a new root at the same path");
    std::fs::write(scratch.at("note.md"), b"old").expect("the same bytes in the new root");

    for staged in staged {
        let refusal = scratch.publish(staged).expect_err("a replaced root");
        let Refusal::RootReplaced {
            staged: was,
            current,
            ..
        } = &refusal
        else {
            panic!("a replaced root was not reported as one: {refusal}");
        };
        assert_eq!(*was, staged_root);
        assert_ne!(*current, staged_root);
    }

    assert_eq!(bytes_at(&scratch.at("note.md")), b"old");
    assert_eq!(bytes_at(&aside.join("note.md")), b"old");
    assert!(!exists(&scratch.at("fresh.md")));
    assert!(!exists(&aside.join("fresh.md")));
    assert!(scratch.shadow_names().is_empty());
}

/// **The bar on a lost shadow.** A shadow gone between staging and publication
/// refuses as an I/O failure, not as drift, and the target is untouched.
///
/// A staged shadow outlives its staging call, so a sweep or a sync client can
/// reach it. The forbidden shapes are drift — the caller would re-plan a target
/// nobody changed — and a publication that renames whatever holds the name.
#[test]
fn a_missing_shadow_refuses_as_an_io_failure() {
    let scratch = Scratch::new("shadow-missing");
    let path = scratch.place("note.md", b"old");
    let staged = staged(
        scratch
            .stage(
                "note.md",
                Transition::Replace {
                    before: hash(b"old"),
                    content: b"new",
                },
            )
            .expect("staged"),
    );
    #[allow(clippy::disallowed_methods)] // Harness scaffolding: a sweep taking the shadow.
    std::fs::remove_file(scratch.only_shadow()).expect("removing the shadow");

    let refusal = scratch.publish(staged).expect_err("a missing shadow");

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
    assert_eq!(bytes_at(&path), b"old");
}

/// A shadow edited in place between staging and publication refuses as an I/O
/// failure, and its bytes are never published.
#[test]
fn an_edited_shadow_refuses_as_an_io_failure() {
    let scratch = Scratch::new("shadow-edited");
    let staged = staged(
        scratch
            .stage("fresh.md", Transition::Create { content: b"ours" })
            .expect("staged"),
    );
    {
        use std::io::Write as _;
        #[allow(clippy::disallowed_methods, clippy::disallowed_types)]
        // Harness scaffolding: a foreign writer in the home.
        let mut shadow = std::fs::OpenOptions::new()
            .append(true)
            .open(scratch.only_shadow())
            .expect("the shadow");
        shadow
            .write_all(b" and theirs")
            .expect("editing the shadow");
    }

    let refusal = scratch.publish(staged).expect_err("an edited shadow");

    assert!(
        matches!(
            &refusal,
            Refusal::Environment {
                kind: std::io::ErrorKind::InvalidData,
                ..
            }
        ),
        "{refusal}"
    );
    assert!(
        !exists(&scratch.at("fresh.md")),
        "an edited shadow was published"
    );
    assert!(
        scratch.shadow_names().is_empty(),
        "our own edited shadow was left"
    );
}

/// **The bar on a re-send that finds its target landed.** Confirming a landed
/// target reports its after-state and syncs its folder.
#[test]
#[allow(clippy::disallowed_methods)] // The kernel's own suite: its write entry points are what it exercises.
fn a_landed_target_is_confirmed_and_its_folder_synced() {
    let scratch = Scratch::new("confirm-landed");
    let path = scratch.place("folder/note.md", b"the after-state");
    let Staging::Landed(landed) = scratch
        .stage(
            "folder/note.md",
            Transition::Replace {
                before: hash(b"the before-state"),
                content: b"the after-state",
            },
        )
        .expect("a target at its after-state")
    else {
        panic!("a target at its after-state staged");
    };

    let confirmed = confirm_landed(&scratch.vault(), &landed).expect("a confirmed landing");

    let AfterState::Present(state) = confirmed.after else {
        panic!("a landed replacement reported {:?}", confirmed.after);
    };
    assert_eq!(state.content_hash, hash(b"the after-state"));
    assert_eq!((state.dev, state.ino), identity_at(&path));
    assert!(matches!(confirmed.durability, Durability::Synced));

    // And a landing that no longer holds is not confirmed.
    scratch.place("folder/note.md", b"theirs");
    let refusal = confirm_landed(&scratch.vault(), &landed).expect_err("a landing that moved");
    assert!(matches!(refusal, Refusal::Drifted { .. }), "{refusal}");
}

/// **The bar on the shadow's identity.** A shadow replaced between the phases
/// by another file holding the same bytes refuses as an I/O failure, and the
/// other file is neither published nor removed.
///
/// The hash alone agrees, so only the identity says the file is not the one
/// staging made. The forbidden shape is a confirmation by content: it
/// publishes whatever a sweep or a sync client left at the shadow's name.
#[test]
#[allow(clippy::disallowed_methods)] // Harness scaffolding: a foreign writer in the home.
fn a_shadow_replaced_by_a_copy_of_its_bytes_refuses() {
    let scratch = Scratch::new("shadow-copied");
    let path = scratch.place("note.md", b"old");
    let staged = staged(
        scratch
            .stage(
                "note.md",
                Transition::Replace {
                    before: hash(b"old"),
                    content: b"new",
                },
            )
            .expect("staged"),
    );
    let shadow = scratch.only_shadow();
    let copy = shadow.with_file_name("a-copy");
    std::fs::write(&copy, b"new").expect("a copy of the shadow's bytes");
    std::fs::rename(&copy, &shadow).expect("the copy in the shadow's place");
    let foreign = identity_at(&shadow);

    let refusal = scratch
        .publish(staged)
        .expect_err("a shadow that is another file");

    assert!(
        matches!(
            &refusal,
            Refusal::Environment {
                kind: std::io::ErrorKind::InvalidData,
                ..
            }
        ),
        "{refusal}"
    );
    assert_eq!(bytes_at(&path), b"old");
    assert_eq!(
        identity_at(&shadow),
        foreign,
        "the cleanup removed a file it did not stage"
    );
}

/// **The shadow is confirmed last.** A landing another writer made is found
/// whatever became of the shadow, and a create refused at its shadow after
/// making folders takes them back.
///
/// The forbidden shape is confirming the shadow first: a re-send over a target
/// somebody else finished would refuse for a shadow it does not need, and the
/// window between the confirmation and the rename would span the whole
/// verification and the folders.
#[test]
#[allow(clippy::disallowed_methods)] // Harness scaffolding: a sweep taking the shadow.
fn the_shadow_is_confirmed_after_the_target_and_the_folders() {
    let scratch = Scratch::new("shadow-last");
    scratch.place("note.md", b"old");
    let replace = staged(
        scratch
            .stage(
                "note.md",
                Transition::Replace {
                    before: hash(b"old"),
                    content: b"new",
                },
            )
            .expect("staged"),
    );
    std::fs::remove_file(scratch.only_shadow()).expect("a sweep taking the shadow");
    scratch.place("note.md", b"new");
    let _ = found(
        scratch
            .publish(replace)
            .expect("a landing another writer made"),
    );

    let create = staged(
        scratch
            .stage("a/b/fresh.md", Transition::Create { content: b"fresh" })
            .expect("staged"),
    );
    std::fs::remove_file(scratch.only_shadow()).expect("a sweep taking the shadow");
    let refusal = scratch
        .publish(create)
        .expect_err("a create with no shadow");
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
    assert!(
        !exists(&scratch.at("a")),
        "the folders made before the shadow refused were left"
    );
}

/// **Every call is held to the plan's root.** Staging and emptying folders
/// under a root identity the vault root is not refuse as a replaced root, and
/// read nothing.
#[test]
#[allow(clippy::disallowed_methods)] // The kernel's own suite: its write entry points are what it exercises.
fn a_root_the_plan_was_not_made_against_refuses_staging_and_emptying() {
    let scratch = Scratch::new("root-expected");
    scratch.place("note.md", b"old");
    scratch.directory("empty");
    let elsewhere = norn_fs::path_identity(scratch.vault().parent().expect("a tree"))
        .expect("the tree")
        .expect("a tree");

    let staging = norn_fs::stage(
        &scratch.vault(),
        elsewhere,
        std::path::Path::new("note.md"),
        Transition::Remove {
            before: hash(b"old"),
        },
        scratch.shadows(),
    );
    assert!(
        matches!(staging, Err(Refusal::RootReplaced { .. })),
        "{staging:?}"
    );
    let emptying =
        norn_fs::remove_empty_folders(&scratch.vault(), elsewhere, std::path::Path::new("empty"));
    assert!(
        matches!(emptying, Err(Refusal::RootReplaced { .. })),
        "{emptying:?}"
    );
    assert!(exists(&scratch.at("empty")));
}

/// **A removal's landing is absence.** Confirming a removal staging found
/// landed refuses where a file has since come to its name, rather than
/// reporting it absent.
#[test]
#[allow(clippy::disallowed_methods)] // The kernel's own suite: its write entry points are what it exercises.
fn confirming_a_removal_that_finds_a_file_refuses() {
    let scratch = Scratch::new("confirm-remove-file");
    let Staging::Landed(landed) = scratch
        .stage(
            "note.md",
            Transition::Remove {
                before: hash(b"old"),
            },
        )
        .expect("a landed removal")
    else {
        panic!("a removal of nothing staged");
    };
    scratch.place("note.md", b"somebody's new document");

    let refusal = confirm_landed(&scratch.vault(), &landed).expect_err("a name taken again");
    assert!(
        matches!(refusal, Refusal::DestinationExists { .. }),
        "{refusal}"
    );
}
