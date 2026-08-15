//! Making watch coverage fail without the platform cooperating.
//!
//! Three of this crate's watcher claims are about conditions a test cannot
//! arrange over a temporary directory: an operating system that refuses to
//! register a watch, a backend that stops or overflows under coverage already
//! installed, and a synchronization boundary that never arrives. Each decides
//! what a host does with an entry — refuse the attach, withdraw trust, widen
//! work to a full rescan — and none of the three is reachable by writing files
//! into a scratch tree.
//!
//! So each of the watcher's effect boundaries asks [`WatchFaults`] whether it
//! is the boundary that fails. This is the sibling of the write protocol's seam
//! ([`crate::faults`]), and the two stay separate: they answer at different
//! boundaries, in different vocabularies, and a record names which of the two
//! fired.
//!
//! **The seam is deliberately small.** It names *where* coverage can be made to
//! fail — installing it, the stream under it, the boundary that proves it — and
//! *how*: a refused registration, a terminal backend error, an overflow the
//! backend flags for rescan, a boundary that is never reached. It never names
//! what the watcher does next, which is the code under test.
//!
//! # The three stages
//!
//! - [`Stage::Install`] answers at the registration call, before any edge is
//!   staged, with the typed refusal the call itself would return. Nothing else
//!   in the establishment path changes: the refusal travels the same
//!   [`backend`] conversion and the same teardown a platform's own refusal
//!   travels, so no subscription reaches a caller.
//! - [`Stage::Stream`] answers at the handler boundary, upstream of ingest and
//!   the coalescer, by standing in place of the message the backend delivered.
//!   It therefore fires on the **next real delivery after establishment** and
//!   exactly once — one armed answer, one firing, one record.
//! - [`Stage::Barrier`] answers at establishment. From there the subscription
//!   withholds its `Live` publication on every platform path and its
//!   synchronization wait reaches its own expiry, so the caller's authored
//!   deadline is never the thing that has to elapse.
//!
//! # Reaching it from outside
//!
//! One stage of widening is taken and no more: under the `induced-failure`
//! feature, watch establishment arms itself from this process's environment
//! rather than passing an empty arm. Two variables carry it, both read once per
//! process:
//!
//! - `NORN_FS_WATCH_ARMED_STAGES` — the arm, as comma-separated `stage=answer`
//!   pairs, spelled `install`, `stream`, `barrier` and `refuses`, `fails`,
//!   `rescans`, `expires`. A pair this module cannot read — an unknown name, or
//!   an answer the stage does not carry — is a mistake in the harness rather
//!   than a stage nothing is armed at, so it panics.
//! - `NORN_FS_ARM_HITS` — the write seam's record file, and the same one here.
//!   A fired arm appends `seam=norn-fs/watch stage=<name>
//!   answer=<name>` to it before it answers, so a harness reads which boundary
//!   the watcher actually reached rather than inferring it from a subscription
//!   that failed for some other reason. The `seam` field is what tells a record
//!   written here apart from one the write protocol wrote.
//!
//! Nothing outside this crate arms anything without the feature, and a shipped
//! build has no reader for either variable.
//!
//! # What consumes it
//!
//! The lockdown and certification suites over watcher failure, which arm one
//! stage per case through the environment a process is started with. They are
//! the only thing that arms it: an ordinary process reads an empty arm and every
//! stage passes through.

#[cfg(any(test, feature = "induced-failure"))]
use std::path::{Path, PathBuf};
use std::time::Duration;

use notify::Event;
use notify::event::{EventKind, Flag};

use super::{WatchError, backend};

/// The environment variable naming the watcher stages this process is armed at.
#[cfg(feature = "induced-failure")]
pub(crate) const ARMED_STAGES: &str = "NORN_FS_WATCH_ARMED_STAGES";

/// The seam a record written here names itself under, which is what tells it
/// apart from a write protocol record in the same file.
#[cfg(any(test, feature = "induced-failure"))]
const SEAM: &str = "norn-fs/watch";

/// A boundary of the watcher's effect surface that can be made to fail.
///
/// The three are the boundaries whose failure has a *different* required
/// outcome, which is what makes each worth naming. A registration that refuses
/// never yields a subscription at all; a stream that fails or overflows changes
/// what an established subscription reports; a boundary that is never reached
/// is a typed expiry rather than coverage claiming to be live.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Stage {
    /// Registering the coverage plan with the platform backend.
    Install,
    /// The backend's event stream, under coverage that is already installed.
    Stream,
    /// The synchronization boundary that proves coverage is live.
    Barrier,
}

// The stage vocabulary — the roster and the names — is what a harness arms and
// what a record names, so it compiles where something reads it and nowhere
// else. It still belongs to the seam rather than to the suite: the names are
// what the widened half is stated over, and a build that carried a different
// set would be a different seam.
impl Stage {
    /// Every stage, in the order watch establishment reaches them.
    #[cfg(any(test, feature = "induced-failure"))]
    pub(crate) const ALL: [Stage; 3] = [Stage::Install, Stage::Stream, Stage::Barrier];

    /// The name a harness arms this stage under, which is also the name a
    /// record of it carries.
    #[cfg(any(test, feature = "induced-failure"))]
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Stage::Install => "install",
            Stage::Stream => "stream",
            Stage::Barrier => "barrier",
        }
    }

    /// The stage `name` spells, or nothing where it spells none.
    #[cfg(any(test, feature = "induced-failure"))]
    fn named(name: &str) -> Option<Stage> {
        Stage::ALL.into_iter().find(|stage| stage.name() == name)
    }

    /// Whether this stage answers the way `answer` says.
    ///
    /// Answers are not interchangeable across stages here: registration returns
    /// an error and delivers nothing, the stream delivers a message and returns
    /// nothing, and the barrier does neither. A pair naming a stage together
    /// with an answer it does not carry is therefore unreadable rather than
    /// approximately right, and is refused where it is spelled.
    #[cfg(any(test, feature = "induced-failure"))]
    const fn answers(self, answer: Answer) -> bool {
        matches!(
            (self, answer),
            (Stage::Install, Answer::Refuses)
                | (Stage::Stream, Answer::Fails | Answer::Rescans)
                | (Stage::Barrier, Answer::Expires)
        )
    }
}

/// What an armed stage does when the watcher reaches it.
///
/// Each is a shape the platform fails in, and each has a different required
/// outcome: a registration call the operating system refuses, a backend that
/// stops reporting for good, a backend that says it lost the path set, and a
/// boundary that never arrives.
// The allow stays at the enum rather than moving onto its items: nothing in a
// build that arms nothing constructs an answer at all, and a variant nobody
// names is what the lint reads as dead. The variants are the seam's vocabulary
// even so — an unarmed build has to hold the same four, or the arm a harness
// spells means something different from the arm this seam answers.
#[cfg_attr(not(any(test, feature = "induced-failure")), allow(dead_code))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Answer {
    /// The registration call refuses, the way an operating system refuses to
    /// register a watch over a path it will not let this process observe.
    Refuses,
    /// The backend delivers a failure instead of an event, which is the last
    /// thing a subscription ever carries.
    Fails,
    /// The backend delivers the event it emits when it dropped events and the
    /// path set is no longer known: inotify's queue overflow and FSEvents'
    /// must-scan-subdirectories flag both arrive as exactly this message.
    ///
    /// It is the *backend's* overflow and not this crate's. A dirty set that
    /// outgrows [`DIRTY_ROOT_CAP`](super::DIRTY_ROOT_CAP) widens to the same
    /// vault rescan from inside the coalescer, reachable by a burst a test can
    /// write, and is a candidate this vocabulary could be supplemented with
    /// rather than a case anything here requires.
    Rescans,
    /// The synchronization boundary is never reached, so the wait for it takes
    /// its expiry branch.
    Expires,
}

impl Answer {
    /// The name a harness arms this answer under.
    #[cfg(any(test, feature = "induced-failure"))]
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Answer::Refuses => "refuses",
            Answer::Fails => "fails",
            Answer::Rescans => "rescans",
            Answer::Expires => "expires",
        }
    }

    /// The answer `name` spells, or nothing where it spells none.
    #[cfg(any(test, feature = "induced-failure"))]
    fn named(name: &str) -> Option<Answer> {
        [
            Answer::Refuses,
            Answer::Fails,
            Answer::Rescans,
            Answer::Expires,
        ]
        .into_iter()
        .find(|answer| answer.name() == name)
    }
}

/// Which boundaries of a watch fail, and how.
///
/// A list rather than one entry, for the same reason the write seam holds one:
/// a stage armed alone says nothing about a watcher meeting two conditions, and
/// the vocabulary should not be the thing that decides whether such a case can
/// be stated. The default is the empty list — a watch that fails only where the
/// platform makes it fail — and it is what every process that armed nothing
/// carries.
#[derive(Clone, Debug, Default)]
pub(crate) struct WatchFaults {
    /// The stages this watch is armed at, in the order they were named.
    armed: &'static [(Stage, Answer)],
    /// The file a fired arm records itself in, where anything named one.
    #[cfg(any(test, feature = "induced-failure"))]
    hits: Option<PathBuf>,
}

impl WatchFaults {
    /// A watch that answers each named stage the named way, recording nothing.
    #[cfg(test)]
    pub(crate) fn at(armed: &'static [(Stage, Answer)]) -> WatchFaults {
        for (stage, answer) in armed {
            assert!(
                stage.answers(*answer),
                "the {} stage answers no `{}`",
                stage.name(),
                answer.name()
            );
        }
        WatchFaults { armed, hits: None }
    }

    /// The same arm, recording each fired stage in `hits`.
    ///
    /// The record file is a value here rather than a reading of the process's
    /// environment, so a case in this crate's own suite reads the record its own
    /// arm wrote without arming the process every other case runs in.
    #[cfg(test)]
    pub(crate) fn recording_at(armed: &'static [(Stage, Answer)], hits: PathBuf) -> WatchFaults {
        WatchFaults {
            hits: Some(hits),
            ..WatchFaults::at(armed)
        }
    }

    /// What watch establishment passes.
    ///
    /// Without the `induced-failure` feature this is an empty arm and each
    /// boundary asks one comparison against an empty list. With it, the
    /// answer is whatever this process was started armed with — read once, and
    /// empty in every process that armed nothing.
    pub(crate) fn entry() -> WatchFaults {
        #[cfg(feature = "induced-failure")]
        {
            WatchFaults {
                armed: armed::stages(),
                hits: armed::hits().cloned(),
            }
        }
        #[cfg(not(feature = "induced-failure"))]
        {
            WatchFaults::default()
        }
    }

    /// The answer this watch is armed with at `stage`, if it is armed there.
    fn answer(&self, stage: Stage) -> Option<Answer> {
        self.armed
            .iter()
            .find_map(|(armed, answer)| (*armed == stage).then_some(*answer))
    }

    /// The refusal the registration call meets, if the install stage is armed.
    ///
    /// What is simulated is the platform call's own typed answer and nothing
    /// around it: the refusal is built and converted exactly as a real one is,
    /// so it reaches a caller through the same teardown, carrying the same
    /// error shape.
    // The answer a fired arm names is what its record carries, so a build with
    // no record in it reads the answer nowhere.
    #[cfg_attr(not(any(test, feature = "induced-failure")), allow(unused_variables))]
    pub(crate) fn registration(&self) -> Result<(), WatchError> {
        let Some(answer) = self.answer(Stage::Install) else {
            return Ok(());
        };
        #[cfg(any(test, feature = "induced-failure"))]
        self.record(Stage::Install, answer);
        Err(backend(notify::Error::generic(
            "an armed watcher fault refused this registration",
        )))
    }

    /// Fire the barrier arm, if this watch carries one.
    ///
    /// The barrier answers at establishment rather than at some later delivery,
    /// so this is where its record is written — once, before anything it
    /// answers. What it answers from here on is [`Self::withholds_live`] and
    /// [`Self::synchronization_wait`].
    // The answer a fired arm names is what its record carries, so a build with
    // no record in it reads the answer nowhere.
    #[cfg_attr(not(any(test, feature = "induced-failure")), allow(unused_variables))]
    pub(crate) fn fire_barrier(&self) {
        let Some(answer) = self.answer(Stage::Barrier) else {
            return;
        };
        #[cfg(any(test, feature = "induced-failure"))]
        self.record(Stage::Barrier, answer);
    }

    /// Whether the `Live` publication is withheld.
    ///
    /// Every platform path asks: the publication that follows registration
    /// where registration is the boundary, and the native macOS event-history
    /// marker where it is not. A withheld publication leaves the subscription
    /// synchronizing, which is the state a boundary that never arrives leaves
    /// it in.
    pub(crate) fn withholds_live(&self) -> bool {
        self.answer(Stage::Barrier).is_some()
    }

    /// How long the wait for the synchronization boundary lasts.
    ///
    /// An armed barrier makes it nothing: the boundary it withheld is not going
    /// to arrive, and a case about expiry should not spend the caller's authored
    /// deadline proving it. Everything the expiry does afterwards — the typed
    /// terminal error, the record into the subscription's state, the wake that
    /// carries it to the stream — is the production path, reached because the
    /// wait genuinely ended with the subscription still synchronizing.
    pub(crate) fn synchronization_wait(&self, authored: Duration) -> Duration {
        match self.answer(Stage::Barrier) {
            Some(_) => Duration::ZERO,
            None => authored,
        }
    }

    /// The stream arm this watch owes its next real delivery.
    pub(crate) fn stream_arm(&self) -> StreamArm {
        StreamArm {
            owed: self.answer(Stage::Stream),
            #[cfg(any(test, feature = "induced-failure"))]
            hits: self.hits.clone(),
        }
    }

    #[cfg(any(test, feature = "induced-failure"))]
    fn record(&self, stage: Stage, answer: Answer) {
        record(self.hits.as_deref(), stage, answer);
    }
}

/// One armed stream answer, held until a real delivery displaces it.
///
/// The arm stands at the handler boundary, upstream of ingest and the
/// coalescer, because what a stream failure or an overflow *is* to the rest of
/// the watcher is a message arriving there. It waits for a delivery rather than
/// firing at establishment so that it stands in place of an event the platform
/// really produced, and it is taken when it fires: one armed answer, one
/// firing, one record.
pub(crate) struct StreamArm {
    owed: Option<Answer>,
    #[cfg(any(test, feature = "induced-failure"))]
    hits: Option<PathBuf>,
}

impl StreamArm {
    /// What the watcher ingests in place of `delivered`.
    pub(crate) fn answer(&mut self, delivered: notify::Result<Event>) -> notify::Result<Event> {
        let Some(answer) = self.owed.take() else {
            return delivered;
        };
        #[cfg(any(test, feature = "induced-failure"))]
        record(self.hits.as_deref(), Stage::Stream, answer);
        match answer {
            Answer::Fails => Err(notify::Error::generic(
                "an armed watcher fault ended this event stream",
            )),
            // The exact message both platform backends emit when they dropped
            // events: inotify on a queue overflow, FSEvents on
            // must-scan-subdirectories. Nothing in it names a path, because a
            // path set that was lost is what it reports.
            Answer::Rescans => Ok(Event::new(EventKind::Other).set_flag(Flag::Rescan)),
            other => panic!("the stream stage answers no {other:?}"),
        }
    }
}

/// Append one record saying which boundary fired and how it answered.
///
/// Best effort by construction: a harness that named no record file wants none.
/// What it must not do is buffer — a record still in this process's memory when
/// the process ends is a record the harness never reads — so it opens, writes,
/// syncs and closes each time.
#[cfg(any(test, feature = "induced-failure"))]
#[allow(clippy::disallowed_methods, clippy::disallowed_types)] // The arm's own record file, outside the vault.
fn record(hits: Option<&Path>, stage: Stage, answer: Answer) {
    use std::io::Write as _;

    let Some(path) = hits else {
        return;
    };
    let record = format!(
        "seam={SEAM} stage={} answer={}\n",
        stage.name(),
        answer.name()
    );
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = file.write_all(record.as_bytes());
        let _ = file.sync_all();
    }
}

/// The arm this process was started under.
///
/// Both readings happen once and are then held: a watch asks the seam at every
/// boundary it crosses and a stream asks it per delivery, so re-reading the
/// environment there would make the feature's cost a function of how much the
/// platform reports.
#[cfg(feature = "induced-failure")]
mod armed {
    use std::sync::OnceLock;

    use super::{ARMED_STAGES, Answer, Stage};
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

    /// Read `stage=answer` pairs, and refuse a spelling that names neither.
    ///
    /// A misspelled arm that quietly armed nothing would pass every bar it was
    /// supposed to carry, so an unreadable pair ends the process saying so.
    pub(super) fn parse(spelling: &str) -> Vec<(Stage, Answer)> {
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
                assert!(
                    stage.answers(answer),
                    "the {} stage in {ARMED_STAGES} answers no `{}`",
                    stage.name(),
                    answer.name()
                );
                (stage, answer)
            })
            .collect()
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
        let stages: std::collections::BTreeSet<&str> =
            Stage::ALL.iter().map(|stage| stage.name()).collect();
        assert_eq!(stages.len(), Stage::ALL.len());
        let answers = [
            Answer::Refuses,
            Answer::Fails,
            Answer::Rescans,
            Answer::Expires,
        ];
        let names: std::collections::BTreeSet<&str> =
            answers.iter().map(|answer| answer.name()).collect();
        assert_eq!(names.len(), answers.len());
        for stage in Stage::ALL {
            assert_eq!(Stage::named(stage.name()), Some(stage));
        }
        for answer in answers {
            assert_eq!(Answer::named(answer.name()), Some(answer));
        }
        assert_eq!(Stage::named("registration"), None);
        assert_eq!(Answer::named("full-disk"), None);
    }

    /// Every stage carries at least one answer and no stage carries another
    /// stage's, which is what makes a mismatched pair a mistake rather than an
    /// arm with an approximate meaning.
    #[test]
    fn each_stage_answers_only_its_own_vocabulary() {
        for stage in Stage::ALL {
            let carried: Vec<Answer> = [
                Answer::Refuses,
                Answer::Fails,
                Answer::Rescans,
                Answer::Expires,
            ]
            .into_iter()
            .filter(|answer| stage.answers(*answer))
            .collect();
            assert!(!carried.is_empty(), "{} answers nothing", stage.name());
        }
        assert!(!Stage::Install.answers(Answer::Expires));
        assert!(!Stage::Barrier.answers(Answer::Rescans));
        assert!(!Stage::Stream.answers(Answer::Refuses));
    }

    /// Nothing is armed, so every boundary passes through: registration
    /// proceeds, the boundary publishes, the caller's own deadline is the wait,
    /// and a delivery reaches ingest as the backend produced it.
    #[test]
    fn an_unarmed_watch_answers_at_no_boundary() {
        let faults = WatchFaults::default();

        assert_eq!(faults.registration(), Ok(()));
        faults.fire_barrier();
        assert!(!faults.withholds_live());
        assert_eq!(
            faults.synchronization_wait(Duration::from_secs(7)),
            Duration::from_secs(7)
        );
        let mut arm = faults.stream_arm();
        let delivered = arm.answer(Ok(
            Event::new(EventKind::Other).add_path("/vault/one.md".into())
        ));
        assert_eq!(
            delivered.expect("the delivery the backend made").paths,
            [std::path::PathBuf::from("/vault/one.md")]
        );
        for stage in Stage::ALL {
            assert_eq!(faults.answer(stage), None);
        }
    }

    /// One stage answers and the others do not, so a case reaches the boundary
    /// it named rather than the first one establishment happens to cross.
    #[test]
    fn an_armed_stage_answers_at_one_boundary_only() {
        let faults = WatchFaults::at(&[(Stage::Barrier, Answer::Expires)]);

        assert_eq!(faults.registration(), Ok(()));
        assert!(faults.stream_arm().owed.is_none());
        assert!(faults.withholds_live());
        assert_eq!(
            faults.synchronization_wait(Duration::from_secs(3600)),
            Duration::ZERO
        );
    }

    /// The install answer is the registration call's own refusal, carried the
    /// way the platform's would be.
    #[test]
    fn the_install_answer_is_a_typed_registration_refusal() {
        let faults = WatchFaults::at(&[(Stage::Install, Answer::Refuses)]);
        assert!(matches!(faults.registration(), Err(WatchError::Backend(_))));
    }

    /// The stream answers stand in place of the delivery, and the arm is taken
    /// when it fires: a second delivery is the backend's own again.
    #[test]
    fn a_stream_arm_displaces_one_delivery_and_no_more() {
        let mut arm = WatchFaults::at(&[(Stage::Stream, Answer::Rescans)]).stream_arm();
        let answered = arm
            .answer(Ok(
                Event::new(EventKind::Other).add_path("/vault/one.md".into())
            ))
            .expect("a rescan answer is a delivery, not a failure");
        assert!(answered.need_rescan());
        assert!(answered.paths.is_empty());

        let after = arm.answer(Ok(
            Event::new(EventKind::Other).add_path("/vault/two.md".into())
        ));
        assert_eq!(
            after.expect("the delivery the backend made").paths,
            [std::path::PathBuf::from("/vault/two.md")]
        );

        let mut arm = WatchFaults::at(&[(Stage::Stream, Answer::Fails)]).stream_arm();
        assert!(arm.answer(Ok(Event::new(EventKind::Other))).is_err());
        assert!(arm.answer(Ok(Event::new(EventKind::Other))).is_ok());
    }

    /// A fired arm records the boundary it answered at, under this seam's name,
    /// before it answers.
    #[test]
    #[allow(clippy::disallowed_methods)] // Test observation of the arm's own record file.
    fn a_fired_arm_records_the_boundary_and_the_answer() {
        let scratch = crate::scratch::Scratch::new("watch-arm-records");
        let hits = scratch.path("arm-hits");

        WatchFaults::recording_at(&[(Stage::Install, Answer::Refuses)], hits.clone())
            .registration()
            .expect_err("the armed registration");
        WatchFaults::recording_at(&[(Stage::Barrier, Answer::Expires)], hits.clone())
            .fire_barrier();
        WatchFaults::recording_at(&[(Stage::Stream, Answer::Fails)], hits.clone())
            .stream_arm()
            .answer(Ok(Event::new(EventKind::Other)))
            .expect_err("the armed stream");

        assert_eq!(
            std::fs::read_to_string(&hits).expect("the record file"),
            "seam=norn-fs/watch stage=install answer=refuses\n\
             seam=norn-fs/watch stage=barrier answer=expires\n\
             seam=norn-fs/watch stage=stream answer=fails\n"
        );
    }

    /// An arm that named no record file records nothing and answers the same.
    #[test]
    fn an_arm_with_no_record_file_still_answers() {
        assert!(
            WatchFaults::at(&[(Stage::Install, Answer::Refuses)])
                .registration()
                .is_err()
        );
    }

    /// A spelling this module cannot read is a mistake in the harness, and a
    /// harness whose arm quietly armed nothing would pass every bar it carries.
    #[cfg(feature = "induced-failure")]
    #[test]
    fn an_unreadable_pair_refuses_rather_than_arming_nothing() {
        for spelling in [
            "install",
            "registration=refuses",
            "install=full-disk",
            "install=expires",
            "stream=refuses",
        ] {
            assert!(
                std::panic::catch_unwind(|| armed::parse(spelling)).is_err(),
                "`{spelling}` was read as an arm"
            );
        }
    }

    /// Every readable pair reaches the arm it spells, including two at once.
    #[cfg(feature = "induced-failure")]
    #[test]
    fn a_readable_pair_arms_the_stage_it_names() {
        assert_eq!(
            armed::parse("install=refuses"),
            vec![(Stage::Install, Answer::Refuses)]
        );
        assert_eq!(
            armed::parse("stream=rescans,barrier=expires"),
            vec![
                (Stage::Stream, Answer::Rescans),
                (Stage::Barrier, Answer::Expires)
            ]
        );
        assert_eq!(armed::parse(""), vec![]);
    }
}
