//! Arming the watcher fault seam the way anything outside this crate arms it.
//!
//! The seam's own suite constructs an arm and hands it to watch establishment
//! directly, which is what an in-crate case can do and what nothing else can.
//! The half that carries every consumer of this seam is the other one: two
//! environment variables, read once by the process the watch is established in,
//! spelled by a parent that never links against the seam at all.
//!
//! That half cannot be stated in a process that is already running — the arm is
//! read once, and a suite that mutated its own environment would arm every case
//! sharing the binary — so each case here is two processes. A child is spawned
//! with the arm in its environment, establishes coverage through the ordinary
//! public entry point, and records what it got; this parent reads that beside
//! the record the seam itself wrote.
//!
//! **The control is what makes the record assertions mean something.** The same
//! child, spawned the same way with the same live record file and nothing armed,
//! reaches a live subscription and writes no seam record at all — so a case that
//! asserts a record asserts something this run proves is not free.
//!
//! **The backend is the polling one, deliberately.** What is under test is the
//! arm's route from an environment variable to an answered boundary, which is
//! the same route on every backend; polling is the one that costs no turn at the
//! machine-wide notification service.
//!
//! **The record file sits outside every watched edge**, in a directory beside
//! the tree. The seam writes it while coverage is live, so a record file inside
//! the vault or in the vault's own parent would be a filesystem change the
//! backend reports back into the batches under assertion.

#![allow(clippy::disallowed_methods, clippy::disallowed_types)] // Acceptance fixture: arranging and judging a tree.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use norn_fs::{RescanScope, SubscriptionState, WatchError};
use norn_testkit::attestation::{Attestation, SEAM};
use norn_testkit::process::{Run, RunStatus, Sandbox};
use norn_testkit::wait::{Budget, Observed, wait_until};

/// The variable that tells a run it is the child, and where its tree is.
const ROLE: &str = "NORN_FS_WATCH_LOCKDOWN_ROOT";

/// The variable a harness arms the watcher seam through.
const ARMED_STAGES: &str = "NORN_FS_WATCH_ARMED_STAGES";

/// The variable naming the file every fired arm records itself in.
const ARM_HITS: &str = "NORN_FS_ARM_HITS";

/// The seam the widened watcher hooks record themselves under.
const WATCH_SEAM: &str = "norn-fs/watch";

/// The seam the child records its own return under.
const CHILD_SEAM: &str = "lockdown/watch-child";

/// A child establishes a watch and waits out one boundary. A child still
/// running after this wedged, and killing it reports better than a suite that
/// hangs.
const CHILD_DEADLINE: Duration = Duration::from_secs(60);

/// The signal an abort raises, which is how an arm that cannot record itself
/// reports that to the parent.
const SIGABRT: i32 = 6;

/// What the child gives a boundary that is not withheld. Long enough that a
/// loaded machine crossing it slowly is not a failure, and never reached at all
/// by a child whose barrier is armed.
const SYNCHRONIZATION_DEADLINE: Duration = Duration::from_secs(30);

/// What the child gives the backend to report a change it made itself.
fn budget() -> Budget {
    Budget::new(Duration::from_secs(20), Duration::from_millis(250))
}

// ---------------------------------------------------------------------------
// The child role
// ---------------------------------------------------------------------------

/// The watch every case in this file establishes, in whichever process runs it.
///
/// In the child it is the subject: the arm is already in the environment when
/// this binary starts, so the call below is the ordinary public entry point and
/// nothing here knows a boundary was armed. In an ordinary run of the suite it
/// spawns itself with nothing armed, which is the control.
#[test]
fn the_child_role_watches_under_whatever_it_was_armed_at() {
    if let Some(root) = std::env::var_os(ROLE) {
        watch_under_whatever_is_armed(Path::new(&root));
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
    let attested = tree.attestation();
    // A live subscription that reported the child's own change by path: the
    // whole route an armed case takes, with nothing standing in it anywhere.
    attested.assert_reached(
        "a watch with nothing armed",
        &[(SEAM, CHILD_SEAM), ("outcome", "reported")],
    );
    attested.assert_never_reached("a watch with nothing armed", &[(SEAM, WATCH_SEAM)]);
    attested.assert_count("a watch with nothing armed", 1);
}

/// Establish coverage through the public entry point and record what happened.
///
/// The record is the child's own half of the attestation: the seam says which
/// boundary the watcher reached, and this says what a caller that lived to ask
/// was given. The two together are what separate a refused registration from a
/// withheld boundary from a displaced delivery — outcomes a parent cannot tell
/// apart from the tree the child leaves behind, because a watch leaves nothing
/// behind at all.
fn watch_under_whatever_is_armed(root: &Path) {
    let (subscription, _own_writes) = match norn_fs::watch_polling(&vault(root), &schema(root)) {
        Ok(watched) => watched,
        Err(WatchError::Backend(_)) => {
            record(root, "outcome=refused kind=backend");
            return;
        }
        Err(other) => {
            record(root, &format!("outcome=refused kind={other:?}"));
            return;
        }
    };
    match subscription.synchronize(SYNCHRONIZATION_DEADLINE) {
        Err(WatchError::SynchronizationExpired) => {
            record(root, "outcome=expired");
            return;
        }
        Err(other) => {
            record(root, &format!("outcome=terminal kind={other:?}"));
            return;
        }
        Ok(()) => {}
    }
    assert_eq!(subscription.state(), SubscriptionState::Live);

    // A change of the child's own, so that a stream arm has a delivery to stand
    // in place of. What comes back says which: the rescan a lost path set is
    // reported as, or the path itself.
    std::fs::write(vault(root).join("one.md"), b"one\n").expect("a change under the watch");
    let observed = wait_until(
        "the backend to report the child's own change",
        budget(),
        || match subscription.try_recv() {
            Err(WatchError::Backend(_)) => Observed::Met("outcome=failed".to_owned()),
            Err(other) => Observed::Met(format!("outcome=terminal kind={other:?}")),
            Ok(Some(batch)) if batch.rescans().contains(&RescanScope::Vault) => {
                Observed::Met("outcome=rescanned".to_owned())
            }
            Ok(Some(batch)) if !batch.vault_roots().is_empty() => {
                Observed::Met("outcome=reported".to_owned())
            }
            other => Observed::Pending(format!("{other:?}")),
        },
    )
    .unwrap_or_else(|failure| panic!("{failure}"));
    record(root, &observed);
}

/// Append one record of what the child was given, beside the seam's own.
fn record(root: &Path, fields: &str) {
    use std::io::Write as _;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(hits_path(root))
        .expect("the arm-hit record");
    writeln!(file, "seam={CHILD_SEAM} {fields}").expect("recording what the watch answered");
    file.sync_all().expect("recording what the watch answered");
}

// ---------------------------------------------------------------------------
// Every stage, armed through the environment
// ---------------------------------------------------------------------------

/// **Each stage answers when a process is started armed at it.** The variable
/// name, the pair spelling, the record file and the seam's own record are one
/// route, and this is the case that walks it end to end for every stage.
///
/// The outcome column is what a caller was given, which is a different fact
/// from the record beside it: a watch that refused for some reason of its own
/// would satisfy the first and not the second, and a seam whose hook was
/// removed would satisfy neither.
#[test]
fn every_watcher_stage_is_armed_through_the_environment() {
    for (label, arm, answer, outcome) in [
        ("install", "install=refuses", "refuses", "refused"),
        ("barrier", "barrier=expires", "expires", "expired"),
        ("stream", "stream=rescans", "rescans", "rescanned"),
        ("stream", "stream=fails", "fails", "failed"),
    ] {
        let tree = Tree::new(&format!("armed-{label}-{answer}"));
        let run = tree.spawn(&[(ARMED_STAGES, arm)]);

        assert_eq!(
            run.status,
            RunStatus::Exited(0),
            "{arm}: {}",
            run.stderr_text()
        );
        let attested = tree.attestation();
        attested.assert_reached(
            arm,
            &[(SEAM, WATCH_SEAM), ("stage", label), ("answer", answer)],
        );
        attested.assert_reached(arm, &[(SEAM, CHILD_SEAM), ("outcome", outcome)]);
        // One watch per process, one firing per arm: a second record under this
        // seam would mean the arm answered somewhere else too.
        assert_eq!(
            attested
                .hits()
                .iter()
                .filter(|hit| hit.get(SEAM) == Some(WATCH_SEAM))
                .count(),
            1,
            "{arm}: the arm fired more than once"
        );
    }
}

/// **An arm this seam cannot read ends the process rather than arming
/// nothing.** A harness that misspells a pair, or spells one stage twice, has
/// written a case that would otherwise pass by never meeting its condition at
/// all.
#[test]
fn an_unreadable_arm_refuses_the_process_it_was_spelled_for() {
    for spelling in [
        "install",
        "registration=refuses",
        "install=full-disk",
        "barrier=rescans",
        "stream=rescans,stream=fails",
    ] {
        let tree = Tree::new("unreadable");
        let run = tree.spawn(&[(ARMED_STAGES, spelling)]);

        assert_ne!(
            run.status,
            RunStatus::Exited(0),
            "`{spelling}` was read as an arm"
        );
        tree.attestation()
            .assert_never_reached(spelling, &[(SEAM, CHILD_SEAM)]);
    }
}

/// **An arm that cannot write the record it was given ends the process.** The
/// harness named a file, so it wants every firing in it; a firing that recorded
/// nothing reads to a parent exactly like a boundary the watcher never reached,
/// and that is the one reading this seam must never allow.
///
/// The child dies by abort rather than by unwinding, and the reason is on its
/// standard error. An unwind would be enough on the establishing thread and not
/// enough on the backend's delivery thread — where one of the three boundaries
/// answers, and where an unwind ends that thread and leaves the subscription
/// standing.
#[test]
fn an_arm_that_cannot_record_itself_ends_the_process() {
    let tree = Tree::new("unwritable-record");
    let unwritable = tree.root().join("records/no-such-directory/arm-hits");

    let run = tree.spawn(&[
        (ARMED_STAGES, "install=refuses"),
        (ARM_HITS, &unwritable.to_string_lossy()),
    ]);

    assert_eq!(
        run.status,
        RunStatus::Signaled(SIGABRT),
        "an arm answered without the record its harness asked for\n{}",
        run.stderr_text()
    );
    assert!(
        run.stderr_text().contains("could not record itself"),
        "the abort said nothing about why: {}",
        run.stderr_text()
    );
    assert!(
        !unwritable.exists(),
        "the record the arm could not write is there after all"
    );
}

// ---------------------------------------------------------------------------
// The tree
// ---------------------------------------------------------------------------

/// Distinguishes two trees taken in the same process.
static SERIAL: AtomicU64 = AtomicU64::new(0);

/// A vault, its schema source, and the record file the arm and the child share.
struct Tree {
    sandbox: Sandbox,
    root: PathBuf,
}

impl Tree {
    fn new(label: &str) -> Tree {
        let sandbox = Sandbox::new(
            Path::new(env!("CARGO_TARGET_TMPDIR")),
            &format!(
                "watch-lockdown-{label}-{}-{}",
                std::process::id(),
                SERIAL.fetch_add(1, Ordering::Relaxed)
            ),
        )
        .expect("a sandbox");
        let root = sandbox.work_dir();
        std::fs::create_dir_all(vault(&root)).expect("a vault root");
        std::fs::write(schema(&root), b"version: 1\n").expect("a schema source");
        // Beside the tree rather than in it: the watched edges are the vault
        // and the vault's parent, and a record written under either is an event
        // the backend reports.
        std::fs::create_dir_all(root.join("records")).expect("a record directory");
        Tree { sandbox, root }
    }

    fn root(&self) -> &Path {
        &self.root
    }

    /// Run this binary again as the child, armed as `arm` says.
    ///
    /// The record file is named first, so a case that means to name a different
    /// one — or an unwritable one — spells it among the arm's own variables.
    fn spawn(&self, arm: &[(&str, &str)]) -> norn_testkit::process::Outcome {
        let mut run = Run::new(&self.sandbox, std::env::current_exe().expect("this binary"))
            .args([
                "--exact",
                "the_child_role_watches_under_whatever_it_was_armed_at",
                "--nocapture",
            ])
            .deadline(CHILD_DEADLINE)
            .env(ROLE, &self.root)
            .env(ARM_HITS, hits_path(&self.root));
        for (name, value) in arm {
            run = run.env(*name, value);
        }
        run.wait().expect("running the watch lockdown child")
    }

    fn attestation(&self) -> Attestation {
        Attestation::read(&hits_path(&self.root))
    }
}

fn vault(root: &Path) -> PathBuf {
    root.join("tree/vault")
}

fn schema(root: &Path) -> PathBuf {
    vault(root).join("schema.yml")
}

/// The file every arm in one case records itself in. It sits outside every
/// watched edge, so nothing the backend reports is a record.
fn hits_path(root: &Path) -> PathBuf {
    root.join("records/arm-hits")
}
