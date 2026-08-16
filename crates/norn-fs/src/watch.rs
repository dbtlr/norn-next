#![forbid(unsafe_code)]
//! High-fidelity filesystem invalidation, independent of the platform backend.
//!
//! [`watch`] subscribes to a vault tree and its schema source. Backend events are
//! deliberately erased into order-independent [`Batch`] values: paths say only
//! what may have changed, and a [`RescanScope`] says notification loss made that
//! path set incomplete. The bias is one-way: redundant invalidation is cheap;
//! an omitted change can support a wrong answer.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, Weak, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use notify::event::{CreateKind, EventKind, ModifyKind};
use notify::{Config, Event, PollWatcher, RecursiveMode};

use crate::PostState;
use crate::exclusion::Exclusions;
use crate::hash::hashed_from;
use crate::path::{NormalizedPath, PathError, PathNormalizer};
use crate::write::{Landed, Moved, Vacated};

mod faults;

use faults::{Boundary, HealWindow, WatchFaults};

/// A trailing quiet period that closes a naturally settled batch.
pub const QUIET_WINDOW: Duration = Duration::from_millis(50);
/// The longest a continuous event stream may postpone delivery.
pub const MAX_BATCH_AGE: Duration = Duration::from_millis(500);
/// How long an unobserved own-write outcome remains eligible for suppression.
pub const OWN_WRITE_TTL: Duration = Duration::from_secs(5);
/// The authored bound on distinct dirty roots retained before widening to a rescan.
///
/// 8,192 holds one complete 7,320-file soak-profile burst (6,000 documents plus
/// its generated clutter) with headroom, while still bounding an adversarial burst.
pub const DIRTY_ROOT_CAP: usize = 8192;
/// The authored bound on pending own-write outcomes. Overflow fails open.
///
/// 4,096 holds two complete 2,000-document realistic-profile rewrites during
/// the ledger's short lifetime.
pub const OWN_WRITE_CAP: usize = 4096;

/// The control-plane state of one watcher subscription.
///
/// This state is deliberately separate from [`Batch`]: readiness is proof
/// about coverage, not a filesystem fact to reconcile.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SubscriptionState {
    /// Coverage edges are still establishing their backend boundary.
    Synchronizing,
    /// Every planned edge has crossed its backend boundary.
    Live,
    /// Coverage establishment or established coverage failed permanently.
    Terminal(WatchError),
}

/// A coverage partition whose exact changed paths are no longer known.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum RescanScope {
    /// The recursively watched vault tree.
    Vault,
    /// The configured schema source.
    Schema,
}

/// One settled, backend-independent set of filesystem facts.
#[derive(Debug, Default, Eq, PartialEq)]
pub struct Batch {
    vault_roots: BTreeSet<NormalizedPath>,
    schema_dirty: bool,
    rescans: BTreeSet<RescanScope>,
}

impl Batch {
    /// One widened invalidation, useful when a host learns uncertainty outside
    /// the platform backend itself.
    pub fn rescan(scope: RescanScope) -> Self {
        Self {
            rescans: BTreeSet::from([scope]),
            ..Self::default()
        }
    }
    /// One settled schema invalidation and no vault path.
    ///
    /// This is the fact an edit to the configured schema source carries,
    /// separated from whatever else the events reporting it touched: a batch
    /// the coalescer settles for an in-vault schema also names that schema's
    /// own path among its dirty roots, and one settled for an external source
    /// names nothing inside the vault at all.
    ///
    /// The batch's fields are private, so this constructor is the one way a
    /// consumer outside this crate stands the fact up without a coalescer;
    /// the host's schema-reconcile tests are its present callers.
    pub fn schema_change() -> Self {
        Self {
            schema_dirty: true,
            ..Self::default()
        }
    }
    /// One settled invalidation of a vault path, and no other fact.
    ///
    /// The root stands for its own entry and every descendant of it, which is
    /// the shape a coalesced backend event settles into. The batch's fields
    /// are private, so this constructor is the one way a consumer outside this
    /// crate stands a per-path invalidation up without a coalescer: the host's
    /// absorbing-wait cases, which drive a pump with the reports they name
    /// rather than the ones a platform happens to deliver, are its present
    /// callers.
    pub fn vault_change(root: NormalizedPath) -> Self {
        Self {
            vault_roots: BTreeSet::from([root]),
            ..Self::default()
        }
    }
    /// Normalized vault-relative roots whose entries and descendants are invalid.
    pub fn vault_roots(&self) -> &BTreeSet<NormalizedPath> {
        &self.vault_roots
    }
    /// Whether the configured schema source may have changed.
    pub fn schema_dirty(&self) -> bool {
        self.schema_dirty
    }
    /// Coverage partitions whose precise dirty set was lost.
    pub fn rescans(&self) -> &BTreeSet<RescanScope> {
        &self.rescans
    }

    /// Merge another settled batch without losing any uncertainty.
    pub fn merge(&mut self, other: Batch) {
        self.vault_roots.extend(other.vault_roots);
        self.schema_dirty |= other.schema_dirty;
        self.rescans.extend(other.rescans);
        if self.vault_roots.len() > DIRTY_ROOT_CAP {
            self.vault_roots.clear();
            self.rescans.insert(RescanScope::Vault);
        }
    }

    /// Whether this envelope carries no invalidation fact.
    pub fn is_empty(&self) -> bool {
        self.vault_roots.is_empty() && !self.schema_dirty && self.rescans.is_empty()
    }
}

/// A terminal failure of watcher setup or established coverage.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WatchError {
    /// Coverage could not be installed or the backend later failed.
    Backend(String),
    /// The vault root itself disappeared or ceased to be covered.
    CoverageLost(PathBuf),
    /// Coverage did not become live before the caller's authored deadline.
    SynchronizationExpired,
}

impl fmt::Display for WatchError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Backend(message) => write!(formatter, "filesystem watcher failed: {message}"),
            Self::CoverageLost(path) => {
                write!(formatter, "watch coverage was lost for {}", path.display())
            }
            Self::SynchronizationExpired => {
                write!(formatter, "filesystem watcher synchronization expired")
            }
        }
    }
}
impl std::error::Error for WatchError {}

/// The single-consumer source of settled batches returned by [`watch`].
///
/// Delivery is pull-only. [`Subscription::try_recv`] takes at most one settled
/// batch and returns at once, so a consumer reads on a tick of its own. That is
/// what the host needs: one thread scans every attachment in turn, and a
/// receive that parked it would spend the whole scan on whichever vault is
/// quietest.
///
/// A delivered batch is closed, and the delivery slot holds one. A consumer
/// that polls slowly therefore holds the coalescer at its next send, and
/// everything arriving meanwhile merges into the batch still pending behind
/// it — so slow polling widens the batches yet to come rather than dropping
/// facts, and a dirty set past [`DIRTY_ROOT_CAP`] widens to a
/// [`RescanScope::Vault`] rescan instead of growing. More than one batch can
/// be waiting, so a consumer that wants everything settled receives until
/// `try_recv` reports no batch.
///
/// A terminal [`WatchError`] is the last fact a subscription carries: it is
/// delivered once, no batch follows it, and a receive after it reports either
/// no batch or that the watcher stopped.
///
/// It is not cloneable — one subscription, one consumer — and dropping it tears
/// the backend down and joins the worker.
pub struct Subscription {
    batches: Option<mpsc::Receiver<Result<Batch, WatchError>>>,
    watcher: Option<Box<dyn notify::Watcher + Send>>,
    worker: Option<thread::JoinHandle<()>>,
    control: Arc<(Mutex<SubscriptionState>, Condvar)>,
    state: Arc<Mutex<State>>,
    wake: Option<mpsc::SyncSender<()>>,
    faults: WatchFaults,
    /// Closed by the first [`Subscription::finish_heal`], and read by the fault
    /// seam's stream arm: the first delivery a consumer sees as a live change
    /// is the first one after a heal has taken up the coverage.
    first_heal: HealWindow,
}

impl Subscription {
    /// Wait until every coverage edge is live, or until coverage fails.
    ///
    /// Expiry is returned as a typed terminal watcher error. It does not
    /// synthesize a filesystem rescan or alter the backend selection.
    ///
    /// A failure here is the whole subscription's terminal fact, not just this
    /// caller's: it is recorded so the batch stream and [`Self::finish_heal`]
    /// refuse too. Coverage that could not prove itself never yields batches a
    /// consumer may treat as a complete account of the tree, whether or not
    /// that consumer read the error returned here.
    pub fn synchronize(&self, deadline: Duration) -> Result<(), WatchError> {
        // An armed barrier withheld the publication this wait is for, so it
        // ends the wait at once instead of holding the caller's authored
        // deadline against a boundary that is not coming. Everything below is
        // reached the way an expiry on a real machine reaches it.
        let outcome =
            wait_for_synchronization(&self.control, self.faults.synchronization_wait(deadline));
        if let Err(error) = &outcome {
            self.state
                .lock()
                .expect("watch state poisoned")
                .terminal
                .get_or_insert_with(|| error.clone());
            // The coalescer is parked with nothing pending, so the recorded
            // failure reaches the stream on this wake rather than behind
            // whatever the backend reports next.
            let _ = self
                .wake
                .as_ref()
                .expect("subscription wake sender present")
                .try_send(());
        }
        outcome
    }

    /// Hold settled output while a hash-authoritative heal is in flight.
    pub fn begin_heal(&self) {
        self.state.lock().expect("watch state poisoned").healing = true;
    }

    /// Close the heal window and take every fact accumulated during it.
    ///
    /// Everything the backend reported across the window is handed back here,
    /// so the first delivery this consumer meets as a change of its own is the
    /// next one. That is what the fault seam's stream stage stands in place of,
    /// and closing the first window is what makes its arm eligible.
    pub fn finish_heal(&self) -> Result<Batch, WatchError> {
        self.first_heal.closed();
        let work = {
            let mut state = self.state.lock().expect("watch state poisoned");
            state.healing = false;
            if let Some(error) = state.terminal.take() {
                state.pending.take();
                return Err(error);
            }
            let root = state.root.clone();
            let ledger = state.ledger.clone();
            state
                .pending
                .take()
                .map(|pending| (root, ledger, pending.batch))
        };
        let _ = self
            .wake
            .as_ref()
            .expect("subscription wake sender present")
            .try_send(());
        Ok(work.map_or_else(Batch::default, |(root, ledger, batch)| {
            suppress(&root, &ledger, batch)
        }))
    }

    /// Observe the current control state without consuming it.
    pub fn state(&self) -> SubscriptionState {
        self.control
            .0
            .lock()
            .expect("watch control state poisoned")
            .clone()
    }

    /// Receive one already-settled batch without waiting.
    pub fn try_recv(&self) -> Result<Option<Batch>, WatchError> {
        match self
            .batches
            .as_ref()
            .expect("subscription receiver present")
            .try_recv()
        {
            Ok(Ok(batch)) => Ok(Some(batch)),
            Ok(Err(error)) => Err(error),
            Err(mpsc::TryRecvError::Empty) => Ok(None),
            Err(mpsc::TryRecvError::Disconnected) => {
                Err(WatchError::Backend("watcher stopped".into()))
            }
        }
    }
}

fn wait_for_synchronization(
    control: &Arc<(Mutex<SubscriptionState>, Condvar)>,
    deadline: Duration,
) -> Result<(), WatchError> {
    let (state, changed) = &**control;
    let state = state.lock().expect("watch control state poisoned");
    let (mut state, _) = changed
        .wait_timeout_while(state, deadline, |state| {
            matches!(state, SubscriptionState::Synchronizing)
        })
        .expect("watch control state poisoned");
    match &*state {
        SubscriptionState::Live => Ok(()),
        SubscriptionState::Terminal(error) => Err(error.clone()),
        SubscriptionState::Synchronizing => {
            let error = WatchError::SynchronizationExpired;
            *state = SubscriptionState::Terminal(error.clone());
            changed.notify_all();
            Err(error)
        }
    }
}

fn publish_control(control: &Arc<(Mutex<SubscriptionState>, Condvar)>, next: SubscriptionState) {
    let (state, changed) = &**control;
    let mut state = state.lock().expect("watch control state poisoned");
    if !matches!(*state, SubscriptionState::Terminal(_)) {
        *state = next;
        changed.notify_all();
    }
}

impl Drop for Subscription {
    fn drop(&mut self) {
        self.watcher.take();
        // Disconnect a worker blocked behind the bounded delivery slot before
        // joining it. Backend callbacks write only shared pending state, so
        // delivery backpressure never blocks event intake.
        self.batches.take();
        self.wake.take();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

/// A cloneable recorder for outcomes produced by Norn's write kernel.
///
/// Recorder handles do not keep the subscription or platform watcher alive.
/// Layer 4 plan-apply keeps one beside its attachment so writes made through
/// Norn can be hash-confirmed and suppressed when the watcher reports them
/// back. It is intentionally live before that layer exists.
#[derive(Clone)]
pub struct OwnWrites {
    ledger: Weak<Mutex<Ledger>>,
    root: PathBuf,
    normalizer: PathNormalizer,
}

impl OwnWrites {
    /// Records a successful publish. [`Landed::Unchanged`] records nothing.
    ///
    /// `path` may be vault-relative or absolute beneath the watched vault root.
    /// An absolute path outside that root is refused as [`PathError::Absolute`].
    pub fn landed(&self, path: &Path, landed: Landed) -> Result<(), PathError> {
        let path = self.normalize(path)?;
        if landed.wrote() {
            self.record(path, Expected::Present(landed.post_state()));
        }
        Ok(())
    }

    /// Records a successful removal, at a vault-relative or contained absolute path.
    pub fn vacated(&self, path: &Path, vacated: Vacated) -> Result<(), PathError> {
        let path = self.normalize(path)?;
        let _ = vacated;
        self.record(path, Expected::Absent);
        Ok(())
    }

    /// Records both legs of a successful move.
    ///
    /// Both paths may be vault-relative or absolute beneath the watched root.
    pub fn moved(&self, source: &Path, destination: &Path, moved: Moved) -> Result<(), PathError> {
        let source = self.normalize(source)?;
        let destination = self.normalize(destination)?;
        self.record(destination, Expected::Present(moved.created));
        self.record(source, Expected::Absent);
        Ok(())
    }

    fn record(&self, path: NormalizedPath, expected: Expected) {
        let Some(ledger) = self.ledger.upgrade() else {
            return;
        };
        let mut ledger = ledger.lock().expect("own-write ledger poisoned");
        if ledger.entries.len() + usize::from(!ledger.entries.contains_key(&path)) >= OWN_WRITE_CAP
        {
            ledger.entries.clear();
            return;
        }
        ledger.entries.insert(
            path,
            LedgerEntry {
                expected,
                recorded: Instant::now(),
            },
        );
    }

    fn normalize(&self, path: &Path) -> Result<NormalizedPath, PathError> {
        if path.is_absolute() {
            if let Ok(relative) = path.strip_prefix(&self.root) {
                return self.normalizer.normalize(relative);
            }
            // Write callers can retain `/var/...` while the watcher keeps the
            // canonical `/private/var/...` spelling reported by FSEvents. Do
            // not resolve the replaceable file itself: resolve its parent and
            // reattach the name, just as schema coverage does.
            let canonical = canonical_parent_path(path).map_err(|_| PathError::Absolute)?;
            let relative = canonical
                .strip_prefix(&self.root)
                .map_err(|_| PathError::Absolute)?;
            self.normalizer.normalize(relative)
        } else {
            self.normalizer.normalize(path)
        }
    }
}

/// Starts watching only after every requested coverage edge is installed.
///
/// This is the first stage of attach, not a declaration that an entry is ready:
/// the caller waits for [`Subscription::synchronize`], heals, and consumes all
/// resulting batches before exposing the entry as ready. Later batches remain
/// the same subscription's warm invalidation stream.
///
/// **Returning means coverage is installed, not that the subscription is
/// [`SubscriptionState::Live`].** Which of the two returning implies is the
/// backend's to say, and [`Subscription::synchronize`] is the one place a
/// caller asks. Once a subscription is live, every change from before its
/// edges were installed is either reported by the stream or already visible to
/// a read of the tree, so the heal that follows synchronization plus the
/// batches settling behind it leave no window a change can fall through.
pub fn watch(
    vault_root: &Path,
    schema_source: &Path,
) -> Result<(Subscription, OwnWrites), WatchError> {
    watch_with(vault_root, schema_source, false)
}

/// Starts polling coverage through the same backend-erased watch seam.
///
/// Layer 1 host registration uses this degraded-latency path for filesystems
/// whose native notification stream is unreliable. Backend event vocabulary
/// and construction remain private to this filesystem effect seam.
pub fn watch_polling(
    vault_root: &Path,
    schema_source: &Path,
) -> Result<(Subscription, OwnWrites), WatchError> {
    watch_with(vault_root, schema_source, true)
}

fn watch_with(
    vault_root: &Path,
    schema_source: &Path,
    poll: bool,
) -> Result<(Subscription, OwnWrites), WatchError> {
    establish(
        vault_root,
        schema_source,
        poll,
        // The one place the watcher fault seam is widened, and only under the
        // `induced-failure` feature: the bars over refused coverage, a stream
        // that fails or overflows, and a boundary that never arrives are stated
        // about a process the platform does not cooperate with, so the arm is
        // read from the environment that process was started with. Every other
        // caller of `establish` passes the arm it is stating a case over.
        WatchFaults::entry(),
    )
}

/// Establish coverage under `faults`, which say which of the watcher's own
/// boundaries fail rather than waiting for a platform that fails there.
#[allow(clippy::disallowed_methods)] // norn-fs owns vault path resolution.
fn establish(
    vault_root: &Path,
    schema_source: &Path,
    poll: bool,
    faults: WatchFaults,
) -> Result<(Subscription, OwnWrites), WatchError> {
    // macOS FSEvents reports the canonical `/private/var/...` spelling even
    // when a caller registered `/var/...`. Keep registration and callback
    // classification in that same spelling without resolving the schema file
    // itself (its name is expected to be atomically replaced).
    let root = std::fs::canonicalize(vault_root)
        .map_err(|error| WatchError::Backend(format!("cannot resolve vault root: {error}")))?;
    let schema_source = canonical_parent_path(schema_source)?;
    let normalizer =
        PathNormalizer::detect(&root).map_err(|error| WatchError::Backend(error.to_string()))?;
    let schema = schema_location(&root, &schema_source, &normalizer)?;
    // Every coverage edge is decided before a backend resource exists, so a
    // plan this vault cannot express refuses without a watcher to tear down.
    let plan = coverage_plan(&root, &schema)?;
    let ledger = Arc::new(Mutex::new(Ledger::default()));
    let shared = Arc::new(Mutex::new(State::new(
        root.clone(),
        normalizer.clone(),
        schema,
        ledger.clone(),
    )));
    let (wake_tx, wake_rx) = mpsc::sync_channel(1);
    // The queue is one delivered batch, the one the worker is handing over
    // behind it, and the bounded shared pending state. A stalled consumer
    // backpressures only this worker; callbacks keep merging into State and
    // widen to Rescan at the authored cap.
    let (batch_tx, batch_rx) = mpsc::sync_channel(1);
    let control = Arc::new((Mutex::new(SubscriptionState::Synchronizing), Condvar::new()));
    let callback_state = shared.clone();
    let callback_wake = wake_tx.clone();
    let callback_control = control.clone();
    // The two facts that make a delivery one a consumer sees as a live change:
    // this subscription has crossed its synchronization boundary, and the
    // consumer's first heal window over it has closed. The stream stage is
    // stated over such a delivery, so the arm answers nothing before both hold.
    let boundary = Boundary::default();
    let first_heal = HealWindow::default();
    let mut stream_arm = faults.stream_arm(boundary.clone(), first_heal.clone());
    let handler = move |result| {
        // The stream stage answers here, upstream of ingest and the coalescer,
        // by standing in place of the message the backend delivered: what a
        // stream that failed or overflowed is to the rest of the watcher is a
        // message arriving at this boundary. It asks the same state ingest is
        // about to fold into whether this delivery is one that reaches a batch
        // at all. An unarmed watch hands the delivery through unchanged.
        let result = stream_arm.answer(result, |event| folds_into_a_batch(&callback_state, event));
        // A backend failure is control state as well as the subscription's
        // last fact: a caller waiting on the boundary is waiting on this
        // thread, so it learns of the failure here rather than at the
        // deadline.
        if let Some(error) = ingest(&callback_state, result) {
            publish_control(&callback_control, SubscriptionState::Terminal(error));
        }
        let _ = callback_wake.try_send(());
    };
    let mut watcher: Box<dyn notify::Watcher + Send> = match poll {
        false => native_watcher(handler, &control, &faults, boundary.clone())?,
        true => Box::new(
            PollWatcher::new(
                handler,
                Config::default()
                    .with_poll_interval(Duration::from_millis(250))
                    // Polling exists for filesystems whose notification and
                    // timestamp fidelity is suspect. Layer 1's hash-authority
                    // rule forbids a same-stat edit from disappearing here.
                    .with_compare_contents(true),
            )
            .map_err(backend)?,
        ),
    };

    install(watcher.as_mut(), &plan, &faults)?;

    if registration_is_the_boundary(poll) {
        // Reached, whatever is published for it: an armed barrier withholds the
        // publication below, and the complete plan is registered from here
        // either way. This backend queues everything inside the registration
        // call, so the boundary really is the point past which every delivery
        // is a change that followed it.
        boundary.reached();
        let established = shared
            .lock()
            .expect("watch state poisoned")
            .terminal
            .clone()
            .map_or(SubscriptionState::Live, SubscriptionState::Terminal);
        match established {
            SubscriptionState::Live if faults.withholds_live() => {}
            established => publish_control(&control, established),
        }
    }

    let worker_state = shared.clone();
    let worker = thread::spawn(move || run_coalescer(worker_state, wake_rx, batch_tx));
    let owns = OwnWrites {
        ledger: Arc::downgrade(&ledger),
        root,
        normalizer,
    };
    Ok((
        Subscription {
            batches: Some(batch_rx),
            watcher: Some(watcher),
            worker: Some(worker),
            control,
            state: shared,
            wake: Some(wake_tx),
            faults,
            first_heal,
        },
        owns,
    ))
}

/// Whether returning from bulk registration is itself the synchronization
/// boundary for the selected backend.
///
/// Polling answers yes on every platform: [`PollWatcher`] builds each edge's
/// initial snapshot synchronously inside registration, so the committed bulk
/// registration closes the baseline for the complete plan.
///
/// Native watching answers per platform. Where a backend establishes each
/// watch inside the registration call and queues everything after it — inotify
/// does — registration is the boundary. macOS FSEvents does not: its stream
/// starts asynchronously after the stream is created, and its boundary is the
/// event-history marker [`native_watcher`] installs, which is the only thing
/// that publishes [`SubscriptionState::Live`] there.
#[cfg(target_os = "macos")]
fn registration_is_the_boundary(poll: bool) -> bool {
    poll
}

#[cfg(not(target_os = "macos"))]
fn registration_is_the_boundary(_poll: bool) -> bool {
    true
}

/// Build the native backend behind whatever proof of coverage it can give.
///
/// On macOS the stream is created with the per-host event identifier read
/// here, before any coverage edge is installed. The stream therefore replays
/// the backlog the host had recorded past that reading, delivers it to the
/// handler, and then reports the history boundary — which is what publishes
/// [`SubscriptionState::Live`]. Identifiers come from one per-host source and
/// increase across every attached volume, so the one boundary covers the
/// complete plan, including an edge on another volume.
///
/// Nothing else publishes `Live` for this backend: an installed stream that
/// never reports its boundary leaves the subscription synchronizing until the
/// caller's deadline, which is a typed [`WatchError::SynchronizationExpired`]
/// rather than a subscription that claims coverage it cannot prove.
///
/// **What the boundary proves is that coverage is installed, not that the tree
/// is quiet.** See [`history_barrier`] for what the replay does and does not
/// account for.
#[cfg(target_os = "macos")]
fn native_watcher(
    handler: impl notify::EventHandler,
    control: &Arc<(Mutex<SubscriptionState>, Condvar)>,
    faults: &WatchFaults,
    boundary: Boundary,
) -> Result<Box<dyn notify::Watcher + Send>, WatchError> {
    let since = history_boundary(notify::fsevent::current_event_id())?;
    Ok(Box::new(
        notify::fsevent::FsEventWatcher::with_event_history(
            handler,
            since,
            history_barrier(control, faults, boundary),
        )
        .map_err(backend)?,
    ))
}

/// What the native macOS backend calls when its event-history replay is
/// complete, and the only thing that publishes `Live` for that backend.
///
/// It is also this platform's half of the barrier stage: an armed watch
/// withholds the publication here, exactly as one whose boundary is
/// registration withholds it there, so a barrier that never arrives is the same
/// fact on both platform paths.
///
/// # What this boundary does not exclude
///
/// It says the replay of the recorded backlog is finished. It does **not** say
/// every change already made to the tree has been delivered, and it cannot:
/// `FSEventsGetCurrentEventId` reports the last identifier *fseventsd assigned*,
/// and the daemon assigns one when it processes the kernel's notification
/// rather than when the syscall returned. A change whose syscall completed
/// before the reading can therefore be numbered after it, fall outside the
/// backlog, and arrive on the live side of this marker — and the daemon's
/// per-path coalescing can carry it there under a later identifier still.
///
/// Coverage is unaffected: such a delivery is folded into a batch and the tree
/// is reread, which is the same bias every other report gets. What it costs is
/// determinism for anything that wants *the* first live change, and the fault
/// seam's stream stage is the one such caller. That is why its arm waits for a
/// consumer's first heal window to close as well
/// ([`faults::HealWindow`]): the tail belongs to the establishment a heal takes
/// up, and past the heal there is nothing left to arrive from before the watch.
#[cfg(target_os = "macos")]
fn history_barrier(
    control: &Arc<(Mutex<SubscriptionState>, Condvar)>,
    faults: &WatchFaults,
    boundary: Boundary,
) -> impl Fn() + Send + Sync + 'static {
    let control = control.clone();
    let faults = faults.clone();
    move || {
        // Reached before anything is published for it, and whether or not
        // anything is: the replay is done and coverage is installed, which is
        // what the barrier stage is stated over.
        boundary.reached();
        if !faults.withholds_live() {
            publish_control(&control, SubscriptionState::Live);
        }
    }
}

/// The reading a native macOS stream may replay from.
///
/// Two readings are not boundaries. Zero means the host recorded no event to
/// start after, and replaying from it asks for every event the host still
/// holds. `u64::MAX` is the sentinel for starting at stream creation, which is
/// the window the barrier exists to close. Both refuse rather than watch
/// without proof; polling remains available as an explicit selection.
#[cfg(target_os = "macos")]
fn history_boundary(current: u64) -> Result<u64, WatchError> {
    match current {
        0 | u64::MAX => Err(WatchError::Backend(
            "macOS reported no usable event-history boundary, so native watching cannot prove \
             coverage"
                .into(),
        )),
        reading => Ok(reading),
    }
}

/// Why a recommended backend of this kind cannot serve a native registration.
///
/// Notify falls back to its polling backend where a platform offers no native
/// notification API. Building it here would give a caller that asked for
/// native watching the cost and latency of polling under a subscription that
/// reports itself live, and polling is an explicit selection through
/// [`watch_polling`]. Every other kind is a native backend whose registration
/// establishes each edge, which is the boundary this platform publishes.
#[cfg(not(target_os = "macos"))]
fn native_kind_refusal(kind: notify::WatcherKind) -> Option<WatchError> {
    matches!(kind, notify::WatcherKind::PollWatcher).then(|| {
        WatchError::Backend(
            "this platform recommends the polling backend for native watching, and polling is an \
             explicit selection rather than a substitution made for a caller"
                .into(),
        )
    })
}

#[cfg(not(target_os = "macos"))]
fn native_watcher(
    handler: impl notify::EventHandler,
    _control: &Arc<(Mutex<SubscriptionState>, Condvar)>,
    _faults: &WatchFaults,
    // Every non-macOS backend establishes each edge inside registration, so
    // `establish` says the boundary was reached there rather than here.
    _boundary: Boundary,
) -> Result<Box<dyn notify::Watcher + Send>, WatchError> {
    use notify::Watcher as _;

    if let Some(refusal) = native_kind_refusal(notify::RecommendedWatcher::kind()) {
        return Err(refusal);
    }
    Ok(Box::new(
        notify::RecommendedWatcher::new(handler, Config::default()).map_err(backend)?,
    ))
}

/// Every coverage edge one subscription installs, in registration order.
///
/// The vault tree is covered recursively. Its parent is covered
/// non-recursively for one fact the tree's own watch cannot report: the name
/// the root occupies, whose removal or rename is the end of coverage rather
/// than a change inside the vault. A schema source outside the vault adds its
/// own parent, and only when neither of those two already covers it.
fn coverage_plan(
    root: &Path,
    schema: &SchemaLocation,
) -> Result<Vec<(PathBuf, RecursiveMode)>, WatchError> {
    let parent = root
        .parent()
        .ok_or_else(|| WatchError::Backend("vault root has no parent".into()))?;
    let mut plan = vec![
        (root.to_owned(), RecursiveMode::Recursive),
        (parent.to_owned(), RecursiveMode::NonRecursive),
    ];
    if let SchemaLocation::External {
        parent: schema_parent,
        ..
    } = schema
        && schema_parent != parent
    {
        plan.push((schema_parent.clone(), RecursiveMode::NonRecursive));
    }
    Ok(plan)
}

/// Install a whole coverage plan through the backend's bulk-registration seam.
///
/// A native backend can commit every edge together and avoid rebuilding its
/// event stream per edge. Notify's polling backend implements the same seam as
/// sequential registrations with a no-op commit; [`watch`] still exposes no
/// subscription until the whole plan is installed, so no intermediate prefix
/// of coverage reaches a caller.
///
/// **The batch is committed whether or not every edge was accepted.** A
/// `PathsMut` dropped without a commit leaves the watcher in a state notify
/// declines to specify — started, stopped, changes applied or ignored — so a
/// refused edge commits what was staged before it and then returns that first
/// refusal unchanged, path identity included. Partial coverage is never
/// returned as success: the caller of a failed install has a watcher whose
/// only sound use is to drop it, and dropping it is now deterministic.
fn install(
    watcher: &mut dyn notify::Watcher,
    plan: &[(PathBuf, RecursiveMode)],
    faults: &WatchFaults,
) -> Result<(), WatchError> {
    // The install stage stands ahead of the registration it simulates, and what
    // it simulates is the platform call's own typed answer: nothing is staged,
    // nothing is committed, and the refusal reaches the caller through the same
    // teardown a refused edge reaches it through.
    faults.registration()?;
    let mut paths = watcher.paths_mut();
    let refused = plan
        .iter()
        .find_map(|(path, mode)| paths.add(path, *mode).err());
    let committed = paths.commit();
    match refused {
        Some(error) => Err(backend(error)),
        None => committed.map_err(backend),
    }
}

#[allow(clippy::disallowed_methods)] // norn-fs owns vault/schema path resolution.
fn canonical_parent_path(path: &Path) -> Result<PathBuf, WatchError> {
    let parent = path
        .parent()
        .ok_or_else(|| WatchError::Backend("schema source has no parent".into()))?;
    let name = path
        .file_name()
        .ok_or_else(|| WatchError::Backend("schema source has no file name".into()))?;
    let parent = std::fs::canonicalize(parent).map_err(|error| {
        WatchError::Backend(format!("cannot resolve schema source parent: {error}"))
    })?;
    Ok(parent.join(name))
}

fn backend(error: notify::Error) -> WatchError {
    WatchError::Backend(error.to_string())
}

#[derive(Clone, Debug)]
enum SchemaLocation {
    InVault(NormalizedPath),
    External {
        name: NormalizedPath,
        parent: PathBuf,
        normalizer: PathNormalizer,
    },
}

fn schema_location(
    root: &Path,
    schema: &Path,
    normalizer: &PathNormalizer,
) -> Result<SchemaLocation, WatchError> {
    if let Ok(relative) = schema.strip_prefix(root)
        && let Ok(path) = normalizer.normalize(relative)
    {
        return Ok(SchemaLocation::InVault(path));
    }
    let parent = schema
        .parent()
        .ok_or_else(|| WatchError::Backend("schema source has no parent".into()))?
        .to_owned();
    let schema_normalizer =
        PathNormalizer::detect(&parent).map_err(|error| WatchError::Backend(error.to_string()))?;
    let name = schema_normalizer
        .normalize(Path::new(schema.file_name().ok_or_else(|| {
            WatchError::Backend("schema source has no file name".into())
        })?))
        .map_err(|error| WatchError::Backend(error.to_string()))?;
    Ok(SchemaLocation::External {
        name,
        parent,
        normalizer: schema_normalizer,
    })
}

#[derive(Default)]
struct Ledger {
    entries: BTreeMap<NormalizedPath, LedgerEntry>,
}
struct LedgerEntry {
    expected: Expected,
    recorded: Instant,
}
#[derive(Clone, Copy)]
enum Expected {
    Present(PostState),
    Absent,
}

struct Pending {
    batch: Batch,
    first: Instant,
    last: Instant,
}
struct State {
    root: PathBuf,
    normalizer: PathNormalizer,
    exclusions: Exclusions,
    schema: SchemaLocation,
    ledger: Arc<Mutex<Ledger>>,
    pending: Option<Pending>,
    terminal: Option<WatchError>,
    healing: bool,
}

impl State {
    fn new(
        root: PathBuf,
        normalizer: PathNormalizer,
        schema: SchemaLocation,
        ledger: Arc<Mutex<Ledger>>,
    ) -> Self {
        let exclusions =
            Exclusions::new(&normalizer, &[]).expect("an empty root list refuses nothing");
        Self {
            root,
            normalizer,
            exclusions,
            schema,
            ledger,
            pending: None,
            terminal: None,
            healing: false,
        }
    }
    fn batch(&mut self) -> &mut Batch {
        let now = Instant::now();
        let pending = self.pending.get_or_insert_with(|| Pending {
            batch: Batch::default(),
            first: now,
            last: now,
        });
        pending.last = now;
        &mut pending.batch
    }
    fn rescan(&mut self, scope: RescanScope) {
        let batch = self.batch();
        batch.rescans.insert(scope);
        if scope == RescanScope::Vault {
            batch.vault_roots.clear();
        }
    }
}

/// Whether a delivered event is an access, which is the one kind no batch
/// carries whatever path it names.
///
/// **Reading a covered path is a delivery on some backends.** inotify asks for
/// `IN_OPEN`, so every document the heal opens under live coverage arrives here
/// as an access; FSEvents reports no such thing. An access says nothing about
/// what any path holds, so no batch carries one — and the difference between
/// the backends is invisible above this crate for exactly that reason.
///
/// This is one half of what [`ingest`] discards and not the whole of it. What a
/// delivery *at a path* is worth is [`classify_path`]'s answer, and
/// [`folds_into_a_batch`] is where both halves are asked together.
fn is_an_access(event: &Event) -> bool {
    matches!(event.kind, EventKind::Access(_))
}

/// Whether the watcher folds `event` into a batch — [`ingest`]'s own rule,
/// asked without ingesting anything.
///
/// **The fault seam's stream arm asks it.** [`faults::StreamArm`] stands in
/// place of a delivery, upstream of ingest, and what it is stated over is a
/// delivery a consumer could act on: an arm spent on something ingest discards
/// would report a stream failure or an overflow over a delivery no consumer
/// ever hears about, and which deliveries those are differs per backend and per
/// tree. It is the same code rather than a second predicate because two
/// spellings of one rule drift, and the drift is invisible — it shows up only as
/// an armed case firing somewhere it was not stated over.
///
/// The lock is taken only where an arm is still owed: the caller checks what it
/// holds first, so an unarmed watch asks nothing here.
fn folds_into_a_batch(state: &Mutex<State>, event: &Event) -> bool {
    if is_an_access(event) {
        return false;
    }
    // A backend saying the path set is incomplete names no path and widens
    // every partition, which is the loudest thing a delivery can be.
    if event.need_rescan() || event.paths.is_empty() {
        return true;
    }
    let state = state.lock().expect("watch state poisoned");
    event
        .paths
        .iter()
        .any(|path| !classify_path(&state, event.kind, path).is_nothing())
}

/// Fold one backend delivery into shared state, reporting the terminal failure
/// the subscription now carries.
///
/// The report is the whole recorded cause rather than what this delivery
/// contributed: the first cause is the one kept, and a caller waiting on the
/// synchronization boundary needs it whichever delivery recorded it.
fn ingest(shared: &Arc<Mutex<State>>, result: notify::Result<Event>) -> Option<WatchError> {
    let mut state = shared.lock().expect("watch state poisoned");
    match result {
        Err(error) => {
            state.terminal.get_or_insert_with(|| backend(error));
        }
        Ok(event) if is_an_access(&event) => {}
        Ok(event) => {
            // A backend saying the path set is incomplete is the one report
            // that widens work to a rescan: an explicit rescan flag over
            // dropped events, or a delivery with no path to name at all.
            if event.need_rescan() || event.paths.is_empty() {
                state.rescan(RescanScope::Vault);
                state.rescan(RescanScope::Schema);
                return state.terminal.clone();
            }
            for path in event.paths {
                ingest_path(&mut state, event.kind, &path);
            }
        }
    }
    state.terminal.clone()
}

/// Whether a kind delivered at a watched directory's own path is a fact about
/// that directory entry and nothing below it.
///
/// The set is closed: the directory being created, a change to its metadata,
/// and the backend's unspecified modify. Every change to something inside the
/// directory arrives at the path that changed, so a kind in this set names no
/// dirty path at all. Access-only kinds never reach this predicate — both
/// callers of [`classify_path`] drop them before any path is examined.
///
/// Every other kind at that same path can replace what the directory holds
/// without one event per item — macOS reports a volume mounted over a watched
/// path as `Create(CreateKind::Other)`, which substitutes a whole tree — so
/// outside this set the delivery is a report that the path set below is no
/// longer known.
fn names_only_the_directory_entry(kind: EventKind) -> bool {
    matches!(
        kind,
        EventKind::Create(CreateKind::Folder)
            | EventKind::Modify(ModifyKind::Metadata(_))
            | EventKind::Modify(ModifyKind::Any)
    )
}

/// What one delivered path does to the watcher's account of the vault.
///
/// **Classifying is separate from applying because two questions ask it.**
/// [`ingest_path`] asks what to fold in; [`folds_into_a_batch`] asks whether
/// there is anything to fold in at all, on behalf of the fault seam's stream
/// arm, and it must not move any of the state the answer is read out of. One
/// classification answers both.
///
/// Nothing at all — every field at its default — is a path this watcher reports
/// nothing about: outside the vault and not its root's own name, outside the
/// schema, or refused by the vault's exclusions.
#[derive(Debug, Default, Eq, PartialEq)]
struct PathEffect {
    /// The path the coverage was installed over is not the entry's any more.
    coverage_lost: bool,
    /// The vault partition's path set is no longer known.
    vault_rescan: bool,
    /// The schema partition's path set is no longer known.
    schema_rescan: bool,
    /// The schema source may hold something else.
    schema_dirty: bool,
    /// The vault-relative path a scoped increment starts from.
    dirty_root: Option<NormalizedPath>,
}

impl PathEffect {
    /// Whether this delivery changes nothing the watcher reports.
    fn is_nothing(&self) -> bool {
        *self == PathEffect::default()
    }
}

fn classify_path(state: &State, kind: EventKind, path: &Path) -> PathEffect {
    let mut effect = PathEffect::default();
    if path == state.root {
        if kind.is_remove() || matches!(kind, EventKind::Modify(ModifyKind::Name(_))) {
            effect.coverage_lost = true;
            return effect;
        }
        // A kind naming only the root directory entry carries no dirty path,
        // and it is emphatically not a report that the path set is incomplete:
        // widening it would cost the full heal and publish watcher overflow
        // for an event that lost nothing. Anything else at the root can put a
        // different tree behind that name with no event per item, which is
        // exactly what [`RescanScope::Vault`] reports.
        effect.vault_rescan = !names_only_the_directory_entry(kind);
        return effect;
    }
    let root_name_relevant = state.root.parent().is_some_and(|parent| {
        path.parent() == Some(parent) && path.file_name() == state.root.file_name()
    });
    if root_name_relevant {
        effect.coverage_lost = true;
        return effect;
    }
    let (schema_dirty, schema_rescan) = match &state.schema {
        SchemaLocation::InVault(schema) => (
            path.strip_prefix(&state.root)
                .ok()
                .and_then(|p| state.normalizer.normalize(p).ok())
                // An event at or above the schema reaches it: the file itself,
                // or a directory whose replacement substitutes the file with
                // no event naming it. `NormalizedPath::starts_with` answers
                // both, on the vault's comparison key, so the configured
                // spelling and the reported one agree on case exactly as far
                // as the volume does.
                .is_some_and(|event| schema.starts_with(&event)),
            false,
        ),
        SchemaLocation::External {
            name,
            parent,
            normalizer,
        } => {
            if path == parent {
                // The schema's own parent directory is watched for one file,
                // so the same closed rule the vault root uses applies here: a
                // kind naming only that directory entry says nothing about the
                // schema source, and any other kind can replace what the
                // directory holds without naming the file.
                (false, !names_only_the_directory_entry(kind))
            } else {
                (
                    path.strip_prefix(parent)
                        .ok()
                        .and_then(|relative| normalizer.normalize(relative).ok())
                        .is_some_and(|event| event == *name),
                    false,
                )
            }
        }
    };
    effect.schema_rescan = schema_rescan;
    effect.schema_dirty = schema_dirty;

    let Ok(relative) = path.strip_prefix(&state.root) else {
        return effect;
    };
    let Ok(normalized) = state.normalizer.normalize(relative) else {
        effect.vault_rescan = true;
        return effect;
    };
    if state.exclusions.excludes(&normalized) {
        return effect;
    }
    effect.dirty_root = Some(normalized);
    effect
}

/// Fold one delivered path's effect into the batch this watcher is building.
fn ingest_path(state: &mut State, kind: EventKind, path: &Path) {
    let PathEffect {
        coverage_lost,
        vault_rescan,
        schema_rescan,
        schema_dirty,
        dirty_root,
    } = classify_path(state, kind, path);
    if coverage_lost {
        let root = state.root.clone();
        state.terminal.get_or_insert(WatchError::CoverageLost(root));
        return;
    }
    if schema_rescan {
        state.rescan(RescanScope::Schema);
    }
    if schema_dirty {
        state.batch().schema_dirty = true;
    }
    if vault_rescan {
        state.rescan(RescanScope::Vault);
    }
    let Some(normalized) = dirty_root else {
        return;
    };
    let batch = state.batch();
    if !batch.rescans.contains(&RescanScope::Vault) {
        batch.vault_roots.insert(normalized);
        if batch.vault_roots.len() >= DIRTY_ROOT_CAP {
            batch.vault_roots.clear();
            batch.rescans.insert(RescanScope::Vault);
        }
    }
}

fn run_coalescer(
    state: Arc<Mutex<State>>,
    wake: mpsc::Receiver<()>,
    output: mpsc::SyncSender<Result<Batch, WatchError>>,
) {
    loop {
        let wait = {
            let locked = state.lock().expect("watch state poisoned");
            if locked.healing {
                Duration::from_secs(3600)
            } else if locked.terminal.is_some() {
                Duration::ZERO
            } else if let Some(p) = &locked.pending {
                let at = std::cmp::min(p.last + QUIET_WINDOW, p.first + MAX_BATCH_AGE);
                at.saturating_duration_since(Instant::now())
            } else {
                Duration::from_secs(3600)
            }
        };
        // A ready wake must not outrank a hard deadline. Under continuous
        // churn the capacity-one channel may always be readable; receiving and
        // looping unconditionally would then postpone a batch forever despite
        // MAX_BATCH_AGE.
        if !wait.is_zero() {
            match wake.recv_timeout(wait) {
                Ok(()) => continue,
                Err(mpsc::RecvTimeoutError::Disconnected) => return,
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
        }
        let work = {
            let mut locked = state.lock().expect("watch state poisoned");
            if locked.healing {
                Ok(None)
            } else if let Some(error) = locked.terminal.take() {
                Err(error)
            } else {
                let root = locked.root.clone();
                let ledger = locked.ledger.clone();
                Ok(locked
                    .pending
                    .take()
                    .map(|pending| (root, ledger, pending.batch)))
            }
        };
        // Filesystem observation for suppression is deliberately outside both
        // watcher locks. A large own-written file must not block the backend
        // callback from recording newer events, and one slow hash must not
        // prevent write outcomes from entering the ledger.
        let item = match work {
            Err(error) => Some(Err(error)),
            Ok(Some((root, ledger, batch))) => Some(Ok(suppress(&root, &ledger, batch))),
            Ok(None) => None,
        };
        if let Some(item) = item {
            let terminal = item.is_err();
            if output.send(item).is_err() || terminal {
                return;
            }
        }
    }
}

fn suppress(root: &Path, ledger: &Arc<Mutex<Ledger>>, mut batch: Batch) -> Batch {
    let now = Instant::now();
    let candidates: BTreeMap<_, _> = {
        let mut ledger = ledger.lock().expect("own-write ledger poisoned");
        ledger
            .entries
            .retain(|_, entry| now.duration_since(entry.recorded) <= OWN_WRITE_TTL);
        batch
            .vault_roots
            .iter()
            .filter_map(|path| {
                ledger
                    .entries
                    .remove(path)
                    .map(|entry| (path.clone(), entry.expected))
            })
            .collect()
    };
    batch.vault_roots.retain(|path| {
        let Some(expected) = candidates.get(path) else {
            return true;
        };
        !matches_expected(&root.join(path.as_path()), expected)
    });
    batch
}

#[allow(clippy::disallowed_methods, clippy::disallowed_types)] // norn-fs owns vault handles and stat.
fn matches_expected(path: &Path, expected: &Expected) -> bool {
    match expected {
        Expected::Absent => match std::fs::symlink_metadata(path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => true,
            Ok(_) | Err(_) => false,
        },
        Expected::Present(expected) => {
            let Ok(mut file) = std::fs::OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
                .open(path)
            else {
                return false;
            };
            let Ok(metadata) = file.metadata() else {
                return false;
            };
            if !metadata.is_file() {
                return false;
            }
            if metadata.len() != expected.len || metadata.modified().ok() != Some(expected.mtime) {
                return false;
            }
            hashed_from(&mut file)
                .is_ok_and(|(hash, len)| hash == expected.content_hash && len == expected.len)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::faults::{Answer, Stage};
    use super::*;
    use notify::event::{
        AccessKind, AccessMode, CreateKind, DataChange, Flag, MetadataKind, ModifyKind, RemoveKind,
    };

    use crate::ContentHash;
    use crate::identity::post_state;
    use crate::path::CaseSensitivity;
    use crate::scratch::Scratch;

    fn normalizer() -> PathNormalizer {
        PathNormalizer::detect(Path::new(".")).expect("working directory has case evidence")
    }

    fn state() -> Arc<Mutex<State>> {
        let root = PathBuf::from("/vault");
        let normalizer = normalizer();
        Arc::new(Mutex::new(State::new(
            root,
            normalizer.clone(),
            SchemaLocation::InVault(normalizer.normalize(Path::new("schema.yml")).unwrap()),
            Arc::new(Mutex::new(Ledger::default())),
        )))
    }

    fn external_schema(parent: &str) -> SchemaLocation {
        let normalizer = normalizer();
        SchemaLocation::External {
            name: normalizer.normalize(Path::new("schema.yml")).unwrap(),
            parent: PathBuf::from(parent),
            normalizer,
        }
    }

    fn state_with(schema: SchemaLocation) -> Arc<Mutex<State>> {
        Arc::new(Mutex::new(State::new(
            PathBuf::from("/vault"),
            normalizer(),
            schema,
            Arc::new(Mutex::new(Ledger::default())),
        )))
    }

    /// A state whose in-vault schema is `schema`, on a vault with stated case
    /// behavior, so a case names the spellings and the behavior judging them
    /// rather than inheriting the volume this suite happens to run on.
    fn state_with_in_vault_schema(sensitivity: CaseSensitivity, schema: &str) -> Arc<Mutex<State>> {
        let normalizer = PathNormalizer::for_sensitivity(sensitivity);
        Arc::new(Mutex::new(State::new(
            PathBuf::from("/vault"),
            normalizer.clone(),
            SchemaLocation::InVault(normalizer.normalize(Path::new(schema)).unwrap()),
            Arc::new(Mutex::new(Ledger::default())),
        )))
    }

    /// Every kind that names only a watched directory entry, and one of each
    /// shape that does not.
    ///
    /// The second list is what keeps the first closed: a kind moved from one
    /// list to the other fails whichever case reads it, so widening the
    /// benign set cannot pass silently.
    const NAMES_ONLY_THE_DIRECTORY: &[EventKind] = &[
        EventKind::Create(CreateKind::Folder),
        EventKind::Modify(ModifyKind::Metadata(MetadataKind::Any)),
        EventKind::Modify(ModifyKind::Metadata(MetadataKind::Permissions)),
        EventKind::Modify(ModifyKind::Any),
    ];

    /// Kinds at a watched directory's own path that can put different content
    /// behind it with no event per item. `Create(CreateKind::Other)` is the
    /// one macOS reports for a volume mounted over the watched path.
    const REPLACES_WHAT_THE_DIRECTORY_HOLDS: &[EventKind] = &[
        EventKind::Create(CreateKind::Other),
        EventKind::Create(CreateKind::Any),
        EventKind::Create(CreateKind::File),
        EventKind::Modify(ModifyKind::Data(DataChange::Any)),
        EventKind::Modify(ModifyKind::Other),
        EventKind::Any,
        EventKind::Other,
    ];

    #[test]
    fn the_plan_covers_the_vault_tree_and_the_name_the_root_occupies() {
        let normalizer = normalizer();
        let in_vault =
            SchemaLocation::InVault(normalizer.normalize(Path::new("schema.yml")).unwrap());
        assert_eq!(
            coverage_plan(Path::new("/vaults/notes"), &in_vault).unwrap(),
            [
                (PathBuf::from("/vaults/notes"), RecursiveMode::Recursive),
                (PathBuf::from("/vaults"), RecursiveMode::NonRecursive),
            ]
        );
    }

    #[test]
    fn a_rootless_vault_path_is_refused_before_any_backend_exists() {
        let normalizer = normalizer();
        let in_vault =
            SchemaLocation::InVault(normalizer.normalize(Path::new("schema.yml")).unwrap());
        assert!(matches!(
            coverage_plan(Path::new("/"), &in_vault),
            Err(WatchError::Backend(_))
        ));
    }

    #[test]
    fn an_external_schema_adds_the_one_edge_the_vault_plan_does_not_reach() {
        assert_eq!(
            coverage_plan(
                Path::new("/vaults/notes"),
                &external_schema("/etc/norn/schemas")
            )
            .unwrap(),
            [
                (PathBuf::from("/vaults/notes"), RecursiveMode::Recursive),
                (PathBuf::from("/vaults"), RecursiveMode::NonRecursive),
                (
                    PathBuf::from("/etc/norn/schemas"),
                    RecursiveMode::NonRecursive
                ),
            ]
        );
    }

    #[test]
    fn an_external_schema_beside_the_vault_root_adds_no_edge() {
        assert_eq!(
            coverage_plan(Path::new("/vaults/notes"), &external_schema("/vaults")).unwrap(),
            [
                (PathBuf::from("/vaults/notes"), RecursiveMode::Recursive),
                (PathBuf::from("/vaults"), RecursiveMode::NonRecursive),
            ]
        );
    }

    #[test]
    #[allow(clippy::disallowed_methods)] // Test arrangement inside Scratch-owned paths.
    fn polling_registration_publishes_live_only_after_the_complete_plan_scan() {
        let scratch = Scratch::new("polling-synchronization");
        let vault = scratch.path("vault");
        let schema_parent = scratch.path("schema");
        std::fs::create_dir_all(&vault).unwrap();
        std::fs::create_dir_all(&schema_parent).unwrap();
        let schema = schema_parent.join("vault.yml");
        std::fs::write(&schema, "version: 1\n").unwrap();

        let (subscription, _) = watch_polling(&vault, &schema).unwrap();

        assert_eq!(subscription.state(), SubscriptionState::Live);
        assert_eq!(subscription.synchronize(Duration::ZERO), Ok(()));
    }

    #[test]
    #[allow(clippy::disallowed_methods)] // Test arrangement inside Scratch-owned paths.
    fn the_heal_window_returns_unsettled_facts_before_ready_can_be_published() {
        let scratch = Scratch::new("heal-window");
        let vault = scratch.path("vault");
        std::fs::create_dir_all(&vault).unwrap();
        let schema = vault.join("schema.yml");
        std::fs::write(&schema, "version: 1\n").unwrap();
        let (subscription, _) = watch_polling(&vault, &schema).unwrap();
        subscription.begin_heal();
        let observed_root = subscription.state.lock().unwrap().root.clone();

        ingest(
            &subscription.state,
            Ok(Event::new(EventKind::Modify(ModifyKind::Any))
                .add_path(observed_root.join("during.md"))),
        );

        let observed = subscription.finish_heal().unwrap();
        assert_eq!(
            observed
                .vault_roots()
                .iter()
                .map(|path| path.as_path())
                .collect::<Vec<_>>(),
            [Path::new("during.md")]
        );
    }

    #[test]
    #[allow(clippy::disallowed_methods)] // Test arrangement inside Scratch-owned paths.
    fn the_heal_window_surfaces_terminal_loss_before_any_ready_handoff() {
        let scratch = Scratch::new("heal-terminal");
        let vault = scratch.path("vault");
        std::fs::create_dir_all(&vault).unwrap();
        let schema = vault.join("schema.yml");
        std::fs::write(&schema, "version: 1\n").unwrap();
        let (subscription, _) = watch_polling(&vault, &schema).unwrap();
        subscription.begin_heal();
        subscription.state.lock().unwrap().terminal =
            Some(WatchError::Backend("lost during heal".into()));

        assert_eq!(
            subscription.finish_heal(),
            Err(WatchError::Backend("lost during heal".into()))
        );
    }

    /// **The bar under the native barrier.** Returning from registration is
    /// not the synchronization boundary on macOS.
    ///
    /// The forbidden shape is publishing `Live` once the coverage plan is
    /// installed, the way polling may. The FSEvents stream starts after it is
    /// created, so a subscription that called itself live there would be
    /// claiming observation of a window nothing was watching.
    #[cfg(target_os = "macos")]
    #[test]
    fn native_registration_is_not_the_synchronization_boundary() {
        assert!(
            !registration_is_the_boundary(false),
            "native macOS registration was treated as the synchronization boundary, which \
             publishes Live over the window before the stream starts"
        );
        assert!(registration_is_the_boundary(true));
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn registration_establishes_every_edge_on_this_platform() {
        assert!(registration_is_the_boundary(false));
        assert!(registration_is_the_boundary(true));
    }

    /// **One registration never switches from native watching to polling.**
    /// A platform whose recommended backend is the polling fallback refuses.
    ///
    /// The forbidden shape is building that fallback here. It would give a
    /// caller of [`watch`] the poll interval's latency and the poll scan's
    /// cost under a subscription reporting itself live, and it would hide the
    /// absence of a native backend behind coverage that still looks correct.
    /// Polling is reached through [`watch_polling`] and nowhere else.
    #[cfg(not(target_os = "macos"))]
    #[test]
    fn a_recommended_backend_that_is_polling_refuses_a_native_registration() {
        assert!(matches!(
            native_kind_refusal(notify::WatcherKind::PollWatcher),
            Some(WatchError::Backend(_))
        ));
        for kind in [
            notify::WatcherKind::Inotify,
            notify::WatcherKind::Kqueue,
            notify::WatcherKind::Fsevent,
            notify::WatcherKind::ReadDirectoryChangesWatcher,
        ] {
            assert!(native_kind_refusal(kind).is_none(), "{kind:?}");
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn only_the_history_marker_publishes_live_for_the_native_backend() {
        let control = Arc::new((Mutex::new(SubscriptionState::Synchronizing), Condvar::new()));
        let marker = history_barrier(&control, &WatchFaults::default(), Boundary::default());

        assert_eq!(
            *control.0.lock().unwrap(),
            SubscriptionState::Synchronizing,
            "coverage was live before the event-history replay reported its boundary"
        );
        marker();
        assert_eq!(*control.0.lock().unwrap(), SubscriptionState::Live);
    }

    /// **An armed barrier withholds the publication on this platform path
    /// too.** The native backend publishes `Live` from the event-history
    /// marker and nowhere else, so a marker that published anyway would leave
    /// the arm meaning one thing on macOS and another everywhere else.
    #[cfg(target_os = "macos")]
    #[test]
    fn an_armed_barrier_withholds_the_native_history_marker() {
        let control = Arc::new((Mutex::new(SubscriptionState::Synchronizing), Condvar::new()));
        let boundary = Boundary::default();
        let marker = history_barrier(
            &control,
            &WatchFaults::at(&[(Stage::Barrier, Answer::Expires)]),
            boundary.clone(),
        );

        marker();

        assert_eq!(
            *control.0.lock().unwrap(),
            SubscriptionState::Synchronizing,
            "an armed barrier published the boundary it withholds"
        );
        assert!(
            boundary.was_reached(),
            "a withheld publication left the boundary unreached, which takes the \
             stream stage away from a watch whose barrier is armed"
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn expiry_during_the_history_replay_survives_a_late_marker() {
        let control = Arc::new((Mutex::new(SubscriptionState::Synchronizing), Condvar::new()));
        let marker = history_barrier(&control, &WatchFaults::default(), Boundary::default());

        assert_eq!(
            wait_for_synchronization(&control, Duration::ZERO),
            Err(WatchError::SynchronizationExpired)
        );
        marker();

        assert_eq!(
            *control.0.lock().unwrap(),
            SubscriptionState::Terminal(WatchError::SynchronizationExpired)
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn a_reading_that_is_not_a_boundary_refuses_native_watching() {
        for reading in [0, u64::MAX] {
            assert!(
                matches!(history_boundary(reading), Err(WatchError::Backend(_))),
                "reading {reading} was accepted as an event-history boundary"
            );
        }
        assert_eq!(history_boundary(1), Ok(1));
        assert!(notify::fsevent::current_event_id() > 0);
    }

    /// A failure recorded while history is replaying reaches the caller
    /// waiting on the boundary, and the marker behind it cannot overwrite it.
    #[test]
    fn a_terminal_during_replay_is_published_and_preserved() {
        let state = state();
        let control = Arc::new((Mutex::new(SubscriptionState::Synchronizing), Condvar::new()));

        let reported = ingest(&state, Err(notify::Error::generic("lost while replaying")))
            .expect("a delivery that recorded a terminal failure reports it");
        publish_control(&control, SubscriptionState::Terminal(reported.clone()));

        assert_eq!(
            wait_for_synchronization(&control, Duration::from_secs(3600)),
            Err(reported.clone())
        );
        publish_control(&control, SubscriptionState::Live);
        assert_eq!(
            *control.0.lock().unwrap(),
            SubscriptionState::Terminal(reported)
        );
    }

    #[test]
    fn a_delivery_that_records_nothing_terminal_reports_nothing() {
        let state = state();
        assert_eq!(
            ingest(
                &state,
                Ok(Event::new(EventKind::Modify(ModifyKind::Any))
                    .add_path("/vault/note.md".into()))
            ),
            None
        );
    }

    /// **The relevance rule the fault seam asks is ingest's own answer.** Every
    /// delivery below is put to both: [`folds_into_a_batch`] predicts, and then
    /// ingest itself runs over a state of its own and is asked whether anything
    /// moved. The two must agree on each of them.
    ///
    /// The forbidden shapes are both directions of drift. A rule answering yes
    /// where ingest discards spends an armed answer on a delivery no consumer
    /// ever hears about — which is what a directory created beside the vault
    /// root is, since coverage takes in the vault's own parent. A rule answering
    /// no where ingest folds in leaves the arm owed past the change a case
    /// arranged, and the case then waits for a transition nothing produces.
    #[test]
    fn the_relevance_rule_answers_what_ingest_does() {
        let deliveries: [(&str, Event); 8] = [
            (
                "a document in the vault",
                Event::new(EventKind::Modify(ModifyKind::Data(DataChange::Any)))
                    .add_path("/vault/one.md".into()),
            ),
            (
                "the schema source in the vault",
                Event::new(EventKind::Modify(ModifyKind::Data(DataChange::Any)))
                    .add_path("/vault/schema.yml".into()),
            ),
            (
                "a directory created beside the vault root",
                Event::new(EventKind::Create(CreateKind::Folder)).add_path("/data".into()),
            ),
            (
                "the vault root's own name, taken away",
                Event::new(EventKind::Remove(RemoveKind::Folder)).add_path("/vault".into()),
            ),
            (
                "an access under the vault",
                Event::new(EventKind::Access(AccessKind::Open(AccessMode::Read)))
                    .add_path("/vault/one.md".into()),
            ),
            (
                "a path under no watched edge at all",
                Event::new(EventKind::Modify(ModifyKind::Data(DataChange::Any)))
                    .add_path("/elsewhere/one.md".into()),
            ),
            (
                "a backend reporting its path set lost",
                Event::new(EventKind::Other).set_flag(Flag::Rescan),
            ),
            ("a delivery naming no path", Event::new(EventKind::Other)),
        ];

        for (label, event) in deliveries {
            let state = state();
            let predicted = folds_into_a_batch(&state, &event);
            let moved = {
                ingest(&state, Ok(event.clone()));
                let state = state.lock().unwrap();
                state.pending.is_some() || state.terminal.is_some()
            };
            assert_eq!(
                predicted, moved,
                "{label}: the relevance rule and ingest disagree"
            );
        }
    }

    #[test]
    fn synchronization_expiry_is_terminal_and_a_late_live_signal_cannot_revive_it() {
        let control = Arc::new((Mutex::new(SubscriptionState::Synchronizing), Condvar::new()));

        assert_eq!(
            wait_for_synchronization(&control, Duration::ZERO),
            Err(WatchError::SynchronizationExpired)
        );
        publish_control(&control, SubscriptionState::Live);
        assert_eq!(
            *control.0.lock().unwrap(),
            SubscriptionState::Terminal(WatchError::SynchronizationExpired)
        );
    }

    /// **Expiry is the subscription's fact, not the waiting caller's.** A
    /// heal that runs past an expired boundary finishes with the failure.
    ///
    /// The forbidden shape is a successful [`Subscription::finish_heal`] here.
    /// A caller that dropped the error from [`Subscription::synchronize`]
    /// would otherwise take a clean batch out of the heal window and publish
    /// readiness on coverage that never proved itself.
    #[test]
    #[allow(clippy::disallowed_methods)] // Test arrangement inside Scratch-owned paths.
    fn an_expired_boundary_is_terminal_for_the_batch_stream_too() {
        let scratch = Scratch::new("synchronization-expiry");
        let vault = scratch.path("vault");
        std::fs::create_dir_all(&vault).unwrap();
        let schema = vault.join("schema.yml");
        std::fs::write(&schema, "version: 1\n").unwrap();
        let (subscription, _) = watch_polling(&vault, &schema).unwrap();
        // The heal window is where the hazard lives, and it is also what
        // holds the recorded failure still: the coalescer takes no terminal
        // while a heal is open, so `finish_heal` is the one place it surfaces.
        subscription.begin_heal();
        *subscription.control.0.lock().unwrap() = SubscriptionState::Synchronizing;

        assert_eq!(
            subscription.synchronize(Duration::ZERO),
            Err(WatchError::SynchronizationExpired)
        );

        assert_eq!(
            subscription.state.lock().unwrap().terminal,
            Some(WatchError::SynchronizationExpired)
        );
        assert_eq!(
            subscription.finish_heal(),
            Err(WatchError::SynchronizationExpired)
        );
    }

    /// The stream half of the same contract: with no heal window open, the
    /// recorded expiry reaches the batch receiver as its own refusal.
    #[test]
    #[allow(clippy::disallowed_methods)] // Test arrangement inside Scratch-owned paths.
    fn an_expired_boundary_reaches_the_batch_stream_without_a_heal() {
        let scratch = Scratch::new("synchronization-expiry-stream");
        let vault = scratch.path("vault");
        std::fs::create_dir_all(&vault).unwrap();
        let schema = vault.join("schema.yml");
        std::fs::write(&schema, "version: 1\n").unwrap();
        let (subscription, _) = watch_polling(&vault, &schema).unwrap();
        *subscription.control.0.lock().unwrap() = SubscriptionState::Synchronizing;

        assert_eq!(
            subscription.synchronize(Duration::ZERO),
            Err(WatchError::SynchronizationExpired)
        );

        norn_testkit::wait::wait_until(
            "the batch stream to refuse with the recorded expiry",
            norn_testkit::wait::Budget::new(Duration::from_secs(5), Duration::from_secs(1)),
            || match subscription.try_recv() {
                Err(WatchError::SynchronizationExpired) => norn_testkit::wait::Observed::Met(()),
                other => norn_testkit::wait::Observed::Pending(format!("{other:?}")),
            },
        )
        .unwrap();
    }

    // -----------------------------------------------------------------------
    // The watcher fault seam, at the boundaries it is widened at
    // -----------------------------------------------------------------------

    /// A vault, its in-vault schema, and the file a fired arm records itself
    /// in.
    ///
    /// The record file sits in a directory of its own, outside every edge the
    /// coverage plan installs. Writing it is a filesystem change like any
    /// other, so a record file inside the vault — or directly inside the
    /// vault's parent, which is watched too — would put the seam's own writes
    /// into the batches a case is asserting over, and could be the very
    /// delivery a stream arm answers.
    #[allow(clippy::disallowed_methods)] // Test arrangement inside Scratch-owned paths.
    fn armed_tree(label: &str) -> (Scratch, PathBuf, PathBuf, PathBuf) {
        let scratch = Scratch::new(label);
        let vault = scratch.path("vault");
        let schema = vault.join("schema.yml");
        std::fs::write(&schema, "version: 1\n").expect("a schema source");
        let hits = scratch.directory("records").join("arm-hits");
        (scratch, vault, schema, hits)
    }

    /// Everything the arm recorded, in the order it fired.
    #[allow(clippy::disallowed_methods)] // Test observation of the arm's own record file.
    fn recorded(hits: &Path) -> String {
        std::fs::read_to_string(hits).unwrap_or_else(|error| {
            panic!("the arm recorded nothing in {}: {error}", hits.display())
        })
    }

    /// A watcher case's own wait: the poll interval is a quarter second, so
    /// what a case here waits for is several of them and not a machine-sized
    /// number.
    fn watch_budget() -> norn_testkit::wait::Budget {
        norn_testkit::wait::Budget::new(Duration::from_secs(15), Duration::from_millis(250))
    }

    /// **An armed install refuses establishment the way a platform refusal
    /// does.** No subscription is returned, and the refusal is the typed
    /// backend error a refused registration carries.
    #[test]
    fn an_armed_install_refuses_the_watch_and_records_the_boundary() {
        let (_scratch, vault, schema, hits) = armed_tree("watch-install-armed");

        let established = establish(
            &vault,
            &schema,
            true,
            WatchFaults::recording_at(&[(Stage::Install, Answer::Refuses)], hits.clone()),
        );

        let Err(refusal) = established else {
            panic!("an armed registration handed back a subscription");
        };
        assert!(matches!(refusal, WatchError::Backend(_)), "{refusal:?}");
        assert_eq!(
            recorded(&hits),
            "seam=norn-fs/watch stage=install answer=refuses\n"
        );
    }

    /// **An armed barrier is a boundary that never arrives.** The subscription
    /// stays synchronizing, the wait for it ends in the typed expiry without
    /// the caller's authored deadline elapsing, and the expiry is the
    /// subscription's own fact rather than the waiting caller's.
    #[test]
    #[allow(clippy::disallowed_methods)] // Test arrangement inside Scratch-owned paths.
    fn an_armed_barrier_expires_without_consuming_the_authored_deadline() {
        let (_scratch, vault, schema, hits) = armed_tree("watch-barrier-armed");
        let (subscription, _) = establish(
            &vault,
            &schema,
            true,
            WatchFaults::recording_at(&[(Stage::Barrier, Answer::Expires)], hits.clone()),
        )
        .expect("coverage installs");

        assert_eq!(
            subscription.state(),
            SubscriptionState::Synchronizing,
            "an armed barrier published the boundary it withholds"
        );
        let authored = Duration::from_secs(3600);
        let began = Instant::now();
        assert_eq!(
            subscription.synchronize(authored),
            Err(WatchError::SynchronizationExpired)
        );
        assert!(
            began.elapsed() < Duration::from_secs(1),
            "the wait spent the caller's authored deadline rather than reaching expiry"
        );
        assert_eq!(
            subscription.state(),
            SubscriptionState::Terminal(WatchError::SynchronizationExpired)
        );

        norn_testkit::wait::wait_until(
            "the batch stream to refuse with the recorded expiry",
            watch_budget(),
            || match subscription.try_recv() {
                Err(WatchError::SynchronizationExpired) => norn_testkit::wait::Observed::Met(()),
                other => norn_testkit::wait::Observed::Pending(format!("{other:?}")),
            },
        )
        .unwrap_or_else(|failure| panic!("{failure}"));
        assert_eq!(
            recorded(&hits),
            "seam=norn-fs/watch stage=barrier answer=expires\n"
        );
    }

    /// A subscription past its synchronization boundary, over `poll`'s
    /// backend, with whatever that backend needs held beside it.
    ///
    /// A native backend is one subscription to a service the machine runs once
    /// for everything on it, and that service answers a crowd by going silent
    /// rather than by going slow — so a case that installs one holds the
    /// workspace's real-watcher lease for as long as its subscription lives.
    /// Polling is this process's own thread and contends with nothing.
    ///
    /// Waiting for the boundary is the point rather than tidiness: the stream
    /// stage is stated over a live subscription, and its arm answers nothing
    /// until the backend has crossed the boundary its own establishment
    /// crosses — event-history replay on macOS, bulk registration elsewhere.
    fn established_past_the_boundary(
        vault: &Path,
        schema: &Path,
        poll: bool,
        faults: WatchFaults,
    ) -> (Subscription, Option<norn_testkit::isolation::Lease>) {
        let lease = (!poll).then(|| {
            norn_testkit::isolation::Lease::hold(
                norn_testkit::isolation::REAL_WATCHER,
                norn_testkit::isolation::acquisition_budget(watch_budget()),
            )
        });
        let (subscription, _) = establish(vault, schema, poll, faults).expect("coverage installs");
        subscription
            .synchronize(watch_budget().work())
            .expect("the backend's own synchronization boundary");
        (subscription, lease)
    }

    /// Take up the coverage the way a consumer does: open a heal window over
    /// it and close it again.
    ///
    /// **This is the other half of what makes a stream arm eligible.** A
    /// consumer heals the tree under the watch it just installed and takes
    /// everything the backend reported across that window back as the heal's
    /// own batch, so the first delivery it meets as a change is the one after
    /// the window closes. Nothing is read out of the heal here: what the cases
    /// below are about is what happens *after* it.
    fn taken_up_by_a_heal(subscription: &Subscription) {
        subscription.begin_heal();
        subscription
            .finish_heal()
            .expect("the heal window closes over live coverage");
    }

    /// The two backends a stream case runs over, and what a failure calls each.
    const BACKENDS: [(&str, bool); 2] = [("polling", true), ("native", false)];

    /// **Establishing a watch, and healing under it, are not deliveries an
    /// armed stream answers.** The tree is written immediately before coverage
    /// is installed, which is what a native backend replays into the handler
    /// while it establishes itself, and a consumer's first heal window closes
    /// over whatever else arrived meanwhile. The arm is still owed after both:
    /// the subscription reaches its boundary, publishes `Live`, takes up a
    /// heal, and has recorded nothing.
    ///
    /// The forbidden shape is an arm answering there. It would refuse the
    /// attach — establishment failing, not a live subscription failing — while
    /// writing the same record a legitimate firing writes, so a case reading
    /// that record would pass over the wrong condition entirely.
    #[test]
    #[allow(clippy::disallowed_methods)] // Test observation of the arm's own record file.
    fn an_armed_stream_answers_nothing_while_the_backend_establishes_itself() {
        for (label, poll) in BACKENDS {
            let (_scratch, vault, schema, hits) =
                armed_tree(&format!("watch-stream-establishing-{label}"));
            let (subscription, _lease) = established_past_the_boundary(
                &vault,
                &schema,
                poll,
                WatchFaults::recording_at(&[(Stage::Stream, Answer::Fails)], hits.clone()),
            );

            assert_eq!(subscription.state(), SubscriptionState::Live, "{label}");
            taken_up_by_a_heal(&subscription);
            assert!(
                std::fs::metadata(&hits).is_err(),
                "{label}: the stream arm answered a delivery from before a consumer met one"
            );
        }
    }

    /// **A delivery the watcher folds into no batch never spends the arm.**
    /// Coverage over a vault takes in the vault's own parent, so a directory
    /// created beside the tree is delivered to the handler and then discarded
    /// by ingest: no batch carries it and no consumer hears about it. The arm
    /// stands past it and is spent on the change to the vault that follows.
    ///
    /// **The forbidden shape is read off what the batches name.** An arm spent
    /// on the sibling writes the same record a legitimate firing writes, so the
    /// record alone cannot tell the two apart — but the vault's own change
    /// would then go on to be reported by path, which is exactly what a
    /// displaced delivery is not. So the case reads both: the rescan arrives,
    /// and nothing ever names the document whose delivery the arm was supposed
    /// to stand in place of.
    ///
    /// The settle before the vault is touched is ordering rather than proof: it
    /// puts the sibling's delivery at the handler first, so an arm that answers
    /// on paths it should discard answers on that one.
    #[test]
    #[allow(clippy::disallowed_methods)] // Test arrangement inside Scratch-owned paths.
    fn an_armed_stream_answers_nothing_for_a_delivery_that_reaches_no_batch() {
        for (label, poll) in BACKENDS {
            let (scratch, vault, schema, hits) =
                armed_tree(&format!("watch-stream-discarded-{label}"));
            let (subscription, _lease) = established_past_the_boundary(
                &vault,
                &schema,
                poll,
                WatchFaults::recording_at(&[(Stage::Stream, Answer::Rescans)], hits.clone()),
            );
            taken_up_by_a_heal(&subscription);

            std::fs::create_dir(scratch.path("beside-the-vault"))
                .expect("a directory beside the tree, under the parent edge");
            std::thread::sleep(DELIVERED_BY_NOW);
            std::fs::write(vault.join("one.md"), b"one\n")
                .expect("a real change under a real watch");

            let widened = norn_testkit::wait::wait_until(
                "a batch carrying the rescan the backend reports",
                watch_budget(),
                || match subscription.try_recv() {
                    Ok(Some(batch)) if !batch.rescans().is_empty() => {
                        norn_testkit::wait::Observed::Met(batch)
                    }
                    other => norn_testkit::wait::Observed::Pending(format!("{other:?}")),
                },
            )
            .unwrap_or_else(|failure| panic!("{label}: {failure}"));
            assert!(widened.vault_roots().is_empty(), "{label}");

            // The arm stood in place of the document's delivery, so nothing
            // reports the document by path. A batch that names it is an arm
            // that was spent on the sibling directory instead.
            std::thread::sleep(DELIVERED_BY_NOW);
            while let Ok(Some(batch)) = subscription.try_recv() {
                assert!(
                    !batch
                        .vault_roots()
                        .iter()
                        .any(|root| root.as_path() == Path::new("one.md")),
                    "{label}: the arm was spent on a delivery the watcher folds into no batch, so \
                     the vault's own change was reported by path"
                );
            }
            assert_eq!(
                recorded(&hits),
                "seam=norn-fs/watch stage=stream answer=rescans\n",
                "{label}"
            );
        }
    }

    /// **A relevant delivery inside the first heal window leaves the arm
    /// owed.** A consumer takes up coverage by healing under it, and what the
    /// backend reports across that window comes back as the heal's own batch —
    /// so the arm waits for the window to close and is spent on the first
    /// change the consumer meets afterwards.
    ///
    /// The forbidden shape is the arm answering inside the window. The heal
    /// absorbs whatever it answered with, and the case that armed it then waits
    /// for a transition that already happened where nobody was looking.
    #[test]
    #[allow(clippy::disallowed_methods)] // Test arrangement inside Scratch-owned paths.
    fn an_armed_stream_answers_nothing_inside_the_first_heal_window() {
        for (label, poll) in BACKENDS {
            let (_scratch, vault, schema, hits) =
                armed_tree(&format!("watch-stream-healing-{label}"));
            let (subscription, _lease) = established_past_the_boundary(
                &vault,
                &schema,
                poll,
                WatchFaults::recording_at(&[(Stage::Stream, Answer::Rescans)], hits.clone()),
            );

            subscription.begin_heal();
            std::fs::write(vault.join("healed.md"), b"healed\n")
                .expect("a change the heal window is open across");
            std::thread::sleep(DELIVERED_BY_NOW);
            let observed = subscription
                .finish_heal()
                .expect("the heal window closes over live coverage");
            assert!(
                observed.rescans().is_empty(),
                "{label}: the arm answered inside the heal window, so the heal absorbed it: \
                 {observed:?}"
            );
            assert!(
                std::fs::metadata(&hits).is_err(),
                "{label}: the stream arm answered a delivery the heal window was open across"
            );

            // Past the window, the next change is the one the arm stands in
            // place of.
            std::fs::write(vault.join("one.md"), b"one\n")
                .expect("a real change under a real watch");
            norn_testkit::wait::wait_until(
                "a batch carrying the rescan the backend reports",
                watch_budget(),
                || match subscription.try_recv() {
                    Ok(Some(batch)) if !batch.rescans().is_empty() => {
                        norn_testkit::wait::Observed::Met(())
                    }
                    other => norn_testkit::wait::Observed::Pending(format!("{other:?}")),
                },
            )
            .unwrap_or_else(|failure| panic!("{label}: {failure}"));
            assert_eq!(
                recorded(&hits),
                "seam=norn-fs/watch stage=stream answer=rescans\n",
                "{label}"
            );
        }
    }

    /// How long a case waits before it says a delivery that was going to arrive
    /// has arrived.
    ///
    /// Four polling intervals, which is the slower of the two backends by a
    /// wide margin: a native stream is built with no latency at all. It orders
    /// two deliveries against each other rather than proving a negative — every
    /// case here reads its verdict off a positive signal that follows.
    const DELIVERED_BY_NOW: Duration = Duration::from_secs(1);

    /// **An armed stream failure is the last thing the subscription carries.**
    /// It stands in place of a delivery the backend really made, and what
    /// follows is what follows a backend that failed on its own: a terminal
    /// error, control state to match, and nothing that revives it.
    ///
    /// Both backends run it. The seam answers at one boundary for both, and
    /// the native one is where the eligibility rule earns its keep: a stream
    /// that replays event history at establishment delivers before the
    /// subscription is live, and those deliveries are not what this case is
    /// about.
    #[test]
    #[allow(clippy::disallowed_methods)] // Test arrangement inside Scratch-owned paths.
    fn an_armed_stream_failure_displaces_a_real_delivery_and_ends_the_stream() {
        for (label, poll) in BACKENDS {
            let (_scratch, vault, schema, hits) =
                armed_tree(&format!("watch-stream-fails-{label}"));
            let (subscription, _lease) = established_past_the_boundary(
                &vault,
                &schema,
                poll,
                WatchFaults::recording_at(&[(Stage::Stream, Answer::Fails)], hits.clone()),
            );
            taken_up_by_a_heal(&subscription);

            std::fs::write(vault.join("one.md"), b"one\n")
                .expect("a real change under a real watch");

            let error = norn_testkit::wait::wait_until(
                "the armed failure to reach the batch stream",
                watch_budget(),
                || match subscription.try_recv() {
                    Err(error) => norn_testkit::wait::Observed::Met(error),
                    Ok(batch) => norn_testkit::wait::Observed::Pending(format!("{batch:?}")),
                },
            )
            .unwrap_or_else(|failure| panic!("{label}: {failure}"));
            assert!(
                matches!(error, WatchError::Backend(_)),
                "{label}: {error:?}"
            );
            assert!(
                matches!(
                    subscription.state(),
                    SubscriptionState::Terminal(WatchError::Backend(_))
                ),
                "{label}: {:?}",
                subscription.state()
            );

            // A terminal fact is the last one: a later change reports nothing,
            // and the stream keeps refusing.
            std::fs::write(vault.join("two.md"), b"two\n").expect("a change after the failure");
            assert!(subscription.try_recv().is_err(), "{label}");
            assert_eq!(
                recorded(&hits),
                "seam=norn-fs/watch stage=stream answer=fails\n",
                "{label}"
            );
        }
    }

    /// **An armed rescan is the report a backend makes when it lost the path
    /// set.** The batch carrying it names both coverage partitions and no path
    /// at all — which is what a backend that dropped events knows — coverage
    /// stays installed, and the arm is spent: the change after it is reported
    /// by path.
    #[test]
    #[allow(clippy::disallowed_methods)] // Test arrangement inside Scratch-owned paths.
    fn an_armed_stream_rescan_widens_one_batch_and_leaves_coverage_installed() {
        for (label, poll) in BACKENDS {
            let (_scratch, vault, schema, hits) =
                armed_tree(&format!("watch-stream-rescans-{label}"));
            let (subscription, _lease) = established_past_the_boundary(
                &vault,
                &schema,
                poll,
                WatchFaults::recording_at(&[(Stage::Stream, Answer::Rescans)], hits.clone()),
            );
            taken_up_by_a_heal(&subscription);

            std::fs::write(vault.join("one.md"), b"one\n")
                .expect("a real change under a real watch");

            let widened = norn_testkit::wait::wait_until(
                "a batch carrying the rescan the backend reports",
                watch_budget(),
                || match subscription.try_recv() {
                    Ok(Some(batch)) if !batch.rescans().is_empty() => {
                        norn_testkit::wait::Observed::Met(batch)
                    }
                    other => norn_testkit::wait::Observed::Pending(format!("{other:?}")),
                },
            )
            .unwrap_or_else(|failure| panic!("{label}: {failure}"));
            assert_eq!(
                *widened.rescans(),
                BTreeSet::from([RescanScope::Vault, RescanScope::Schema]),
                "{label}: an overflow names no path, so every partition it covered is lost"
            );
            assert!(widened.vault_roots().is_empty(), "{label}");
            assert_eq!(subscription.state(), SubscriptionState::Live, "{label}");

            // One armed answer, one firing: the next change is the backend's
            // own report again, named by path.
            std::fs::write(vault.join("two.md"), b"two\n").expect("a change after the rescan");
            norn_testkit::wait::wait_until(
                "the change after the rescan, reported by path",
                watch_budget(),
                || match subscription.try_recv() {
                    Ok(Some(batch))
                        if batch
                            .vault_roots()
                            .iter()
                            .any(|root| root.as_path() == Path::new("two.md")) =>
                    {
                        norn_testkit::wait::Observed::Met(())
                    }
                    other => norn_testkit::wait::Observed::Pending(format!("{other:?}")),
                },
            )
            .unwrap_or_else(|failure| panic!("{label}: {failure}"));
            assert_eq!(
                recorded(&hits),
                "seam=norn-fs/watch stage=stream answer=rescans\n",
                "{label}"
            );
        }
    }

    #[test]
    fn host_side_merge_unions_every_kind_of_uncertainty() {
        let normalizer = normalizer();
        let mut pending = Batch::default();
        pending
            .vault_roots
            .insert(normalizer.normalize(Path::new("one.md")).unwrap());
        let mut next = Batch::default();
        next.vault_roots
            .insert(normalizer.normalize(Path::new("two.md")).unwrap());
        next.schema_dirty = true;
        next.rescans.insert(RescanScope::Schema);

        pending.merge(next);

        assert_eq!(pending.vault_roots.len(), 2);
        assert!(pending.schema_dirty);
        assert_eq!(pending.rescans, BTreeSet::from([RescanScope::Schema]));
        assert!(!pending.is_empty());
        assert!(Batch::default().is_empty());
    }

    fn own_write_harness(label: &str) -> (Scratch, Arc<Mutex<Ledger>>, OwnWrites) {
        let scratch = Scratch::new(label);
        let root = scratch.path("vault");
        let normalizer = PathNormalizer::detect(&root).expect("a vault normalizer");
        let ledger = Arc::new(Mutex::new(Ledger::default()));
        let recorder = OwnWrites {
            ledger: Arc::downgrade(&ledger),
            root,
            normalizer,
        };
        (scratch, ledger, recorder)
    }

    #[allow(clippy::disallowed_methods)] // Test observation of the file arranged by Scratch.
    fn observed(path: &Path) -> PostState {
        let bytes = std::fs::read(path).expect("test bytes");
        let metadata = std::fs::metadata(path).expect("test metadata");
        post_state(ContentHash::of(&bytes), bytes.len() as u64, &metadata)
    }

    fn dirty(recorder: &OwnWrites, paths: &[&str]) -> Batch {
        Batch {
            vault_roots: paths
                .iter()
                .map(|path| recorder.normalizer.normalize(Path::new(path)).unwrap())
                .collect(),
            ..Batch::default()
        }
    }

    #[test]
    fn synthetic_events_form_an_ordered_dirty_set_and_mark_schema() {
        let state = state();
        ingest(
            &state,
            Ok(Event::new(EventKind::Modify(ModifyKind::Any))
                .add_path("/vault/z.md".into())
                .add_path("/vault/a.md".into())
                .add_path("/vault/schema.yml".into())),
        );
        let mut locked = state.lock().unwrap();
        let batch = &locked.pending.as_mut().unwrap().batch;
        let paths: Vec<_> = batch.vault_roots.iter().map(|p| p.as_path()).collect();
        assert_eq!(
            paths,
            [
                Path::new("a.md"),
                Path::new("schema.yml"),
                Path::new("z.md")
            ]
        );
        assert!(batch.schema_dirty);
    }

    /// **An event above a nested in-vault schema reaches it.** A schema below
    /// the vault root has directories between the two, and an event at one of
    /// them can put a different file behind the schema's name without naming
    /// the file. Which spellings of that directory reach the schema is the
    /// vault's case behavior — the same behavior that decides whether an event
    /// names the schema outright — and a component short of a directory name
    /// reaches nothing.
    #[test]
    fn an_event_above_an_in_vault_schema_marks_it_on_the_vault_case_behavior() {
        for (sensitivity, spelling, reaches) in [
            (CaseSensitivity::Sensitive, "/vault/Notes", true),
            (CaseSensitivity::Sensitive, "/vault/Notes/schema.yml", true),
            (CaseSensitivity::Sensitive, "/vault/notes", false),
            (CaseSensitivity::Insensitive, "/vault/Notes", true),
            (CaseSensitivity::Insensitive, "/vault/notes", true),
            (
                CaseSensitivity::Insensitive,
                "/vault/notes/schema.yml",
                true,
            ),
            (CaseSensitivity::Insensitive, "/vault/notes-archive", false),
        ] {
            let state = state_with_in_vault_schema(sensitivity, "Notes/schema.yml");
            ingest(
                &state,
                Ok(Event::new(EventKind::Modify(ModifyKind::Any)).add_path(spelling.into())),
            );

            let mut locked = state.lock().unwrap();
            let marked = locked
                .pending
                .as_mut()
                .is_some_and(|pending| pending.batch.schema_dirty);
            assert_eq!(
                marked, reaches,
                "{sensitivity:?} vault, event at {spelling}"
            );
        }
    }

    #[test]
    fn pathless_relevant_event_widens_both_scopes() {
        let state = state();
        ingest(&state, Ok(Event::new(EventKind::Other)));
        let mut locked = state.lock().unwrap();
        assert_eq!(
            locked.pending.as_mut().unwrap().batch.rescans,
            BTreeSet::from([RescanScope::Vault, RescanScope::Schema])
        );
    }

    /// The vault root is the load-bearing row: inotify reports an open of a
    /// watched directory at the directory's own path, so a read of the root
    /// must record nothing — no dirty path, no rescan, no terminal.
    #[test]
    fn every_access_only_event_is_ignored() {
        for path in ["/vault/note.md", "/vault"] {
            for kind in [
                AccessKind::Any,
                AccessKind::Read,
                AccessKind::Open(AccessMode::Write),
                AccessKind::Close(AccessMode::Write),
                AccessKind::Other,
            ] {
                let state = state();
                ingest(
                    &state,
                    Ok(Event::new(EventKind::Access(kind)).add_path(path.into())),
                );
                let locked = state.lock().unwrap();
                assert!(locked.pending.is_none(), "{kind:?} at {path}");
                assert!(locked.terminal.is_none(), "{kind:?} at {path}");
            }
        }
    }

    #[test]
    fn a_saturated_wake_keeps_every_dirty_path_in_shared_state() {
        let state = state();
        let (wake, receiver) = mpsc::sync_channel(1);
        for path in ["/vault/first.md", "/vault/second.md"] {
            ingest(
                &state,
                Ok(Event::new(EventKind::Modify(ModifyKind::Any)).add_path(path.into())),
            );
            let _ = wake.try_send(());
        }
        assert!(matches!(
            wake.try_send(()),
            Err(mpsc::TrySendError::Full(()))
        ));
        assert_eq!(receiver.try_recv(), Ok(()));
        let mut locked = state.lock().unwrap();
        let paths: Vec<_> = locked
            .pending
            .as_mut()
            .unwrap()
            .batch
            .vault_roots
            .iter()
            .map(|path| path.as_path())
            .collect();
        assert_eq!(paths, [Path::new("first.md"), Path::new("second.md")]);
    }

    #[test]
    fn only_the_root_mechanism_directory_is_excluded() {
        let state = state();
        ingest(
            &state,
            Ok(Event::new(EventKind::Modify(ModifyKind::Any))
                .add_path("/vault/.norn/tmp/shadow".into())
                .add_path("/vault/notes/.norn/tmp/document.md".into())),
        );
        let mut locked = state.lock().unwrap();
        let paths: Vec<_> = locked
            .pending
            .as_mut()
            .unwrap()
            .batch
            .vault_roots
            .iter()
            .map(|p| p.as_path())
            .collect();
        assert_eq!(paths, [Path::new("notes/.norn/tmp/document.md")]);
    }

    /// An event naming only the vault root's own directory entry is a fact
    /// about that entry and nothing else.
    ///
    /// The forbidden shape is widening it to [`RescanScope::Vault`]. That
    /// widening costs a full tree walk and publishes watcher overflow — the
    /// report that the path set was lost — for an event that lost nothing. A
    /// fresh vault, whose root is created moments before coverage is
    /// installed, produces exactly this event on every attach.
    #[test]
    fn a_vault_root_event_naming_only_its_entry_widens_nothing() {
        for &kind in NAMES_ONLY_THE_DIRECTORY {
            let state = state();
            ingest(&state, Ok(Event::new(kind).add_path("/vault".into())));

            let mut locked = state.lock().unwrap();
            assert!(locked.terminal.is_none(), "{kind:?}");
            assert!(
                locked.pending.as_mut().is_none(),
                "an event on the vault root itself produced invalidation work: {kind:?}"
            );
        }
    }

    /// **The bar under the drop at the vault root.** The drop set is closed,
    /// and every other kind at the root still widens.
    ///
    /// The forbidden shape is silence for whatever kind is not one of the
    /// three named. `Create(CreateKind::Other)` is what macOS reports when a
    /// volume is mounted over the watched path: the whole tree is replaced,
    /// no per-item event describes it, and a subscription that recorded
    /// nothing would go on answering from the tree that is no longer there.
    #[test]
    fn a_vault_root_event_that_can_replace_the_tree_widens_to_a_vault_rescan() {
        for &kind in REPLACES_WHAT_THE_DIRECTORY_HOLDS {
            let state = state();
            ingest(&state, Ok(Event::new(kind).add_path("/vault".into())));

            let mut locked = state.lock().unwrap();
            assert!(locked.terminal.is_none(), "{kind:?}");
            let batch = &locked
                .pending
                .as_mut()
                .unwrap_or_else(|| {
                    panic!("an event that can substitute the whole vault tree recorded nothing: {kind:?}")
                })
                .batch;
            assert_eq!(
                batch.rescans,
                BTreeSet::from([RescanScope::Vault]),
                "{kind:?}"
            );
        }
    }

    /// The schema parent's edge covers one file, so an event naming only that
    /// directory entry carries nothing about the schema source.
    ///
    /// The forbidden shape is [`RescanScope::Schema`] for any event that
    /// happens to name the directory. That parent is often a directory the
    /// vault does not own — a mount point, a configuration directory — whose
    /// ordinary metadata churn would otherwise reload the schema on every
    /// touch.
    #[test]
    fn a_schema_parent_event_naming_only_its_entry_widens_nothing() {
        for &kind in NAMES_ONLY_THE_DIRECTORY {
            let state = state_with(external_schema("/etc/norn/schemas"));
            ingest(
                &state,
                Ok(Event::new(kind).add_path("/etc/norn/schemas".into())),
            );

            let mut locked = state.lock().unwrap();
            assert!(locked.terminal.is_none(), "{kind:?}");
            assert!(
                locked.pending.as_mut().is_none(),
                "an event on the schema parent itself produced invalidation work: {kind:?}"
            );
        }
    }

    /// **The bar under the drop at the schema parent.** Everything the drop
    /// does not name still reaches the schema.
    #[test]
    fn the_schema_parent_drop_reaches_neither_the_file_nor_the_other_kinds() {
        let state = state_with(external_schema("/etc/norn/schemas"));
        ingest(
            &state,
            Ok(
                Event::new(EventKind::Modify(ModifyKind::Metadata(MetadataKind::Any)))
                    .add_path("/etc/norn/schemas/schema.yml".into()),
            ),
        );
        assert!(
            state
                .lock()
                .unwrap()
                .pending
                .as_ref()
                .unwrap()
                .batch
                .schema_dirty,
            "an event at the schema file itself was dropped with the parent's noise"
        );

        for &kind in REPLACES_WHAT_THE_DIRECTORY_HOLDS {
            let state = state_with(external_schema("/etc/norn/schemas"));
            ingest(
                &state,
                Ok(Event::new(kind).add_path("/etc/norn/schemas".into())),
            );

            let mut locked = state.lock().unwrap();
            let batch = &locked
                .pending
                .as_mut()
                .unwrap_or_else(|| {
                    panic!("an event that can substitute the schema directory recorded nothing: {kind:?}")
                })
                .batch;
            assert_eq!(
                batch.rescans,
                BTreeSet::from([RescanScope::Schema]),
                "{kind:?}"
            );
        }
    }

    #[test]
    fn a_benign_vault_root_event_keeps_the_paths_already_named() {
        let state = state();
        ingest(
            &state,
            Ok(Event::new(EventKind::Modify(ModifyKind::Any)).add_path("/vault/note.md".into())),
        );
        ingest(
            &state,
            Ok(
                Event::new(EventKind::Modify(ModifyKind::Metadata(MetadataKind::Any)))
                    .add_path("/vault".into()),
            ),
        );

        let mut locked = state.lock().unwrap();
        let batch = &locked
            .pending
            .as_mut()
            .expect("the earlier path fact")
            .batch;
        assert_eq!(
            batch
                .vault_roots
                .iter()
                .map(|path| path.as_path())
                .collect::<Vec<_>>(),
            [Path::new("note.md")]
        );
        assert!(batch.rescans.is_empty());
    }

    /// The backend's own report that the path set is incomplete still widens,
    /// and takes the exact paths of its batch with it.
    #[test]
    fn a_backend_rescan_flag_widens_both_scopes_and_drops_exact_paths() {
        let state = state();
        ingest(
            &state,
            Ok(Event::new(EventKind::Modify(ModifyKind::Any)).add_path("/vault/before.md".into())),
        );
        ingest(
            &state,
            Ok(Event::new(EventKind::Other)
                .add_path("/vault/dropped.md".into())
                .set_flag(Flag::Rescan)),
        );
        ingest(
            &state,
            Ok(Event::new(EventKind::Modify(ModifyKind::Any)).add_path("/vault/after.md".into())),
        );

        let mut locked = state.lock().unwrap();
        let batch = &locked.pending.as_mut().unwrap().batch;
        assert_eq!(
            batch.rescans,
            BTreeSet::from([RescanScope::Vault, RescanScope::Schema])
        );
        assert!(
            batch.vault_roots.is_empty(),
            "a widened batch kept exact paths, which reads as a complete path set"
        );
    }

    #[test]
    fn root_lifecycle_event_is_terminal() {
        let state = state();
        ingest(
            &state,
            Ok(Event::new(EventKind::Remove(RemoveKind::Folder)).add_path("/vault".into())),
        );
        assert_eq!(
            state.lock().unwrap().terminal,
            Some(WatchError::CoverageLost(PathBuf::from("/vault")))
        );
    }

    #[test]
    fn the_first_terminal_cause_is_preserved() {
        let state = state();
        ingest(
            &state,
            Ok(Event::new(EventKind::Remove(RemoveKind::Folder)).add_path("/vault".into())),
        );
        ingest(&state, Err(notify::Error::generic("later backend noise")));

        assert_eq!(
            state.lock().unwrap().terminal,
            Some(WatchError::CoverageLost(PathBuf::from("/vault")))
        );
    }

    #[test]
    fn a_queued_wake_cannot_starve_the_maximum_batch_age() {
        let state = state();
        {
            let mut locked = state.lock().unwrap();
            let now = Instant::now();
            locked.pending = Some(Pending {
                batch: Batch {
                    vault_roots: BTreeSet::from([locked
                        .normalizer
                        .normalize(Path::new("changed.md"))
                        .unwrap()]),
                    ..Batch::default()
                },
                first: now - MAX_BATCH_AGE,
                last: now,
            });
        }
        let (wake_tx, wake_rx) = mpsc::sync_channel(1);
        wake_tx.try_send(()).unwrap();
        drop(wake_tx);
        let (output_tx, output_rx) = mpsc::sync_channel(1);

        let worker = thread::spawn(move || run_coalescer(state, wake_rx, output_tx));
        let batch = output_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("the hard deadline to outrank the queued wake")
            .expect("a batch");
        worker.join().unwrap();

        assert!(
            batch
                .vault_roots()
                .iter()
                .any(|path| path.as_path() == Path::new("changed.md"))
        );
    }

    #[test]
    fn authored_caps_cover_the_generated_profiles_they_name() {
        const {
            assert!(DIRTY_ROOT_CAP >= 6_000 + 6 * 220);
            assert!(OWN_WRITE_CAP >= 2 * 2_000);
        }
    }

    #[test]
    fn reaching_the_dirty_cap_clears_exact_roots_into_a_sticky_vault_rescan() {
        let state = state();
        for index in 0..DIRTY_ROOT_CAP {
            ingest(
                &state,
                Ok(Event::new(EventKind::Modify(ModifyKind::Any))
                    .add_path(PathBuf::from(format!("/vault/{index}.md")))),
            );
        }
        ingest(
            &state,
            Ok(Event::new(EventKind::Modify(ModifyKind::Any))
                .add_path("/vault/after-cap.md".into())),
        );

        let mut locked = state.lock().unwrap();
        let batch = &locked.pending.as_mut().unwrap().batch;
        assert!(batch.vault_roots.is_empty());
        assert_eq!(batch.rescans, BTreeSet::from([RescanScope::Vault]));
    }

    #[test]
    fn unchanged_outcome_never_primes_suppression() {
        let (scratch, ledger, recorder) = own_write_harness("watch-unchanged");
        let path = scratch.place("same.md", b"same");
        recorder
            .landed(&path, Landed::Unchanged(observed(&path)))
            .unwrap();
        assert!(ledger.lock().unwrap().entries.is_empty());
    }

    #[test]
    #[allow(clippy::disallowed_methods)] // Test arrangement for alternate absolute root spellings.
    fn own_writes_accept_an_absolute_path_under_the_canonical_root() {
        let (scratch, _ledger, mut recorder) = own_write_harness("watch-canonical-recorder");
        let path = scratch.place("note.md", b"bytes");
        recorder.root = std::fs::canonicalize(&recorder.root).expect("a canonical vault root");

        assert_eq!(
            recorder.normalize(&path).unwrap().as_path(),
            Path::new("note.md")
        );
    }

    #[test]
    fn present_suppression_requires_the_content_hash_even_when_stat_and_identity_match() {
        let (scratch, _, _) = own_write_harness("watch-present-hash");
        let path = scratch.place("same-stat.md", b"actual");
        let mut claimed = observed(&path);
        claimed.content_hash = ContentHash::of(b"forged");
        assert_eq!(claimed.len, 6);
        assert!(!matches_expected(&path, &Expected::Present(claimed)));
        assert!(matches_expected(&path, &Expected::Present(observed(&path))));
    }

    #[cfg(unix)]
    #[test]
    #[allow(clippy::disallowed_methods)] // Test scaffolding: arrange a dangling name.
    fn absent_suppression_requires_explicit_absence() {
        use std::os::unix::fs::symlink;

        let (scratch, _, _) = own_write_harness("watch-absence");
        let absent = scratch.at("absent.md");
        assert!(matches_expected(&absent, &Expected::Absent));
        let present = scratch.place("present.md", b"bytes");
        assert!(!matches_expected(&present, &Expected::Absent));
        let dangling = scratch.at("dangling.md");
        symlink(scratch.at("missing-target"), &dangling).unwrap();
        assert!(!matches_expected(&dangling, &Expected::Absent));
    }

    #[test]
    fn latest_outcome_for_a_path_wins() {
        let (scratch, ledger, recorder) = own_write_harness("watch-latest");
        let path = scratch.place("note.md", b"before removal");
        let state = observed(&path);
        recorder.landed(&path, Landed::Written(state)).unwrap();
        recorder.vacated(&path, Vacated { removed: state }).unwrap();
        let normalized = recorder.normalize(&path).unwrap();
        assert!(matches!(
            ledger.lock().unwrap().entries[&normalized].expected,
            Expected::Absent
        ));
    }

    #[test]
    fn unrelated_batch_does_not_consume_a_recorded_outcome() {
        let (scratch, ledger, recorder) = own_write_harness("watch-unrelated");
        let path = scratch.place("ours.md", b"ours");
        recorder
            .landed(&path, Landed::Written(observed(&path)))
            .unwrap();
        let batch = suppress(&recorder.root, &ledger, dirty(&recorder, &["other.md"]));
        assert_eq!(batch.vault_roots().len(), 1);
        assert!(
            ledger
                .lock()
                .unwrap()
                .entries
                .contains_key(&recorder.normalize(&path).unwrap())
        );
    }

    #[test]
    fn expired_outcome_fails_open_and_is_not_suppressed() {
        let (scratch, ledger, recorder) = own_write_harness("watch-expired");
        let path = scratch.place("old.md", b"old");
        let normalized = recorder.normalize(&path).unwrap();
        ledger.lock().unwrap().entries.insert(
            normalized,
            LedgerEntry {
                expected: Expected::Present(observed(&path)),
                recorded: Instant::now() - OWN_WRITE_TTL - Duration::from_millis(1),
            },
        );
        let batch = suppress(&recorder.root, &ledger, dirty(&recorder, &["old.md"]));
        assert_eq!(batch.vault_roots().len(), 1);
        assert!(ledger.lock().unwrap().entries.is_empty());
    }

    #[test]
    fn reaching_the_ledger_cap_clears_it_and_fails_open() {
        let (_scratch, ledger, recorder) = own_write_harness("watch-ledger-cap");
        for index in 0..OWN_WRITE_CAP {
            recorder.record(
                recorder
                    .normalizer
                    .normalize(Path::new(&format!("{index}.md")))
                    .unwrap(),
                Expected::Absent,
            );
        }
        assert!(ledger.lock().unwrap().entries.is_empty());
        let batch = suppress(&recorder.root, &ledger, dirty(&recorder, &["0.md"]));
        assert_eq!(batch.vault_roots().len(), 1);
    }

    #[test]
    fn moved_records_the_present_destination_and_absent_source() {
        let (scratch, ledger, recorder) = own_write_harness("watch-moved");
        let destination = scratch.place("to.md", b"moved");
        let state = observed(&destination);
        recorder
            .moved(
                &scratch.at("from.md"),
                &destination,
                Moved {
                    created: state,
                    vacated: state,
                },
            )
            .unwrap();
        let ledger = ledger.lock().unwrap();
        assert!(matches!(
            ledger.entries[&recorder.normalize(&scratch.at("from.md")).unwrap()].expected,
            Expected::Absent
        ));
        assert!(
            matches!(ledger.entries[&recorder.normalize(&destination).unwrap()].expected, Expected::Present(found) if found == state)
        );
    }
}
