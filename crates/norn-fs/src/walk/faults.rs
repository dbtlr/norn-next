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
//! - [`Answer::Vanishes`] — the stat meets `ENOENT`. **The page states the name
//!   as one it read nothing at.** A walk begun now yields no entry at that name
//!   either, so that is the answer the walk converges on, and it is the same
//!   answer the window between a yielded fact and its open has given since that
//!   window converged.
//! - [`Answer::Replaced`] — the stat observes a kind the listing did not name.
//!   **The page states the same vanishing**: the name the listing named was
//!   unlinked and something else took it inside the same window, so what stands
//!   there is not the entry that was listed and nothing here read it. What
//!   stands there now is a change of the vault's own, and the page claims
//!   nothing about it.
//! - [`Answer::Denied`] — the stat meets `EACCES`. **The walk refuses**, and the
//!   refusal is terminal the way every environmental one is. A machine that will
//!   not answer says nothing about whether an entry is there, and reading it as
//!   absence would let a revoked permission prune derived state. The convergence
//!   above narrows this boundary to absence; it does not remove it.
//!
//! # Which entry an arm stands over
//!
//! **The first entry a page stats of the class the arm reaches**, once in the
//! process that armed it. Two things decide that:
//!
//! - *One class*, [`Reach`], because the entry class is what the outcome is read
//!   off. A file leaving a page is one document; a directory leaving one is a
//!   whole subtree the walk never enters, and the two owe different halves of
//!   the same doctrine. An arm that stood over whichever entry came first could
//!   state neither.
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
//!   panics. An arm spelled through the environment stands over a regular file:
//!   the other reach is a shape this crate's own suite reads a page off, not one
//!   a host case states.
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

/// The class of entry an arm stands over.
///
/// One arm reaches one class, because the two classes are the two halves of what
/// the convergence owes and a case reads exactly one of them. A file that leaves
/// a page is one document: the entries beside it derive and the heal completes.
/// A directory that leaves one is a root the walk never enters: nothing under it
/// is read, so what is stored beneath it converges while the findings there stay
/// withheld.
// The reach a harness spells is the file one, so the other is constructed by
// this crate's own cases alone. Both belong to the seam even so: the class an
// arm stands over is what a case is stated about, and a build carrying one of
// them would answer a different question.
#[cfg_attr(not(test), allow(dead_code))]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum Reach {
    /// A regular file. What an arm spelled through the environment stands over.
    #[default]
    File,
    /// A directory, and with it the subtree the walk would have entered.
    Directory,
}

impl Reach {
    /// The listed kind this reach stands over.
    fn kind(self) -> EntryKind {
        match self {
            Reach::File => EntryKind::File,
            Reach::Directory => EntryKind::Directory,
        }
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
    /// The class of entry the arm stands over.
    reach: Reach,
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
    ///
    /// An arm spelled in code is held to the same reading as one spelled through
    /// the environment: every stage of this seam answers every answer of it, so
    /// a stage named twice is the whole of what an arm here can be given wrong.
    #[cfg(test)]
    pub(crate) fn at(armed: &'static [(Stage, Answer)]) -> WalkFaults {
        crate::faults::refuse_a_stage_armed_twice(armed, |stage| stage.name(), "this arm");
        WalkFaults {
            armed,
            ..WalkFaults::default()
        }
    }

    /// The same arm, standing over `reach` rather than over a regular file.
    #[cfg(test)]
    pub(crate) fn over(self, reach: Reach) -> WalkFaults {
        WalkFaults { reach, ..self }
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
                hits: crate::faults::armed_hits().cloned(),
                spent: armed::spent(),
                ..WalkFaults::default()
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
    /// The arm is spent here, on the first entry of its reach's class, and every
    /// stat after it is the machine's own again. An unarmed walk pays one
    /// comparison against an empty list and nothing else, which is what keeps
    /// the feature's cost off a heal-scale enumeration.
    pub(crate) fn paging(&self, listed: EntryKind) -> Paged {
        let Some(answer) = self.answer(Stage::Page) else {
            return Paged::default();
        };
        if listed != self.reach.kind() || self.spent.swap(true, Ordering::AcqRel) {
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
            // The coarsest kind change there is, in whichever direction the
            // reach leaves open: a directory standing where the listing named a
            // file, or a file standing where it named a directory. A walk begun
            // now reads the successor the other way round from the entry that
            // was listed, which is what makes the listed entry one nothing read.
            Answer::Replaced => Paged {
                observes: Some(match self.reach {
                    Reach::File => EntryKind::Directory,
                    Reach::Directory => EntryKind::File,
                }),
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

    use super::{ARMED_STAGES, Answer, Stage};

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

    /// The one firing this process has, held across every walk it runs.
    pub(super) fn spent() -> Arc<AtomicBool> {
        static SPENT: OnceLock<Arc<AtomicBool>> = OnceLock::new();
        SPENT.get_or_init(Arc::default).clone()
    }

    /// Read `stage=answer` pairs, through the grammar all three seams share.
    pub(super) fn parse(spelling: &str) -> Vec<(Stage, Answer)> {
        crate::faults::read_armed_pairs(
            spelling,
            ARMED_STAGES,
            Stage::named,
            Answer::named,
            |stage| stage.name(),
        )
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

    /// **A reach names one class and the arm waits for it.** The two classes are
    /// the two halves of what the convergence owes — one document, or a root and
    /// the subtree under it — and an arm answering whichever entry came first
    /// could state neither.
    #[test]
    fn a_reach_decides_which_class_of_entry_the_arm_waits_for() {
        let over_directories =
            WalkFaults::at(&[(Stage::Page, Answer::Vanishes)]).over(Reach::Directory);
        assert!(
            over_directories.paging(EntryKind::File).meets.is_none(),
            "an arm reaching directories was spent on a file"
        );
        assert_eq!(
            over_directories.paging(EntryKind::Directory).meets,
            Some(Errno::NOENT)
        );

        // A replacement is a kind the listing did not name, which is the other
        // kind whichever way round the reach runs.
        assert_eq!(
            WalkFaults::at(&[(Stage::Page, Answer::Replaced)])
                .over(Reach::Directory)
                .paging(EntryKind::Directory)
                .observes,
            Some(EntryKind::File)
        );
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
