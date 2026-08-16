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
//!   the coalescer, by standing in place of an event the backend delivered. The
//!   delivery it stands in place of is **the first one a consumer meets as a
//!   live change**, and three conditions say which one that is:
//!
//!   1. The subscription's synchronization boundary has been **reached**. A
//!      backend establishing itself delivers too, and macOS FSEvents replays
//!      event history there, so an arm answered before the boundary states a
//!      case about establishment.
//!   2. The consumer's **first heal window has closed**. A consumer takes up
//!      coverage by healing the tree under it, and everything the backend
//!      reports across that window is handed back as the heal's own batch
//!      rather than met as a change — so a delivery there is absorbed by the
//!      work already running. The boundary alone cannot stand in for this on
//!      macOS: a change made before the watch was registered can be delivered
//!      *after* the history marker, because fseventsd numbers an event when it
//!      processes it rather than when the syscall returned.
//!   3. The watcher **folds the delivery into a batch**. Ingest's own rule
//!      answers, shared rather than restated: an access reaches no batch on
//!      either backend, and neither does a path the coverage plan takes in and
//!      ingest then discards — a sibling of the vault root, an excluded place.
//!      An arm spent on one of those would report a failed or overflowing
//!      stream over a delivery no consumer ever hears about, on whichever
//!      platform and tree happened to produce it.
//!
//!   The first delivery meeting all three takes the arm, exactly once.
//! - [`Stage::Barrier`] answers at establishment. From there the subscription
//!   withholds its `Live` publication on every platform path, and the first
//!   wait for that boundary ends at once — so the caller's authored deadline is
//!   never the thing that has to elapse, and the record of the arm attests that
//!   a caller really reached the boundary it withheld.
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
//!   `rescans`, `expires`. A pair this module cannot read — an unknown name, an
//!   answer the stage does not carry, or one stage armed twice — is a mistake in
//!   the harness rather than a stage nothing is armed at, so it panics.
//! - `NORN_FS_ARM_HITS` — the write seam's record file, and the same one here. A
//!   fired arm appends `seam=norn-fs/watch stage=<name> answer=<name>` to it
//!   before it answers, so a harness reads which boundary the watcher actually
//!   reached rather than inferring it from a subscription that failed for some
//!   other reason. The `seam` field is what tells a record written here apart
//!   from one the write protocol wrote. A file this process named and cannot
//!   write ends the process saying so — an abort, with the reason on standard
//!   error — because a parent cannot tell a record that was never written from
//!   a boundary that was never reached.
//!
//! **The record file must sit outside every watched tree.** It is written while
//! coverage is live — by this seam, and by the write protocol's seam in the same
//! run — so a file inside the vault, or directly inside the vault's parent, is a
//! filesystem change the backend reports: it becomes an event in a batch under
//! assertion, and it can be the very delivery a stream arm answers. A harness
//! puts it in a directory of its own beside the tree.
//!
//! **The arm is read once per process and applies to every watch that process
//! establishes.** Each establishment reads the same pairs and gets its own
//! one-shot: two watches in one armed process refuse twice, withhold two
//! boundaries, or displace one delivery each — and write one record per firing.
//! A harness asserting exact record content therefore establishes exactly one
//! watch per process.
//!
//! Nothing outside this crate arms anything without the feature, and a shipped
//! build has no reader for either variable.
//!
//! # What consumes it
//!
//! `norn-host`'s lockdown suite arms one stage per case in the child process
//! that case's watch is established in, and it reaches the install and stream
//! stages. A registration that refuses, a stream that ends, and a stream that
//! reports its path set lost are the three conditions those cases meet, and
//! each states the trust transition a host owes for it at the production path,
//! over a real backend and a real attachment. That crate's own
//! `induced-failure` feature forwards to this one, so a lane arming a host has
//! this reader compiled in.
//!
//! This crate's own suites are what carry every stage, including the ones no
//! other crate arms: the in-crate cases over a real backend, and the environment
//! round trip in `tests/watch_lockdown.rs`, which walks the route from a
//! variable to an answered boundary for each of the four pairs. An ordinary
//! process reads an empty arm and every boundary passes through.

#[cfg(any(test, feature = "induced-failure"))]
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
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
    /// The backend's event stream, under coverage that is already installed,
    /// past its synchronization boundary, and taken up by a consumer's first
    /// heal.
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
    /// Answers are not interchangeable across these boundaries: registration
    /// returns an error and delivers nothing, the stream delivers a message and
    /// returns nothing, and the barrier does neither. A pair naming a stage
    /// together with an answer it does not carry is therefore unreadable rather
    /// than approximately right, and it is refused where it is spelled — which
    /// is what lets the boundaries themselves answer without a case for an
    /// answer that cannot reach them.
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
    /// outgrows [`DIRTY_ROOT_CAP`](super::DIRTY_ROOT_CAP) widens to a vault
    /// rescan from inside the coalescer instead, and is reached by a burst a
    /// test can write rather than by an arm.
    Rescans,
    /// The synchronization boundary is never reached, so the wait for it takes
    /// its expiry branch.
    Expires,
}

impl Answer {
    /// Every answer, in the order the stages that carry them are reached.
    #[cfg(any(test, feature = "induced-failure"))]
    pub(crate) const ALL: [Answer; 4] = [
        Answer::Refuses,
        Answer::Fails,
        Answer::Rescans,
        Answer::Expires,
    ];

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
        Answer::ALL.into_iter().find(|answer| answer.name() == name)
    }
}

/// Refuse an arm that names something this seam cannot answer.
///
/// Two spellings are unreadable rather than approximately right: a stage
/// together with an answer it does not carry, and one stage armed twice. The
/// second would otherwise be the one mistake that passes silently — the seam
/// reads a stage's first answer, so the second pair a harness spelled would
/// simply not happen, and the case would report on a condition it never met.
#[cfg(any(test, feature = "induced-failure"))]
fn refuse_an_unreadable_arm(armed: &[(Stage, Answer)], source: &str) {
    for (index, (stage, answer)) in armed.iter().enumerate() {
        assert!(
            stage.answers(*answer),
            "the {} stage in {source} answers no `{}`",
            stage.name(),
            answer.name()
        );
        assert!(
            !armed[..index].iter().any(|(earlier, _)| earlier == stage),
            "the {} stage is armed twice in {source}",
            stage.name()
        );
    }
}

/// One fact about a subscription that turns true once and never back, shared
/// between the thread that establishes coverage and the arm the backend's
/// delivery handler holds.
#[derive(Clone, Debug, Default)]
struct Latch(Arc<AtomicBool>);

impl Latch {
    fn set(&self) {
        self.0.store(true, Ordering::Release);
    }

    fn holds(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

/// Whether a subscription has reached its synchronization boundary.
///
/// The watcher crosses that boundary in one of two places depending on the
/// platform and the backend — returning from bulk registration, or the native
/// macOS event-history marker — and both say so here. It is *reached* rather
/// than published: an armed barrier withholds the publication, and what the
/// boundary says is true either way, which is that the complete coverage plan
/// is installed and whatever the backend replays for its own establishment has
/// been replayed.
///
/// **It is not the point past which every earlier change has been delivered.**
/// On macOS it cannot be: fseventsd numbers an event when it processes the
/// kernel's notification rather than when the syscall returned, so a change made
/// before the watch existed can fall outside the replayed backlog and arrive
/// after the marker. That is why the stream stage waits for [`HealWindow`] as
/// well — the watcher's own docs for the native barrier carry the full reading.
#[derive(Clone, Debug, Default)]
pub(crate) struct Boundary(Latch);

impl Boundary {
    /// Say the subscription has crossed it.
    pub(crate) fn reached(&self) {
        self.0.set();
    }

    pub(crate) fn was_reached(&self) -> bool {
        self.0.holds()
    }
}

/// Whether a subscription's first heal window has closed.
///
/// A consumer takes up coverage by healing the tree under the watch it just
/// installed, and everything the backend reports across that window belongs to
/// the heal: [`Subscription::finish_heal`](crate::watch::Subscription::finish_heal)
/// hands the whole accumulation back as one batch, which the consumer folds
/// into the work it was already doing. So the first delivery a consumer sees
/// *as a change* is the first one after that window closes, and that is the
/// delivery the stream stage stands in place of.
///
/// **A subscription nobody heals over never closes it**, and an arm on such a
/// subscription is never spent. That is the honest reading rather than a gap:
/// this crate's consumers heal before they act on a delivery, and an arm that
/// fired before any of them had is stating a case about establishment.
#[derive(Clone, Debug, Default)]
pub(crate) struct HealWindow(Latch);

impl HealWindow {
    /// Say the first heal window has closed. Later heals say it again and
    /// change nothing.
    pub(crate) fn closed(&self) {
        self.0.set();
    }

    pub(crate) fn has_closed(&self) -> bool {
        self.0.holds()
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
    /// Whether the barrier arm has already recorded itself. Shared across the
    /// clones one establishment makes, so a subscription waited on twice
    /// records one firing.
    #[cfg(any(test, feature = "induced-failure"))]
    barrier_recorded: Arc<AtomicBool>,
}

impl WatchFaults {
    /// A watch that answers each named stage the named way, recording nothing.
    #[cfg(test)]
    pub(crate) fn at(armed: &'static [(Stage, Answer)]) -> WatchFaults {
        refuse_an_unreadable_arm(armed, "this arm");
        WatchFaults {
            armed,
            ..WatchFaults::default()
        }
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
    /// boundary asks one comparison against an empty list. With it, the answer
    /// is whatever this process was started armed with — read once, and empty in
    /// every process that armed nothing.
    pub(crate) fn entry() -> WatchFaults {
        #[cfg(feature = "induced-failure")]
        {
            WatchFaults {
                armed: armed::stages(),
                hits: armed::hits().cloned(),
                barrier_recorded: Arc::default(),
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

    /// Whether the `Live` publication is withheld.
    ///
    /// Every platform path asks: the publication that follows registration
    /// where registration is the boundary, and the native macOS event-history
    /// marker where it is not. A withheld publication leaves the subscription
    /// synchronizing, which is the state a boundary that never arrives leaves
    /// it in. The boundary is still *reached* either way, so a stream arm on
    /// the same watch stays statable.
    pub(crate) fn withholds_live(&self) -> bool {
        self.answer(Stage::Barrier).is_some()
    }

    /// How long the wait for the synchronization boundary lasts.
    ///
    /// An armed barrier makes it nothing: the publication it withheld is not
    /// coming, and a case about expiry should not spend the caller's authored
    /// deadline proving it. Everything the expiry does afterwards — the typed
    /// terminal error, the record into the subscription's state, the wake that
    /// carries it to the stream — is the production path, reached because the
    /// wait genuinely ended with the subscription still synchronizing.
    ///
    /// **This is where the barrier arm records itself**, once per
    /// establishment. What a record attests is a boundary the watcher reached,
    /// and the boundary this arm is about is reached by a caller waiting on one
    /// that was withheld — so a harness that reads the record knows a wait
    /// happened, rather than knowing only that a watch was armed.
    // The answer a fired arm names is what its record carries, so a build with
    // no record in it reads the answer nowhere.
    #[cfg_attr(not(any(test, feature = "induced-failure")), allow(unused_variables))]
    pub(crate) fn synchronization_wait(&self, authored: Duration) -> Duration {
        let Some(answer) = self.answer(Stage::Barrier) else {
            return authored;
        };
        #[cfg(any(test, feature = "induced-failure"))]
        if !self.barrier_recorded.swap(true, Ordering::AcqRel) {
            self.record(Stage::Barrier, answer);
        }
        Duration::ZERO
    }

    /// The stream arm this watch owes the first delivery a consumer could act
    /// on: past `boundary`, past the close of `heal`, and one the watcher folds
    /// into a batch.
    pub(crate) fn stream_arm(&self, boundary: Boundary, heal: HealWindow) -> StreamArm {
        StreamArm {
            // Only the answers this stage carries are held, which is what makes
            // the delivery boundary below total: an arm cannot reach it owing
            // something it has no message for.
            owed: self
                .answer(Stage::Stream)
                .filter(|answer| Stage::Stream.answers(*answer)),
            boundary,
            heal,
            #[cfg(any(test, feature = "induced-failure"))]
            hits: self.hits.clone(),
        }
    }

    #[cfg(any(test, feature = "induced-failure"))]
    fn record(&self, stage: Stage, answer: Answer) {
        record(self.hits.as_deref(), stage, answer);
    }
}

/// One armed stream answer, held until a delivery a consumer could act on
/// displaces it.
///
/// The arm stands at the handler boundary, upstream of ingest and the
/// coalescer, because what a stream failure or an overflow *is* to the rest of
/// the watcher is a message arriving there. It waits for three things, and each
/// one is about standing in place of a delivery that reaches a consumer as a
/// live change: the synchronization boundary, the close of the first heal
/// window, and then a delivery the watcher folds into a batch. It is taken when
/// it fires: one armed answer, one firing, one record, per establishment.
pub(crate) struct StreamArm {
    owed: Option<Answer>,
    boundary: Boundary,
    heal: HealWindow,
    #[cfg(any(test, feature = "induced-failure"))]
    hits: Option<PathBuf>,
}

impl StreamArm {
    /// What the watcher ingests in place of `delivered`.
    ///
    /// `folds_into_a_batch` is the watcher's own relevance rule — ingest's,
    /// shared rather than restated — asked about a delivery that is still the
    /// backend's own. It is a parameter rather than a call from inside here so
    /// that the seam holds no watch state: the handler already has the state
    /// ingest folds into, and this asks it the same question ingest will.
    pub(crate) fn answer(
        &mut self,
        delivered: notify::Result<Event>,
        folds_into_a_batch: impl Fn(&Event) -> bool,
    ) -> notify::Result<Event> {
        // The owed check comes first: an unowed arm — every delivery in an
        // unarmed watch — pays nothing for the gates below, which matter only
        // while an answer is still held.
        if self.owed.is_none() || !self.boundary.was_reached() || !self.heal.has_closed() {
            return delivered;
        }
        let event = match delivered {
            Ok(event) => event,
            // A failure the backend reported is already the subscription's last
            // fact, and it is not this arm's to replace: the arm stands in place
            // of an event and never in place of a failure, so it stays owed and
            // the real cause is the one the subscription carries.
            delivered @ Err(_) => return delivered,
        };
        // A delivery the watcher folds into no batch is not one this arm stands
        // in place of. inotify asks for `IN_OPEN`, so under live coverage every
        // document a heal reads arrives here as an access; and every backend
        // reports paths this vault's coverage plan takes in and its ingest then
        // discards — a sibling of the vault root, an excluded place. An arm
        // spent on one of those would report a stream that failed or overflowed
        // over a delivery no consumer ever hears about.
        if !folds_into_a_batch(&event) {
            return Ok(event);
        }
        let Some(answer) = self.owed.take() else {
            return Ok(event);
        };
        #[cfg(any(test, feature = "induced-failure"))]
        record(self.hits.as_deref(), Stage::Stream, answer);
        match answer {
            // The exact message both platform backends emit when they dropped
            // events: inotify on a queue overflow, FSEvents on
            // must-scan-subdirectories. Nothing in it names a path, because a
            // path set that was lost is what it reports.
            Answer::Rescans => Ok(Event::new(EventKind::Other).set_flag(Flag::Rescan)),
            // `fails`, and nothing else: an arm only ever holds an answer the
            // stream stage carries.
            _ => Err(notify::Error::generic(
                "an armed watcher fault ended this event stream",
            )),
        }
    }
}

/// Append one record saying which boundary fired and how it answered.
///
/// A harness that named no record file wants none. A harness that named one
/// wants every firing in it, so a file that cannot be written ends the process
/// rather than leaving a parent to read the silence as a boundary the watcher
/// never reached. That is the one place this seam parts from the write
/// protocol's, whose arms fire in a process that is already dying.
///
/// **Deliberately an abort rather than a panic.** One of the three boundaries
/// answers on the backend's own delivery thread, where an unwind ends that
/// thread and leaves the subscription standing — which is the same silence,
/// reached a different way. Nothing here is recoverable in any case: the
/// harness named a file this process cannot write, and every later arm would
/// meet it too. The reason goes to standard error first, because an abort says
/// nothing on its own.
#[cfg(any(test, feature = "induced-failure"))]
fn record(hits: Option<&Path>, stage: Stage, answer: Answer) {
    let Some(hits) = hits else {
        return;
    };
    if let Err(error) = crate::faults::append_record(hits, SEAM, stage.name(), answer.name()) {
        eprintln!(
            "norn-fs: the {} arm could not record itself in {}: {error}",
            stage.name(),
            hits.display()
        );
        std::process::abort();
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

    /// A boundary the subscription has already crossed, which is what every
    /// case about the stream stage stands past.
    fn crossed() -> Boundary {
        let boundary = Boundary::default();
        boundary.reached();
        boundary
    }

    /// A first heal window a consumer has already closed, which is the other
    /// thing every case about a firing arm stands past.
    fn taken_up() -> HealWindow {
        let heal = HealWindow::default();
        heal.closed();
        heal
    }

    /// An arm eligible at both gates, holding `armed`.
    fn eligible(armed: &'static [(Stage, Answer)]) -> StreamArm {
        WatchFaults::at(armed).stream_arm(crossed(), taken_up())
    }

    /// A relevance rule that folds every delivery in, which is what a case
    /// about the other gates holds constant.
    fn folds_everything(_: &Event) -> bool {
        true
    }

    /// A relevance rule that folds nothing in: the answer ingest gives a
    /// delivery it discards by path.
    fn folds_nothing(_: &Event) -> bool {
        false
    }

    /// Every stage and every answer a harness arms round-trips through the name
    /// it is armed under, so a widened seam and the suite arming it cannot drift
    /// into naming different things.
    #[test]
    fn every_stage_and_answer_has_one_name() {
        let stages: std::collections::BTreeSet<&str> =
            Stage::ALL.iter().map(|stage| stage.name()).collect();
        assert_eq!(stages.len(), Stage::ALL.len());
        let names: std::collections::BTreeSet<&str> =
            Answer::ALL.iter().map(|answer| answer.name()).collect();
        assert_eq!(names.len(), Answer::ALL.len());
        for stage in Stage::ALL {
            assert_eq!(Stage::named(stage.name()), Some(stage));
        }
        for answer in Answer::ALL {
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
            let carried: Vec<Answer> = Answer::ALL
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
    /// proceeds, the caller's own deadline is the wait, and a delivery reaches
    /// ingest as the backend produced it.
    #[test]
    fn an_unarmed_watch_answers_at_no_boundary() {
        let faults = WatchFaults::default();

        assert_eq!(faults.registration(), Ok(()));
        assert!(!faults.withholds_live());
        assert_eq!(
            faults.synchronization_wait(Duration::from_secs(7)),
            Duration::from_secs(7)
        );
        let mut arm = faults.stream_arm(crossed(), taken_up());
        let delivered = arm.answer(
            Ok(Event::new(EventKind::Other).add_path("/vault/one.md".into())),
            folds_everything,
        );
        assert_eq!(
            delivered.expect("the delivery the backend made").paths,
            [PathBuf::from("/vault/one.md")]
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
        assert!(faults.stream_arm(crossed(), taken_up()).owed.is_none());
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

    /// **A stream arm answers nothing before the boundary is reached.** A
    /// backend establishing itself delivers — macOS replays event history
    /// there — and an arm that answered one of those deliveries would put the
    /// failure it stands for before the coverage it is stated over.
    #[test]
    fn a_stream_arm_waits_for_the_synchronization_boundary() {
        let boundary = Boundary::default();
        let mut arm = WatchFaults::at(&[(Stage::Stream, Answer::Fails)])
            .stream_arm(boundary.clone(), taken_up());

        for _ in 0..3 {
            assert!(
                arm.answer(
                    Ok(Event::new(EventKind::Other).add_path("/vault/replayed.md".into())),
                    folds_everything,
                )
                .is_ok(),
                "an arm answered a delivery from before the boundary"
            );
        }

        boundary.reached();
        assert!(
            arm.answer(Ok(Event::new(EventKind::Other)), folds_everything)
                .is_err()
        );
    }

    /// **A stream arm answers nothing until the first heal window closes.** A
    /// consumer takes up coverage by healing under it, everything the backend
    /// reports across that window comes back as the heal's own batch, and a
    /// change made before the watch existed can still be delivered there — macOS
    /// numbers an event when the daemon processes it, not when the syscall
    /// returned. An arm answered inside the window would therefore be absorbed
    /// by the heal, or would stand in place of a change from before the
    /// coverage it is stated over.
    #[test]
    fn a_stream_arm_waits_for_the_first_heal_window_to_close() {
        let heal = HealWindow::default();
        let mut arm =
            WatchFaults::at(&[(Stage::Stream, Answer::Fails)]).stream_arm(crossed(), heal.clone());

        for _ in 0..3 {
            assert!(
                arm.answer(
                    Ok(Event::new(EventKind::Other).add_path("/vault/healed.md".into())),
                    folds_everything,
                )
                .is_ok(),
                "an arm answered a delivery the heal window was still absorbing"
            );
        }

        heal.closed();
        assert!(
            arm.answer(
                Ok(Event::new(EventKind::Other).add_path("/vault/live.md".into())),
                folds_everything,
            )
            .is_err()
        );
    }

    /// **A delivery the watcher folds into no batch leaves the arm owed.** The
    /// coverage plan takes in paths ingest then discards — a sibling of the
    /// vault root, an excluded place — and no consumer ever hears about one, so
    /// an arm spent there would report a failed stream over a delivery that
    /// changed nothing.
    #[test]
    fn a_delivery_that_reaches_no_batch_leaves_the_arm_owed() {
        let mut arm = eligible(&[(Stage::Stream, Answer::Fails)]);

        for _ in 0..3 {
            let passed = arm.answer(
                Ok(
                    Event::new(EventKind::Create(notify::event::CreateKind::Folder))
                        .add_path("/beside-the-vault/data".into()),
                ),
                folds_nothing,
            );
            assert_eq!(
                passed.expect("a discarded delivery is not a failure").paths,
                [PathBuf::from("/beside-the-vault/data")]
            );
        }

        assert!(
            arm.answer(
                Ok(Event::new(EventKind::Other).add_path("/vault/one.md".into())),
                folds_everything,
            )
            .is_err(),
            "the arm was spent on a delivery the watcher folds into no batch"
        );
    }

    /// The stream answers stand in place of the delivery, and the arm is taken
    /// when it fires: a second delivery is the backend's own again.
    #[test]
    fn a_stream_arm_displaces_one_delivery_and_no_more() {
        let mut arm = eligible(&[(Stage::Stream, Answer::Rescans)]);
        let answered = arm
            .answer(
                Ok(Event::new(EventKind::Other).add_path("/vault/one.md".into())),
                folds_everything,
            )
            .expect("a rescan answer is a delivery, not a failure");
        assert!(answered.need_rescan());
        assert!(answered.paths.is_empty());

        let after = arm.answer(
            Ok(Event::new(EventKind::Other).add_path("/vault/two.md".into())),
            folds_everything,
        );
        assert_eq!(
            after.expect("the delivery the backend made").paths,
            [PathBuf::from("/vault/two.md")]
        );

        let mut arm = eligible(&[(Stage::Stream, Answer::Fails)]);
        assert!(
            arm.answer(Ok(Event::new(EventKind::Other)), folds_everything)
                .is_err()
        );
        assert!(
            arm.answer(Ok(Event::new(EventKind::Other)), folds_everything)
                .is_ok()
        );
    }

    /// **A failure the backend really reported is never displaced.** The arm
    /// stands in place of an event; a delivered failure is already the
    /// subscription's last fact, and standing in front of it would report a
    /// live-and-overflowing stream over a stream that had stopped.
    #[test]
    fn a_delivered_failure_passes_through_and_leaves_the_arm_owed() {
        let mut arm = eligible(&[(Stage::Stream, Answer::Rescans)]);

        let passed = arm.answer(
            Err(notify::Error::generic("the backend's own failure")),
            folds_everything,
        );

        let error = passed.expect_err("the backend's failure reaches ingest");
        assert!(error.to_string().contains("the backend's own failure"));
        assert!(
            arm.answer(Ok(Event::new(EventKind::Other)), folds_everything)
                .expect("the arm is still owed")
                .need_rescan()
        );
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
            .synchronization_wait(Duration::from_secs(3600));
        WatchFaults::recording_at(&[(Stage::Stream, Answer::Fails)], hits.clone())
            .stream_arm(crossed(), taken_up())
            .answer(Ok(Event::new(EventKind::Other)), folds_everything)
            .expect_err("the armed stream");

        assert_eq!(
            std::fs::read_to_string(&hits).expect("the record file"),
            "seam=norn-fs/watch stage=install answer=refuses\n\
             seam=norn-fs/watch stage=barrier answer=expires\n\
             seam=norn-fs/watch stage=stream answer=fails\n"
        );
    }

    /// **The barrier records the wait, not the arming.** A watch nobody waits
    /// on reached no boundary, and one waited on twice reached it once.
    #[test]
    #[allow(clippy::disallowed_methods)] // Test observation of the arm's own record file.
    fn the_barrier_records_once_when_a_caller_reaches_the_withheld_boundary() {
        let scratch = crate::scratch::Scratch::new("watch-barrier-records");
        let hits = scratch.path("arm-hits");
        let faults = WatchFaults::recording_at(&[(Stage::Barrier, Answer::Expires)], hits.clone());

        assert!(faults.withholds_live());
        assert!(
            !scratch.exists(&hits),
            "a watch nobody waited on recorded a boundary it never reached"
        );

        for _ in 0..3 {
            assert_eq!(
                faults.synchronization_wait(Duration::from_secs(3600)),
                Duration::ZERO
            );
        }

        assert_eq!(
            std::fs::read_to_string(&hits).expect("the record file"),
            "seam=norn-fs/watch stage=barrier answer=expires\n"
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
            // One stage armed twice: the seam answers a stage once, so the
            // second pair is a spelling that would silently do nothing.
            "stream=rescans,stream=fails",
            "barrier=expires,install=refuses,barrier=expires",
        ] {
            assert!(
                std::panic::catch_unwind(|| armed::parse(spelling)).is_err(),
                "`{spelling}` was read as an arm"
            );
        }
    }

    /// The same refusals bind an arm a case spells in code, which is the other
    /// door onto the same vocabulary. The door itself is what is held here —
    /// [`WatchFaults::at`], not the refusal function — so a constructor that
    /// stopped asking would fail this case rather than inherit its green.
    #[test]
    fn an_arm_spelled_in_code_meets_the_same_refusals() {
        for armed in [
            &[(Stage::Stream, Answer::Expires)][..],
            &[
                (Stage::Stream, Answer::Fails),
                (Stage::Stream, Answer::Rescans),
            ][..],
        ] {
            assert!(
                std::panic::catch_unwind(|| WatchFaults::at(armed)).is_err(),
                "{armed:?} was read as an arm"
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
