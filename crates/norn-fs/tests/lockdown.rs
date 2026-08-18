//! What a compare-and-swap publication leaves at rest when it does not finish.
//!
//! Every other suite over this crate's write protocol asks what a call
//! *returned*. Three of the protocol's claims are not about a return value at
//! all: a machine that stops between two system calls, a disk with no room left
//! at the stage that needs it, and a destination that moved out from under a
//! precondition. The first of those cannot be stated by a caller — the process
//! that meets it does not reach an assertion — so the case is two processes: a
//! child arms one checkpoint through the environment the public entry point
//! reads and attempts the publication, and this parent reads what is at rest
//! afterwards.
//!
//! **The bar is the same for every checkpoint.** Whatever the child was armed
//! at, the destination afterwards holds either the old document or the complete
//! new one, never a prefix of either and never anything else; a shadow left
//! behind is inert, standing at a name in the shadow home rather than at the
//! destination.
//!
//! **Every case asserts on the arm's own record as well as on the outcome.**
//! State at rest cannot tell a checkpoint that fired from a checkpoint that was
//! deleted: a write with no hooks in it at all leaves the old document too, and
//! satisfies the outcome bar without carrying anything. So the seam records
//! which checkpoint it answered at, in the child, before it answers — and a
//! hook removed from the protocol fails these cases on the missing record. The
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
    ContentHash, Landed, MaintainershipKey, Placement, Precondition, Refusal, ShadowHome,
};
use norn_testkit::attestation::{Attestation, SEAM};
use norn_testkit::process::{Run, RunStatus, Sandbox};

/// The variable that tells a run it is the child, and where its tree is.
const ROLE: &str = "NORN_FS_LOCKDOWN_ROOT";

/// The variable naming the precondition the child publishes under.
const PRECONDITION: &str = "NORN_FS_LOCKDOWN_PRECONDITION";

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
/// this binary starts, so the call below is the ordinary public entry point and
/// nothing here knows a checkpoint was armed.
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
        publish(Path::new(&root));
        return;
    }

    let tree = Tree::new("unarmed");
    let outcome = tree.spawn(&[], None);

    assert_eq!(
        outcome.status,
        RunStatus::Exited(0),
        "{}",
        outcome.stderr_text()
    );
    assert_eq!(tree.destination_bytes(), Some(NEW.to_vec()));
    assert_eq!(tree.shadow_names(), Vec::<String>::new());

    let attested = tree.attestation();
    attested.assert_never_reached("a publication with nothing armed", &[(SEAM, WRITE_SEAM)]);
    attested.assert_reached(
        "a publication with nothing armed",
        &[(SEAM, CHILD_SEAM), ("outcome", "written")],
    );
    attested.assert_count("a publication with nothing armed", 1);
}

/// Publish through the public entry point and record what it answered.
///
/// The record is the child's own half of the attestation: the seam says which
/// checkpoint the protocol reached, and this says what the call returned to a
/// caller that survived to read it. A checkpoint armed to end the process
/// writes the first and never the second, which is how a parent tells "it died
/// there" from "it refused there".
fn publish(root: &Path) {
    let expected = match std::env::var_os(PRECONDITION)
        .as_deref()
        .and_then(|it| it.to_str())
    {
        Some("stale") => ContentHash::of(STALE),
        _ => ContentHash::of(OLD),
    };
    let shadows = shadow_home(root);
    let outcome = norn_fs::write(
        &root.join("vault").join(DOCUMENT),
        NEW,
        Precondition::Replace(expected),
        &shadows,
    );
    record(root, &child_record(&outcome));
}

/// What the child says about a return it lived to see.
fn child_record(outcome: &Result<Landed, Refusal>) -> String {
    match outcome {
        Ok(Landed::Written(_)) => format!("seam={CHILD_SEAM} outcome=written"),
        Ok(Landed::Unchanged(_)) => format!("seam={CHILD_SEAM} outcome=unchanged"),
        Err(Refusal::Drifted { .. }) => format!("seam={CHILD_SEAM} outcome=drifted"),
        Err(refusal @ Refusal::Environment { .. }) => {
            let errno = if refusal.is_os_error(libc::ENOSPC) {
                "ENOSPC"
            } else {
                "other"
            };
            format!("seam={CHILD_SEAM} outcome=environment errno={errno}")
        }
        Err(other) => format!("seam={CHILD_SEAM} outcome=refused kind={other:?}"),
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
    let outcome = tree.spawn(&[], Some("stale"));

    assert_eq!(
        outcome.status,
        RunStatus::Exited(0),
        "{}",
        outcome.stderr_text()
    );
    assert_eq!(
        tree.destination_bytes(),
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

/// **The process ends at each checkpoint of the publication, one at a time.**
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
        ("create", "create=ends", OLD, Staged::Nothing),
        ("write", "write=ends", OLD, Staged::Empty),
        ("sync", "sync=ends", OLD, Staged::Holding(NEW)),
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
        let outcome = tree.spawn(&[("NORN_FS_ARMED_STAGES", arm)], None);

        assert_eq!(
            outcome.status,
            RunStatus::Signaled(SIGABRT),
            "the {label} checkpoint did not end the process it was armed in\n{}",
            outcome.stderr_text()
        );
        assert_eq!(
            tree.destination_bytes(),
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
            &[(SEAM, WRITE_SEAM), ("stage", label), ("answer", "ends")],
        );
        attested.assert_never_reached(label, &[(SEAM, CHILD_SEAM)]);
    }
}

// ---------------------------------------------------------------------------
// A full disk
// ---------------------------------------------------------------------------

/// **A full disk at a staging checkpoint refuses, and publishes nothing.**
///
/// `ENOSPC` has no [`std::io::ErrorKind`] of its own, so the refusal carries the
/// error number and the child reads its classification off that rather than off
/// the message. Every one of these stages is before the swap, so the required
/// outcome is one sentence: the destination is exactly what it was, and the
/// shadow the refusal abandoned is removed on the way out.
#[test]
fn a_full_disk_before_the_swap_refuses_and_leaves_the_destination_alone() {
    for (label, arm) in [
        ("create", "create=full-disk"),
        ("write", "write=full-disk"),
        ("sync", "sync=full-disk"),
        ("swap", "swap=full-disk"),
    ] {
        let tree = Tree::new(&format!("full-{label}"));
        let outcome = tree.spawn(&[("NORN_FS_ARMED_STAGES", arm)], None);

        assert_eq!(
            outcome.status,
            RunStatus::Exited(0),
            "{label}: {}",
            outcome.stderr_text()
        );
        assert_eq!(
            tree.destination_bytes(),
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

/// **A full disk after the swap is not a write that did not happen.**
///
/// The parent directory's fsync runs after the rename has already published the
/// name, and this crate's protocol reports its failure nowhere: the change is at
/// the name and every reader can see it, so a refusal would say something false
/// about a file the caller can already read. What the bar holds is the half that
/// is not silence — the destination holds the *complete* new document, never a
/// prefix of it, and nothing is left staged.
#[test]
fn a_full_disk_at_the_parent_sync_leaves_the_new_document_published() {
    let tree = Tree::new("full-parent-sync");
    let outcome = tree.spawn(&[("NORN_FS_ARMED_STAGES", "parent-sync=full-disk")], None);

    assert_eq!(
        outcome.status,
        RunStatus::Exited(0),
        "{}",
        outcome.stderr_text()
    );
    assert_eq!(tree.destination_bytes(), Some(NEW.to_vec()));
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
        &[(SEAM, CHILD_SEAM), ("outcome", "written")],
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
    fn new(label: &str) -> Tree {
        let sandbox = Sandbox::new(
            Path::new(env!("CARGO_TARGET_TMPDIR")),
            &format!("lockdown-{label}"),
        )
        .expect("a sandbox");
        let root = sandbox.work_dir();
        std::fs::create_dir_all(root.join("vault")).expect("a vault root");
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

    /// Run this binary again as the child, armed as `arm` says.
    fn spawn(
        &self,
        arm: &[(&str, &str)],
        precondition: Option<&str>,
    ) -> norn_testkit::process::Outcome {
        let mut run = Run::new(&self.sandbox, std::env::current_exe().expect("this binary"))
            .args([
                "--exact",
                "the_child_role_publishes_under_whatever_it_was_armed_at",
                "--nocapture",
            ])
            .deadline(CHILD_DEADLINE)
            .env(ROLE, self.root())
            .env("NORN_FS_ARM_HITS", hits_path(self.root()));
        for (name, value) in arm {
            run = run.env(*name, value);
        }
        if let Some(precondition) = precondition {
            run = run.env(PRECONDITION, precondition);
        }
        run.wait().expect("running the lockdown child")
    }

    fn destination_bytes(&self) -> Option<Vec<u8>> {
        std::fs::read(self.root.join("vault").join(DOCUMENT)).ok()
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
