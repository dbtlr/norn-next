//! Making a write fail without the environment cooperating.
//!
//! Two of this kernel's contract claims are about conditions a test cannot
//! arrange: a disk that fills between the shadow's first byte and its last, and
//! an fsync that fails after a rename has already published a name. Both are
//! real, both are the reason the code has the shape it has, and neither is
//! reachable by writing files into a temporary directory.
//!
//! So each stage of the protocol asks [`Faults`] whether it is the stage that
//! fails. [`Faults::entry`] is what the public entry points pass; the in-crate
//! suite passes a stage and an [`Answer`] and reads what the protocol did with
//! it.
//!
//! The same shape carries the other half of what a test cannot arrange: a
//! foreign writer landing inside a window one call wide. [`Window`] names those
//! windows, and a disturbance is handed the window it is standing in.
//!
//! **The seam is deliberately small.** It names *where* a write can be made to
//! fail, and *how* — an error, a full disk, or the end of the process — and
//! never what the protocol does next, which is the code under test.
//!
//! # Reaching it from outside
//!
//! One stage of the widening is taken and no more: under the `induced-failure`
//! feature, [`write`](crate::write) arms itself from this process's environment
//! rather than passing [`Faults::NONE`]. That is what a lockdown suite's
//! process-death bars need and what nothing else can give them — a stage whose
//! required outcome is "this process does not survive here" cannot be reached
//! by a caller that has to return to make its assertion, so the arm is read by
//! the child that dies and the assertion is made by the parent that spawned it.
//!
//! Two variables carry it, both read once:
//!
//! - `NORN_FS_ARMED_STAGES` — the arm, as comma-separated `stage=answer` pairs,
//!   spelled `create`, `write`, `sync`, `swap`, `parent-sync`, `cleanup` and
//!   `fails`, `full-disk`, `ends`. A pair this module cannot read is a mistake
//!   in the harness rather than a stage nothing is armed at, so it panics.
//! - `NORN_FS_ARM_HITS` — a file each fired arm appends one record to before it
//!   answers, so a parent reads *which* checkpoint the protocol reached rather
//!   than inferring it from what the child left behind. Neutering a stage's
//!   `check` call takes its record away, which is what makes a bypassed hook
//!   fail the case it was supposed to carry.
//!
//! Nothing outside this crate arms anything without the feature, and a shipped
//! build has no reader for either variable.
//!
//! Two sibling seams carry the effect surfaces this one does not: the watcher's,
//! widened once at watch establishment, and the walk's, widened once at a walk's
//! construction. Both stand under the same feature and append to the same record
//! file, under the `norn-fs/watch` and `norn-fs/walk` seam names. Each seam
//! answers at its own boundary and at no other, which is what the `seam` field in
//! a record says. All three write their records through [`append_record`] here,
//! so the one discipline they share cannot drift into three.

use std::io;

/// The environment variable naming the stages this process is armed at.
#[cfg(feature = "induced-failure")]
pub(crate) const ARMED_STAGES: &str = "NORN_FS_ARMED_STAGES";

/// The environment variable naming the file fired arms record themselves in.
#[cfg(feature = "induced-failure")]
pub(crate) const ARM_HITS: &str = "NORN_FS_ARM_HITS";

/// The seam a record written by the write protocol names itself under.
#[cfg(feature = "induced-failure")]
const SEAM: &str = "norn-fs/write";

/// Append one record of a fired arm to the file `hits`.
///
/// All three fault seams write through here, because the discipline is one and a
/// second copy of it is a copy that drifts: one line per firing, and no
/// buffering — a record still in this process's memory when the process ends is
/// a record the harness never reads — so it opens, writes, syncs and closes
/// each time.
///
/// **What a failure means is the caller's, and the callers differ.** The
/// write protocol's arm often fires in a process that is about to abort, where
/// best effort is the only kind of effort there is; the watcher's and the walk's
/// fire in a process that lives to be asked, where a named file that cannot be
/// written would leave a parent reading silence as a boundary that was never
/// reached. [`record_or_abort`] is that second reading.
#[cfg(any(test, feature = "induced-failure"))]
#[allow(clippy::disallowed_methods, clippy::disallowed_types)] // The arm's own record file, outside the vault.
pub(crate) fn append_record(
    hits: &std::path::Path,
    seam: &str,
    stage: &str,
    answer: &str,
) -> io::Result<()> {
    use std::io::Write as _;

    let record = format!("seam={seam} stage={stage} answer={answer}\n");
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(hits)?;
    file.write_all(record.as_bytes())?;
    file.sync_all()
}

/// Append one record of a fired arm, or end the process saying it could not.
///
/// The reading the seams that outlive their arms share. A harness that named no
/// record file wants none. A harness that named one wants every firing in it, so
/// a file that cannot be written ends the process rather than leaving a parent
/// to read the silence as a boundary the code never reached.
///
/// **Deliberately an abort rather than a panic.** One of the boundaries this
/// carries answers on a backend's own delivery thread, where an unwind ends that
/// thread and leaves the subscription standing — which is the same silence,
/// reached a different way. Nothing here is recoverable in any case: the harness
/// named a file this process cannot write, and every later arm would meet it
/// too. The reason goes to standard error first, because an abort says nothing
/// on its own.
#[cfg(any(test, feature = "induced-failure"))]
pub(crate) fn record_or_abort(
    hits: Option<&std::path::Path>,
    seam: &str,
    stage: &str,
    answer: &str,
) {
    let Some(hits) = hits else {
        return;
    };
    if let Err(error) = append_record(hits, seam, stage, answer) {
        eprintln!(
            "norn-fs: the {stage} arm could not record itself in {}: {error}",
            hits.display()
        );
        std::process::abort();
    }
}

/// A point in the write protocol that can be made to fail.
///
/// The stages are the ones whose failure has a *different* required outcome,
/// which is what makes each of them worth naming. A failure before the swap
/// refuses and leaves the destination alone; a failure after it has already
/// published a name and must never read as a write that did not happen.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Stage {
    /// Opening the shadow, or the destination of an exclusive create.
    Create,
    /// Putting the content into it.
    Write,
    /// Getting those bytes onto the disk, before any name points at them.
    Sync,
    /// The rename that publishes them.
    Swap,
    /// The parent directory's fsync, which happens after the name is already
    /// live.
    ParentSync,
    /// Removing a shadow, or a destination a create could not finish. Injecting
    /// here stands in for a removal the filesystem refused — the one condition
    /// whose required behavior is to change nothing at all about the outcome.
    Cleanup,
}

// The stage vocabulary — the roster and the names — is what a harness arms and
// what a record names, so it compiles where something reads it and nowhere
// else. It still belongs to the seam rather than to the suite: the names are
// what the widened half is stated over, and a build that carried a different
// set would be a different seam. The variants themselves carry no such gate:
// the protocol names one at every stage it checks, in every build.
impl Stage {
    /// Every stage, in the order the replacement protocol runs them.
    #[cfg(any(test, feature = "induced-failure"))]
    pub(crate) const ALL: [Stage; 6] = [
        Stage::Create,
        Stage::Write,
        Stage::Sync,
        Stage::Swap,
        Stage::ParentSync,
        Stage::Cleanup,
    ];

    /// The name a harness arms this stage under, which is also the name a
    /// record of it carries.
    #[cfg(any(test, feature = "induced-failure"))]
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Stage::Create => "create",
            Stage::Write => "write",
            Stage::Sync => "sync",
            Stage::Swap => "swap",
            Stage::ParentSync => "parent-sync",
            Stage::Cleanup => "cleanup",
        }
    }

    /// The stage `name` spells, or nothing where it spells none.
    #[cfg(feature = "induced-failure")]
    fn named(name: &str) -> Option<Stage> {
        Stage::ALL.into_iter().find(|stage| stage.name() == name)
    }
}

/// What an armed stage does when the protocol reaches it.
///
/// The three are the three shapes a write's environment fails in, and each has
/// a different required outcome: an error the protocol refuses on, a disk with
/// no room left, and a machine that stops between two system calls. Naming them
/// apart is what lets one arm say `ENOSPC` — which has no
/// [`io::ErrorKind`](std::io::ErrorKind) of its own, and which a caller reads
/// off the refusal as an error number — and another say nothing at all, because
/// the process it was armed in does not reach a return.
// The allow stays at the enum rather than moving onto its items: nothing in a
// build that arms nothing constructs an answer at all, and a variant nobody
// names is what the lint reads as dead. The variants are the seam's vocabulary
// even so — an unarmed build has to hold the same three, or the arm a harness
// spells means something different from the arm this seam answers.
#[cfg_attr(not(feature = "induced-failure"), allow(dead_code))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Answer {
    /// The stage meets this error.
    Fails(io::ErrorKind),
    /// The stage meets a full disk: `ENOSPC`, carrying the error number, which
    /// is the only way a refusal can be told apart from any other write failure.
    MeetsAFullDisk,
    /// The process ends at the stage, before the stage's own work runs. No
    /// unwinding, no destructor, no shadow removed on the way out — which is
    /// what a machine losing power between two system calls leaves behind, and
    /// the only thing the process-death bars can be stated over.
    Ends,
}

impl Answer {
    /// The name a harness arms this answer under.
    #[cfg(any(test, feature = "induced-failure"))]
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Answer::Fails(_) => "fails",
            Answer::MeetsAFullDisk => "full-disk",
            Answer::Ends => "ends",
        }
    }

    /// The answer `name` spells, or nothing where it spells none.
    #[cfg(feature = "induced-failure")]
    fn named(name: &str) -> Option<Answer> {
        match name {
            "fails" => Some(Answer::Fails(io::ErrorKind::Other)),
            "full-disk" => Some(Answer::MeetsAFullDisk),
            "ends" => Some(Answer::Ends),
            _ => None,
        }
    }

    /// The error this answer meets a stage with.
    fn error(self, stage: Stage) -> io::Error {
        match self {
            Answer::Fails(kind) => {
                io::Error::new(kind, format!("injected failure at the {stage:?} stage"))
            }
            Answer::MeetsAFullDisk => io::Error::from_raw_os_error(libc::ENOSPC),
            // Nothing reaches this: `check` ends the process before it asks for
            // an error to return.
            Answer::Ends => io::Error::other("the process was armed to end here"),
        }
    }
}

/// A point in the protocol where a foreign actor's act can be made to land.
///
/// Each window is one call wide in a real run, which is why a test arranges it
/// rather than races for it: the defense would otherwise be asserted instead of
/// checked. The windows are the ones where something outside this process can
/// make a statement the protocol is about to make untrue.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Window {
    /// A replacement's precondition is satisfied and nothing is staged yet.
    Composed,
    /// A create has claimed its name and has not filled it.
    Claimed,
    /// A removal's precondition is satisfied and the name has not been confirmed
    /// to still resolve to the handle that was read.
    Vacating,
    /// A move's source has been read and the name has not been confirmed to
    /// still resolve to the handle it was read through.
    SourceRead,
    /// A move's destination holds the document and its source has not been
    /// removed yet.
    BetweenLegs,
}

/// Which stages of a write fail, and how.
///
/// A list rather than one entry, because two of the claims are about what
/// happens when a *second* thing goes wrong: a swap that fails and then a
/// cleanup that cannot happen either is the condition under which a shadow
/// leaks, and it is unreachable if only one stage at a time can be made to fail.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Faults {
    injected: &'static [(Stage, Answer)],
}

impl Faults {
    /// A write that fails only where the machine makes it fail.
    pub(crate) const NONE: Faults = Faults { injected: &[] };

    /// A write that answers each named stage the named way.
    #[cfg(test)]
    pub(crate) const fn at(injected: &'static [(Stage, Answer)]) -> Faults {
        Faults { injected }
    }

    /// What a public entry point passes.
    ///
    /// Without the `induced-failure` feature this is [`Faults::NONE`] and the
    /// entry point asks one comparison against an empty list. With it, the
    /// answer is whatever this process was started armed with — read once, and
    /// empty in every process that armed nothing, which is every process that
    /// is not a lockdown suite's child.
    pub(crate) fn entry() -> Faults {
        #[cfg(feature = "induced-failure")]
        {
            Faults {
                injected: armed::stages(),
            }
        }
        #[cfg(not(feature = "induced-failure"))]
        {
            Faults::NONE
        }
    }

    /// The error `stage` is supposed to meet, if it is one of the injected ones.
    ///
    /// A stage the arm names is recorded before it is answered, so a record
    /// stands for every checkpoint the protocol actually reached — including the
    /// one the process does not return from.
    pub(crate) fn check(&self, stage: Stage) -> io::Result<()> {
        for (injected, answer) in self.injected {
            if *injected == stage {
                #[cfg(feature = "induced-failure")]
                armed::record(stage, *answer);
                if *answer == Answer::Ends {
                    // Deliberately an abort rather than a panic: a panic unwinds,
                    // and an unwind runs the removals that make a half-published
                    // write tidy again. The tidy end is not the one this bar is
                    // about.
                    std::process::abort();
                }
                return Err(answer.error(stage));
            }
        }
        Ok(())
    }
}

/// The arm this process was started under.
///
/// Both readings happen once and are then held: a write asks the seam six times
/// and a heal-scale run asks it a great many more, so re-reading the environment
/// per stage would make the feature's cost a function of how much is written.
#[cfg(feature = "induced-failure")]
mod armed {
    use std::sync::OnceLock;

    use super::{ARM_HITS, ARMED_STAGES, Answer, Stage};

    /// The stages this process is armed at, in the order they were named.
    pub(super) fn stages() -> &'static [(Stage, Answer)] {
        static STAGES: OnceLock<Vec<(Stage, Answer)>> = OnceLock::new();
        STAGES.get_or_init(|| match std::env::var_os(ARMED_STAGES) {
            None => Vec::new(),
            Some(spelling) => parse(
                spelling
                    .to_str()
                    .unwrap_or_else(|| panic!("{ARMED_STAGES} is not UTF-8")),
            ),
        })
    }

    /// Read `stage=answer` pairs, and refuse a spelling that names neither.
    ///
    /// A misspelled arm that quietly armed nothing would pass every bar it was
    /// supposed to carry, so an unreadable pair ends the process saying so.
    fn parse(spelling: &str) -> Vec<(Stage, Answer)> {
        spelling
            .split(',')
            .filter(|pair| !pair.is_empty())
            .map(|pair| {
                let (stage, answer) = pair
                    .split_once('=')
                    .unwrap_or_else(|| panic!("`{pair}` in {ARMED_STAGES} is not `stage=answer`"));
                let stage = Stage::named(stage)
                    .unwrap_or_else(|| panic!("`{stage}` in {ARMED_STAGES} names no stage"));
                let answer = Answer::named(answer)
                    .unwrap_or_else(|| panic!("`{answer}` in {ARMED_STAGES} names no answer"));
                (stage, answer)
            })
            .collect()
    }

    /// Append one record saying which checkpoint fired and how it answered.
    ///
    /// Best effort by construction: the process this runs in is often about to
    /// end, and a harness that armed no record file wants none. A record that
    /// could not be written is dropped rather than raised, because raising it
    /// would replace the death this arm exists to cause with a different one.
    pub(super) fn record(stage: Stage, answer: Answer) {
        static HITS: OnceLock<Option<std::path::PathBuf>> = OnceLock::new();
        let Some(path) = HITS
            .get_or_init(|| std::env::var_os(ARM_HITS).map(std::path::PathBuf::from))
            .as_ref()
        else {
            return;
        };
        let _ = super::append_record(path, super::SEAM, stage.name(), answer.name());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The seam asks nothing when nothing is injected. A default that failed
    /// somewhere would make every ordinary write a test of this module.
    #[test]
    fn no_fault_lets_every_stage_through() {
        for stage in Stage::ALL {
            Faults::NONE.check(stage).expect("no injected failure");
        }
    }

    /// One stage fails and the others do not, so a bar reaches the stage it
    /// names rather than the first one the protocol happens to run.
    #[test]
    fn an_injected_fault_fires_at_one_stage_only() {
        let faults = Faults::at(&[(Stage::Swap, Answer::Fails(io::ErrorKind::PermissionDenied))]);
        let error = faults.check(Stage::Swap).expect_err("the injected stage");
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        assert!(error.to_string().contains("Swap"), "{error}");
        for other in Stage::ALL.into_iter().filter(|it| *it != Stage::Swap) {
            faults.check(other).expect("a stage nothing injected");
        }
    }

    /// A full disk carries the error number a caller classifies on. There is no
    /// [`io::ErrorKind`] for it, so a refusal that lost the number is a refusal
    /// nothing can tell apart from any other failed write.
    #[test]
    fn a_full_disk_carries_the_error_number_that_names_it() {
        let faults = Faults::at(&[(Stage::Write, Answer::MeetsAFullDisk)]);
        let error = faults.check(Stage::Write).expect_err("the injected stage");
        assert_eq!(error.raw_os_error(), Some(libc::ENOSPC));
    }

    /// Every stage and every answer a harness arms round-trips through the name
    /// it is armed under, so a widened seam and the suite arming it cannot drift
    /// into naming different things.
    #[test]
    fn every_stage_and_answer_has_one_name() {
        let names: std::collections::BTreeSet<&str> =
            Stage::ALL.iter().map(|stage| stage.name()).collect();
        assert_eq!(names.len(), Stage::ALL.len());
        for answer in [
            Answer::Fails(io::ErrorKind::Other),
            Answer::MeetsAFullDisk,
            Answer::Ends,
        ] {
            assert!(!answer.name().is_empty());
        }
    }
}
