//! What a staged and published change leaves at rest when it does not finish.
//!
//! Every other suite over this crate's write protocol asks what a call
//! *returned*. Some of the protocol's claims are not about a return value at
//! all: a machine that stops between two system calls, a disk with no room left
//! at the stage that needs it, a foreign writer inside a publication, and a
//! destination that moved out from under a precondition. The first of those
//! cannot be stated by a caller — the process that meets it does not reach an
//! assertion — so the case is two processes: a child arms one checkpoint
//! through the environment the public entry points read, stages and publishes
//! one scenario, and this parent reads what is at rest afterwards.
//!
//! **The bar is the same for every checkpoint.** Whatever the child was armed
//! at, a target afterwards holds either its old state or its complete new one,
//! never a prefix of either and never anything else; a shadow left behind is
//! inert, standing at a name in the shadow home rather than at the target.
//!
//! **Every case asserts on the arm's own record as well as on the outcome.**
//! State at rest cannot tell a checkpoint that fired from a checkpoint that was
//! deleted: a write with no hooks in it at all leaves the old document too, and
//! satisfies the outcome bar without carrying anything. So the seam records
//! which checkpoint it answered at, in which publication and about which path,
//! in the child, before it answers — and a hook removed from the protocol fails
//! these cases on the missing record. The
//! [`unarmed`](the_child_role_publishes_under_whatever_it_was_armed_at) control
//! is the other half: the same code, spawned the same way with the same live
//! record file and nothing armed, records nothing and lands the write.
//!
//! This is a binary of its own because its cases spawn processes, and a suite
//! that spawns none should not wait on one that does.

#![allow(clippy::disallowed_methods, clippy::disallowed_types)] // Acceptance fixture: arranging and judging a tree.

use std::path::{Path, PathBuf};
use std::time::Duration;

use norn_fs::{
    CaseSensitivity, ContentHash, Durability, MaintainershipKey, PathNormalizer, Placement,
    Publication, Refusal, ShadowHome, Staging, Transition,
};
use norn_testkit::attestation::{Attestation, SEAM};
use norn_testkit::churn::{Folding, runs_where_the_volume_folds};
use norn_testkit::process::{Run, RunStatus, Sandbox};

/// The variable that tells a run it is the child, and where its tree is.
const ROLE: &str = "NORN_FS_LOCKDOWN_ROOT";

/// The variable naming the precondition the child publishes under.
const PRECONDITION: &str = "NORN_FS_LOCKDOWN_PRECONDITION";

/// The variable naming the scenario the child runs; a replace where unnamed.
const SCENARIO: &str = "NORN_FS_LOCKDOWN_SCENARIO";

/// The variable the write seam's arm is read from.
const ARMED: &str = "NORN_FS_ARMED_STAGES";

/// The seam the widened fault hook records itself under.
const WRITE_SEAM: &str = "norn-fs/write";

/// The seam the child records its own return under.
const CHILD_SEAM: &str = "lockdown/child";

/// What stands at the destination before the publication.
const OLD: &[u8] = b"the document that was there\n";

/// What the publication is trying to put there. Longer than [`OLD`], so a
/// destination holding a prefix of it is a destination holding neither.
const NEW: &[u8] = b"the document the publication is trying to put there instead\n";

/// A hash of bytes nothing ever wrote here, which is what a destination that
/// moved under a caller looks like to the precondition.
const STALE: &[u8] = b"bytes this destination never held\n";

/// What the write seam's foreign writer puts at a name, as its documentation
/// fixes it.
const FOREIGN: &[u8] = b"bytes a foreign writer put here\n";

/// The name inside the tree the publication is aimed at.
const DOCUMENT: &str = "note.md";

/// A child does milliseconds of work. A child still running after this is a
/// child that wedged, and killing it reports better than a suite that hangs.
const CHILD_DEADLINE: Duration = Duration::from_secs(30);

/// The signal an abort raises, which is how a checkpoint armed to end the
/// process reports itself to the parent.
const SIGABRT: i32 = 6;

// ---------------------------------------------------------------------------
// The child role
// ---------------------------------------------------------------------------

/// The publication every case in this file runs, in whichever process runs it.
///
/// In the child it is the subject: the arm is already in the environment when
/// this binary starts, so the calls below are the ordinary public entry points
/// and nothing here knows a checkpoint was armed.
///
/// In an ordinary run of the suite it spawns itself with nothing armed, which
/// is the **control**, and that is what makes the record assertions mean
/// something: the same call reaches the end of the protocol, lands the write,
/// and writes no seam record at all. A case that asserts a record therefore
/// asserts something this run proves is not free.
///
/// **The control runs as a child for the sake of the record file.** An arm
/// records itself only where the environment names a file to record into, so a
/// control that ran here in the parent — where nothing names one — would satisfy
/// "no checkpoint recorded" by having no sink, and the absence assertion every
/// death case rests on would hold over a protocol with no checkpoints left in
/// it. Spawned, the control differs from an armed case in the arm and in
/// nothing else.
#[test]
fn the_child_role_publishes_under_whatever_it_was_armed_at() {
    if let Some(root) = std::env::var_os(ROLE) {
        run_scenario(Path::new(&root));
        return;
    }

    let tree = Tree::new("unarmed");
    let outcome = tree.spawn(&[]);

    assert_eq!(
        outcome.status,
        RunStatus::Exited(0),
        "{}",
        outcome.stderr_text()
    );
    assert_eq!(tree.bytes_at(DOCUMENT), Some(NEW.to_vec()));
    assert_eq!(tree.shadow_names(), Vec::<String>::new());

    let attested = tree.attestation();
    attested.assert_never_reached("a publication with nothing armed", &[(SEAM, WRITE_SEAM)]);
    attested.assert_reached(
        "a publication with nothing armed",
        &[
            (SEAM, CHILD_SEAM),
            ("outcome", "written"),
            ("durability", "synced"),
        ],
    );
    attested.assert_count("a publication with nothing armed", 1);
}

/// Run the scenario the environment names, recording what each call answered.
///
/// The record is the child's own half of the attestation: the seam says which
/// checkpoint the protocol reached, and this says what the call returned to a
/// caller that survived to read it. A checkpoint armed to end the process
/// writes the first and never the second, which is how a parent tells "it died
/// there" from "it refused there".
fn run_scenario(root: &Path) {
    let vault = root.join("vault");
    let shadows = shadow_home(root);
    let vault_root = norn_fs::path_identity(&vault)
        .expect("the vault root")
        .expect("a vault root");
    let scenario = std::env::var(SCENARIO).unwrap_or_else(|_| "replace".to_string());
    let before = match std::env::var(PRECONDITION).as_deref() {
        Ok("stale") => ContentHash::of(STALE),
        _ => ContentHash::of(OLD),
    };
    let respelled = Path::new("Note.md");
    let targets: Vec<(&str, Transition<'_>)> = match scenario.as_str() {
        "replace" => vec![(
            DOCUMENT,
            Transition::Replace {
                before,
                content: NEW,
            },
        )],
        "create" => vec![("fresh.md", Transition::Create { content: NEW })],
        "create-deep" => vec![("a/b/fresh.md", Transition::Create { content: NEW })],
        "remove" => vec![(DOCUMENT, Transition::Remove { before })],
        "respell" => vec![(
            DOCUMENT,
            Transition::Respell {
                to: respelled,
                before,
                content: None,
            },
        )],
        "respell-content" => vec![(
            DOCUMENT,
            Transition::Respell {
                to: respelled,
                before,
                content: Some(NEW),
            },
        )],
        "three" => ["one.md", "two.md", "three.md"]
            .into_iter()
            .map(|name| (name, Transition::Create { content: NEW }))
            .collect(),
        "rmdir" => {
            let removed =
                norn_fs::remove_empty_folders(&vault, vault_root, Path::new("empty/deeper"));
            let line = match removed {
                Ok(removed) => format!("outcome=removed count={}", removed.removed.len()),
                Err(refusal) => refused(&refusal),
            };
            record(
                root,
                &format!("seam={CHILD_SEAM} target=empty/deeper {line}"),
            );
            return;
        }
        other => panic!("`{other}` names no lockdown scenario"),
    };

    // Every target stages before any publishes, as the applier runs a plan.
    let staged: Vec<_> = targets
        .into_iter()
        .map(|(path, transition)| {
            (
                path,
                norn_fs::stage(&vault, vault_root, Path::new(path), transition, &shadows),
            )
        })
        .collect();
    for (path, staging) in staged {
        let line = match staging {
            Err(refusal) => refused(&refusal),
            Ok(Staging::Landed(_)) => "outcome=landed".to_string(),
            Ok(Staging::Staged(staged)) => match norn_fs::publish(&vault, staged, &shadows) {
                Ok(Publication::Wrote(published)) => match &published.durability {
                    Durability::Synced => "outcome=written durability=synced".to_string(),
                    Durability::NotSynced(error) => format!(
                        "outcome=written durability=not-synced errno={}",
                        errno(error.raw_os_error())
                    ),
                },
                Ok(Publication::Found(_)) => "outcome=found".to_string(),
                Ok(Publication::Interrupted(interrupted)) => format!(
                    "outcome=interrupted {}",
                    refused(&interrupted.cause).replacen("outcome=", "cause=", 1)
                ),
                Err(refusal) => refused(&refusal),
            },
        };
        record(root, &format!("seam={CHILD_SEAM} target={path} {line}"));
    }
}

/// What the child says about a refusal it lived to see.
fn refused(refusal: &Refusal) -> String {
    match refusal {
        Refusal::Drifted { .. } => "outcome=drifted".to_string(),
        Refusal::DestinationExists { .. } => "outcome=taken".to_string(),
        Refusal::Environment { raw_os_error, .. } => {
            format!("outcome=environment errno={}", errno(*raw_os_error))
        }
        other => format!("outcome=refused {other:?}")
            .split_whitespace()
            .take(2)
            .collect::<Vec<_>>()
            .join(" "),
    }
}

/// The error number a record names: `ENOSPC`, or `other`.
fn errno(raw: Option<i32>) -> &'static str {
    if raw == Some(libc::ENOSPC) {
        "ENOSPC"
    } else {
        "other"
    }
}

/// Append one record to the file the arms share.
fn record(root: &Path, line: &str) {
    use std::io::Write as _;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(hits_path(root))
        .expect("the arm-hit record");
    writeln!(file, "{line}").expect("recording what the publication answered");
    file.sync_all()
        .expect("recording what the publication answered");
}

// ---------------------------------------------------------------------------
// Drift
// ---------------------------------------------------------------------------

/// **A destination that moved is refused, and is not replaced.** The
/// precondition names bytes that are not there, so the publication has nothing
/// to compose against — and the forbidden outcome is the one where it publishes
/// anyway and the write that moved the destination is silently lost.
///
/// No checkpoint is armed: drift is a condition a tree can be put into, and
/// arming a stage for it would test the arm rather than the precondition. What
/// pins the hook here is the destination's own bytes — a protocol that stopped
/// checking would leave [`NEW`] at the name.
#[test]
fn a_destination_that_drifted_is_refused_without_being_replaced() {
    let tree = Tree::new("drift");
    let outcome = tree.spawn(&[(PRECONDITION, "stale")]);

    assert_eq!(
        outcome.status,
        RunStatus::Exited(0),
        "{}",
        outcome.stderr_text()
    );
    assert_eq!(
        tree.bytes_at(DOCUMENT),
        Some(OLD.to_vec()),
        "a drifted precondition published anyway"
    );
    assert_eq!(tree.shadow_names(), Vec::<String>::new());

    let attested = tree.attestation();
    attested.assert_reached(
        "a drifted precondition",
        &[(SEAM, CHILD_SEAM), ("outcome", "drifted")],
    );
    attested.assert_never_reached("a drifted precondition", &[(SEAM, WRITE_SEAM)]);
}

// ---------------------------------------------------------------------------
// Process death at each named checkpoint
// ---------------------------------------------------------------------------

/// **The process ends at each checkpoint of a replacement, one at a time.**
///
/// The table is the contract, checkpoint by checkpoint, and the two columns are
/// what has to be true of the destination and of the shadow home afterwards.
/// The swap is where the two halves meet: before it the destination is
/// untouched however much has been staged, and after it the destination is the
/// complete new document and nothing is left staged at all.
///
/// `cleanup` is reached only by a publication already failing, so it is armed as
/// a pair — the swap refuses, and the removal that would tidy the shadow away
/// cannot happen either. That leaves the one shadow this protocol is allowed to
/// leak, at a name in the shadow home and never at the destination.
#[test]
fn process_death_at_every_checkpoint_leaves_one_whole_document() {
    for (label, arm, destination, shadow) in [
        ("stage-create", "stage-create=ends", OLD, Staged::Nothing),
        ("stage-write", "stage-write=ends", OLD, Staged::Empty),
        ("stage-sync", "stage-sync=ends", OLD, Staged::Holding(NEW)),
        ("swap", "swap=ends", OLD, Staged::Holding(NEW)),
        ("parent-sync", "parent-sync=ends", NEW, Staged::Nothing),
        (
            "cleanup",
            "swap=fails,cleanup=ends",
            OLD,
            Staged::Holding(NEW),
        ),
    ] {
        let tree = Tree::new(&format!("ends-{label}"));
        let outcome = tree.spawn(&[(ARMED, arm)]);

        assert_eq!(
            outcome.status,
            RunStatus::Signaled(SIGABRT),
            "the {label} checkpoint did not end the process it was armed in\n{}",
            outcome.stderr_text()
        );
        assert_eq!(
            tree.bytes_at(DOCUMENT),
            Some(destination.to_vec()),
            "the {label} checkpoint left neither the old document nor the new one"
        );
        tree.assert_staged(label, shadow);

        // The arm's own record. A publication whose hooks were removed dies
        // nowhere and lands the write, so without this the outcome column above
        // is satisfied by a protocol that no longer has a checkpoint at all.
        let attested = tree.attestation();
        attested.assert_reached(
            label,
            &[
                (SEAM, WRITE_SEAM),
                ("stage", label),
                ("path", DOCUMENT),
                ("answer", "ends"),
            ],
        );
        attested.assert_never_reached(label, &[(SEAM, CHILD_SEAM)]);
    }
}

/// **The process ends at each publication position of each kind.**
///
/// What a crash there leaves is the recovery claim a re-send rests on: before
/// the publication act the target is untouched and the shadow is the only
/// residue, and after it the target is landed and nothing is staged.
#[test]
fn process_death_at_each_publication_position_leaves_the_documented_state() {
    type Judge = fn(&Tree);
    let cases: [(&str, &str, &str, &str, Judge); 8] = [
        ("create-swap", "create", "swap=ends", "fresh.md", |tree| {
            assert_eq!(tree.bytes_at("fresh.md"), None);
            tree.assert_staged("create swap", Staged::Holding(NEW));
        }),
        (
            "create-parent-sync",
            "create",
            "parent-sync=ends",
            "fresh.md",
            |tree| {
                assert_eq!(tree.bytes_at("fresh.md"), Some(NEW.to_vec()));
                tree.assert_staged("create parent-sync", Staged::Nothing);
            },
        ),
        ("replace-swap", "replace", "swap=ends", DOCUMENT, |tree| {
            assert_eq!(tree.bytes_at(DOCUMENT), Some(OLD.to_vec()));
            tree.assert_staged("replace swap", Staged::Holding(NEW));
        }),
        (
            "replace-parent-sync",
            "replace",
            "parent-sync=ends",
            DOCUMENT,
            |tree| {
                assert_eq!(tree.bytes_at(DOCUMENT), Some(NEW.to_vec()));
                tree.assert_staged("replace parent-sync", Staged::Nothing);
            },
        ),
        ("remove-unlink", "remove", "unlink=ends", DOCUMENT, |tree| {
            assert_eq!(tree.bytes_at(DOCUMENT), Some(OLD.to_vec()));
            tree.assert_staged("remove unlink", Staged::Nothing);
        }),
        (
            "remove-parent-sync",
            "remove",
            "parent-sync=ends",
            DOCUMENT,
            |tree| {
                assert_eq!(tree.bytes_at(DOCUMENT), None);
                tree.assert_staged("remove parent-sync", Staged::Nothing);
            },
        ),
        ("create-mkdir", "create-deep", "mkdir=ends", "a", |tree| {
            assert!(!tree.vault().join("a").exists(), "a folder was made");
            tree.assert_staged("create mkdir", Staged::Holding(NEW));
        }),
        (
            "create-deep-parent-sync",
            "create-deep",
            "parent-sync=ends",
            "a/b/fresh.md",
            |tree| {
                assert_eq!(tree.bytes_at("a/b/fresh.md"), Some(NEW.to_vec()));
                tree.assert_staged("create-deep parent-sync", Staged::Nothing);
            },
        ),
    ];
    for (label, scenario, arm, path, judge) in cases {
        let tree = Tree::new(&format!("position-{label}"));
        let outcome = tree.spawn(&[(SCENARIO, scenario), (ARMED, arm)]);

        assert_eq!(
            outcome.status,
            RunStatus::Signaled(SIGABRT),
            "{label} did not end the process\n{}",
            outcome.stderr_text()
        );
        judge(&tree);
        let stage = arm.split('=').next().expect("a stage");
        let attested = tree.attestation();
        attested.assert_reached(
            label,
            &[
                (SEAM, WRITE_SEAM),
                ("stage", stage),
                ("ordinal", "1"),
                ("path", path),
                ("answer", "ends"),
            ],
        );
        attested.assert_never_reached(label, &[(SEAM, CHILD_SEAM)]);
    }
}

/// **The process ends at a respell's rename**, and the file stands under its
/// old spelling with its bytes. Runs only where the tree's root folds case.
#[test]
fn process_death_at_the_respell_leaves_the_old_spelling() {
    let tree = Tree::new("position-respell");
    if !tree.folds("process_death_at_the_respell_leaves_the_old_spelling") {
        return;
    }
    let outcome = tree.spawn(&[(SCENARIO, "respell"), (ARMED, "respell=ends")]);

    assert_eq!(
        outcome.status,
        RunStatus::Signaled(SIGABRT),
        "{}",
        outcome.stderr_text()
    );
    assert_eq!(tree.vault_names(), vec![DOCUMENT.to_string()]);
    assert_eq!(tree.bytes_at(DOCUMENT), Some(OLD.to_vec()));
    tree.attestation().assert_reached(
        "respell",
        &[
            (SEAM, WRITE_SEAM),
            ("stage", "respell"),
            ("ordinal", "1"),
            ("answer", "ends"),
        ],
    );
}

/// **A respell whose rename fails after its content landed is interrupted,
/// not refused.** The new content stands under the old spelling, the answer
/// says the first step landed and why the rename did not, and a re-send finds
/// the respell halfway. Runs only where the tree's root folds case.
#[test]
fn a_respell_whose_rename_fails_after_its_content_landed_is_interrupted() {
    let tree = Tree::new("respell-interrupted");
    if !tree.folds("a_respell_whose_rename_fails_after_its_content_landed_is_interrupted") {
        return;
    }
    let outcome = tree.spawn(&[(SCENARIO, "respell-content"), (ARMED, "respell=fails")]);

    assert_eq!(
        outcome.status,
        RunStatus::Exited(0),
        "{}",
        outcome.stderr_text()
    );
    assert_eq!(tree.vault_names(), vec![DOCUMENT.to_string()]);
    assert_eq!(tree.bytes_at(DOCUMENT), Some(NEW.to_vec()));
    let attested = tree.attestation();
    attested.assert_reached(
        "an interrupted respell",
        &[
            (SEAM, WRITE_SEAM),
            ("stage", "respell"),
            ("answer", "fails"),
        ],
    );
    attested.assert_reached(
        "an interrupted respell",
        &[
            (SEAM, CHILD_SEAM),
            ("outcome", "interrupted"),
            ("cause", "environment"),
        ],
    );
}

// ---------------------------------------------------------------------------
// The ordinal
// ---------------------------------------------------------------------------

/// **An arm with an ordinal fires in that publication only.** Three targets
/// staged and published in order, armed to end at the second swap, leave
/// exactly the first landed and the other two staged.
#[test]
fn an_ordinal_arm_fires_at_the_nth_publication_only() {
    let tree = Tree::new("ordinal");
    let outcome = tree.spawn(&[(SCENARIO, "three"), (ARMED, "swap@2=ends")]);

    assert_eq!(
        outcome.status,
        RunStatus::Signaled(SIGABRT),
        "{}",
        outcome.stderr_text()
    );
    assert_eq!(tree.bytes_at("one.md"), Some(NEW.to_vec()));
    assert_eq!(tree.bytes_at("two.md"), None);
    assert_eq!(tree.bytes_at("three.md"), None);
    assert_eq!(tree.shadow_names().len(), 2, "{:?}", tree.shadow_names());

    let attested = tree.attestation();
    attested.assert_reached(
        "the second publication",
        &[
            (SEAM, WRITE_SEAM),
            ("stage", "swap"),
            ("ordinal", "2"),
            ("path", "two.md"),
            ("answer", "ends"),
        ],
    );
    attested.assert_reached(
        "the first publication",
        &[
            (SEAM, CHILD_SEAM),
            ("target", "one.md"),
            ("outcome", "written"),
        ],
    );
    attested.assert_count("three staged, one published, one died", 2);
}

// ---------------------------------------------------------------------------
// A foreign writer inside a publication
// ---------------------------------------------------------------------------

/// **A foreign writer inside a publication meets the outcome ADR 0032 names.**
///
/// An edit or a removal of a replace's or a remove's target is drift, and an
/// edit or a take at a create's name is a name taken. A removal of a remove's
/// target is the one that lands it: absence is the remove's after-state, so it
/// is found landed by another writer. A removal against a create finds nothing
/// and the create lands, and a take against a replace does nothing.
#[test]
fn a_foreign_writer_inside_a_publication_meets_the_outcome_adr_0032_names() {
    for (scenario, act, path, outcome, left) in [
        ("replace", "edit", DOCUMENT, "drifted", Some(FOREIGN)),
        ("replace", "remove", DOCUMENT, "drifted", None),
        ("remove", "edit", DOCUMENT, "drifted", Some(FOREIGN)),
        ("remove", "remove", DOCUMENT, "found", None),
        ("create", "edit", "fresh.md", "taken", Some(FOREIGN)),
        ("create", "take", "fresh.md", "taken", Some(FOREIGN)),
        ("create", "remove", "fresh.md", "written", Some(NEW)),
        ("replace", "take", DOCUMENT, "written", Some(NEW)),
    ] {
        let label = format!("{act} against a {scenario}");
        let tree = Tree::new(&format!("foreign-{scenario}-{act}"));
        let armed = format!("foreign@1={act}");
        let run = tree.spawn(&[(SCENARIO, scenario), (ARMED, armed.as_str())]);

        assert_eq!(
            run.status,
            RunStatus::Exited(0),
            "{label}: {}",
            run.stderr_text()
        );
        assert_eq!(tree.bytes_at(path), left.map(<[u8]>::to_vec), "{label}");
        assert_eq!(tree.shadow_names(), Vec::<String>::new(), "{label}");
        let attested = tree.attestation();
        attested.assert_reached(
            &label,
            &[
                (SEAM, WRITE_SEAM),
                ("stage", "foreign"),
                ("ordinal", "1"),
                ("path", path),
                ("answer", act),
            ],
        );
        attested.assert_reached(&label, &[(SEAM, CHILD_SEAM), ("outcome", outcome)]);
    }
}

// ---------------------------------------------------------------------------
// A full disk
// ---------------------------------------------------------------------------

/// **A full disk at a checkpoint before the swap refuses, and publishes
/// nothing.**
///
/// `ENOSPC` has no [`std::io::ErrorKind`] of its own, so the refusal carries the
/// error number and the child reads its classification off that rather than off
/// the message. The first three are staging's and the swap is publication's;
/// every one is before the rename, so the required outcome is one sentence: the
/// destination is exactly what it was, and the shadow the refusal abandoned is
/// removed on the way out.
#[test]
fn a_full_disk_before_the_swap_refuses_and_leaves_the_destination_alone() {
    for (label, arm) in [
        ("stage-create", "stage-create=full-disk"),
        ("stage-write", "stage-write=full-disk"),
        ("stage-sync", "stage-sync=full-disk"),
        ("swap", "swap=full-disk"),
    ] {
        let tree = Tree::new(&format!("full-{label}"));
        let outcome = tree.spawn(&[(ARMED, arm)]);

        assert_eq!(
            outcome.status,
            RunStatus::Exited(0),
            "{label}: {}",
            outcome.stderr_text()
        );
        assert_eq!(
            tree.bytes_at(DOCUMENT),
            Some(OLD.to_vec()),
            "a full disk at {label} changed the destination"
        );
        assert_eq!(
            tree.shadow_names(),
            Vec::<String>::new(),
            "a full disk at {label} left a shadow the refusal should have removed"
        );

        let attested = tree.attestation();
        attested.assert_reached(
            label,
            &[
                (SEAM, WRITE_SEAM),
                ("stage", label),
                ("answer", "full-disk"),
            ],
        );
        attested.assert_reached(
            label,
            &[
                (SEAM, CHILD_SEAM),
                ("outcome", "environment"),
                ("errno", "ENOSPC"),
            ],
        );
    }
}

/// **A failure at an unlink, a folder's making, or a folder's removal is a
/// typed refusal**, and what it was about is as it was.
#[test]
fn a_failure_at_unlink_mkdir_or_rmdir_is_a_typed_refusal() {
    for (scenario, stage, path) in [
        ("remove", "unlink", DOCUMENT),
        ("create-deep", "mkdir", "a"),
        ("rmdir", "rmdir", "empty/deeper"),
    ] {
        for (answer, errno) in [("fails", "other"), ("full-disk", "ENOSPC")] {
            let label = format!("{answer} at {stage}");
            let tree = Tree::new(&format!("typed-{stage}-{answer}"));
            let armed = format!("{stage}={answer}");
            let run = tree.spawn(&[(SCENARIO, scenario), (ARMED, armed.as_str())]);

            assert_eq!(
                run.status,
                RunStatus::Exited(0),
                "{label}: {}",
                run.stderr_text()
            );
            match scenario {
                "remove" => assert_eq!(tree.bytes_at(DOCUMENT), Some(OLD.to_vec()), "{label}"),
                "create-deep" => assert!(!tree.vault().join("a").exists(), "{label}"),
                _ => assert!(tree.vault().join("empty/deeper").is_dir(), "{label}"),
            }
            assert_eq!(tree.shadow_names(), Vec::<String>::new(), "{label}");
            let attested = tree.attestation();
            attested.assert_reached(
                &label,
                &[
                    (SEAM, WRITE_SEAM),
                    ("stage", stage),
                    ("path", path),
                    ("answer", answer),
                ],
            );
            attested.assert_reached(
                &label,
                &[
                    (SEAM, CHILD_SEAM),
                    ("outcome", "environment"),
                    ("errno", errno),
                ],
            );
        }
    }
}

/// **A full disk after the swap is not a write that did not happen, and it is
/// not silent either.**
///
/// The folder's fsync runs after the rename has already published the name: the
/// change is at the name and every reader can see it, so a refusal would say
/// something false about a file the caller can already read. What the bar holds
/// is both halves — the destination holds the *complete* new document, nothing
/// is left staged, and the publication reports the change not synced, carrying
/// the error number that says why.
#[test]
fn a_full_disk_at_the_parent_sync_leaves_the_new_document_published() {
    let tree = Tree::new("full-parent-sync");
    let outcome = tree.spawn(&[(ARMED, "parent-sync=full-disk")]);

    assert_eq!(
        outcome.status,
        RunStatus::Exited(0),
        "{}",
        outcome.stderr_text()
    );
    assert_eq!(tree.bytes_at(DOCUMENT), Some(NEW.to_vec()));
    assert_eq!(tree.shadow_names(), Vec::<String>::new());

    let attested = tree.attestation();
    attested.assert_reached(
        "a full disk at the parent sync",
        &[
            (SEAM, WRITE_SEAM),
            ("stage", "parent-sync"),
            ("answer", "full-disk"),
        ],
    );
    attested.assert_reached(
        "a full disk at the parent sync",
        &[
            (SEAM, CHILD_SEAM),
            ("outcome", "written"),
            ("durability", "not-synced"),
            ("errno", "ENOSPC"),
        ],
    );
}

// ---------------------------------------------------------------------------
// The tree
// ---------------------------------------------------------------------------

/// What a shadow home holds after a publication stopped.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Staged {
    /// No shadow at all: either none was opened, or the swap consumed it.
    Nothing,
    /// A shadow that was opened and never filled.
    Empty,
    /// A shadow holding exactly these bytes, published nowhere.
    Holding(&'static [u8]),
}

/// A vault, its shadow home, and the record file its arms share.
struct Tree {
    sandbox: Sandbox,
    root: PathBuf,
}

impl Tree {
    /// A tree whose vault holds [`DOCUMENT`] at [`OLD`] and an empty folder
    /// chain `empty/deeper`, for the scenario that empties it.
    fn new(label: &str) -> Tree {
        let sandbox = Sandbox::new(
            Path::new(env!("CARGO_TARGET_TMPDIR")),
            &format!("lockdown-{label}"),
        )
        .expect("a sandbox");
        let root = sandbox.work_dir();
        std::fs::create_dir_all(root.join("vault/empty/deeper")).expect("a vault root");
        std::fs::create_dir_all(root.join("data/vaults/notes/tmp")).expect("a shadow home");
        std::fs::write(root.join("vault").join(DOCUMENT), OLD).expect("the old document");
        let tree = Tree { sandbox, root };
        assert_eq!(
            shadow_home(tree.root()).placement(),
            Placement::DataRoot,
            "the tree straddles two filesystems, so no case here is exercising \
             the placement the contract wants"
        );
        tree
    }

    fn root(&self) -> &Path {
        &self.root
    }

    fn vault(&self) -> PathBuf {
        self.root.join("vault")
    }

    /// Whether `case`, stated over a root that folds case, runs on this tree's
    /// vault root: skipped where the root does not fold, and failed there on
    /// macOS.
    #[track_caller]
    fn folds(&self, case: &str) -> bool {
        let folding = match PathNormalizer::detect(&self.vault())
            .expect("the tree's case behavior")
            .case_sensitivity()
        {
            CaseSensitivity::Insensitive => Folding::Folded,
            CaseSensitivity::Sensitive => Folding::Distinct,
        };
        runs_where_the_volume_folds(folding, case)
    }

    /// Run this binary again as the child, with `env` set.
    fn spawn(&self, env: &[(&str, &str)]) -> norn_testkit::process::Outcome {
        let mut run = Run::new(&self.sandbox, std::env::current_exe().expect("this binary"))
            .args([
                "--exact",
                "the_child_role_publishes_under_whatever_it_was_armed_at",
                "--nocapture",
            ])
            .deadline(CHILD_DEADLINE)
            .env(ROLE, self.root())
            .env("NORN_FS_ARM_HITS", hits_path(self.root()));
        for (name, value) in env {
            run = run.env(*name, value);
        }
        run.wait().expect("running the lockdown child")
    }

    /// The bytes at `relative` in the vault, or nothing where nothing is.
    fn bytes_at(&self, relative: &str) -> Option<Vec<u8>> {
        std::fs::read(self.vault().join(relative)).ok()
    }

    /// The vault's own document names, as its listing spells them.
    fn vault_names(&self) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(self.vault())
            .expect("the vault")
            .map(|entry| {
                entry
                    .expect("an entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .filter(|name| name != "empty")
            .collect();
        names.sort();
        names
    }

    fn shadow_names(&self) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(shadow_home(self.root()).directory())
            .expect("the shadow home")
            .map(|entry| {
                entry
                    .expect("an entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        names.sort();
        names
    }

    /// Judge what the shadow home holds, and that whatever it holds is not the
    /// destination: a shadow at rest is inert only while nothing reads it as a
    /// document.
    #[track_caller]
    fn assert_staged(&self, label: &str, expected: Staged) {
        let names = self.shadow_names();
        match expected {
            Staged::Nothing => assert!(
                names.is_empty(),
                "the {label} checkpoint left {names:?} staged"
            ),
            Staged::Empty | Staged::Holding(_) => {
                assert_eq!(names.len(), 1, "the {label} checkpoint staged {names:?}");
                let staged = shadow_home(self.root()).directory().join(&names[0]);
                let bytes = std::fs::read(&staged).expect("the staged shadow");
                let wanted: &[u8] = match expected {
                    Staged::Empty => b"",
                    Staged::Holding(content) => content,
                    Staged::Nothing => unreachable!(),
                };
                assert_eq!(bytes, wanted, "the {label} checkpoint staged other bytes");
                assert!(
                    norn_fs::is_shadow_name(std::ffi::OsStr::new(&names[0])),
                    "the {label} checkpoint left {} outside the shadow naming",
                    names[0]
                );
            }
        }
    }

    fn attestation(&self) -> Attestation {
        Attestation::read(&hits_path(self.root()))
    }
}

/// The shadow home over a tree, resolved the way a production placement
/// resolves it.
fn shadow_home(root: &Path) -> ShadowHome {
    ShadowHome::resolve(
        &root.join("vault"),
        &root.join("data/vaults/notes/tmp"),
        &MaintainershipKey::new("norn-dev", "notes", "0123456789abcdef")
            .expect("three path components"),
    )
    .expect("a shadow home")
}

/// The file every arm in one case records itself in. It sits beside the vault
/// rather than inside it, so nothing a walk of the tree would read is a record.
fn hits_path(root: &Path) -> PathBuf {
    root.join("arm-hits")
}
