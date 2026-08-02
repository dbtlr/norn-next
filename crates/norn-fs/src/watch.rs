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
use std::sync::{Arc, Mutex, Weak, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use notify::event::EventKind;
use notify::{Config, Event, RecommendedWatcher, RecursiveMode, Watcher as _};

use crate::PostState;
use crate::hash::hashed_from;
use crate::path::{NormalizedPath, PathError, PathNormalizer};
use crate::shadow::FALLBACK;
use crate::write::{Landed, Moved, Vacated};

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
}

impl fmt::Display for WatchError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Backend(message) => write!(formatter, "filesystem watcher failed: {message}"),
            Self::CoverageLost(path) => {
                write!(formatter, "watch coverage was lost for {}", path.display())
            }
        }
    }
}
impl std::error::Error for WatchError {}

/// The single-consumer stream returned by [`watch`].
///
/// Dropping it tears down the backend. It is intentionally not cloneable.
pub struct Subscription {
    batches: Option<mpsc::Receiver<Result<Batch, WatchError>>>,
    watcher: Option<RecommendedWatcher>,
    worker: Option<thread::JoinHandle<()>>,
}

impl Subscription {
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

    /// Blocks until a settled batch or terminal error is available.
    pub fn recv(&self) -> Result<Batch, WatchError> {
        self.batches
            .as_ref()
            .expect("subscription receiver present")
            .recv()
            .unwrap_or_else(|_| Err(WatchError::Backend("watcher stopped".into())))
    }

    /// Waits at most `timeout` for a settled batch or terminal error.
    pub fn recv_timeout(&self, timeout: Duration) -> Result<Option<Batch>, WatchError> {
        match self
            .batches
            .as_ref()
            .expect("subscription receiver present")
            .recv_timeout(timeout)
        {
            Ok(Ok(batch)) => Ok(Some(batch)),
            Ok(Err(error)) => Err(error),
            Err(mpsc::RecvTimeoutError::Timeout) => Ok(None),
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                Err(WatchError::Backend("watcher stopped".into()))
            }
        }
    }
}

impl Drop for Subscription {
    fn drop(&mut self) {
        self.watcher.take();
        // Disconnect a worker blocked behind the bounded delivery slot before
        // joining it. Backend callbacks write only shared pending state, so
        // delivery backpressure never blocks event intake.
        self.batches.take();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

/// A cloneable recorder for outcomes produced by Norn's write kernel.
///
/// Recorder handles do not keep the subscription or platform watcher alive.
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

/// Starts watching only after every requested coverage edge is active.
///
/// This is the first stage of attach, not a declaration that an entry is ready:
/// the caller heals after this function returns and consumes all resulting
/// batches before exposing the entry as ready. Later batches remain the same
/// subscription's warm invalidation stream.
#[allow(clippy::disallowed_methods)] // norn-fs owns vault path resolution.
pub fn watch(
    vault_root: &Path,
    schema_source: &Path,
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
    let ledger = Arc::new(Mutex::new(Ledger::default()));
    let shared = Arc::new(Mutex::new(State::new(
        root.clone(),
        normalizer.clone(),
        schema,
        ledger.clone(),
    )));
    let (wake_tx, wake_rx) = mpsc::sync_channel(1);
    // One delivered batch plus the bounded shared pending state is the whole
    // queue. A stalled consumer backpressures only this worker; callbacks keep
    // merging into State and widen to Rescan at the authored cap.
    let (batch_tx, batch_rx) = mpsc::sync_channel(1);
    let callback_state = shared.clone();
    let callback_wake = wake_tx.clone();
    let mut watcher = RecommendedWatcher::new(
        move |result| {
            ingest(&callback_state, result);
            let _ = callback_wake.try_send(());
        },
        Config::default(),
    )
    .map_err(backend)?;

    watcher
        .watch(&root, RecursiveMode::Recursive)
        .map_err(backend)?;
    let parent = root
        .parent()
        .ok_or_else(|| WatchError::Backend("vault root has no parent".into()))?;
    watcher
        .watch(parent, RecursiveMode::NonRecursive)
        .map_err(backend)?;
    if let SchemaLocation::External { parent, .. } =
        &shared.lock().expect("watch state poisoned").schema
        && parent != root.parent().expect("checked above")
    {
        watcher
            .watch(parent, RecursiveMode::NonRecursive)
            .map_err(backend)?;
    }

    let worker_state = shared;
    drop(wake_tx);
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
        },
        owns,
    ))
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
    schema: SchemaLocation,
    ledger: Arc<Mutex<Ledger>>,
    pending: Option<Pending>,
    terminal: Option<WatchError>,
}

impl State {
    fn new(
        root: PathBuf,
        normalizer: PathNormalizer,
        schema: SchemaLocation,
        ledger: Arc<Mutex<Ledger>>,
    ) -> Self {
        Self {
            root,
            normalizer,
            schema,
            ledger,
            pending: None,
            terminal: None,
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

fn ingest(shared: &Arc<Mutex<State>>, result: notify::Result<Event>) {
    let mut state = shared.lock().expect("watch state poisoned");
    match result {
        Err(error) => {
            state.terminal.get_or_insert_with(|| backend(error));
        }
        Ok(event) if matches!(event.kind, EventKind::Access(_)) => {}
        Ok(event) => {
            if event.need_rescan() {
                state.rescan(RescanScope::Vault);
                state.rescan(RescanScope::Schema);
                return;
            }
            if event.paths.is_empty() {
                state.rescan(RescanScope::Vault);
                state.rescan(RescanScope::Schema);
                return;
            }
            for path in event.paths {
                ingest_path(&mut state, event.kind, &path);
            }
        }
    }
}

fn ingest_path(state: &mut State, kind: EventKind, path: &Path) {
    if path == state.root {
        if kind.is_remove() || matches!(kind, EventKind::Modify(notify::event::ModifyKind::Name(_)))
        {
            let root = state.root.clone();
            state.terminal.get_or_insert(WatchError::CoverageLost(root));
        } else {
            state.rescan(RescanScope::Vault);
        }
        return;
    }
    let root_name_relevant = state.root.parent().is_some_and(|parent| {
        path.parent() == Some(parent) && path.file_name() == state.root.file_name()
    });
    if root_name_relevant {
        let root = state.root.clone();
        state.terminal.get_or_insert(WatchError::CoverageLost(root));
        return;
    }
    let (schema_dirty, schema_rescan) = match &state.schema {
        SchemaLocation::InVault(schema) => (
            path.strip_prefix(&state.root)
                .ok()
                .and_then(|p| state.normalizer.normalize(p).ok())
                .is_some_and(|event| {
                    event == *schema || schema.as_path().starts_with(event.as_path())
                }),
            false,
        ),
        SchemaLocation::External {
            name,
            parent,
            normalizer,
        } => {
            if path == parent {
                (false, true)
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
    if schema_rescan {
        state.rescan(RescanScope::Schema);
    }
    if schema_dirty {
        state.batch().schema_dirty = true;
    }

    let Ok(relative) = path.strip_prefix(&state.root) else {
        return;
    };
    let Ok(normalized) = state.normalizer.normalize(relative) else {
        state.rescan(RescanScope::Vault);
        return;
    };
    if is_mechanism(&normalized) {
        return;
    }
    let batch = state.batch();
    if !batch.rescans.contains(&RescanScope::Vault) {
        batch.vault_roots.insert(normalized);
        if batch.vault_roots.len() >= DIRTY_ROOT_CAP {
            batch.vault_roots.clear();
            batch.rescans.insert(RescanScope::Vault);
        }
    }
}

fn is_mechanism(path: &NormalizedPath) -> bool {
    let candidate = path.as_path();
    candidate == Path::new(FALLBACK) || candidate.starts_with(Path::new(FALLBACK))
}

fn run_coalescer(
    state: Arc<Mutex<State>>,
    wake: mpsc::Receiver<()>,
    output: mpsc::SyncSender<Result<Batch, WatchError>>,
) {
    loop {
        let wait = {
            let locked = state.lock().expect("watch state poisoned");
            if locked.terminal.is_some() {
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
            if let Some(error) = locked.terminal.take() {
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
    use super::*;
    use notify::event::{AccessKind, AccessMode, ModifyKind, RemoveKind};

    use crate::ContentHash;
    use crate::identity::post_state;
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

    #[test]
    fn every_access_only_event_is_ignored() {
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
                Ok(Event::new(EventKind::Access(kind)).add_path("/vault/note.md".into())),
            );
            assert!(state.lock().unwrap().pending.is_none(), "{kind:?}");
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
