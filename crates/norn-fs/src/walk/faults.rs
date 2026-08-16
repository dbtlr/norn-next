//! Making one entry leave a directory page without a foreign writer
//! cooperating.
//!
//! Paging a directory is two observations: the listing that names the entries,
//! and the stat of each name it named. Another writer editing the vault can
//! unlink a name between them — or unlink it and give the name to something
//! else — and what the page owes that is a claim no test can arrange over a
//! temporary directory. The window is one call wide, so racing for it would
//! assert the convergence rather than check it.
//!
//! So the paging stat asks [`WalkFaults`] whether an arm stands in place of the
//! observation it was about to make. This is the third of this crate's fault
//! seams, beside the write protocol's ([`crate::faults`]) and the watcher's
//! (`crate::watch::faults`). The three answer at different boundaries, in
//! different vocabularies, and a record names which of them fired.
//!
//! **The seam is deliberately small.** It names *where* an observation can be
//! made to differ — the stat of one name a listing named — and *how*: the name
//! is gone, the machine refuses, the name holds a different kind now. It never
//! names what the page does next, which is the code under test. An arm hands
//! back an error number and a kind; the page's own answers to those are what a
//! case reads.
//!
//! # The one stage and its three answers
//!
//! [`Stage::Page`] is the whole roster, because the window is one place. Its
//! answers are the three conditions that window holds, and two required
//! outcomes across them:
//!
//! - [`Answer::Vanishes`] — the stat meets `ENOENT`. **The page drops the
//!   entry.** A walk begun now yields no entry at that name either, so dropping
//!   it is the answer the walk converges on, and it is the same answer the
//!   window between a yielded fact and its open has given since that window
//!   converged.
//! - [`Answer::Replaced`] — the stat observes a kind the listing did not name.
//!   **The page drops the entry too**: the name the listing named was unlinked
//!   and something else took it inside the same window, so what stands there is
//!   not the entry that was listed. The successor is a change of the vault's
//!   own, and the watcher events that change raised are what carry it.
//! - [`Answer::Denied`] — the stat meets `EACCES`. **The walk refuses**, and the
//!   refusal is terminal the way every environmental one is. A machine that will
//!   not answer says nothing about whether an entry is there, and reading it as
//!   absence would let a revoked permission prune derived state. The convergence
//!   above narrows this boundary to absence; it does not remove it.
//!
//! # Which entry an arm stands over
//!
//! **The first entry a page stats that the listing named a regular file**, once
//! in the process that armed it. Two things decide that:
//!
//! - *A regular file*, because the entry class is what makes the outcome
//!   readable. A file leaving a page is one document; a directory leaving one
//!   takes a whole subtree with it, and a case stated over that could not say
//!   whether the entries beside it were reached.
//! - *Once*, because the condition is one foreign edit landing in one window. An
//!   arm that answered every stat would state a vault every writer is emptying,
//!   which is a different claim and not one this seam is for — and a second walk
//!   of the same tree is not a second edit, so the firing is the process's rather
//!   than each walk's.
//!
//! Everything else in the page is stat'd by the machine, which is what lets a
//! case read the entries beside the one that left.
//!
//! # Reaching it from outside
//!
//! One stage of widening is taken and no more: under the `induced-failure`
//! feature, a walk arms itself from this process's environment rather than
//! carrying an empty arm. Two variables carry it, both read once per process:
//!
//! - `NORN_FS_WALK_ARMED_STAGES` — the arm, as comma-separated `stage=answer`
//!   pairs, spelled `page` and `vanishes`, `replaced`, `denied`. A pair this
//!   module cannot read — an unknown name, or one stage armed twice — is a
//!   mistake in the harness rather than a stage nothing is armed at, so it
//!   panics.
//! - `NORN_FS_ARM_HITS` — the record file the other two seams write, and the
//!   same one here. A fired arm appends `seam=norn-fs/walk stage=page
//!   answer=<name>` to it before it answers, so a harness reads which window the
//!   walk actually reached rather than inferring it from a heal that converged
//!   for some other reason. A file this process named and cannot write ends the
//!   process saying so, because a parent cannot tell a record that was never
//!   written from a window that was never reached.
//!
//! Nothing outside this crate arms anything without the feature, and a shipped
//! build has no reader for either variable.
//!
//! # What consumes it
//!
//! `norn-host`'s lockdown suite arms one answer per case in a child process,
//! attaches a real host over a real tree, and states what the heal owes: an
//! entry that vanishes is dropped and the heal completes with every other
//! document derived, and an entry the machine will not stat refuses the heal and
//! leaves the vault entry untrusted. This crate's own cases arm the seam in code
//! and read the page itself, which is the observation a host cannot make.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use rustix::io::Errno;

use super::EntryKind;

/// The environment variable naming the walk stages this process is armed at.
#[cfg(feature = "induced-failure")]
pub(crate) const ARMED_STAGES: &str = "NORN_FS_WALK_ARMED_STAGES";

/// The seam a record written here names itself under, which is what tells it
/// apart from the two sibling seams' records in the same file.
#[cfg(any(test, feature = "induced-failure"))]
const SEAM: &str = "norn-fs/walk";

/// A window in the walk that can be made to hold a foreign writer's edit.
///
/// One, because the walk has one such window: everything else it does with a
/// directory is a single call whose failure is the machine's own. A roster of
/// one still carries the `stage=answer` spelling the other two seams are armed
/// with, so a harness arms all three the same way.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Stage {
    /// Between the listing that names a directory's entries and the stat of one
    /// of those names.
    Page,
}

// The stage vocabulary — the roster and the names — is what a harness arms and
// what a record names, so it compiles where something reads it and nowhere
// else. It still belongs to the seam rather than to the suite: the names are
// what the widened half is stated over, and a build that carried a different
// set would be a different seam.
impl Stage {
    /// Every stage the walk holds.
    #[cfg(any(test, feature = "induced-failure"))]
    pub(crate) const ALL: [Stage; 1] = [Stage::Page];

    /// The name a harness arms this stage under, which is also the name a
    /// record of it carries.
    #[cfg(any(test, feature = "induced-failure"))]
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Stage::Page => "page",
        }
    }

    /// The stage `name` spells, or nothing where it spells none.
    #[cfg(any(test, feature = "induced-failure"))]
    fn named(name: &str) -> Option<Stage> {
        Stage::ALL.into_iter().find(|stage| stage.name() == name)
    }
}

/// What a foreign writer did inside the window, as the stat that follows it
/// observes.
///
/// Each is a thing the *observation* was, never a thing the page does with it.
/// Two of them are the same edit seen twice — a name taken away, and a name
/// taken away and given to something else — and the third is the machine
/// declining to answer at all, which the page owes the opposite outcome.
// The allow stays at the enum rather than moving onto its items: nothing in a
// build that arms nothing constructs an answer at all, and a variant nobody
// names is what the lint reads as dead. The variants are the seam's vocabulary
// even so — an unarmed build has to hold the same three, or the arm a harness
// spells means something different from the arm this seam answers.
#[cfg_attr(not(any(test, feature = "induced-failure")), allow(dead_code))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Answer {
    /// The entry is unlinked before the stat reaches it, so the stat meets
    /// `ENOENT`.
    Vanishes,
    /// The entry is unlinked and its name is taken by something of another
    /// kind, so the stat succeeds and observes a kind the listing did not name.
    Replaced,
    /// The machine will not answer for the name, so the stat meets `EACCES`.
    Denied,
}

impl Answer {
    /// Every answer, in the order this seam's doc names them.
    #[cfg(any(test, feature = "induced-failure"))]
    pub(crate) const ALL: [Answer; 3] = [Answer::Vanishes, Answer::Replaced, Answer::Denied];

    /// The name a harness arms this answer under.
    #[cfg(any(test, feature = "induced-failure"))]
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Answer::Vanishes => "vanishes",
            Answer::Replaced => "replaced",
            Answer::Denied => "denied",
        }
    }

    /// The answer `name` spells, or nothing where it spells none.
    #[cfg(any(test, feature = "induced-failure"))]
    fn named(name: &str) -> Option<Answer> {
        Answer::ALL.into_iter().find(|answer| answer.name() == name)
    }
}

/// Refuse an arm that names one stage twice.
///
/// It is the one mistake that would otherwise pass silently: the seam reads a
/// stage's first answer, so a second pair a harness spelled would simply not
/// happen, and the case would report on a condition it never met.
#[cfg(any(test, feature = "induced-failure"))]
fn refuse_an_unreadable_arm(armed: &[(Stage, Answer)], source: &str) {
    for (index, (stage, _)) in armed.iter().enumerate() {
        assert!(
            !armed[..index].iter().any(|(earlier, _)| earlier == stage),
            "the {} stage is armed twice in {source}",
            stage.name()
        );
    }
}

/// What an arm puts in place of one entry's paging stat.
///
/// Both fields describe the observation and neither describes the outcome: an
/// error number the stat meets instead of running, and the kind the stat
/// observed. The page's own answer to each — dropped, refused, kept — is the
/// code a case is reading, so an arm that decided it would be an arm standing in
/// front of its own subject. Nothing armed is both fields empty, which is what
/// every stat in an ordinary walk gets.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Paged {
    /// The error number the stat meets in place of running.
    pub(crate) meets: Option<Errno>,
    /// The kind the stat observed, where that is not the kind the machine's own
    /// mode bits say.
    pub(crate) observes: Option<EntryKind>,
}

/// Which window of a walk holds a foreign edit, and which one.
///
/// The default is the empty arm — a walk whose observations are all the
/// machine's own — and it is what every process that armed nothing carries.
///
/// **An armed process spends one arm across every walk it runs.** The condition
/// is one foreign edit landing in one window, and a second walk of the same tree
/// is not a second edit — so a heal that walks again after the entry left the
/// page sees the tree the machine holds, which is what a case reads a
/// convergence off.
#[derive(Debug, Default)]
pub(crate) struct WalkFaults {
    /// The stages this walk is armed at, in the order they were named.
    armed: &'static [(Stage, Answer)],
    /// The file a fired arm records itself in, where anything named one.
    #[cfg(any(test, feature = "induced-failure"))]
    hits: Option<std::path::PathBuf>,
    /// Whether the arm has already been spent on an entry. Shared with every
    /// other walk of an armed process, and this walk's own where a case spelled
    /// the arm in code.
    spent: Arc<AtomicBool>,
}

impl WalkFaults {
    /// A walk that answers each named stage the named way, recording nothing.
    #[cfg(test)]
    pub(crate) fn at(armed: &'static [(Stage, Answer)]) -> WalkFaults {
        refuse_an_unreadable_arm(armed, "this arm");
        WalkFaults {
            armed,
            ..WalkFaults::default()
        }
    }

    /// The same arm, recording each fired stage in `hits`.
    ///
    /// The record file is a value here rather than a reading of the process's
    /// environment, so a case in this crate's own suite reads the record its own
    /// arm wrote without arming the process every other case runs in.
    #[cfg(test)]
    pub(crate) fn recording_at(
        armed: &'static [(Stage, Answer)],
        hits: std::path::PathBuf,
    ) -> WalkFaults {
        WalkFaults {
            hits: Some(hits),
            ..WalkFaults::at(armed)
        }
    }

    /// What a walk's construction passes.
    ///
    /// Without the `induced-failure` feature this is an empty arm and each
    /// paging stat asks one comparison against an empty list. With it, the
    /// answer is whatever this process was started armed with — read once, and
    /// empty in every process that armed nothing — over the one firing that
    /// process has.
    pub(crate) fn entry() -> WalkFaults {
        #[cfg(feature = "induced-failure")]
        {
            WalkFaults {
                armed: armed::stages(),
                hits: armed::hits().cloned(),
                spent: armed::spent(),
            }
        }
        #[cfg(not(feature = "induced-failure"))]
        {
            WalkFaults::default()
        }
    }

    /// The answer this walk is armed with at `stage`, if it is armed there.
    fn answer(&self, stage: Stage) -> Option<Answer> {
        self.armed
            .iter()
            .find_map(|(armed, answer)| (*armed == stage).then_some(*answer))
    }

    /// What stands in place of the paging stat of an entry the listing named a
    /// `listed`.
    ///
    /// The arm is spent here, on the first regular file it is offered, and every
    /// stat after it is the machine's own again. An unarmed walk pays one
    /// comparison against an empty list and nothing else, which is what keeps
    /// the feature's cost off a heal-scale enumeration.
    pub(crate) fn paging(&self, listed: EntryKind) -> Paged {
        let Some(answer) = self.answer(Stage::Page) else {
            return Paged::default();
        };
        if listed != EntryKind::File || self.spent.swap(true, Ordering::AcqRel) {
            return Paged::default();
        }
        #[cfg(any(test, feature = "induced-failure"))]
        crate::faults::record_or_abort(
            self.hits.as_deref(),
            SEAM,
            Stage::Page.name(),
            answer.name(),
        );
        match answer {
            Answer::Vanishes => Paged {
                meets: Some(Errno::NOENT),
                ..Paged::default()
            },
            Answer::Denied => Paged {
                meets: Some(Errno::ACCESS),
                ..Paged::default()
            },
            // A directory standing where the listing named a file: the coarsest
            // kind change there is, and the one whose successor a walk begun now
            // descends into rather than yields.
            Answer::Replaced => Paged {
                observes: Some(EntryKind::Directory),
                ..Paged::default()
            },
        }
    }
}

/// The arm this process was started under.
///
/// Both readings happen once and are then held: a heal-scale walk asks the seam
/// once per entry, so re-reading the environment there would make the feature's
/// cost a function of how many documents the vault holds.
#[cfg(feature = "induced-failure")]
mod armed {
    use std::sync::Arc;
    use std::sync::OnceLock;
    use std::sync::atomic::AtomicBool;

    use super::{ARMED_STAGES, Answer, Stage, refuse_an_unreadable_arm};
    use crate::faults::ARM_HITS;

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

    /// The file fired arms record themselves in, where this process named one.
    pub(super) fn hits() -> Option<&'static std::path::PathBuf> {
        static HITS: OnceLock<Option<std::path::PathBuf>> = OnceLock::new();
        HITS.get_or_init(|| std::env::var_os(ARM_HITS).map(std::path::PathBuf::from))
            .as_ref()
    }

    /// The one firing this process has, held across every walk it runs.
    pub(super) fn spent() -> Arc<AtomicBool> {
        static SPENT: OnceLock<Arc<AtomicBool>> = OnceLock::new();
        SPENT.get_or_init(Arc::default).clone()
    }

    /// Read `stage=answer` pairs, and refuse a spelling this seam cannot answer.
    ///
    /// A misspelled arm that quietly armed nothing would pass every bar it was
    /// supposed to carry, so an unreadable pair ends the process saying so.
    pub(super) fn parse(spelling: &str) -> Vec<(Stage, Answer)> {
        let armed: Vec<(Stage, Answer)> = spelling
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
            .collect();
        refuse_an_unreadable_arm(&armed, ARMED_STAGES);
        armed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every stage and every answer a harness arms round-trips through the name
    /// it is armed under, so a widened seam and the suite arming it cannot drift
    /// into naming different things.
    #[test]
    fn every_stage_and_answer_has_one_name() {
        let names: std::collections::BTreeSet<&str> =
            Answer::ALL.iter().map(|answer| answer.name()).collect();
        assert_eq!(names.len(), Answer::ALL.len());
        for stage in Stage::ALL {
            assert_eq!(Stage::named(stage.name()), Some(stage));
        }
        for answer in Answer::ALL {
            assert_eq!(Answer::named(answer.name()), Some(answer));
        }
        assert_eq!(Stage::named("paging"), None);
        assert_eq!(Answer::named("full-disk"), None);
    }

    /// Nothing armed is an observation the machine makes, whatever the listing
    /// named.
    #[test]
    fn an_unarmed_walk_stands_in_place_of_no_observation() {
        let faults = WalkFaults::default();
        for listed in [
            EntryKind::File,
            EntryKind::Directory,
            EntryKind::Symlink,
            EntryKind::Special(crate::walk::FileKind::Fifo),
        ] {
            let paged = faults.paging(listed);
            assert!(paged.meets.is_none() && paged.observes.is_none());
        }
    }

    /// Each answer is the observation it names, and nothing about the outcome.
    #[test]
    fn each_answer_stands_in_place_of_its_own_observation() {
        assert_eq!(
            WalkFaults::at(&[(Stage::Page, Answer::Vanishes)])
                .paging(EntryKind::File)
                .meets,
            Some(Errno::NOENT)
        );
        assert_eq!(
            WalkFaults::at(&[(Stage::Page, Answer::Denied)])
                .paging(EntryKind::File)
                .meets,
            Some(Errno::ACCESS)
        );
        let replaced = WalkFaults::at(&[(Stage::Page, Answer::Replaced)]).paging(EntryKind::File);
        assert_eq!(replaced.observes, Some(EntryKind::Directory));
        assert_eq!(
            replaced.meets, None,
            "a replaced name is there to be stat'd"
        );
    }

    /// **The arm stands over a regular file and spends itself on one.** A
    /// directory leaving a page takes a subtree with it, and an arm answering
    /// every stat would state a vault every writer is emptying — neither is the
    /// condition this seam is for.
    #[test]
    fn the_arm_waits_for_a_file_and_is_spent_on_one() {
        let faults = WalkFaults::at(&[(Stage::Page, Answer::Vanishes)]);

        for listed in [
            EntryKind::Directory,
            EntryKind::Symlink,
            EntryKind::Special(crate::walk::FileKind::Socket),
        ] {
            assert!(
                faults.paging(listed).meets.is_none(),
                "the arm was spent on an entry that is not a regular file"
            );
        }

        assert_eq!(faults.paging(EntryKind::File).meets, Some(Errno::NOENT));
        for _ in 0..3 {
            assert!(
                faults.paging(EntryKind::File).meets.is_none(),
                "the arm answered a second entry"
            );
        }
    }

    /// A fired arm records the window it answered at, under this seam's name,
    /// before it answers. A walk nothing fired in records nothing.
    #[test]
    #[allow(clippy::disallowed_methods)] // Test observation of the arm's own record file.
    fn a_fired_arm_records_the_window_and_the_answer() {
        let scratch = crate::scratch::Scratch::new("walk-arm-records");
        let hits = scratch.path("arm-hits");

        WalkFaults::recording_at(&[(Stage::Page, Answer::Replaced)], hits.clone())
            .paging(EntryKind::Directory);
        assert!(
            !scratch.exists(&hits),
            "an arm that stood over no entry recorded a window it never answered at"
        );

        let faults = WalkFaults::recording_at(&[(Stage::Page, Answer::Replaced)], hits.clone());
        faults.paging(EntryKind::File);
        faults.paging(EntryKind::File);

        assert_eq!(
            std::fs::read_to_string(&hits).expect("the record file"),
            "seam=norn-fs/walk stage=page answer=replaced\n"
        );
    }

    /// A spelling this module cannot read is a mistake in the harness, and a
    /// harness whose arm quietly armed nothing would pass every bar it carries.
    #[cfg(feature = "induced-failure")]
    #[test]
    fn an_unreadable_pair_refuses_rather_than_arming_nothing() {
        for spelling in [
            "page",
            "paging=vanishes",
            "page=full-disk",
            // One stage armed twice: the seam answers a stage once, so the
            // second pair is a spelling that would silently do nothing.
            "page=vanishes,page=denied",
        ] {
            assert!(
                std::panic::catch_unwind(|| armed::parse(spelling)).is_err(),
                "`{spelling}` was read as an arm"
            );
        }
    }

    /// Every readable pair reaches the arm it spells.
    #[cfg(feature = "induced-failure")]
    #[test]
    fn a_readable_pair_arms_the_stage_it_names() {
        for answer in Answer::ALL {
            assert_eq!(
                armed::parse(&format!("page={}", answer.name())),
                vec![(Stage::Page, answer)]
            );
        }
        assert_eq!(armed::parse(""), vec![]);
    }
}
