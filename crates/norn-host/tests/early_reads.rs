//! Early reads of the canonical store a from-zero build fills, over the public
//! Host path, held at the build's own step boundaries.
#![cfg(all(unix, feature = "induced-failure"))]
#![allow(clippy::disallowed_methods)] // Every path is inside this test's owned fixture.

use norn_config::{ConfigDirs, registry::VaultRoot};
use norn_host::{
    Host, LifecyclePolicy, LoadingBoundary, LoadingProbe, ProductionEntryOps, ProductionPolicy,
    Registration, RegistryRead,
};
use norn_store::Step;
use norn_testkit::{
    isolation::{self, Lease},
    scratch::Scratch,
    wait::{Budget, Observed, wait_until},
};
use norn_wire::{
    ApplyMode, AuthoredValue, Column, CountParams, DescribeParams, DocumentPath, FieldChange,
    FindParams, FindingKind, GetParams, Predicate, ReasonCode, ResolutionTarget, SearchParams,
    SetParams, TrustState, ValidateParams, VaultAddress, VaultName, WriteTarget,
};
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, mpsc};
use std::time::{Duration, Instant};

const BUDGET: Budget = Budget::new(Duration::from_secs(20), Duration::from_millis(100));
/// The markdown documents the fixture writes.
const DOCUMENTS: u64 = 13;

type Answer<R, W> = Result<norn_host::Answered<R, W>, norn_wire::ErrorEnvelope>;
type GetAnswer = Answer<norn_wire::GetReport, norn_store::GetWork>;
type FindAnswer = Answer<norn_wire::FindReport, norn_store::FindWork>;

#[derive(Default)]
struct ProbeState {
    next: VecDeque<LoadingBoundary>,
    current: Option<LoadingBoundary>,
    released: bool,
    stop: bool,
    seen: Vec<LoadingBoundary>,
    /// Disposable-store paths met under the derived directory at any boundary.
    loading_paths: Vec<PathBuf>,
}

/// Holds construction at each boundary of its queue in turn until released,
/// and checks the derived directory for disposable stores at every boundary.
struct Probe {
    state: Mutex<ProbeState>,
    moved: Condvar,
    derived: Option<PathBuf>,
}

impl Probe {
    fn new(points: impl IntoIterator<Item = LoadingBoundary>) -> Arc<Self> {
        Self::watching(points, None)
    }
    fn watching(
        points: impl IntoIterator<Item = LoadingBoundary>,
        derived: Option<PathBuf>,
    ) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(ProbeState {
                next: points.into_iter().collect(),
                ..ProbeState::default()
            }),
            moved: Condvar::new(),
            derived,
        })
    }
    fn wait(&self, point: LoadingBoundary) {
        wait_until("construction boundary", BUDGET, || {
            let state = self.state.lock().unwrap();
            if state.current == Some(point) {
                Observed::Met(())
            } else {
                Observed::pending(format!("expected {point:?}, current {:?}", state.current))
            }
        })
        .unwrap();
    }
    fn release(&self) {
        self.state.lock().unwrap().released = true;
        self.moved.notify_all();
    }
    fn finish(&self) {
        self.state.lock().unwrap_or_else(|p| p.into_inner()).stop = true;
        self.moved.notify_all();
    }
    fn seen(&self) -> Vec<LoadingBoundary> {
        self.state.lock().unwrap().seen.clone()
    }
    fn loading_paths(&self) -> Vec<PathBuf> {
        self.state.lock().unwrap().loading_paths.clone()
    }
}

impl LoadingProbe for Probe {
    fn at(&self, boundary: LoadingBoundary) {
        let loading = self.derived.as_ref().map(|derived| {
            std::fs::read_dir(derived)
                .into_iter()
                .flatten()
                .flatten()
                .map(|entry| entry.path())
                .filter(|path| path.to_string_lossy().contains(".loading"))
                .collect::<Vec<_>>()
        });
        let mut state = self.state.lock().unwrap();
        state.seen.push(boundary);
        state.loading_paths.extend(loading.into_iter().flatten());
        if state.stop || state.next.front() != Some(&boundary) {
            return;
        }
        state.next.pop_front();
        state.current = Some(boundary);
        state.released = false;
        self.moved.notify_all();
        let (mut state, timeout) = self
            .moved
            .wait_timeout_while(state, Duration::from_secs(60), |s| !s.released && !s.stop)
            .unwrap();
        state.current = None;
        assert!(
            !timeout.timed_out(),
            "construction boundary {boundary:?} was never released"
        );
    }
}

/// Construction boundaries to one probe, read boundaries to another.
struct SplitProbe {
    construction: Arc<Probe>,
    reads: Arc<Probe>,
}
impl LoadingProbe for SplitProbe {
    fn at(&self, boundary: LoadingBoundary) {
        match boundary {
            LoadingBoundary::QueryEstablished | LoadingBoundary::QueryBuilt => {
                self.reads.at(boundary)
            }
            _ => self.construction.at(boundary),
        }
    }
}

struct Release<'a>(&'a Probe);
impl Drop for Release<'_> {
    fn drop(&mut self) {
        self.0.finish();
    }
}

struct Fixture {
    scratch: Scratch,
    root: PathBuf,
    dirs: ConfigDirs,
    _watcher: Lease,
}

impl Fixture {
    fn new(label: &str) -> Self {
        Self::with_schema(label, "version: 1\nfields:\n  rank: { type: number }\n")
    }
    fn with_schema(label: &str, schema: &str) -> Self {
        let scratch = Scratch::under(Path::new(env!("CARGO_TARGET_TMPDIR")), label).unwrap();
        let root = scratch.join("vault");
        std::fs::create_dir_all(root.join(".norn")).unwrap();
        std::fs::write(root.join(".norn/schema.yaml"), schema).unwrap();
        for n in 0..12 {
            std::fs::write(
                root.join(format!("doc{n:02}.md")),
                format!(
                    "---\nrank: {n}\n---\n# Title {n}\nBody {n} #group{} [[doc{:02}]].\n",
                    n % 2,
                    (n + 1) % 12
                ),
            )
            .unwrap();
        }
        std::fs::write(
            root.join("first.md"),
            "---\nrank: nope\ntags: [one, two]\n---\n# First\n\
             [[doc01#Missing]] [[absent]] [[asset.png]]\nText ^block\n",
        )
        .unwrap();
        std::fs::write(root.join("asset.png"), "file").unwrap();
        let dirs = ConfigDirs::new(scratch.join("config"), scratch.join("data")).unwrap();
        let watcher = Lease::hold(
            isolation::REAL_WATCHER,
            isolation::acquisition_budget(BUDGET),
        );
        Self {
            scratch,
            root,
            dirs,
            _watcher: watcher,
        }
    }
    /// Write `bytes` at `relative` under the vault, making its directories.
    fn add(&self, relative: &str, bytes: impl AsRef<[u8]>) {
        let path = self.root.join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
    }
    fn derived(&self) -> PathBuf {
        self.dirs.derived_dir(&name())
    }
    fn store_path(&self) -> PathBuf {
        self.derived().join("store.sqlite3")
    }
    fn ops(&self, probe: Arc<dyn LoadingProbe>) -> ProductionEntryOps {
        ProductionEntryOps::new(self.dirs.clone(), ProductionPolicy::new(2, 2).unwrap())
            .with_loading_probe(probe)
    }
    fn host(&self, probe: Arc<dyn LoadingProbe>) -> Host<ProductionEntryOps> {
        self.host_bound(probe, norn_host::READ_SETTLE_BOUND)
    }
    fn host_bound(
        &self,
        probe: Arc<dyn LoadingProbe>,
        bound: Duration,
    ) -> Host<ProductionEntryOps> {
        self.host_over(self.ops(probe), bound)
    }
    fn host_over(&self, ops: ProductionEntryOps, bound: Duration) -> Host<ProductionEntryOps> {
        self.host_ticking(ops, bound, Duration::from_millis(25))
    }
    /// A host whose dispatcher ticks every `tick`. A read bounded to zero
    /// takes the entry gate with one try, so a case making one keeps the
    /// dispatcher off the gate by ticking it rarely.
    fn host_ticking(
        &self,
        ops: ProductionEntryOps,
        bound: Duration,
        tick: Duration,
    ) -> Host<ProductionEntryOps> {
        // FSEvents can deliver the fixture's writes after the host attaches.
        norn_fs::watch::settle_prior_changes(self.scratch.root(), BUDGET.work()).unwrap();
        self.host_unsettled(ops, bound, tick)
    }
    fn host_unsettled(
        &self,
        ops: ProductionEntryOps,
        bound: Duration,
        tick: Duration,
    ) -> Host<ProductionEntryOps> {
        Host::new(
            RegistryRead::from_entries([Registration::new(
                name(),
                VaultRoot::new(&self.root).unwrap(),
            )]),
            ops,
            LifecyclePolicy {
                idle_after: Duration::from_secs(7200),
                worker_slots: 1,
                watch_poll_interval: tick,
                read_settle_bound: bound,
            },
        )
        .unwrap()
    }
}

fn name() -> VaultName {
    VaultName::new("notes").unwrap()
}
fn address() -> VaultAddress {
    VaultAddress::name(name())
}
fn get(target: &str) -> GetParams {
    GetParams::new(address(), ResolutionTarget::new(target).unwrap())
}
fn find() -> FindParams {
    FindParams::new(address()).with_columns([Column::path(), Column::fields()])
}
fn ranked(rank: &str) -> FindParams {
    find().with_predicates([Predicate::equal_to("rank", rank)])
}
/// A Find whose part names a key the schema does not declare.
fn unknown_key() -> FindParams {
    find().with_predicates([Predicate::equal_to("unknown", "x")])
}
fn target(text: &str) -> ResolutionTarget {
    ResolutionTarget::new(text).unwrap()
}
fn ready(host: &Host<ProductionEntryOps>) {
    wait_until("canonical Ready", BUDGET, || match host.state(&name()) {
        Ok(TrustState::Ready) => Observed::Met(()),
        reading => Observed::pending(format!("{reading:?}")),
    })
    .unwrap();
}
fn waits(host: &Host<ProductionEntryOps>, at_least: u64) {
    wait_until("public caller waiting for construction", BUDGET, || {
        let seen = host.read_evidence().settle_waits;
        if seen >= at_least {
            Observed::Met(())
        } else {
            Observed::pending(format!("{seen} < {at_least}"))
        }
    })
    .unwrap();
}
fn fact_recorded(account: &norn_host::JobEvidence, after: u64) {
    wait_until("build fact accepted", BUDGET, || {
        let seen = account.read().build_facts_recorded;
        if seen > after {
            Observed::Met(())
        } else {
            Observed::pending("watcher has not accepted the fact")
        }
    })
    .unwrap();
}
fn spawn_get<'scope>(
    scope: &'scope std::thread::Scope<'scope, '_>,
    host: &'scope Host<ProductionEntryOps>,
    params: GetParams,
) -> mpsc::Receiver<GetAnswer> {
    let (send, receive) = mpsc::sync_channel(1);
    scope.spawn(move || {
        let _ = send.send(host.get(&params));
    });
    receive
}
fn spawn_find<'scope>(
    scope: &'scope std::thread::Scope<'scope, '_>,
    host: &'scope Host<ProductionEntryOps>,
    params: FindParams,
) -> mpsc::Receiver<FindAnswer> {
    let (send, receive) = mpsc::sync_channel(1);
    scope.spawn(move || {
        let _ = send.send(host.find(&params));
    });
    receive
}
fn warming(trust: &TrustState) -> bool {
    matches!(trust, TrustState::Warming { .. })
}
macro_rules! json {
    ($value:expr) => {
        serde_json::to_value($value).unwrap()
    };
}

/// A Ready answer as a build that has not judged relations renders it: every
/// link's targets and health, and every findings column, read as loading.
fn relations_loading(mut value: serde_json::Value) -> serde_json::Value {
    match &mut value {
        serde_json::Value::Object(object) => {
            if object.contains_key("health") && object.contains_key("targets") {
                object.insert("targets".into(), serde_json::Value::Null);
                object.insert("health".into(), "loading".into());
            }
            for (key, child) in object.iter_mut() {
                if key == "findings" && child.get("state").is_some() {
                    *child = serde_json::json!({"state": "unknown", "reason": "loading"});
                } else {
                    *child = relations_loading(child.take());
                }
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                *item = relations_loading(item.take());
            }
        }
        _ => {}
    }
    value
}

/// The refusal of a capability not yet admitted, or one past its deadline.
fn assert_still_indexing(error: &norn_wire::ErrorEnvelope) {
    assert_eq!(error.code(), &ReasonCode::HostEntryNotReady, "{error:?}");
    assert_eq!(error.message(), "this vault is still being indexed");
    let detail = json!(error.detail()).to_string();
    assert!(
        detail.contains("warming") && detail.contains("healing"),
        "{detail}"
    );
}

/// The order the attach opens a store over `vault` under: the one the root
/// proves. An open under the other one would rebuild the store it is reading.
fn proven_order(vault: &Path) -> norn_store::StoredPathOrder {
    norn_host::stored_path_order(
        norn_fs::PathNormalizer::detect(vault)
            .expect("detect the vault's case behaviour")
            .case_sensitivity(),
    )
}

/// The store a host attaching `fixture`'s tree with no early reader derives,
/// projected for comparison.
fn from_scratch(fixture: &Fixture, label: &str) -> norn_testkit::equivalence::StoreProjection {
    let dirs = ConfigDirs::new(
        fixture.scratch.join(format!("{label}-config")),
        fixture.scratch.join(format!("{label}-data")),
    )
    .unwrap();
    let path = dirs.derived_dir(&name()).join("store.sqlite3");
    {
        let reference = Host::new(
            RegistryRead::from_entries([Registration::new(
                name(),
                VaultRoot::new(&fixture.root).unwrap(),
            )]),
            ProductionEntryOps::new(dirs, ProductionPolicy::new(2, 2).unwrap()),
            LifecyclePolicy {
                idle_after: Duration::from_secs(7200),
                worker_slots: 1,
                watch_poll_interval: Duration::from_millis(25),
                read_settle_bound: norn_host::READ_SETTLE_BOUND,
            },
        )
        .unwrap();
        let _demand = reference.demand(&name()).unwrap();
        ready(&reference);
    }
    project(&path, &fixture.root)
}

fn project(path: &Path, root: &Path) -> norn_testkit::equivalence::StoreProjection {
    let mut store =
        norn_store::Store::open(path, proven_order(root), norn_host::DERIVATION_VERSION).unwrap();
    norn_testkit::equivalence::assert_operationally_valid(&mut store, "a finished store");
    norn_testkit::equivalence::StoreProjection::read(&mut store).unwrap()
}

/// Make every page of every table and index unreadable, the way a corrupt
/// file is: the write-ahead log is folded into the database first, so the file
/// holds every page, and then each page's type byte is overwritten with one no
/// b-tree page carries. The schema page stands, so the file still opens.
fn damage_every_table(database: &Path) {
    let (pages, page_size) = match norn_db::connect(database).expect("connecting to the store") {
        norn_db::Attempt::Connected(connection) => {
            connection
                .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))
                .expect("fold the log into the database");
            let page_size: u64 = connection
                .query_row("PRAGMA page_size", [], |row| row.get(0))
                .expect("the page size");
            let mut statement = connection
                .prepare(
                    "SELECT pageno FROM dbstat WHERE name IN \
                     (SELECT name FROM sqlite_schema WHERE tbl_name NOT LIKE 'sqlite_%')",
                )
                .expect("the pages of the documents table");
            let pages: Vec<u64> = statement
                .query_map([], |row| row.get(0))
                .expect("read the pages")
                .collect::<Result<_, _>>()
                .expect("a page number");
            (pages, page_size)
        }
        norn_db::Attempt::Unreadable { detail } => panic!("the store is unreadable: {detail}"),
    };
    assert!(!pages.is_empty() && !pages.contains(&1), "{pages:?}");
    let mut bytes = std::fs::read(database).expect("read the database file");
    for page in pages {
        let at = usize::try_from((page - 1) * page_size).expect("an offset in memory");
        bytes[at] = 0x00;
    }
    std::fs::write(database, bytes).expect("write the damaged database");
}

/// Contract items 7 (Get at vault-wide Structure), 24 and 25 (first half).
#[test]
fn the_first_read_on_a_new_vault_waits_and_a_get_at_structure_equals_ready() {
    let fixture = Fixture::new("early-first-read");
    let _recording = norn_fs::reads::record_files();
    let probe = Probe::watching(
        [LoadingBoundary::StepComplete(Step::Structure)],
        Some(fixture.derived()),
    );
    let ops = fixture.ops(probe.clone());
    let account = ops.account();
    let host = fixture.host_over(ops, norn_host::READ_SETTLE_BOUND);
    let links = find().with_columns([Column::path(), Column::links(), Column::findings()]);
    let (early_get, early_find) = std::thread::scope(|scope| {
        let _release = Release(&probe);
        // The first read attaches the vault and waits for its own capability.
        let first = spawn_get(scope, &host, get("first"));
        waits(&host, 1);
        probe.wait(LoadingBoundary::StepComplete(Step::Structure));
        let early_get = first.recv_timeout(BUDGET.work()).unwrap().unwrap();
        let early_find = host.find(&links).unwrap();
        probe.release();
        (early_get, early_find)
    });
    ready(&host);
    for reading in [&early_get.answer.reading, &early_find.answer.reading] {
        assert!(warming(&reading.trust), "{reading:?}");
    }
    let ready_get = host.get(&get("first")).unwrap();
    let ready_find = host.find(&links).unwrap();
    assert_eq!(
        early_get.answer.reading.epoch,
        ready_get.answer.reading.epoch
    );
    assert_eq!(
        json!(&early_get.answer.report),
        relations_loading(json!(&ready_get.answer.report))
    );
    assert_eq!(
        json!(&early_find.answer.report),
        relations_loading(json!(&ready_find.answer.report))
    );
    assert_ne!(
        json!(&early_get.answer.report),
        json!(&ready_get.answer.report),
        "the fixture's links and findings are relation parts"
    );
    assert!(
        probe.loading_paths().is_empty(),
        "a disposable store stood under the derived directory: {:?}",
        probe.loading_paths()
    );
    // Each document is read once, beside the one read of the declaration.
    let files = account.files_read();
    let documents = files
        .iter()
        .filter(|file| {
            file.path
                .extension()
                .is_some_and(|extension| extension == "md")
        })
        .collect::<Vec<_>>();
    let distinct = documents
        .iter()
        .map(|file| &file.path)
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(documents.len() as u64, DOCUMENTS, "{files:?}");
    assert_eq!(distinct.len() as u64, DOCUMENTS, "{files:?}");
    // The one other open is the declaration's.
    let others = files
        .iter()
        .filter(|file| !documents.contains(file))
        .collect::<Vec<_>>();
    assert!(
        others.len() == 1 && others[0].path.ends_with(".norn/schema.yaml"),
        "{files:?}"
    );
    assert_eq!(account.read().document_opens, DOCUMENTS + 1, "{files:?}");
    assert_eq!(account.read().from_zero_builds, 1);
    assert!(
        probe
            .seen()
            .contains(&LoadingBoundary::Published(Step::Structure))
    );
}

/// Contract item 25, second half.
#[test]
fn an_open_time_rebuild_refuses_before_its_construction_begins_and_waits_after() {
    let fixture = Fixture::new("early-open-time-rebuild");
    // A store left under construction: the next open rebuilds it from zero.
    let seed = norn_store::Store::open(
        fixture.store_path(),
        proven_order(&fixture.root),
        norn_host::DERIVATION_VERSION,
    )
    .unwrap();
    assert!(seed.under_construction());
    drop(seed);
    let probe = Probe::new([
        LoadingBoundary::AuthorityCaptured,
        LoadingBoundary::CanonicalPage,
    ]);
    let host = fixture.host(probe.clone());
    std::thread::scope(|scope| {
        let _release = Release(&probe);
        let _demand = host.demand(&name()).unwrap();
        probe.wait(LoadingBoundary::AuthorityCaptured);
        let began = Instant::now();
        let refused = host.get(&get("first")).unwrap_err();
        assert_eq!(refused.code(), &ReasonCode::HostEntryNotReady);
        assert!(
            began.elapsed() < Duration::from_secs(2),
            "it refused at once"
        );
        assert_eq!(host.read_evidence().settle_waits, 0);
        probe.release();
        probe.wait(LoadingBoundary::CanonicalPage);
        let waiting = spawn_get(scope, &host, get("first"));
        waits(&host, 1);
        probe.release();
        let answered = waiting.recv_timeout(BUDGET.work()).unwrap().unwrap();
        assert!(warming(&answered.answer.reading.trust));
    });
    ready(&host);
}

/// Contract item 12, and the waking half of item 17.
#[test]
fn field_find_opens_when_content_ends_and_its_cursor_continues_across_ready() {
    let fixture = Fixture::new("early-field-find");
    let probe = Probe::new([
        LoadingBoundary::StepComplete(Step::Names),
        LoadingBoundary::StepComplete(Step::Content),
    ]);
    let host = fixture.host(probe.clone());
    let paged = find()
        .with_sort(norn_wire::Sort::new(
            norn_wire::SortKey::path(),
            norn_wire::Direction::Ascending,
        ))
        .with_limit(2);
    let (early, first_page, tagged, unknown) = std::thread::scope(|scope| {
        let _release = Release(&probe);
        let _demand = host.demand(&name()).unwrap();
        probe.wait(LoadingBoundary::StepComplete(Step::Names));
        let waiting = spawn_find(scope, &host, ranked("7"));
        waits(&host, 1);
        assert!(
            waiting.try_recv().is_err(),
            "a field Find waits for content"
        );
        probe.release();
        probe.wait(LoadingBoundary::StepComplete(Step::Content));
        // Publication woke the waiter, which answers while the build is held.
        let early = waiting.recv_timeout(BUDGET.work()).unwrap().unwrap();
        let first_page = host.find(&paged).unwrap();
        // Cursor judgment is the ordinary one: a cursor under another order
        // is refused, and a part naming an undeclared key is unsatisfied as
        // at Ready.
        let reordered = find()
            .with_after(first_page.answer.report.page.next.clone().unwrap())
            .with_sort(norn_wire::Sort::new(
                norn_wire::SortKey::field("rank"),
                norn_wire::Direction::Descending,
            ));
        assert_eq!(
            host.find(&reordered).unwrap_err().code(),
            &ReasonCode::VaultCursorOrderChanged
        );
        let unknown = host.find(&unknown_key()).unwrap();
        let before = host.read_evidence().settle_waits;
        let tagged = spawn_find(
            scope,
            &host,
            find().with_predicates([Predicate::tag("group1")]),
        );
        waits(&host, before + 1);
        assert!(tagged.try_recv().is_err(), "a tag Find waits for structure");
        probe.release();
        let tagged = tagged.recv_timeout(BUDGET.work()).unwrap().unwrap();
        (early, first_page, tagged, unknown)
    });
    assert!(warming(&early.answer.reading.trust));
    assert_eq!(early.answer.report.page.rows.len(), 1);
    assert_eq!(early.answer.report.page.rows[0].path.as_str(), "doc07.md");
    ready(&host);
    let ready_unknown = host.find(&unknown_key()).unwrap();
    assert!(!unknown.answer.unsatisfied.is_empty());
    assert_eq!(
        (unknown.answer.unsatisfied, unknown.answer.report),
        (
            ready_unknown.answer.unsatisfied,
            ready_unknown.answer.report
        )
    );
    assert_eq!(
        host.find(&ranked("7")).unwrap().answer.report,
        early.answer.report
    );
    assert_eq!(
        host.find(&find().with_predicates([Predicate::tag("group1")]))
            .unwrap()
            .answer
            .report,
        tagged.answer.report
    );
    let cursor = first_page.answer.report.page.next.clone().unwrap();
    let continued = host.find(&paged.clone().with_after(cursor)).unwrap();
    assert!(
        continued
            .answer
            .report
            .page
            .moved
            .contains(&norn_wire::Moved::Generation),
        "{:?}",
        continued.answer.report.page
    );
    let whole = host.find(&paged.with_limit(4)).unwrap();
    assert_eq!(
        whole.answer.report.page.rows[2..],
        continued.answer.report.page.rows[..]
    );
}

/// Contract item 13.
#[test]
fn structure_shapes_open_when_structure_ends() {
    let fixture = Fixture::new("early-structure-shapes");
    let probe = Probe::new([
        LoadingBoundary::StepComplete(Step::Content),
        LoadingBoundary::StepComplete(Step::Structure),
    ]);
    let host = fixture.host_bound(probe.clone(), Duration::from_millis(300));
    let shapes = [
        find().with_predicates([Predicate::tag("group0")]),
        find().with_predicates([Predicate::links_to(target("doc01"))]),
        find().with_predicates([Predicate::resolves(target("doc01"))]),
        find().with_columns([Column::path(), Column::headings()]),
        find().with_columns([Column::path(), Column::body()]),
    ];
    let early = std::thread::scope(|_| {
        let _release = Release(&probe);
        let _demand = host.demand(&name()).unwrap();
        probe.wait(LoadingBoundary::StepComplete(Step::Content));
        assert!(
            host.find(&ranked("3")).is_ok(),
            "content shapes are admitted"
        );
        for shape in &shapes {
            assert_still_indexing(&host.find(shape).unwrap_err());
        }
        probe.release();
        probe.wait(LoadingBoundary::StepComplete(Step::Structure));
        let early = shapes
            .iter()
            .map(|shape| host.find(shape).unwrap())
            .collect::<Vec<_>>();
        probe.release();
        early
    });
    ready(&host);
    for (shape, early) in shapes.iter().zip(early) {
        assert!(warming(&early.answer.reading.trust));
        assert_eq!(host.find(shape).unwrap().answer.report, early.answer.report);
    }
}

/// Contract item 14, its HasFinding and link-health parts.
#[test]
fn relations_gate_has_finding_and_link_health() {
    let fixture = Fixture::new("early-relations");
    let probe = Probe::new([
        LoadingBoundary::StepComplete(Step::Structure),
        LoadingBoundary::Published(Step::Relations),
    ]);
    let host = fixture.host_bound(probe.clone(), Duration::from_millis(300));
    let broken = find().with_predicates([Predicate::has_finding(FindingKind::Broken)]);
    let links = find().with_columns([Column::path(), Column::links(), Column::findings()]);
    let (loading, judged) = std::thread::scope(|_| {
        let _release = Release(&probe);
        let _demand = host.demand(&name()).unwrap();
        probe.wait(LoadingBoundary::StepComplete(Step::Structure));
        let began = Instant::now();
        assert_still_indexing(&host.find(&broken).unwrap_err());
        assert!(
            began.elapsed() >= Duration::from_millis(300),
            "a HasFinding part waits for relations before it refuses"
        );
        let loading = (host.get(&get("first")).unwrap(), host.find(&links).unwrap());
        probe.release();
        probe.wait(LoadingBoundary::Published(Step::Relations));
        let judged = (
            host.find(&broken).unwrap(),
            host.get(&get("first")).unwrap(),
            host.find(&links).unwrap(),
        );
        probe.release();
        (loading, judged)
    });
    ready(&host);
    let ready_get = host.get(&get("first")).unwrap().answer.report;
    let ready_links = host.find(&links).unwrap().answer.report;
    assert_eq!(
        json!(&loading.0.answer.report),
        relations_loading(json!(&ready_get))
    );
    assert_eq!(
        json!(&loading.1.answer.report),
        relations_loading(json!(&ready_links))
    );
    assert_eq!(
        host.find(&broken).unwrap().answer.report,
        judged.0.answer.report
    );
    assert_eq!(
        judged.0.answer.report.page.rows[0].path.as_str(),
        "first.md"
    );
    assert_eq!(judged.1.answer.report, ready_get);
    assert_eq!(judged.2.answer.report, ready_links);
}

/// Contract item 14, its validate part: a validate waits for relations and
/// refuses at its deadline before they publish, and once they do it is
/// admitted, its rows and its tally equal to Ready's.
#[test]
fn relations_gate_validate() {
    let fixture = Fixture::new("early-relations-validate");
    let probe = Probe::new([
        LoadingBoundary::StepComplete(Step::Structure),
        LoadingBoundary::Published(Step::Relations),
    ]);
    let ops = fixture.ops(probe.clone());
    let account = ops.account();
    let host = fixture.host_over(ops, Duration::from_millis(300));
    let shapes = [
        ValidateParams::new(address()),
        ValidateParams::new(address()).with_kinds([FindingKind::Broken]),
        ValidateParams::new(address()).with_predicates([Predicate::path("first.md")]),
        ValidateParams::new(address()).summarized(),
    ];
    let early = std::thread::scope(|_| {
        let _release = Release(&probe);
        let _demand = host.demand(&name()).unwrap();
        probe.wait(LoadingBoundary::StepComplete(Step::Structure));
        let began = Instant::now();
        assert_still_indexing(&host.validate(&shapes[0]).unwrap_err());
        assert!(
            began.elapsed() >= Duration::from_millis(300),
            "a validate waits for relations before it refuses"
        );
        probe.release();
        probe.wait(LoadingBoundary::Published(Step::Relations));
        let early = shapes
            .iter()
            .map(|shape| host.validate(shape).unwrap())
            .collect::<Vec<_>>();
        probe.release();
        early
    });
    ready(&host);
    // Each validate was served at relations; the one refused at structure
    // was never served. The build published its five steps and no target.
    assert_eq!(
        host.read_evidence().early_reads_served,
        norn_host::EarlyReadsServed {
            relations: shapes.len() as u64,
            ..Default::default()
        }
    );
    assert_eq!(account.read().construction_publications, 5);
    assert!(
        json!(&early[1].answer.report)
            .to_string()
            .contains("first.md"),
        "the fixture holds broken links"
    );
    for (shape, early) in shapes.iter().zip(early) {
        assert!(warming(&early.answer.reading.trust));
        let ready = host.validate(shape).unwrap();
        assert_eq!(json!(&early.answer.report), json!(&ready.answer.report));
        assert_eq!(early.answer.unsatisfied, ready.answer.unsatisfied);
        assert_eq!(early.answer.advisories, ready.answer.advisories);
    }
}

/// Contract item 15: a search, and a Find matching text, wait while full text
/// is being written and refuse at their deadline; once the closing
/// transaction commits they are admitted, equal to Ready. A host composing no
/// semantic engine answers the lexical rung alone, early as at Ready.
#[test]
fn search_opens_after_the_closing_transaction() {
    let fixture = Fixture::new("early-search");
    let probe = Probe::new([
        LoadingBoundary::FullTextBatch,
        LoadingBoundary::Published(Step::FullTextComplete),
    ]);
    let host = fixture.host_bound(probe.clone(), Duration::from_millis(300));
    let search = SearchParams::new(address(), "Title");
    let filtered = SearchParams::new(address(), "Body")
        .with_predicates([Predicate::tag("group1")])
        .with_columns([Column::path(), Column::fields()]);
    let matching = find().with_predicates([Predicate::matches("Title")]);
    let (searched, filtered_early, matched) = std::thread::scope(|_| {
        let _release = Release(&probe);
        let _demand = host.demand(&name()).unwrap();
        probe.wait(LoadingBoundary::FullTextBatch);
        assert!(
            host.validate(&ValidateParams::new(address())).is_ok(),
            "relations stand while full text is written"
        );
        let began = Instant::now();
        assert_still_indexing(&host.search(&search).unwrap_err());
        assert!(
            began.elapsed() >= Duration::from_millis(300),
            "a search waits for full text before it refuses"
        );
        assert_still_indexing(&host.find(&matching).unwrap_err());
        probe.release();
        probe.wait(LoadingBoundary::Published(Step::FullTextComplete));
        let answers = (
            host.search(&search).unwrap(),
            host.search(&filtered).unwrap(),
            host.find(&matching).unwrap(),
        );
        probe.release();
        answers
    });
    ready(&host);
    for early in [&searched.answer.reading, &filtered_early.answer.reading] {
        assert!(warming(&early.trust), "{early:?}");
    }
    assert!(warming(&matched.answer.reading.trust));
    assert!(!searched.answer.report.page.rows.is_empty());
    for (early, params) in [(&searched, &search), (&filtered_early, &filtered)] {
        let ready = host.search(params).unwrap();
        assert_eq!(json!(&early.answer.report), json!(&ready.answer.report));
        assert_eq!(early.answer.unsatisfied, ready.answer.unsatisfied);
        assert_eq!(early.answer.advisories, ready.answer.advisories);
    }
    assert_eq!(
        host.find(&matching).unwrap().answer.report,
        matched.answer.report
    );
}

/// Contract item 16.
#[test]
fn unsupported_shapes_and_writes_wait_for_ready() {
    let fixture = Fixture::new("early-unsupported");
    let probe = Probe::new([LoadingBoundary::StepComplete(Step::Structure)]);
    let host = fixture.host(probe.clone());
    let change = |mode| {
        SetParams::new(
            address(),
            mode,
            WriteTarget::path(DocumentPath::new("doc03.md").unwrap()),
            vec![FieldChange::set("rank", AuthoredValue::string("33"))],
        )
    };
    let pending = std::thread::scope(|_| {
        let _release = Release(&probe);
        let _demand = host.demand(&name()).unwrap();
        probe.wait(LoadingBoundary::StepComplete(Step::Structure));
        let began = Instant::now();
        let refusals = [
            host.count(&CountParams::new(address())).unwrap_err(),
            host.describe(&DescribeParams::new(address())).unwrap_err(),
            host.set(change(ApplyMode::Preview))
                .unwrap()
                .wait()
                .unwrap_err(),
            host.repair(norn_wire::RepairParams::new(address(), ApplyMode::Preview))
                .map_or_else(|refused| refused, |pending| pending.wait().unwrap_err()),
        ];
        for refusal in &refusals {
            assert_eq!(
                refusal.code(),
                &ReasonCode::HostEntryNotReady,
                "{refusal:?}"
            );
        }
        assert!(host.begin_read(&name()).is_err());
        assert!(
            began.elapsed() < Duration::from_secs(2),
            "shapes with no early capability refuse at once"
        );
        assert_eq!(host.read_evidence().settle_waits, 0);
        // An apply is queued behind the build and runs after Ready.
        let pending = host.set(change(ApplyMode::Apply)).unwrap();
        assert!(
            std::fs::read_to_string(fixture.root.join("doc03.md"))
                .unwrap()
                .contains("rank: 3\n")
        );
        probe.release();
        pending
    });
    let applied = pending.wait().unwrap();
    assert!(matches!(
        applied.report,
        norn_wire::ApplyReport::Applied { .. }
    ));
    let written = std::fs::read_to_string(fixture.root.join("doc03.md")).unwrap();
    assert!(
        written.contains("33") && !written.contains("rank: 3\n"),
        "{written}"
    );
}

/// Contract item 17.
#[test]
fn refusal_names_still_being_indexed_and_shares_one_deadline() {
    let fixture = Fixture::new("early-refusal-deadline");
    let bound = Duration::from_millis(600);
    let probe = Probe::new([LoadingBoundary::StepComplete(Step::Names)]);
    let host = fixture.host_bound(probe.clone(), bound);
    std::thread::scope(|_| {
        let _release = Release(&probe);
        let _demand = host.demand(&name()).unwrap();
        probe.wait(LoadingBoundary::StepComplete(Step::Names));
        let expiries = host.read_evidence().settle_expiries;
        let began = Instant::now();
        let refusal = host.find(&ranked("1")).unwrap_err();
        let waited = began.elapsed();
        assert_still_indexing(&refusal);
        assert!(waited >= bound, "{waited:?}");
        assert!(waited < bound + Duration::from_secs(2), "{waited:?}");
        assert_eq!(host.read_evidence().settle_expiries, expiries + 1);
        probe.release();
    });
    ready(&host);
}

/// Contract item 18: one accepted fact withdraws every capability, waiters
/// refuse at once, the build reaches Ready and the store equals a from-scratch one.
#[test]
fn a_fact_holds_vault_wide_capabilities_until_it_is_derived() {
    let fixture = Fixture::new("early-fact-holds");
    let probe = Probe::new([LoadingBoundary::StepComplete(Step::Content)]);
    let ops = fixture.ops(probe.clone());
    let account = ops.account();
    let host = fixture.host_over(ops, norn_host::READ_SETTLE_BOUND);
    std::thread::scope(|scope| {
        let _release = Release(&probe);
        let _demand = host.demand(&name()).unwrap();
        probe.wait(LoadingBoundary::StepComplete(Step::Content));
        assert!(host.find(&ranked("5")).is_ok());
        std::fs::write(fixture.root.join("late.md"), "---\nrank: 40\n---\n# Late\n").unwrap();
        fact_recorded(&account, 0);
        // The public read observes acceptance before construction resumes.
        let start = host.read_evidence().settle_waits;
        let waiting = spawn_find(scope, &host, ranked("40"));
        waits(&host, start + 1);
        assert!(waiting.try_recv().is_err());
        probe.release();
        assert!(waiting.recv_timeout(BUDGET.work()).unwrap().is_ok());
    });
    ready(&host);
    drop(host);
    project(&fixture.store_path(), &fixture.root).assert_equivalent(
        &from_scratch(&fixture, "reference"),
        "accepted build facts reconcile",
    );
}

/// Holds the first early read at its established snapshot, keeping its
/// interest in its target live, and lets every later read through.
#[derive(Default)]
struct FirstRead {
    /// Whether the first read is held, and whether it was let go.
    state: Mutex<(bool, bool)>,
    moved: Condvar,
}
impl FirstRead {
    fn wait(&self) {
        wait_until("the first early read established", BUDGET, || {
            if self.state.lock().unwrap().0 {
                Observed::Met(())
            } else {
                Observed::pending("no early read established")
            }
        })
        .unwrap();
    }
    fn release(&self) {
        self.state.lock().unwrap_or_else(|p| p.into_inner()).1 = true;
        self.moved.notify_all();
    }
    fn established(&self) {
        let mut state = self.state.lock().unwrap();
        if state.0 {
            return;
        }
        state.0 = true;
        self.moved.notify_all();
        let (_released, timeout) = self
            .moved
            .wait_timeout_while(state, Duration::from_secs(60), |state| !state.1)
            .unwrap();
        assert!(!timeout.timed_out(), "the first read was never released");
    }
}
struct ReleaseFirstRead<'a>(&'a FirstRead);
impl Drop for ReleaseFirstRead<'_> {
    fn drop(&mut self) {
        self.0.release();
    }
}
struct FirstReadProbe {
    construction: Arc<Probe>,
    read: Arc<FirstRead>,
}
impl LoadingProbe for FirstReadProbe {
    fn at(&self, boundary: LoadingBoundary) {
        match boundary {
            LoadingBoundary::QueryEstablished => self.read.established(),
            LoadingBoundary::QueryBuilt => {}
            _ => self.construction.at(boundary),
        }
    }
}

/// Contract item 18: a Get target the build pulled ahead and published ready
/// is withdrawn by a fact that arrives while a live interest holds it.
#[test]
fn a_fact_at_a_ready_target_withdraws_it_and_one_elsewhere_does_not() {
    let fixture = Fixture::new("early-fact-withdraws-ready-target");
    let construction = Probe::new([
        LoadingBoundary::StepComplete(Step::Names),
        LoadingBoundary::CanonicalPage,
        LoadingBoundary::TargetReady,
        LoadingBoundary::StepComplete(Step::Content),
    ]);
    let first = Arc::new(FirstRead::default());
    let ops = fixture.ops(Arc::new(FirstReadProbe {
        construction: construction.clone(),
        read: first.clone(),
    }));
    let account = ops.account();
    let host = fixture.host_over(ops, norn_host::READ_SETTLE_BOUND);
    std::thread::scope(|scope| {
        let _release = Release(&construction);
        let _release_read = ReleaseFirstRead(&first);
        let _demand = host.demand(&name()).unwrap();
        construction.wait(LoadingBoundary::StepComplete(Step::Names));
        construction.release();
        construction.wait(LoadingBoundary::CanonicalPage);
        let held = spawn_get(scope, &host, get("/first"));
        waits(&host, 1);
        construction.release();
        // The build holds after publishing the target ready, and the read it
        // admitted holds its interest in that target at its snapshot.
        construction.wait(LoadingBoundary::TargetReady);
        first.wait();
        let admitted = host.get(&get("/first")).unwrap();
        assert!(
            warming(&admitted.answer.reading.trust),
            "the ready target admits a Get before the content step ends"
        );
        std::fs::write(fixture.root.join("late.md"), "---\nrank: 40\n---\n# Late\n").unwrap();
        fact_recorded(&account, 0);
        // Rooted reads stay independent even when a backend reports a broad fact.
        let untouched = host.get(&get("/first")).unwrap();
        for column in ["body", "headings"] {
            assert_eq!(
                json!(&untouched.answer.report)["document"][column],
                json!(&admitted.answer.report)["document"][column]
            );
        }
        construction.release();
        construction.wait(LoadingBoundary::StepComplete(Step::Content));
        let untouched = host.get(&get("/first")).unwrap();
        for column in ["body", "headings"] {
            assert_eq!(
                json!(&untouched.answer.report)["document"][column],
                json!(&admitted.answer.report)["document"][column]
            );
        }
        let before = account.read().build_facts_recorded;
        std::fs::write(fixture.root.join("first.md"), "# Changed\n").unwrap();
        fact_recorded(&account, before);
        let start = host.read_evidence().settle_waits;
        let waiting = spawn_get(scope, &host, get("/first"));
        waits(&host, start + 1);
        first.release();
        // A fact touching the established target rejects its stale result.
        assert_still_indexing(&held.recv_timeout(BUDGET.work()).unwrap().unwrap_err());
        construction.release();
        assert_changed_body_and_heading(waiting.recv_timeout(BUDGET.work()).unwrap());
    });
    ready(&host);
    wait_until("the late document is served", BUDGET, || {
        match host.find(&ranked("40")) {
            Ok(found) if found.answer.report.page.rows.len() == 1 => Observed::Met(()),
            other => Observed::pending(format!("{other:?}")),
        }
    })
    .unwrap();
}

fn assert_changed_body_and_heading(answer: GetAnswer) {
    let report = json!(&answer.unwrap().answer.report);
    assert_eq!(
        report["document"]["body"]["value"]["text"], "# Changed\n",
        "{report}"
    );
    assert_eq!(
        report["document"]["headings"]["value"]["items"][0]["text"], "Changed",
        "{report}"
    );
}

#[test]
fn a_requeued_pulled_ahead_target_waits_for_its_new_structure() {
    let fixture = Fixture::new("requeued-ready-target-structure");
    let construction = Probe::new([
        LoadingBoundary::StepComplete(Step::Names),
        LoadingBoundary::CanonicalPage,
        LoadingBoundary::TargetReady,
        LoadingBoundary::Requeued,
    ]);
    let first = Arc::new(FirstRead::default());
    let ops = fixture.ops(Arc::new(FirstReadProbe {
        construction: construction.clone(),
        read: first.clone(),
    }));
    let account = ops.account();
    let host = fixture.host_over(ops, norn_host::READ_SETTLE_BOUND);
    std::thread::scope(|scope| {
        let _release = Release(&construction);
        let _release_read = ReleaseFirstRead(&first);
        let _demand = host.demand(&name()).unwrap();
        construction.wait(LoadingBoundary::StepComplete(Step::Names));
        construction.release();
        construction.wait(LoadingBoundary::CanonicalPage);
        let held = spawn_get(scope, &host, get("/first"));
        waits(&host, 1);
        construction.release();
        construction.wait(LoadingBoundary::TargetReady);
        first.wait();
        std::fs::write(fixture.root.join("first.md"), "# Changed\n").unwrap();
        fact_recorded(&account, 0);
        construction.release();
        construction.wait(LoadingBoundary::Requeued);
        let before = host.read_evidence().settle_waits;
        let second = spawn_get(scope, &host, get("/first"));
        let premature = wait_until(
            "the second Get answers or waits for structure",
            BUDGET,
            || match second.try_recv() {
                Ok(answer) => Observed::Met(Some(answer)),
                Err(mpsc::TryRecvError::Empty) if host.read_evidence().settle_waits > before => {
                    Observed::Met(None)
                }
                Err(mpsc::TryRecvError::Empty) => Observed::pending("Get has not met construction"),
                Err(error) => panic!("second Get disconnected: {error}"),
            },
        )
        .unwrap();
        if let Some(answer) = premature {
            assert_changed_body_and_heading(answer);
            panic!("the target still owes structure at the requeue boundary");
        }
        construction.release();
        assert_changed_body_and_heading(second.recv_timeout(BUDGET.work()).unwrap());
        first.release();
        assert_still_indexing(&held.recv_timeout(BUDGET.work()).unwrap().unwrap_err());
    });
    ready(&host);
}

/// Contract item 18: an overflow withdraws every capability, and Ready follows
/// through the ordinary overflow recovery.
#[test]
fn an_overflow_withdraws_every_capability_and_ready_still_follows() {
    let fixture = Fixture::new("early-overflow");
    std::fs::create_dir(fixture.root.join("overflow")).unwrap();
    let probe = Probe::new([LoadingBoundary::StepComplete(Step::Structure)]);
    let host = fixture.host_over(
        ProductionEntryOps::new(
            fixture.dirs.clone(),
            ProductionPolicy::new(128, 128).unwrap(),
        )
        .with_loading_probe(probe.clone()),
        norn_host::READ_SETTLE_BOUND,
    );
    std::thread::scope(|_| {
        let _release = Release(&probe);
        let _demand = host.demand(&name()).unwrap();
        probe.wait(LoadingBoundary::StepComplete(Step::Structure));
        assert!(host.get(&get("first")).is_ok());
        // The watcher's authored cardinality boundary, not a timing load.
        for index in 0..=norn_fs::watch::DIRTY_ROOT_CAP {
            std::fs::write(
                fixture.root.join(format!("overflow/file-{index:05}.txt")),
                "file",
            )
            .unwrap();
        }
        wait_until("an overflow withdraws early reads", BUDGET, || {
            if host.get(&get("first")).is_err() && host.find(&find()).is_err() {
                Observed::Met(())
            } else {
                Observed::pending("early authority still stands")
            }
        })
        .unwrap();
        probe.release();
    });
    ready(&host);
    assert!(host.get(&get("first")).is_ok());
}

/// Contract item 18: an established early hold survives healthy promotion.
#[test]
fn established_holds_survive_promotion() {
    let fixture = Fixture::new("early-promotion");
    let construction = Probe::new([LoadingBoundary::StepComplete(Step::Structure)]);
    let reads = Probe::new([LoadingBoundary::QueryEstablished]);
    let host = fixture.host(Arc::new(SplitProbe {
        construction: construction.clone(),
        reads: reads.clone(),
    }));
    let early = std::thread::scope(|scope| {
        let _release_construction = Release(&construction);
        let _release_reads = Release(&reads);
        let _demand = host.demand(&name()).unwrap();
        construction.wait(LoadingBoundary::StepComplete(Step::Structure));
        let answer = spawn_get(scope, &host, get("first"));
        reads.wait(LoadingBoundary::QueryEstablished);
        construction.release();
        ready(&host);
        reads.release();
        answer.recv_timeout(BUDGET.work()).unwrap().unwrap()
    });
    assert!(warming(&early.answer.reading.trust));
    assert_eq!(
        json!(&early.answer.report),
        relations_loading(json!(host.get(&get("first")).unwrap().answer.report))
    );
}

/// Contract item 18: a contradiction known at exposure refuses an early answer,
/// a source-dependent refusal included: an ambiguity the rows derived, and a
/// bound read under the declaration the withdrawn build pinned.
#[test]
fn a_known_contradiction_fails_the_final_check_for_success_and_data_derived_refusals() {
    for case in ["success", "ambiguous", "unreadable bound"] {
        let fixture = Fixture::new("early-final-check");
        if case == "ambiguous" {
            std::fs::create_dir(fixture.root.join("one")).unwrap();
            std::fs::create_dir(fixture.root.join("two")).unwrap();
            std::fs::rename(
                fixture.root.join("first.md"),
                fixture.root.join("one/first.md"),
            )
            .unwrap();
            std::fs::write(fixture.root.join("two/first.md"), "Other first.\n").unwrap();
        }
        let construction = Probe::new([LoadingBoundary::StepComplete(Step::Structure)]);
        let reads = Probe::new([LoadingBoundary::QueryBuilt]);
        let host = fixture.host(Arc::new(SplitProbe {
            construction: construction.clone(),
            reads: reads.clone(),
        }));
        std::thread::scope(|scope| {
            let _release_construction = Release(&construction);
            let _release_reads = Release(&reads);
            let _demand = host.demand(&name()).unwrap();
            construction.wait(LoadingBoundary::StepComplete(Step::Structure));
            let (sent, result) = mpsc::sync_channel(1);
            let host_ref = &host;
            scope.spawn(move || {
                let error = if case == "unreadable bound" {
                    host_ref.find(&ranked("not-a-number")).map(|_| ())
                } else {
                    host_ref.get(&get("first")).map(|_| ())
                };
                let _ = sent.send(error);
            });
            reads.wait(LoadingBoundary::QueryBuilt);
            std::fs::rename(&fixture.root, fixture.scratch.join("moved-root")).unwrap();
            assert!(
                host.find(&find()).is_err(),
                "the synchronous root proof records the contradiction"
            );
            reads.release();
            let error = result.recv_timeout(BUDGET.work()).unwrap().unwrap_err();
            assert_still_indexing(&error);
            construction.release();
        });
    }
}

/// Contract item 18: an established early Get finds its root gone only at
/// final exposure, where no other request or build step checked it. Moving an
/// unwatched ancestor leaves the covered directory and its files untouched.
#[test]
fn an_established_get_discovers_root_loss_only_at_final_exposure() {
    let mut fixture = Fixture::new("early-final-root-check");
    let ancestor = fixture.scratch.join("ancestor");
    std::fs::create_dir(&ancestor).unwrap();
    let registered = ancestor.join("vault");
    std::fs::rename(&fixture.root, &registered).unwrap();
    fixture.root = registered;
    let construction = Probe::new([LoadingBoundary::StepComplete(Step::Structure)]);
    let reads = Probe::new([LoadingBoundary::QueryBuilt]);
    let host = fixture.host(Arc::new(SplitProbe {
        construction: construction.clone(),
        reads: reads.clone(),
    }));
    std::thread::scope(|scope| {
        let _release_construction = Release(&construction);
        let _release_reads = Release(&reads);
        let _demand = host.demand(&name()).unwrap();
        construction.wait(LoadingBoundary::StepComplete(Step::Structure));
        let result = spawn_get(scope, &host, get("first"));
        reads.wait(LoadingBoundary::QueryBuilt);
        std::fs::rename(&ancestor, fixture.scratch.join("moved-ancestor")).unwrap();
        reads.release();
        assert_still_indexing(&result.recv_timeout(BUDGET.work()).unwrap().unwrap_err());
        construction.release();
    });
}

/// Contract item 19: a build whose registered root is lost after its watcher
/// authority was captured begins no construction, so a first read waiting on
/// the provisional attach is refused at once, never left to expire.
#[test]
fn a_root_lost_before_the_build_begins_refuses_its_waiter() {
    let fixture = Fixture::new("early-declined-binding");
    let registered = fixture.scratch.join("registered");
    std::os::unix::fs::symlink(&fixture.root, &registered).unwrap();
    let probe = Probe::new([
        LoadingBoundary::AuthorityCaptured,
        LoadingBoundary::CanonicalPage,
    ]);
    norn_fs::watch::settle_prior_changes(fixture.scratch.root(), BUDGET.work()).unwrap();
    let host = Host::new(
        RegistryRead::from_entries([Registration::new(
            name(),
            VaultRoot::new(&registered).unwrap(),
        )]),
        fixture.ops(probe.clone()),
        LifecyclePolicy {
            idle_after: Duration::from_secs(7200),
            worker_slots: 1,
            watch_poll_interval: Duration::from_millis(25),
            read_settle_bound: norn_host::READ_SETTLE_BOUND,
        },
    )
    .unwrap();
    std::thread::scope(|scope| {
        let _release = Release(&probe);
        let result = spawn_get(scope, &host, get("first"));
        probe.wait(LoadingBoundary::AuthorityCaptured);
        waits(&host, 1);
        // Coverage keeps its physical root; only the registered spelling is lost.
        std::fs::remove_file(&registered).unwrap();
        probe.release();
        probe.wait(LoadingBoundary::CanonicalPage);
        let error = result.recv_timeout(BUDGET.work()).unwrap().unwrap_err();
        assert_eq!(error.code(), &ReasonCode::HostEntryNotReady);
        assert_eq!(host.read_evidence().settle_expiries, 0);
        probe.release();
    });
}

/// Holds every early read at its establishment while armed, counting them,
/// so reads can be made to occupy the reader budget.
#[derive(Default)]
struct Readers {
    state: Mutex<(bool, usize)>,
    moved: Condvar,
}
impl Readers {
    fn arm(&self) {
        *self.state.lock().unwrap() = (true, 0);
    }
    fn release(&self) {
        self.state.lock().unwrap_or_else(|p| p.into_inner()).0 = false;
        self.moved.notify_all();
    }
    fn wait(&self, count: usize) {
        wait_until("early reads established", BUDGET, || {
            let now = self.state.lock().unwrap().1;
            if now >= count {
                Observed::Met(())
            } else {
                Observed::pending(format!("{now} < {count}"))
            }
        })
        .unwrap();
    }
    fn established(&self) {
        let mut state = self.state.lock().unwrap();
        if !state.0 {
            return;
        }
        state.1 += 1;
        self.moved.notify_all();
        let (_released, timeout) = self
            .moved
            .wait_timeout_while(state, Duration::from_secs(60), |state| state.0)
            .unwrap();
        assert!(!timeout.timed_out(), "the read barrier was never released");
    }
}
struct ReleaseReaders<'a>(&'a Readers);
impl Drop for ReleaseReaders<'_> {
    fn drop(&mut self) {
        self.0.release();
    }
}
struct CapacityProbe {
    construction: Arc<Probe>,
    readers: Arc<Readers>,
}
impl LoadingProbe for CapacityProbe {
    fn at(&self, boundary: LoadingBoundary) {
        match boundary {
            LoadingBoundary::QueryEstablished => self.readers.established(),
            LoadingBoundary::QueryBuilt => {}
            _ => self.construction.at(boundary),
        }
    }
}

/// Contract item 17: an early read waiting for reader capacity keeps its one
/// deadline, refusing as capacity unavailable when it runs out; after Ready it
/// is served canonically; and a withdrawal wakes it to refuse at once, before
/// the reads holding the capacity give it back.
#[test]
fn early_capacity_waits_keep_the_deadline_and_reselect_after_promotion_or_withdrawal() {
    for transition in ["deadline", "promotion", "withdrawal"] {
        let fixture = Fixture::new("early-capacity");
        let construction = Probe::new([LoadingBoundary::StepComplete(Step::Structure)]);
        let readers = Arc::new(Readers::default());
        let mut policy = ProductionPolicy::new(2, 2).unwrap();
        policy.read_budget = norn_store::ReadBudgetPolicy::new(2, Duration::ZERO).unwrap();
        // The closing checkpoint waits out the held reads' snapshots before
        // Ready, so a promotion is given the room to outlast that wait.
        let bound = match transition {
            "deadline" => Duration::from_millis(500),
            "promotion" => Duration::from_secs(30),
            _ => norn_host::READ_SETTLE_BOUND,
        };
        let host = fixture.host_over(
            ProductionEntryOps::new(fixture.dirs.clone(), policy).with_loading_probe(Arc::new(
                CapacityProbe {
                    construction: construction.clone(),
                    readers: readers.clone(),
                },
            )),
            bound,
        );
        std::thread::scope(|scope| {
            let _release_construction = Release(&construction);
            let _release_reads = ReleaseReaders(&readers);
            let _demand = host.demand(&name()).unwrap();
            construction.wait(LoadingBoundary::StepComplete(Step::Structure));
            let initial = host.get(&get("first")).unwrap();
            readers.arm();
            let held_one = spawn_get(scope, &host, get("first"));
            let held_two = spawn_get(scope, &host, get("first"));
            readers.wait(2);
            assert_eq!(host.read_budget_census().live_connections, 2);
            let waiting = spawn_get(scope, &host, get("first"));
            wait_until("a third early read waits for capacity", BUDGET, || {
                if host.read_budget_census().waiting_reads == 1 {
                    Observed::Met(())
                } else {
                    Observed::pending("the third read has not queued")
                }
            })
            .unwrap();
            match transition {
                "deadline" => {
                    assert_eq!(
                        waiting
                            .recv_timeout(BUDGET.work())
                            .unwrap()
                            .unwrap_err()
                            .code(),
                        &ReasonCode::HostReaderUnavailable
                    );
                    assert_eq!(host.read_budget_census().live_connections, 2);
                    readers.release();
                    held_one.recv_timeout(BUDGET.work()).unwrap().unwrap();
                    held_two.recv_timeout(BUDGET.work()).unwrap().unwrap();
                    construction.release();
                    ready(&host);
                }
                "promotion" => {
                    construction.release();
                    ready(&host);
                    assert_eq!(host.read_budget_census().live_connections, 2);
                    readers.release();
                    held_one.recv_timeout(BUDGET.work()).unwrap().unwrap();
                    held_two.recv_timeout(BUDGET.work()).unwrap().unwrap();
                    let promoted = waiting.recv_timeout(BUDGET.work()).unwrap().unwrap();
                    assert_eq!(promoted.answer.reading.trust, TrustState::Ready);
                    assert_eq!(promoted.answer.reading.epoch, initial.answer.reading.epoch);
                    assert_eq!(
                        json!(&initial.answer.report),
                        relations_loading(json!(&promoted.answer.report))
                    );
                }
                "withdrawal" => {
                    std::fs::rename(&fixture.root, fixture.scratch.join("moved-root")).unwrap();
                    assert!(host.find(&find()).is_err());
                    assert_eq!(
                        waiting
                            .recv_timeout(BUDGET.work())
                            .unwrap()
                            .unwrap_err()
                            .code(),
                        &ReasonCode::HostEntryNotReady,
                        "a withdrawal wakes a capacity wait before held reads release"
                    );
                    readers.release();
                    assert!(held_one.recv_timeout(BUDGET.work()).unwrap().is_err());
                    assert!(held_two.recv_timeout(BUDGET.work()).unwrap().is_err());
                    construction.release();
                }
                _ => unreachable!(),
            }
            assert!(host.read_budget_census().peak_connections <= 2);
            assert_eq!(
                host.read_evidence().gate_retakes_within_establishing_holds,
                0
            );
        });
    }
}

/// Contract item 17: an early read whose bound runs out before it holds a
/// connection refuses as reader capacity unavailable, as a canonical read
/// does: the capacity and open wait is a resource wait, not a wait for the
/// build's capability.
#[test]
fn an_early_read_past_its_bound_before_a_connection_refuses_for_capacity() {
    let fixture = Fixture::new("early-capacity-expired");
    // Every capability stands here, so the read below is admitted and what
    // it lacks is a connection, which a bound of zero leaves no time to open.
    let probe = Probe::new([LoadingBoundary::Published(Step::FullTextComplete)]);
    let host = fixture.host_ticking(
        fixture.ops(probe.clone()),
        Duration::ZERO,
        Duration::from_secs(3600),
    );
    std::thread::scope(|_| {
        let _release = Release(&probe);
        let _demand = host.demand(&name()).unwrap();
        probe.wait(LoadingBoundary::Published(Step::FullTextComplete));
        let refused = host.get(&get("first")).unwrap_err();
        assert_eq!(
            refused.code(),
            &ReasonCode::HostReaderUnavailable,
            "{refused:?}"
        );
        assert!(
            format!("{refused:?}").contains("reader capacity was unavailable"),
            "{refused:?}"
        );
        probe.release();
    });
    ready(&host);
}

/// Contract item 20: damage only an early read meets closes the build's
/// capabilities at once, and the attach that installs that store publishes the
/// damage in place of `Ready` and rebuilds it through the claim path.
#[test]
fn early_damage_takes_the_rebuild_path() {
    let fixture = Fixture::new("early-damage");
    // The closing transaction has run: the build writes and reads no more of
    // the store, so the damage below is met by the early read alone.
    let probe = Probe::new([LoadingBoundary::Published(Step::FullTextComplete)]);
    let ops = fixture.ops(probe.clone());
    let account = ops.account();
    let host = fixture.host_over(ops, norn_host::READ_SETTLE_BOUND);
    std::thread::scope(|_| {
        let _release = Release(&probe);
        let _demand = host.demand(&name()).unwrap();
        probe.wait(LoadingBoundary::Published(Step::FullTextComplete));
        damage_every_table(&fixture.store_path());
        let refused = host.find(&ranked("2")).unwrap_err();
        assert_eq!(
            refused.code(),
            &ReasonCode::HostEntryNotReady,
            "{refused:?}"
        );
        let began = Instant::now();
        assert!(host.find(&ranked("2")).is_err());
        assert!(
            began.elapsed() < Duration::from_secs(2),
            "the build's reads are closed"
        );
        probe.release();
    });
    // The first `Ready` is the rebuild's: the attach published none over the
    // store the read found damaged.
    ready(&host);
    assert_eq!(account.read().from_zero_builds, 2);
    assert!(host.get(&get("first")).is_ok());
    assert_eq!(
        host.find(&ranked("2"))
            .unwrap()
            .answer
            .report
            .page
            .rows
            .len(),
        1
    );
}

/// Contract item 8: an inline rung 3 after a failed first build withdraws the
/// first construction's capabilities, and its own bind the new store's epoch.
#[test]
fn an_inline_rung_three_publishes_under_its_own_epoch() {
    let fixture = Fixture::new("early-inline-rung-three");
    let probe = Probe::new([
        LoadingBoundary::StepComplete(Step::Content),
        LoadingBoundary::StepComplete(Step::Structure),
    ]);
    let ops = fixture.ops(probe.clone());
    let account = ops.account();
    let host = fixture.host_over(ops, norn_host::READ_SETTLE_BOUND);
    std::thread::scope(|_| {
        let _release = Release(&probe);
        let _demand = host.demand(&name()).unwrap();
        probe.wait(LoadingBoundary::StepComplete(Step::Content));
        let first_epoch = host.find(&ranked("2")).unwrap().answer.reading.epoch;
        // The build, not a read, meets this damage, and rung 3 rebuilds inline.
        damage_every_table(&fixture.store_path());
        probe.release();
        probe.wait(LoadingBoundary::StepComplete(Step::Structure));
        let rebuilt = host.get(&get("first")).unwrap();
        assert!(warming(&rebuilt.answer.reading.trust));
        assert_ne!(rebuilt.answer.reading.epoch, first_epoch);
        assert_eq!(
            probe
                .seen()
                .iter()
                .filter(|seen| **seen == LoadingBoundary::AuthorityCaptured)
                .count(),
            2,
            "the rebuild captured its own authority"
        );
        probe.release();
    });
    ready(&host);
    assert_eq!(account.read().from_zero_builds, 2);
}

/// Contract item 8, the rebuild leg: a damage-triggered rebuild of a served
/// vault publishes warming once its construction begins and admits reads as a
/// first build does, so a Get issued mid-content is pulled ahead and answers
/// before the content step ends, and the rebuilt store equals one built from
/// scratch.
#[test]
fn a_rebuild_under_a_damage_verdict_serves_early_reads_once_its_construction_begins() {
    let fixture = Fixture::new("early-rebuild-leg");
    let probe = Probe::new([]);
    let ops = fixture.ops(probe.clone());
    let account = ops.account();
    let host = fixture.host_over(ops, norn_host::READ_SETTLE_BOUND);
    let _demand = host.demand(&name()).unwrap();
    ready(&host);
    {
        let mut state = probe.state.lock().unwrap();
        state.seen.clear();
        state.next = [
            LoadingBoundary::StepComplete(Step::Names),
            LoadingBoundary::CanonicalPage,
            LoadingBoundary::RequestsDrained,
        ]
        .into_iter()
        .collect();
    }
    damage_every_table(&fixture.store_path());
    let early = std::thread::scope(|scope| {
        let _release = Release(&probe);
        wait_until(
            "a read meets the damage and the rebuild begins",
            BUDGET,
            || {
                let _ = host.count(&CountParams::new(address()));
                if probe.state.lock().unwrap().current.is_some() {
                    Observed::Met(())
                } else {
                    Observed::pending("no rebuild yet")
                }
            },
        )
        .unwrap();
        probe.wait(LoadingBoundary::StepComplete(Step::Names));
        assert!(
            warming(&host.state(&name()).unwrap()),
            "the rebuild publishes warming once its construction begins"
        );
        probe.release();
        // The first content chunk is written; `first.md` is still a name.
        probe.wait(LoadingBoundary::CanonicalPage);
        let settled = host.read_evidence().settle_waits;
        let answer = spawn_get(scope, &host, get("first"));
        waits(&host, settled + 1);
        probe.release();
        probe.wait(LoadingBoundary::RequestsDrained);
        let early = answer.recv_timeout(BUDGET.work()).unwrap().unwrap();
        assert!(
            !probe
                .seen()
                .contains(&LoadingBoundary::Published(Step::Content)),
            "answered after the content step"
        );
        assert!(probe.seen().contains(&LoadingBoundary::TargetReady));
        probe.release();
        early
    });
    ready(&host);
    assert!(warming(&early.answer.reading.trust));
    let ready_get = host.get(&get("first")).unwrap();
    assert_eq!(
        json!(&early.answer.report),
        relations_loading(json!(&ready_get.answer.report))
    );
    assert_eq!(account.read().from_zero_builds, 2);
    drop(host);
    project(&fixture.store_path(), &fixture.root).assert_equivalent(
        &from_scratch(&fixture, "reference"),
        "a rebuild that served early reads derives the same store",
    );
}

/// Contract item 13: a fact during a damage rebuild holds only touched Gets;
/// untouched Gets also keep answering across its pre-Ready reconcile (C7).
#[test]
fn a_fact_during_a_damage_rebuild_withdraws_only_what_it_touches() {
    let fixture = Fixture::new("early-rebuild-path-fact");
    let probe = Probe::new([]);
    let ops = fixture.ops(probe.clone());
    let account = ops.account();
    let host = fixture.host_over(ops, norn_host::READ_SETTLE_BOUND);
    let _demand = host.demand(&name()).unwrap();
    ready(&host);
    {
        let mut state = probe.state.lock().unwrap();
        state.seen.clear();
        state.next = [
            LoadingBoundary::StepComplete(Step::Names),
            LoadingBoundary::FullTextBatch,
            LoadingBoundary::Reconciling,
        ]
        .into_iter()
        .collect();
    }
    damage_every_table(&fixture.store_path());
    let (early, reconciling, changed) = std::thread::scope(|scope| {
        let _release = Release(&probe);
        wait_until(
            "a read meets the damage and the rebuild begins",
            BUDGET,
            || {
                let _ = host.count(&CountParams::new(address()));
                if probe.state.lock().unwrap().current.is_some() {
                    Observed::Met(())
                } else {
                    Observed::pending("no rebuild yet")
                }
            },
        )
        .unwrap();
        probe.wait(LoadingBoundary::StepComplete(Step::Names));
        assert!(warming(&host.state(&name()).unwrap()));
        probe.release();
        // Both targets have completed their own step 3 before the fact.
        probe.wait(LoadingBoundary::FullTextBatch);
        assert!(host.get(&get("/doc00")).is_ok());
        let before = account.read().build_facts_recorded;
        std::fs::write(fixture.root.join("doc00.md"), "# Changed\n[[first]]\n").unwrap();
        fact_recorded(&account, before);
        let start = host.read_evidence().settle_waits;
        let waiting = spawn_get(scope, &host, get("/doc00"));
        waits(&host, start + 1);
        let early = host.get(&get("/first")).unwrap();
        assert!(warming(&early.answer.reading.trust));
        assert!(waiting.try_recv().is_err());
        probe.release();
        probe.wait(LoadingBoundary::Reconciling);
        let reconciling = host.get(&get("/first")).unwrap();
        assert!(warming(&reconciling.answer.reading.trust));
        assert!(waiting.try_recv().is_err());
        probe.release();
        let changed = waiting.recv_timeout(BUDGET.work()).unwrap().unwrap();
        (early, reconciling, changed)
    });
    ready(&host);
    let ready_get = host.get(&get("/first")).unwrap();
    assert_eq!(
        json!(&early.answer.report),
        relations_loading(json!(&ready_get.answer.report))
    );
    assert_eq!(
        json!(&reconciling.answer.report),
        json!(&early.answer.report)
    );
    assert_eq!(
        json!(&changed.answer.report),
        json!(&host.get(&get("/doc00")).unwrap().answer.report)
    );
    assert_eq!(account.read().from_zero_builds, 2);
    drop(host);
    project(&fixture.store_path(), &fixture.root).assert_equivalent(
        &from_scratch(&fixture, "reference"),
        "a damage rebuild with a path fact derives the final tree",
    );
}

/// Contract items 8 and 20: damage an early read meets on a damage rebuild's
/// own construction is handed to another rebuild, as on a first build, so the
/// first `Ready` is never published over the store the read found damaged.
#[test]
fn early_damage_during_a_damage_rebuild_takes_another_rebuild() {
    let fixture = Fixture::new("early-damage-during-rebuild");
    let probe = Probe::new([]);
    let ops = fixture.ops(probe.clone());
    let account = ops.account();
    let host = fixture.host_over(ops, norn_host::READ_SETTLE_BOUND);
    let _demand = host.demand(&name()).unwrap();
    ready(&host);
    {
        let mut state = probe.state.lock().unwrap();
        state.seen.clear();
        state.next = [LoadingBoundary::Published(Step::FullTextComplete)]
            .into_iter()
            .collect();
    }
    damage_every_table(&fixture.store_path());
    std::thread::scope(|_| {
        let _release = Release(&probe);
        wait_until(
            "a read meets the damage and the rebuild begins",
            BUDGET,
            || {
                let _ = host.count(&CountParams::new(address()));
                if probe.state.lock().unwrap().current.is_some() {
                    Observed::Met(())
                } else {
                    Observed::pending("no rebuild yet")
                }
            },
        )
        .unwrap();
        probe.wait(LoadingBoundary::Published(Step::FullTextComplete));
        assert!(warming(&host.state(&name()).unwrap()));
        // The rebuild's closing transaction has run: the build writes and
        // reads no more of the store, so the early read alone meets this.
        damage_every_table(&fixture.store_path());
        let refused = host.find(&ranked("2")).unwrap_err();
        assert_eq!(
            refused.code(),
            &ReasonCode::HostEntryNotReady,
            "{refused:?}"
        );
        probe.release();
    });
    ready(&host);
    assert_eq!(
        account.read().from_zero_builds,
        3,
        "the first Ready after the rebuild is a third build's"
    );
    assert_eq!(
        host.find(&ranked("2"))
            .unwrap()
            .answer
            .report
            .page
            .rows
            .len(),
        1
    );
}

/// Contract item 20: an early read's environmental failure is not damage.
#[test]
fn an_early_environmental_failure_is_not_damage() {
    use std::os::unix::fs::PermissionsExt;
    let fixture = Fixture::new("early-environmental");
    let probe = Probe::new([LoadingBoundary::StepComplete(Step::Content)]);
    let ops = fixture.ops(probe.clone());
    let account = ops.account();
    let host = fixture.host_over(ops, norn_host::READ_SETTLE_BOUND);
    std::thread::scope(|_| {
        let _release = Release(&probe);
        let _demand = host.demand(&name()).unwrap();
        probe.wait(LoadingBoundary::StepComplete(Step::Content));
        let store = fixture.store_path();
        let mode = std::fs::metadata(&store).unwrap().permissions().mode();
        std::fs::set_permissions(&store, std::fs::Permissions::from_mode(0o000)).unwrap();
        let refused = host.find(&ranked("2"));
        std::fs::set_permissions(&store, std::fs::Permissions::from_mode(mode)).unwrap();
        let refused = refused.unwrap_err();
        assert_eq!(
            refused.code(),
            &ReasonCode::HostReaderUnavailable,
            "{refused:?}"
        );
        assert!(host.find(&ranked("2")).is_ok(), "the capability stands");
        probe.release();
    });
    ready(&host);
    assert_eq!(account.read().from_zero_builds, 1);
}

/// Contract item 26: a recovery that builds over the empty store an
/// undeclarable attach left publishes as a first build does.
#[test]
fn an_empty_store_recovery_publishes() {
    let fixture = Fixture::with_schema("early-empty-recovery", "version: 1\nfields: [oops\n");
    let probe = Probe::new([LoadingBoundary::StepComplete(Step::Structure)]);
    let host = fixture.host(probe.clone());
    std::thread::scope(|_| {
        let _release = Release(&probe);
        let _demand = host.demand(&name()).unwrap();
        wait_until(
            "the undeclarable attach withholds trust",
            BUDGET,
            || match host.state(&name()) {
                Err(error) if error.code() == &ReasonCode::HostEntryUntrusted => Observed::Met(()),
                other => Observed::pending(format!("{other:?}")),
            },
        )
        .unwrap();
        assert!(fixture.store_path().exists());
        std::fs::write(
            fixture.root.join(".norn/schema.yaml"),
            "version: 1\nfields:\n  rank: { type: number }\n",
        )
        .unwrap();
        // The recovery watches anew, and FSEvents can deliver this write
        // after it attaches: an accepted fact that withdraws its build's
        // authority before structure publishes.
        norn_fs::watch::settle_prior_changes(fixture.scratch.root(), BUDGET.work()).unwrap();
        wait_until("the recovery's build reaches structure", BUDGET, || {
            let _ = host.demand(&name());
            if probe
                .seen()
                .contains(&LoadingBoundary::Published(Step::Structure))
            {
                Observed::Met(())
            } else {
                Observed::pending("no recovery build yet")
            }
        })
        .unwrap();
        probe.wait(LoadingBoundary::StepComplete(Step::Structure));
        let early = host.get(&get("first")).unwrap();
        assert!(warming(&early.answer.reading.trust));
        probe.release();
    });
    ready(&host);
}

/// Contract items 20 and 26: a recovery whose from-zero build an early read
/// found damaged publishes that damage in place of what it would otherwise
/// publish, and hands on to the rebuild it owes before any other work runs
/// over the damaged store.
#[test]
fn an_empty_store_recovery_over_damage_an_early_read_met_rebuilds() {
    let fixture = Fixture::with_schema("early-damaged-recovery", "version: 1\nfields: [oops\n");
    // The recovery's closing transaction has run, so the damage below is met
    // by the early read alone; the next build to capture authority is the
    // rebuild's, held there so what ran before it can be read.
    let probe = Probe::new([
        LoadingBoundary::Published(Step::FullTextComplete),
        LoadingBoundary::AuthorityCaptured,
    ]);
    let ops = fixture.ops(probe.clone());
    let account = ops.account();
    let host = fixture.host_over(ops, norn_host::READ_SETTLE_BOUND);
    std::thread::scope(|_| {
        let _release = Release(&probe);
        let _demand = host.demand(&name()).unwrap();
        wait_until(
            "the undeclarable attach withholds trust",
            BUDGET,
            || match host.state(&name()) {
                Err(error) if error.code() == &ReasonCode::HostEntryUntrusted => Observed::Met(()),
                other => Observed::pending(format!("{other:?}")),
            },
        )
        .unwrap();
        std::fs::write(
            fixture.root.join(".norn/schema.yaml"),
            "version: 1\nfields:\n  rank: { type: number }\n",
        )
        .unwrap();
        // The recovery watches anew, and FSEvents can deliver this write
        // after it attaches: an accepted fact that withdraws its build's
        // authority before the damage is met.
        norn_fs::watch::settle_prior_changes(fixture.scratch.root(), BUDGET.work()).unwrap();
        wait_until("the recovery's build closes", BUDGET, || {
            let _ = host.demand(&name());
            if probe
                .seen()
                .contains(&LoadingBoundary::Published(Step::FullTextComplete))
            {
                Observed::Met(())
            } else {
                Observed::pending("no recovery build yet")
            }
        })
        .unwrap();
        probe.wait(LoadingBoundary::Published(Step::FullTextComplete));
        damage_every_table(&fixture.store_path());
        let refused = host.find(&ranked("2")).unwrap_err();
        assert_eq!(
            refused.code(),
            &ReasonCode::HostEntryNotReady,
            "{refused:?}"
        );
        probe.release();
        probe.wait(LoadingBoundary::AuthorityCaptured);
        // The rebuild has captured its authority and built nothing yet, and
        // the entry already answers with the damage the early read met.
        let damaged = host.state(&name()).unwrap_err();
        assert_eq!(
            damaged.code(),
            &ReasonCode::HostEntryUntrusted,
            "{damaged:?}"
        );
        assert_eq!(account.read().rebuilds_run, 1);
        probe.release();
    });
    // The first `Ready` is the rebuild's: the recovery published none over
    // the store the read found damaged.
    ready(&host);
    assert_eq!(account.read().rebuilds_run, 1);
    assert!(host.get(&get("first")).is_ok());
    assert_eq!(
        host.find(&ranked("2"))
            .unwrap()
            .answer
            .report
            .page
            .rows
            .len(),
        1
    );
}

// ---------------------------------------------------------------------------
// Get pull-ahead and commit-on-pending.
// ---------------------------------------------------------------------------

/// Whether `early` is `ready` with relation parts read as loading: a link's
/// targets null beside `health: loading`, and a findings column or page
/// `unknown(loading)`. A page cursor names its snapshot, so either both pages
/// continue or neither does. Every other part is equal.
fn modulo_loading(early: &serde_json::Value, ready: &serde_json::Value) -> bool {
    use serde_json::Value;
    let loading = serde_json::json!({"state": "unknown", "reason": "loading"});
    match (early, ready) {
        (Value::Object(early_object), Value::Object(ready_object)) => {
            let link = early_object.get("health") == Some(&Value::from("loading"))
                && early_object.get("targets") == Some(&Value::Null)
                && ready_object.contains_key("targets");
            early_object.len() == ready_object.len()
                && early_object.iter().all(|(key, value)| {
                    let Some(standing) = ready_object.get(key) else {
                        return false;
                    };
                    (link && (key == "health" || key == "targets"))
                        || (*value == loading && standing.get("state").is_some())
                        // A cursor names its snapshot's generation.
                        || (key == "next" && value.is_string() && standing.is_string())
                        || modulo_loading(value, standing)
                })
        }
        (Value::Array(early_items), Value::Array(ready_items)) => {
            early_items.len() == ready_items.len()
                && early_items
                    .iter()
                    .zip(ready_items)
                    .all(|(early, ready)| modulo_loading(early, ready))
        }
        _ => early == ready,
    }
}

/// An early answer, or its refusal, against the same request's Ready answer:
/// equal modulo loading parts, or the not-ready refusal.
macro_rules! assert_exact {
    ($what:expr, $early:expr, $ready:expr $(,)?) => {{
        let what = $what;
        match ($early, $ready) {
            (Ok(early), Ok(ready)) => {
                assert!(warming(&early.answer.reading.trust), "{what}");
                let (early, ready) = (json!(&early.answer.report), json!(&ready.answer.report));
                assert!(
                    modulo_loading(&early, &ready),
                    "{what}: early {early:#} against ready {ready:#}"
                );
            }
            (Err(early), _) if early.code() == &ReasonCode::HostEntryNotReady => {
                assert_still_indexing(early)
            }
            (Err(early), Err(ready)) => assert_eq!(json!(early), json!(ready), "{what}"),
            (early, ready) => panic!(
                "{what}: early {:?} against ready {:?}",
                early.as_ref().err(),
                ready.as_ref().err()
            ),
        }
    }};
}

/// The first page cursor an answer carries, for its continuation.
fn next_cursor(value: &serde_json::Value) -> Option<norn_wire::Cursor> {
    match value {
        serde_json::Value::Object(object) => object
            .get("next")
            .filter(|next| !next.is_null())
            .map(|next| serde_json::from_value(next.clone()).unwrap())
            .or_else(|| object.values().find_map(next_cursor)),
        serde_json::Value::Array(items) => items.iter().find_map(next_cursor),
        _ => None,
    }
}

/// Contract item 7: a Get issued mid-content is pulled ahead at the next chunk
/// boundary and answers before the content step ends, equal to the same Get
/// at Ready except its loading parts; its continuation reads the same.
#[test]
fn a_requested_document_answers_before_content_ends() {
    let fixture = Fixture::new("early-pull-ahead");
    let probe = Probe::new([
        LoadingBoundary::StepComplete(Step::Names),
        LoadingBoundary::CanonicalPage,
        LoadingBoundary::RequestsDrained,
    ]);
    let ops = fixture.ops(probe.clone());
    let account = ops.account();
    let host = fixture.host_over(ops, norn_host::READ_SETTLE_BOUND);
    let tags = get("first")
        .with_collection(norn_wire::CollectionSelector::Tags)
        .with_limit(1);
    let (early, page, continued) = std::thread::scope(|scope| {
        let _release = Release(&probe);
        let _demand = host.demand(&name()).unwrap();
        probe.wait(LoadingBoundary::StepComplete(Step::Names));
        probe.release();
        // The first content chunk is written; `first.md` is still a name.
        probe.wait(LoadingBoundary::CanonicalPage);
        let answer = spawn_get(scope, &host, get("first"));
        let page = spawn_get(scope, &host, tags.clone());
        waits(&host, 2);
        probe.release();
        probe.wait(LoadingBoundary::RequestsDrained);
        // Both shapes shared one slot and answer while the build is held.
        let early = answer.recv_timeout(BUDGET.work()).unwrap().unwrap();
        let page = page.recv_timeout(BUDGET.work()).unwrap().unwrap();
        assert!(
            !probe
                .seen()
                .contains(&LoadingBoundary::Published(Step::Content)),
            "answered after the content step"
        );
        assert!(probe.seen().contains(&LoadingBoundary::TargetReady));
        // Both were admitted under the names step, their target pulled ahead.
        assert_eq!(
            host.read_evidence().early_reads_served,
            norn_host::EarlyReadsServed {
                names: 2,
                ..Default::default()
            }
        );
        probe.release();
        // A ready target lives only while an interest holds it: the
        // continuation queues again and is answered at a later drain.
        let cursor = next_cursor(&json!(&page.answer.report));
        let continued = cursor.map(|cursor| host.get(&tags.clone().with_after(cursor)));
        (early, page, continued)
    });
    ready(&host);
    assert!(warming(&early.answer.reading.trust));
    let ready_get = host.get(&get("first")).unwrap();
    assert_eq!(
        json!(&early.answer.report),
        relations_loading(json!(&ready_get.answer.report))
    );
    let ready_page = host.get(&tags).unwrap();
    assert_exact!(
        "a tags page",
        &Ok::<_, norn_wire::ErrorEnvelope>(page),
        &Ok::<_, norn_wire::ErrorEnvelope>(ready_page)
    );
    // The continuation reads the same rows, reporting at most that its
    // snapshot's generation moved since the early page.
    let continued = continued.expect("a tags page of one continues").unwrap();
    let continued = json!(&continued.answer.report);
    let cursor = next_cursor(&json!(&host.get(&tags).unwrap().answer.report)).unwrap();
    let ready_continued = json!(
        &host
            .get(&tags.clone().with_after(cursor))
            .unwrap()
            .answer
            .report
    );
    assert_eq!(
        continued["page"]["page"]["value"]["rows"],
        ready_continued["page"]["page"]["value"]["rows"]
    );
    assert!(
        continued["page"]["page"]["value"]["moved"]
            .as_array()
            .unwrap()
            .iter()
            .all(|moved| moved == "generation"),
        "{continued:#}"
    );
    // Its content and structure ran ahead once, each document read once.
    let evidence = account.read();
    assert_eq!(evidence.pull_ahead_documents, 2, "{evidence:?}");
    // Five steps, and at least the one target set published ready.
    assert!(evidence.construction_publications >= 6, "{evidence:?}");
    assert!(evidence.construction_commits_for_requests >= 1);
    assert_eq!(evidence.content_rows_written, DOCUMENTS);
    assert_eq!(evidence.documents_read_by_build, DOCUMENTS);
    // The continuation queues again unless Structure published first.
    assert!((1..=2).contains(&host.read_evidence().pull_ahead_requests));
}

/// Contract item 9: a by-name target ambiguous among provisional names answers
/// ambiguous only once every candidate passed step 2; a byte-quarantined
/// candidate leaves a unique answer; a class over one chunk, and a Get naming
/// an undeclared field, wait for vault-wide Structure.
#[test]
fn a_provisional_ambiguity_waits_for_its_candidates() {
    let fixture = Fixture::new("early-provisional-ambiguity");
    fixture.add("b/dup.md", "# B\n");
    fixture.add("c/dup.md", "# C\n");
    fixture.add("c/solo.md", "# Solo\n");
    fixture.add("d/solo.md", b"\xff\xfe not a document");
    for folder in ["x", "y", "z"] {
        fixture.add(&format!("{folder}/many.md"), "# Many\n");
    }
    let probe = Probe::new([
        LoadingBoundary::StepComplete(Step::Names),
        LoadingBoundary::StepComplete(Step::Content),
    ]);
    let host = fixture.host(probe.clone());
    let undeclared = get("solo").with_columns([Column::path(), Column::field("color")]);
    let (dup, solo, many, colored) = std::thread::scope(|scope| {
        let _release = Release(&probe);
        let _demand = host.demand(&name()).unwrap();
        probe.wait(LoadingBoundary::StepComplete(Step::Names));
        let dup = spawn_get(scope, &host, get("dup"));
        let solo = spawn_get(scope, &host, get("solo"));
        let many = spawn_get(scope, &host, get("many"));
        let colored = spawn_get(scope, &host, undeclared.clone());
        waits(&host, 4);
        probe.release();
        // Held before Structure publishes: only pulled-ahead targets answer.
        probe.wait(LoadingBoundary::StepComplete(Step::Content));
        let dup = dup.recv_timeout(BUDGET.work()).unwrap();
        let solo = solo.recv_timeout(BUDGET.work()).unwrap();
        assert!(
            many.try_recv().is_err(),
            "a class over one chunk was answered before vault-wide Structure"
        );
        assert!(
            colored.try_recv().is_err(),
            "an undeclared field was answered before vault-wide Structure"
        );
        probe.release();
        (
            dup,
            solo,
            many.recv_timeout(BUDGET.work()).unwrap(),
            colored.recv_timeout(BUDGET.work()).unwrap(),
        )
    });
    ready(&host);
    let ambiguous = dup.as_ref().unwrap_err();
    assert_eq!(
        ambiguous.code(),
        &ReasonCode::VaultAmbiguousTarget,
        "{ambiguous:?}"
    );
    assert_eq!(json!(ambiguous), json!(host.get(&get("dup")).unwrap_err()));
    assert_exact!("a unique answer", &solo, &host.get(&get("solo")));
    match &solo.as_ref().unwrap().answer.report {
        norn_wire::GetReport::Record { document, .. } => {
            assert_eq!(document.path.as_str(), "c/solo.md")
        }
        other => panic!("{other:?}"),
    }
    assert_exact!("a class over one chunk", &many, &host.get(&get("many")));
    assert_exact!("an undeclared field", &colored, &host.get(&undeclared));
    assert_eq!(
        many.unwrap_err().code(),
        &ReasonCode::VaultAmbiguousTarget,
        "three candidates"
    );
}

/// C3 amendment: exact-path precedence hides `folder/alpha.md` behind a
/// provisional `alpha.md`. Once step 2 quarantines `alpha.md`, `alpha` names
/// `folder/alpha.md` alone, and it is never answered before that document
/// passed step 2.
#[test]
fn an_exact_candidate_quarantined_exposes_its_suffix_candidate() {
    let fixture = Fixture::new("early-exact-quarantined");
    fixture.add("alpha.md", b"\xff\xfe not a document");
    fixture.add("folder/alpha.md", "---\nrank: 7\n---\n# Alpha\n[[doc01]]\n");
    let probe = Probe::new([
        LoadingBoundary::StepComplete(Step::Names),
        LoadingBoundary::RequestsDrained,
        LoadingBoundary::StepComplete(Step::Content),
    ]);
    let ops = fixture.ops(probe.clone());
    let account = ops.account();
    let host = fixture.host_over(ops, norn_host::READ_SETTLE_BOUND);
    let early = std::thread::scope(|scope| {
        let _release = Release(&probe);
        let _demand = host.demand(&name()).unwrap();
        probe.wait(LoadingBoundary::StepComplete(Step::Names));
        let answer = spawn_get(scope, &host, get("alpha"));
        waits(&host, 1);
        probe.release();
        // The first drain spends its chunk on `alpha.md` and then on the
        // candidate it exposed; the target is not ready until a later drain
        // runs that candidate's structure.
        probe.wait(LoadingBoundary::RequestsDrained);
        assert!(answer.try_recv().is_err(), "answered before its structure");
        assert!(account.read().pull_ahead_documents >= 2);
        probe.release();
        probe.wait(LoadingBoundary::StepComplete(Step::Content));
        let early = answer.recv_timeout(BUDGET.work()).unwrap();
        probe.release();
        early
    });
    ready(&host);
    let ready_get = host.get(&get("alpha"));
    assert_exact!("the exposed suffix candidate", &early, &ready_get);
    match &early.unwrap().answer.report {
        norn_wire::GetReport::Record { document, .. } => {
            assert_eq!(document.path.as_str(), "folder/alpha.md")
        }
        other => panic!("{other:?}"),
    }
}

/// Contract item 10: at most 256 distinct live targets are queued; duplicates
/// share a slot; an expired interest frees its slot; the 257th distinct
/// target gets no pull-ahead and still answers after vault-wide Structure.
#[test]
fn pending_requests_are_bounded_and_duplicates_share_a_slot() {
    const BOUND: usize = 256;
    let fixture = Fixture::new("early-bounded-queue");
    let probe = Probe::new([
        LoadingBoundary::StepComplete(Step::Names),
        LoadingBoundary::RequestsDrained,
    ]);
    let host = fixture.host_bound(probe.clone(), Duration::from_secs(6));
    let joined = |count: u64| {
        wait_until("interests joined", BUDGET, || {
            let seen = host.read_evidence().pull_ahead_requests;
            if seen == count {
                Observed::Met(())
            } else {
                Observed::pending(format!("{seen} != {count}"))
            }
        })
        .unwrap();
    };
    std::thread::scope(|scope| {
        let _release = Release(&probe);
        let _demand = host.demand(&name()).unwrap();
        probe.wait(LoadingBoundary::StepComplete(Step::Names));
        // Interests that expire release their slots, ready or not.
        let gone: Vec<_> = (0..BOUND)
            .map(|n| spawn_get(scope, &host, get(&format!("gone-{n:03}"))))
            .collect();
        joined(BOUND as u64);
        for answer in gone {
            assert_still_indexing(&answer.recv_timeout(BUDGET.work()).unwrap().unwrap_err());
        }
        let waited = host.read_evidence().settle_waits;
        let mut kept: Vec<_> = (0..BOUND)
            .map(|n| spawn_get(scope, &host, get(&format!("kept-{n:03}"))))
            .collect();
        // Duplicates share their target's slot.
        kept.extend((0..2).map(|_| spawn_get(scope, &host, get("kept-000"))));
        joined(2 * BOUND as u64);
        waits(&host, waited + BOUND as u64 + 2);
        let extra = spawn_get(scope, &host, get("extra"));
        wait_until("a request past the bound", BUDGET, || {
            match host.read_evidence().requests_past_bound {
                1 => Observed::Met(()),
                seen => Observed::pending(format!("{seen}")),
            }
        })
        .unwrap();
        probe.release();
        // The first drain readies every queued target; none needs a document.
        probe.wait(LoadingBoundary::RequestsDrained);
        for answer in kept {
            let refused = answer.recv_timeout(BUDGET.work()).unwrap().unwrap_err();
            assert_eq!(
                refused.code(),
                &ReasonCode::VaultUnknownTarget,
                "{refused:?}"
            );
        }
        assert!(
            extra.try_recv().is_err(),
            "the 257th target was pulled ahead"
        );
        probe.release();
        let refused = extra.recv_timeout(BUDGET.work()).unwrap().unwrap_err();
        assert_eq!(
            refused.code(),
            &ReasonCode::VaultUnknownTarget,
            "{refused:?}"
        );
    });
    // Once the answered reads release their slots, the 257th may join.
    let reads = host.read_evidence();
    assert!(
        (2 * BOUND as u64..=2 * BOUND as u64 + 1).contains(&reads.pull_ahead_requests),
        "{reads:?}"
    );
    assert_eq!(reads.requests_past_bound, 1);
    ready(&host);
}

/// Contract item 10: under a continuous stream of distinct requests the build
/// still reaches Ready, each drain pulls ahead at most one chunk, and the pass
/// advances a chunk between drains; no document is read or written twice.
#[test]
fn a_request_stream_cannot_starve_construction() {
    const BULK: u64 = 40;
    let fixture = Fixture::new("early-request-stream");
    for n in 0..BULK {
        fixture.add(
            &format!("bulk/n{n:02}.md"),
            format!(
                "---\nrank: {}\n---\n# Bulk {n}\n[[doc0{}]]\n",
                100 + n,
                n % 10
            ),
        );
    }
    let probe = Probe::new([]);
    let ops = fixture.ops(probe.clone());
    let account = ops.account();
    let host = fixture.host_over(ops, norn_host::READ_SETTLE_BOUND);
    let documents = DOCUMENTS + BULK;
    let stop = std::sync::atomic::AtomicBool::new(false);
    std::thread::scope(|scope| {
        let _demand = host.demand(&name()).unwrap();
        for lane in 0..4u64 {
            let (host, stop) = (&host, &stop);
            scope.spawn(move || {
                let mut n = BULK - 1 - lane;
                while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                    let _ = host.get(&get(&format!("n{n:02}")));
                    n = (n + BULK - 4) % BULK;
                }
            });
        }
        // The lanes stop however this scope ends, so a failure cannot hang it.
        struct Stop<'a>(&'a std::sync::atomic::AtomicBool);
        impl Drop for Stop<'_> {
            fn drop(&mut self) {
                self.0.store(true, std::sync::atomic::Ordering::Relaxed);
            }
        }
        let _stop = Stop(&stop);
        ready(&host);
    });
    let evidence = account.read();
    let seen = probe.seen();
    let drains = seen
        .iter()
        .filter(|point| **point == LoadingBoundary::RequestsDrained)
        .count() as u64;
    let pages = seen
        .iter()
        .filter(|point| **point == LoadingBoundary::CanonicalPage)
        .count() as u64;
    assert!(drains > 0, "{seen:?}");
    assert!(evidence.pull_ahead_documents <= 2 * drains, "{evidence:?}");
    // Drains run as each step's first group opens and after a chunk.
    assert!(drains <= pages + 2, "{drains} drains over {pages} chunks");
    assert_eq!(evidence.content_rows_written, documents);
    assert_eq!(evidence.structure_rows_written, documents);
    assert_eq!(evidence.documents_read_by_build, documents);
}

/// Contract item 11: with a request pending at a chunk boundary the group
/// commits before the target publishes, so the reader's snapshot holds its
/// document; with none pending a step commits as the pass alone does, at most
/// one transaction per 2,048 documents and the closing one.
#[test]
fn a_pending_request_commits_at_the_next_chunk_boundary() {
    let per_step = DOCUMENTS.div_ceil(2048) + 1;
    // No request: the pass's own commits. Each fixture holds the watcher
    // lease, so this one ends before the next begins.
    {
        let quiet = Fixture::new("early-commits-quiet");
        let probe = Probe::new([]);
        let ops = quiet.ops(probe.clone());
        let account = ops.account();
        let host = quiet.host_over(ops, norn_host::READ_SETTLE_BOUND);
        let _demand = host.demand(&name()).unwrap();
        ready(&host);
        let evidence = account.read();
        assert_eq!(evidence.construction_commits_for_requests, 0);
        assert_eq!(evidence.pull_ahead_documents, 0);
        assert!(evidence.content_step_commits <= per_step, "{evidence:?}");
        assert!(evidence.structure_step_commits <= per_step, "{evidence:?}");
        assert!(!probe.seen().contains(&LoadingBoundary::RequestsDrained));
    }
    // A pending request: one early commit, before the target publishes.
    let fixture = Fixture::new("early-commits-pending");
    let probe = Probe::new([
        LoadingBoundary::StepComplete(Step::Names),
        LoadingBoundary::CanonicalPage,
        LoadingBoundary::TargetReady,
    ]);
    let ops = fixture.ops(probe.clone());
    let account = ops.account();
    let host = fixture.host_over(ops, norn_host::READ_SETTLE_BOUND);
    let early = std::thread::scope(|scope| {
        let _release = Release(&probe);
        let _demand = host.demand(&name()).unwrap();
        probe.wait(LoadingBoundary::StepComplete(Step::Names));
        probe.release();
        probe.wait(LoadingBoundary::CanonicalPage);
        let answer = spawn_get(scope, &host, get("first"));
        waits(&host, 1);
        let before = account.read();
        probe.release();
        // Held after publication: the commit has already run.
        probe.wait(LoadingBoundary::TargetReady);
        let held = account.read();
        assert_eq!(
            held.construction_commits_for_requests - before.construction_commits_for_requests,
            1
        );
        assert_eq!(
            held.content_step_commits, before.content_step_commits,
            "step commits are counted when the step ends"
        );
        let early = answer.recv_timeout(BUDGET.work()).unwrap().unwrap();
        probe.release();
        early
    });
    ready(&host);
    // The reader's snapshot held the document's content and structure.
    assert_eq!(
        json!(&early.answer.report),
        relations_loading(json!(&host.get(&get("first")).unwrap().answer.report))
    );
    let evidence = account.read();
    assert_eq!(evidence.construction_commits_for_requests, 1);
    assert!(
        evidence.content_step_commits <= per_step + evidence.construction_commits_for_requests,
        "{evidence:?}"
    );
    assert!(evidence.structure_step_commits <= per_step, "{evidence:?}");
}

/// The requests an exactness sweep issues at each held boundary: every Get
/// shape over documents with links, tags, a broken document, an unlistable
/// directory and an ambiguous slug, and the Finds whose steps publish.
fn sweep_gets() -> Vec<(&'static str, GetParams)> {
    use norn_wire::CollectionSelector;
    vec![
        ("record", get("first")),
        (
            "projected",
            get("doc03").with_columns([Column::path(), Column::fields(), Column::body()]),
        ),
        (
            "declared field",
            get("doc04").with_columns([Column::field("rank")]),
        ),
        ("section", get("first#First")),
        ("block", get("first#^block")),
        (
            "links page",
            get("first").with_collection(CollectionSelector::Links),
        ),
        (
            "findings page",
            get("first").with_collection(CollectionSelector::Findings),
        ),
        (
            "tags page",
            get("first")
                .with_collection(CollectionSelector::Tags)
                .with_limit(1),
        ),
        ("broken", get("broken")),
        ("broken section", get("broken#Anything")),
        ("ambiguous", get("dup")),
        ("unknown", get("absent")),
        ("unlistable", get("locked/inner")),
        ("linked", get("doc05")),
    ]
}

fn sweep_finds() -> Vec<(&'static str, FindParams)> {
    vec![
        ("field", ranked("3")),
        ("path", find().with_predicates([Predicate::path("b/*")])),
        ("tag", find().with_predicates([Predicate::tag("group1")])),
        (
            "links",
            find().with_columns([Column::path(), Column::links(), Column::findings()]),
        ),
    ]
}

/// Contract item 21: at every published step and at a mid-chunk pull-ahead,
/// every request a build admits equals its Ready answer modulo loading parts.
#[test]
fn every_admitted_answer_equals_ready() {
    use std::os::unix::fs::PermissionsExt;
    let fixture = Fixture::new("early-exactness-sweep");
    fixture.add("b/dup.md", "# B dup\n[[doc02]] #group1\n");
    fixture.add("c/dup.md", "# C dup\n");
    fixture.add("broken.md", "# Broken\n");
    fixture.add("locked/inner.md", "# Inner\n");
    std::fs::set_permissions(
        fixture.root.join("broken.md"),
        std::fs::Permissions::from_mode(0o000),
    )
    .unwrap();
    std::fs::set_permissions(
        fixture.root.join("locked"),
        std::fs::Permissions::from_mode(0o000),
    )
    .unwrap();
    let points = [
        LoadingBoundary::StepComplete(Step::Names),
        LoadingBoundary::CanonicalPage,
        LoadingBoundary::RequestsDrained,
        LoadingBoundary::StepComplete(Step::Content),
        LoadingBoundary::StepComplete(Step::Structure),
        LoadingBoundary::Published(Step::Relations),
    ];
    let probe = Probe::new(points);
    // Chunks of eight leave each drain room for every queued target.
    let host = fixture.host_over(
        ProductionEntryOps::new(fixture.dirs.clone(), ProductionPolicy::new(2, 8).unwrap())
            .with_loading_probe(probe.clone()),
        Duration::from_secs(30),
    );
    let (gets, finds, pulled) = std::thread::scope(|scope| {
        let _release = Release(&probe);
        let _demand = host.demand(&name()).unwrap();
        let mut gets = Vec::new();
        let mut finds = Vec::new();
        let mut pulled = None;
        for point in points {
            probe.wait(point);
            if point == LoadingBoundary::CanonicalPage {
                // A Get waiting here is pulled ahead at a later drain.
                let joined = host.read_evidence().pull_ahead_requests;
                pulled = Some(spawn_get(scope, &host, get("doc09")));
                wait_until("the mid-content Get joins", BUDGET, || {
                    match host.read_evidence().pull_ahead_requests {
                        seen if seen > joined => Observed::Met(()),
                        seen => Observed::pending(format!("{seen}")),
                    }
                })
                .unwrap();
                probe.release();
                continue;
            }
            for (what, params) in sweep_gets() {
                gets.push((point, what, params.clone(), spawn_get(scope, &host, params)));
            }
            for (what, params) in sweep_finds() {
                finds.push((
                    point,
                    what,
                    params.clone(),
                    spawn_find(scope, &host, params),
                ));
            }
            if point == LoadingBoundary::StepComplete(Step::Content) {
                let answer = pulled
                    .as_ref()
                    .unwrap()
                    .recv_timeout(BUDGET.work())
                    .unwrap();
                pulled = Some({
                    let (send, receive) = mpsc::sync_channel(1);
                    send.send(answer).unwrap();
                    receive
                });
            }
            // Admitted answers arrive while the build is held here.
            std::thread::sleep(Duration::from_millis(200));
            probe.release();
        }
        let gets: Vec<_> = gets
            .into_iter()
            .map(|(point, what, params, answer)| {
                (
                    point,
                    what,
                    params,
                    answer.recv_timeout(BUDGET.work()).unwrap(),
                )
            })
            .collect();
        let finds: Vec<_> = finds
            .into_iter()
            .map(|(point, what, params, answer)| {
                (
                    point,
                    what,
                    params,
                    answer.recv_timeout(BUDGET.work()).unwrap(),
                )
            })
            .collect();
        (gets, finds, pulled.unwrap().recv().unwrap())
    });
    ready(&host);
    assert_exact!("pulled ahead mid-chunk", &pulled, &host.get(&get("doc09")));
    let mut admitted = 0;
    for (point, what, params, answer) in &gets {
        assert_exact!(&format!("{what} at {point:?}"), answer, &host.get(params));
        admitted += usize::from(
            answer
                .as_ref()
                .is_ok_and(|a| warming(&a.answer.reading.trust)),
        );
    }
    for (point, what, params, answer) in &finds {
        assert_exact!(&format!("{what} at {point:?}"), answer, &host.find(params));
        admitted += usize::from(
            answer
                .as_ref()
                .is_ok_and(|a| warming(&a.answer.reading.trust)),
        );
    }
    assert!(admitted > 0);
    std::fs::set_permissions(
        fixture.root.join("locked"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
}

/// The store a host attaching `fixture`'s tree with no early reader derives.
fn reference_store(fixture: &Fixture, label: &str) -> PathBuf {
    let dirs = ConfigDirs::new(
        fixture.scratch.join(format!("{label}-config")),
        fixture.scratch.join(format!("{label}-data")),
    )
    .unwrap();
    let path = dirs.derived_dir(&name()).join("store.sqlite3");
    let reference = Host::new(
        RegistryRead::from_entries([Registration::new(
            name(),
            VaultRoot::new(&fixture.root).unwrap(),
        )]),
        ProductionEntryOps::new(dirs, ProductionPolicy::new(2, 2).unwrap()),
        LifecyclePolicy {
            idle_after: Duration::from_secs(7200),
            worker_slots: 1,
            watch_poll_interval: Duration::from_millis(25),
            read_settle_bound: norn_host::READ_SETTLE_BOUND,
        },
    )
    .unwrap();
    let demand = reference.demand(&name()).unwrap();
    ready(&reference);
    drop(demand);
    drop(reference);
    path
}

/// Every document's path beside its row id: the identities step 1 assigned.
fn path_ids(path: &Path) -> Vec<(String, i64)> {
    let database = norn_db::open_read_only(path).adopted.unwrap();
    let mut statement = database
        .connection()
        .prepare("SELECT path, id FROM documents ORDER BY path")
        .unwrap();
    statement
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

fn semantic(path: &Path) -> serde_json::Value {
    norn_store::startup_audit::audit(path, |_, _, _| Ok(())).unwrap()["semantic_sha256"].clone()
}

/// Contract item 22: a build that pulled many requests ahead derives the
/// store an ordinary build derives — projection, semantic digest and the
/// path-to-id map — and still reads and writes each document once.
#[test]
fn a_pulled_ahead_build_equals_an_ordinary_build() {
    let fixture = Fixture::new("early-pulled-ahead-equals");
    fixture.add("b/dup.md", "# B dup\n[[doc02]]\n");
    fixture.add("c/dup.md", "# C dup\n");
    fixture.add("alpha.md", b"\xff\xfe not a document");
    fixture.add("folder/alpha.md", "# Alpha\n[[first]] #group1\n");
    let documents = DOCUMENTS + 3;
    let probe = Probe::new([LoadingBoundary::StepComplete(Step::Names)]);
    let ops = fixture.ops(probe.clone());
    let account = ops.account();
    let host = fixture.host_over(ops, norn_host::READ_SETTLE_BOUND);
    let targets: Vec<String> = (0..12)
        .rev()
        .map(|n| format!("doc{n:02}"))
        .chain(["first", "dup", "alpha", "folder/alpha"].map(String::from))
        .collect();
    std::thread::scope(|scope| {
        let _release = Release(&probe);
        let _demand = host.demand(&name()).unwrap();
        probe.wait(LoadingBoundary::StepComplete(Step::Names));
        let answers: Vec<_> = targets
            .iter()
            .map(|target| spawn_get(scope, &host, get(target)))
            .collect();
        waits(&host, targets.len() as u64);
        probe.release();
        for answer in answers {
            let _ = answer.recv_timeout(BUDGET.work()).unwrap();
        }
    });
    ready(&host);
    let evidence = account.read();
    assert!(evidence.pull_ahead_documents > 0, "{evidence:?}");
    assert_eq!(evidence.content_rows_written, documents, "{evidence:?}");
    assert_eq!(evidence.structure_rows_written, documents, "{evidence:?}");
    assert_eq!(
        evidence.documents_read_by_build,
        documents + 1,
        "{evidence:?}"
    );
    assert_eq!(evidence.document_opens, documents + 2, "{evidence:?}");
    drop(host);
    let reference = reference_store(&fixture, "reference");
    project(&fixture.store_path(), &fixture.root).assert_equivalent(
        &project(&reference, &fixture.root),
        "a pulled-ahead build derives the ordinary build's store",
    );
    assert_eq!(semantic(&fixture.store_path()), semantic(&reference));
    assert_eq!(path_ids(&fixture.store_path()), path_ids(&reference));
}

#[test]
fn an_untouched_get_keeps_answering_under_a_fact_elsewhere() {
    untouched_gets_at(&[Step::Structure]);
}

#[test]
fn a_get_after_relations_reads_link_health_loading_while_a_fact_stands() {
    untouched_gets_at(&[Step::Relations]);
}

fn untouched_gets_at(steps: &[Step]) {
    for &step in steps {
        let fixture = Fixture::new("untouched-build-get");
        let probe = Probe::new([LoadingBoundary::Published(step)]);
        let ops = fixture.ops(probe.clone());
        let account = ops.account();
        let host = fixture.host_over(ops, norn_host::READ_SETTLE_BOUND);
        std::thread::scope(|scope| {
            let _release = Release(&probe);
            let _demand = host.demand(&name()).unwrap();
            probe.wait(LoadingBoundary::Published(step));
            std::fs::write(fixture.root.join("late.md"), "# Late\n").unwrap();
            fact_recorded(&account, 0);
            let start = host.read_evidence().settle_waits;
            let waiting = spawn_find(scope, &host, find());
            waits(&host, start + 1);
            let answer = host.get(&get("/first")).unwrap();
            assert!(warming(&answer.answer.reading.trust));
            let report = serde_json::to_value(answer.answer.report).unwrap();
            assert_eq!(report, relations_loading(report.clone()));
            #[cfg(target_os = "linux")]
            assert!(host.get(&get("first")).is_ok());
            probe.release();
            assert!(waiting.recv_timeout(BUDGET.work()).unwrap().is_ok());
        });
        ready(&host);
    }
}

#[test]
fn a_get_whose_target_a_fact_touches_waits() {
    let fixture = Fixture::new("touched-build-get");
    let probe = Probe::new([LoadingBoundary::StepComplete(Step::Structure)]);
    let ops = fixture.ops(probe.clone());
    let account = ops.account();
    let host = fixture.host_over(ops, norn_host::READ_SETTLE_BOUND);
    std::thread::scope(|scope| {
        let _release = Release(&probe);
        let _demand = host.demand(&name()).unwrap();
        probe.wait(LoadingBoundary::StepComplete(Step::Structure));
        std::fs::write(fixture.root.join("first.md"), "# Changed\n").unwrap();
        fact_recorded(&account, 0);
        let start = host.read_evidence().settle_waits;
        let waiting = spawn_get(scope, &host, get("/first"));
        waits(&host, start + 1);
        assert!(waiting.try_recv().is_err());
        probe.release();
        assert!(waiting.recv_timeout(BUDGET.work()).unwrap().is_ok());
    });
    ready(&host);
}

#[test]
fn untouched_gets_answer_through_the_pre_ready_reconcile() {
    let fixture = Fixture::new("reconcile-build-get");
    let probe = Probe::new([LoadingBoundary::FullTextBatch, LoadingBoundary::Reconciling]);
    let ops = fixture.ops(probe.clone());
    let account = ops.account();
    let host = fixture.host_over(ops, norn_host::READ_SETTLE_BOUND);
    std::thread::scope(|scope| {
        let _release = Release(&probe);
        let _demand = host.demand(&name()).unwrap();
        probe.wait(LoadingBoundary::FullTextBatch);
        std::fs::write(fixture.root.join("late.md"), "# Late\n").unwrap();
        fact_recorded(&account, 0);
        let start = host.read_evidence().settle_waits;
        let waiting = spawn_find(scope, &host, find());
        waits(&host, start + 1);
        probe.release();
        probe.wait(LoadingBoundary::Reconciling);
        assert!(host.get(&get("/first")).is_ok());
        assert!(waiting.try_recv().is_err());
        probe.release();
        assert!(waiting.recv_timeout(BUDGET.work()).unwrap().is_ok());
    });
    ready(&host);
}

#[test]
fn facts_at_every_step_converge_to_from_scratch() {
    for point in [
        LoadingBoundary::StepComplete(Step::Names),
        LoadingBoundary::StepComplete(Step::Content),
        LoadingBoundary::StepComplete(Step::Structure),
        LoadingBoundary::LinkHealthChunk,
        LoadingBoundary::FullTextBatch,
    ] {
        let fixture = Fixture::new("build-fact-convergence");
        let probe = Probe::new([point]);
        let host = fixture.host(probe.clone());
        {
            let _release = Release(&probe);
            let _demand = host.demand(&name()).unwrap();
            probe.wait(point);
            std::fs::write(fixture.root.join("first.md"), "# Changed\n[[late]]\n").unwrap();
            std::fs::write(fixture.root.join("late.md"), "# Late\n").unwrap();
            std::fs::create_dir(fixture.root.join("new-folder")).unwrap();
            std::fs::rename(
                fixture.root.join("late.md"),
                fixture.root.join("new-folder/renamed.md"),
            )
            .unwrap();
            std::fs::write(fixture.root.join("deleted.md"), "# Removed\n").unwrap();
            std::fs::remove_file(fixture.root.join("deleted.md")).unwrap();
            norn_fs::watch::settle_prior_changes(fixture.scratch.root(), BUDGET.work()).unwrap();
            probe.release();
            ready(&host);
            wait_until("the renamed document is served", BUDGET, || {
                match host.get(&get("/new-folder/renamed")) {
                    Ok(_) => Observed::Met(()),
                    other => Observed::pending(format!("{other:?}")),
                }
            })
            .unwrap();
            ready(&host);
        }
        drop(host);
        project(&fixture.store_path(), &fixture.root).assert_equivalent(
            &from_scratch(&fixture, "reference"),
            &format!("fact convergence at {point:?}"),
        );
    }
}

#[test]
fn an_attach_right_after_edits_keeps_early_reads_for_untouched_paths() {
    let fixture = Fixture::new("attach-after-edits");
    norn_fs::watch::settle_prior_changes(fixture.scratch.root(), BUDGET.work()).unwrap();
    std::fs::write(fixture.root.join("recent.md"), "# Recent\n").unwrap();
    let probe = Probe::new([LoadingBoundary::StepComplete(Step::Structure)]);
    let host = fixture.host_unsettled(
        fixture.ops(probe.clone()),
        norn_host::READ_SETTLE_BOUND,
        Duration::from_millis(25),
    );
    {
        let _release = Release(&probe);
        let _demand = host.demand(&name()).unwrap();
        probe.wait(LoadingBoundary::StepComplete(Step::Structure));
        assert!(host.get(&get("/first")).is_ok());
        probe.release();
        ready(&host);
    }
    drop(host);
    project(&fixture.store_path(), &fixture.root).assert_equivalent(
        &from_scratch(&fixture, "reference"),
        "attach immediately after edits",
    );
}

#[test]
fn a_later_fact_fails_the_final_check_only_for_what_it_touches() {
    for (touched, known_relations) in [(false, false), (true, false), (false, true)] {
        let fixture = Fixture::new("build-final-fact");
        let point = LoadingBoundary::Published(if known_relations {
            Step::Relations
        } else {
            Step::Structure
        });
        let construction = Probe::new([point]);
        let first = Arc::new(FirstRead::default());
        let ops = fixture.ops(Arc::new(FirstReadProbe {
            construction: construction.clone(),
            read: first.clone(),
        }));
        let account = ops.account();
        let host = fixture.host_over(ops, norn_host::READ_SETTLE_BOUND);
        std::thread::scope(|scope| {
            let _release = Release(&construction);
            let _release_read = ReleaseFirstRead(&first);
            let _demand = host.demand(&name()).unwrap();
            construction.wait(point);
            let held = spawn_get(scope, &host, get("/first"));
            first.wait();
            std::fs::write(
                fixture
                    .root
                    .join(if touched { "first.md" } else { "late.md" }),
                "# Changed\n",
            )
            .unwrap();
            fact_recorded(&account, 0);
            first.release();
            let result = held.recv_timeout(BUDGET.work()).unwrap();
            if touched || known_relations {
                assert_still_indexing(&result.unwrap_err());
            } else {
                assert!(result.is_ok());
            }
            construction.release();
        });
        ready(&host);
    }
}

#[test]
fn a_request_waits_only_for_facts_it_met() {
    let fixture = Fixture::new("fixed-build-requirement");
    let probe = Probe::new([
        LoadingBoundary::StepComplete(Step::Names),
        LoadingBoundary::StepComplete(Step::Content),
    ]);
    let ops = fixture.ops(probe.clone());
    let account = ops.account();
    let host = fixture.host_over(ops, norn_host::READ_SETTLE_BOUND);
    std::thread::scope(|scope| {
        let _release = Release(&probe);
        let _demand = host.demand(&name()).unwrap();
        probe.wait(LoadingBoundary::StepComplete(Step::Names));
        let start = host.read_evidence().settle_waits;
        let waiting = spawn_find(scope, &host, find());
        waits(&host, start + 1);
        std::fs::write(fixture.root.join("late.md"), "# Later\n").unwrap();
        fact_recorded(&account, 0);
        probe.release();
        probe.wait(LoadingBoundary::StepComplete(Step::Content));
        assert!(
            waiting
                .recv_timeout(Duration::from_secs(2))
                .unwrap()
                .is_ok()
        );
        probe.release();
    });
    ready(&host);
}

#[test]
fn a_directory_fact_holds_suffix_gets_and_rooted_gets_beneath_it() {
    let fixture = Fixture::new("directory-build-requirement");
    let probe = Probe::new([LoadingBoundary::StepComplete(Step::Structure)]);
    let ops = fixture.ops(probe.clone());
    let account = ops.account();
    let host = fixture.host_over(ops, norn_host::READ_SETTLE_BOUND);
    std::thread::scope(|scope| {
        let _release = Release(&probe);
        let _demand = host.demand(&name()).unwrap();
        probe.wait(LoadingBoundary::StepComplete(Step::Structure));
        std::fs::create_dir(fixture.root.join("new-folder")).unwrap();
        fact_recorded(&account, 0);
        let start = host.read_evidence().settle_waits;
        let suffix = spawn_get(scope, &host, get("first"));
        let rooted = spawn_get(scope, &host, get("/new-folder/note"));
        waits(&host, start + 2);
        assert!(suffix.try_recv().is_err());
        assert!(rooted.try_recv().is_err());
        assert!(host.get(&get("/first")).is_ok());
        probe.release();
        assert!(suffix.recv_timeout(BUDGET.work()).unwrap().is_ok());
        let _ = rooted.recv_timeout(BUDGET.work()).unwrap();
    });
    ready(&host);
}

#[test]
fn a_schema_or_control_fact_withdraws_everything() {
    for path in [".norn/schema.yaml", norn_config::IN_VAULT_CONFIG_PATH] {
        let fixture = Fixture::new("control-build-withdrawal");
        let probe = Probe::new([LoadingBoundary::StepComplete(Step::Structure)]);
        let ops = fixture.ops(probe.clone());
        let account = ops.account();
        let host = fixture.host_over(ops, norn_host::READ_SETTLE_BOUND);
        {
            let _release = Release(&probe);
            let _demand = host.demand(&name()).unwrap();
            probe.wait(LoadingBoundary::StepComplete(Step::Structure));
            let bytes = if path.ends_with(".yaml") {
                "version: 1
fields:
  rank: { type: number }
"
            } else {
                ""
            };
            std::fs::write(fixture.root.join(path), bytes).unwrap();
            wait_until("control fact withdraws construction", BUDGET, || {
                if account.read().build_fact_withdrawals > 0 {
                    Observed::Met(())
                } else {
                    Observed::pending("control not accepted")
                }
            })
            .unwrap();
            assert_eq!(
                host.get(&get("/first")).unwrap_err().code(),
                &ReasonCode::HostEntryNotReady
            );
            probe.release();
            ready(&host);
        }
    }
}

#[test]
fn facts_derived_between_meeting_and_admission_produce_a_current_find() {
    let fixture = Fixture::new("meeting-admission-facts");
    let probe = Probe::new([
        LoadingBoundary::StepComplete(Step::Names),
        LoadingBoundary::TargetReady,
        LoadingBoundary::StepComplete(Step::Content),
    ]);
    let ops = fixture.ops(probe.clone());
    let account = ops.account();
    let host = fixture.host_over(ops, norn_host::READ_SETTLE_BOUND);
    std::thread::scope(|scope| {
        let _release = Release(&probe);
        let _demand = host.demand(&name()).unwrap();
        probe.wait(LoadingBoundary::StepComplete(Step::Names));
        let start = host.read_evidence().settle_waits;
        let getting = spawn_get(scope, &host, get("/doc00"));
        let finding = spawn_find(scope, &host, find());
        waits(&host, start + 2);
        probe.release();
        probe.wait(LoadingBoundary::TargetReady);
        fixture.add("doc00.md", "---\nrank: 100\n---\n# Changed zero\n");
        fact_recorded(&account, 0);
        let before = account.read().build_facts_recorded;
        fixture.add("doc05.md", "---\nrank: 500\n---\n# Changed five\n");
        fact_recorded(&account, before);
        probe.release();
        probe.wait(LoadingBoundary::StepComplete(Step::Content));
        let get_answer = getting.recv_timeout(Duration::from_secs(2)).unwrap();
        let find_answer = finding.recv_timeout(Duration::from_secs(2)).unwrap();
        let _ = get_answer;
        let answer = find_answer.unwrap();
        let rows = serde_json::to_value(&answer.answer.report).unwrap();
        let rows = rows.to_string();
        assert!(rows.contains("100") && rows.contains("500"), "{rows}");
        probe.release();
    });
    ready(&host);
}

#[test]
fn a_get_answers_a_target_fact_derived_after_its_meeting() {
    let fixture = Fixture::new("get-meeting-admission");
    let probe = Probe::new([
        LoadingBoundary::StepComplete(Step::Names),
        LoadingBoundary::StepComplete(Step::Content),
    ]);
    let ops = fixture.ops(probe.clone());
    let account = ops.account();
    let host = fixture.host_over(ops, norn_host::READ_SETTLE_BOUND);
    std::thread::scope(|scope| {
        let _release = Release(&probe);
        let _demand = host.demand(&name()).unwrap();
        probe.wait(LoadingBoundary::StepComplete(Step::Names));
        let start = host.read_evidence().settle_waits;
        let getting = spawn_get(scope, &host, get("/doc00"));
        waits(&host, start + 1);
        fixture.add("doc00.md", "---\nrank: 100\n---\n# Changed\n");
        fact_recorded(&account, 0);
        probe.release();
        probe.wait(LoadingBoundary::StepComplete(Step::Content));
        assert!(
            getting
                .recv_timeout(Duration::from_secs(2))
                .unwrap()
                .is_ok()
        );
        probe.release();
    });
    ready(&host);
}

#[test]
fn a_requeued_edit_answers_its_new_bytes_before_ready() {
    let fixture = Fixture::new("early-requeued-edit");
    let probe = Probe::new([
        LoadingBoundary::StepComplete(Step::Content),
        LoadingBoundary::StepComplete(Step::Structure),
    ]);
    let ops = fixture.ops(probe.clone());
    let account = ops.account();
    let host = fixture.host_over(ops, norn_host::READ_SETTLE_BOUND);
    let _release = Release(&probe);
    let _demand = host.demand(&name()).unwrap();
    probe.wait(LoadingBoundary::StepComplete(Step::Content));
    std::fs::write(
        fixture.root.join("doc05.md"),
        "---\nrank: 40\n---\n# Requeued\n",
    )
    .unwrap();
    fact_recorded(&account, 0);
    probe.release();
    probe.wait(LoadingBoundary::StepComplete(Step::Structure));
    assert!(
        host.find(&ranked("40")).is_ok(),
        "the build must derive the accepted fact before Ready"
    );
    assert!(host.get(&get("doc05")).is_ok());
    assert_eq!(account.read().requeued_documents, 1);
    probe.release();
    ready(&host);
    assert_eq!(account.read().documents_read_by_build, DOCUMENTS + 1);
    assert_eq!(account.read().document_opens, DOCUMENTS + 2);
    drop(host);
    project(&fixture.store_path(), &fixture.root)
        .assert_equivalent(&from_scratch(&fixture, "reference"), "requeue");
}

#[test]
fn name_changes_land_before_ready_and_step_five_facts_wait_for_reconcile() {
    let fixture = Fixture::new("requeue-names");
    fixture.add("fallback/created.md", "# Fallback\n");
    let probe = Probe::new([
        LoadingBoundary::StepComplete(Step::Content),
        LoadingBoundary::StepComplete(Step::Structure),
        LoadingBoundary::Published(Step::Relations),
        LoadingBoundary::FullTextBatch,
    ]);
    let ops = fixture.ops(probe.clone());
    let account = ops.account();
    let host = fixture.host_over(ops, norn_host::READ_SETTLE_BOUND);
    let _release = Release(&probe);
    let _demand = host.demand(&name()).unwrap();
    probe.wait(LoadingBoundary::StepComplete(Step::Content));
    fixture.add("created.md", "# Created\n");
    std::fs::remove_file(fixture.root.join("doc01.md")).unwrap();
    std::fs::rename(
        fixture.root.join("doc02.md"),
        fixture.root.join("renamed.md"),
    )
    .unwrap();
    wait_until("name facts accepted", BUDGET, || {
        if account.read().build_facts_recorded >= 4 {
            Observed::Met(())
        } else {
            Observed::pending("waiting for name facts")
        }
    })
    .unwrap();
    probe.release();
    probe.wait(LoadingBoundary::StepComplete(Step::Structure));
    let created = host.get(&get("created")).unwrap();
    let norn_wire::GetReport::Record { document, .. } = created.answer.report else {
        panic!("a document Get returns a record");
    };
    assert_eq!(
        document.path.as_str(),
        "created.md",
        "the new exact candidate wins"
    );
    assert!(host.get(&get("renamed")).is_ok());
    for target in ["doc01", "doc02"] {
        assert_eq!(
            host.get(&get(target)).unwrap_err().code(),
            &ReasonCode::VaultUnknownTarget
        );
    }
    probe.release();
    probe.wait(LoadingBoundary::Published(Step::Relations));
    let requeued = account.read().requeued_roots;
    let reports = account.read().build_facts_recorded;
    fixture.add("later.md", "# Later\n");
    fact_recorded(&account, reports);
    probe.release();
    probe.wait(LoadingBoundary::FullTextBatch);
    assert_eq!(account.read().requeued_roots, requeued);
    probe.release();
    ready(&host);
    wait_until("untaken fact derived by reconcile", BUDGET, || {
        let spent = account.read();
        if spent.documents_derived > spent.documents_read_by_build {
            Observed::Met(())
        } else {
            Observed::pending("reconcile evidence not published")
        }
    })
    .unwrap();
    let spent = account.read();
    assert_eq!(spent.requeued_documents, 2);
    assert_eq!(spent.documents_derived, spent.documents_read_by_build + 1);
    drop(host);
    project(&fixture.store_path(), &fixture.root).assert_equivalent(
        &from_scratch(&fixture, "reference"),
        "taken and untaken facts",
    );
}

/// Restore mode bits even if a boundary assertion fails.
struct RestoreMode(PathBuf, u32);
impl Drop for RestoreMode {
    fn drop(&mut self) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(self.1)).unwrap();
    }
}

#[test]
fn a_directory_relisted_during_construction_ends_its_mark_before_step_four() {
    use std::os::unix::fs::PermissionsExt;
    let fixture = Fixture::new("construction-relist");
    fixture.add("locked/inside.md", "# Restored\n[[doc00]]\n");
    let directory = fixture.root.join("locked");
    let restore = RestoreMode(directory.clone(), 0o755);
    std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o000)).unwrap();
    assert!(std::fs::read_dir(&directory).is_err());
    let probe = Probe::new([
        LoadingBoundary::StepComplete(Step::Names),
        LoadingBoundary::StepComplete(Step::Structure),
    ]);
    let ops = fixture.ops(probe.clone());
    let account = ops.account();
    let host = fixture.host_over(ops, norn_host::READ_SETTLE_BOUND);
    let _release = Release(&probe);
    let _demand = host.demand(&name()).unwrap();
    probe.wait(LoadingBoundary::StepComplete(Step::Names));
    drop(restore);
    fact_recorded(&account, 0);
    probe.release();
    probe.wait(LoadingBoundary::StepComplete(Step::Structure));
    let answer = host.get(&get("/locked/inside")).unwrap();
    assert!(
        serde_json::to_string(&answer.answer.report)
            .unwrap()
            .contains("Restored")
    );
    probe.release();
    ready(&host);
    drop(host);
    project(&fixture.store_path(), &fixture.root).assert_equivalent(
        &from_scratch(&fixture, "reference"),
        "directory relisted before Relations",
    );
}

#[test]
fn a_requeued_directory_that_still_denies_keeps_its_mark() {
    use std::os::unix::fs::PermissionsExt;
    let fixture = Fixture::new("construction-still-denied");
    fixture.add("locked/inside.md", "# Held\n[[doc00]]\n");
    let directory = fixture.root.join("locked");
    let _restore = RestoreMode(directory.clone(), 0o755);
    std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o000)).unwrap();
    assert!(std::fs::read_dir(&directory).is_err());
    let probe = Probe::new([LoadingBoundary::StepComplete(Step::Names)]);
    let ops = fixture.ops(probe.clone());
    let account = ops.account();
    let host = fixture.host_over(ops, norn_host::READ_SETTLE_BOUND);
    let _release = Release(&probe);
    let _demand = host.demand(&name()).unwrap();
    probe.wait(LoadingBoundary::StepComplete(Step::Names));
    // The parent's watch reports the directory's attributes; it still denies.
    std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o000)).unwrap();
    fact_recorded(&account, 0);
    probe.release();
    ready(&host);
    assert!(
        account.read().requeued_roots >= 1,
        "the build takes the fact"
    );
    drop(host);
    let built = project(&fixture.store_path(), &fixture.root);
    assert_eq!(
        built
            .findings()
            .iter()
            .filter(|finding| finding.kind == "directory/unreadable")
            .count(),
        1
    );
    assert!(built.document("locked/inside.md").is_none());
    built.assert_equivalent(
        &from_scratch(&fixture, "reference"),
        "a requeued directory that still denies",
    );
}

#[test]
fn a_document_breaking_on_requeue_becomes_names_only() {
    use std::os::unix::fs::PermissionsExt;
    let fixture = Fixture::new("construction-requeue-broken");
    let probe = Probe::new([
        LoadingBoundary::StepComplete(Step::Content),
        LoadingBoundary::StepComplete(Step::Structure),
    ]);
    let ops = fixture.ops(probe.clone());
    let account = ops.account();
    let host = fixture.host_over(ops, norn_host::READ_SETTLE_BOUND);
    let _release = Release(&probe);
    let _demand = host.demand(&name()).unwrap();
    probe.wait(LoadingBoundary::StepComplete(Step::Content));
    let path = fixture.root.join("doc05.md");
    let restore = RestoreMode(path.clone(), 0o644);
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();
    assert!(std::fs::read(&path).is_err());
    fact_recorded(&account, 0);
    probe.release();
    probe.wait(LoadingBoundary::StepComplete(Step::Structure));
    let answer = host.get(&get("/doc05")).unwrap();
    let report = serde_json::to_string(&answer.answer.report).unwrap();
    assert!(
        report.contains("unreadable") && report.contains("document/unreadable"),
        "{report}"
    );
    probe.release();
    ready(&host);
    drop(host);
    project(&fixture.store_path(), &fixture.root)
        .assert_equivalent(&from_scratch(&fixture, "reference"), "broken requeue");
    drop(restore);
}

#[test]
fn requeued_documents_run_ahead_of_the_next_content_page() {
    let fixture = Fixture::new("requeue-before-page");
    let probe = Probe::new([
        LoadingBoundary::StepComplete(Step::Names),
        LoadingBoundary::CanonicalPage,
        LoadingBoundary::Requeued,
    ]);
    let ops = fixture.ops(probe.clone());
    let account = ops.account();
    let host = fixture.host_over(ops, norn_host::READ_SETTLE_BOUND);
    let _release = Release(&probe);
    let _demand = host.demand(&name()).unwrap();
    probe.wait(LoadingBoundary::StepComplete(Step::Names));
    probe.release();
    probe.wait(LoadingBoundary::CanonicalPage);
    let opened = account.read().documents_read_by_build;
    fixture.add("doc00.md", "# Ahead\n");
    fact_recorded(&account, 0);
    probe.release();
    probe.wait(LoadingBoundary::Requeued);
    assert_eq!(account.read().documents_read_by_build, opened + 1);
    assert_eq!(account.read().requeued_documents, 1);
    probe.release();
    ready(&host);
}

#[test]
fn an_overflow_mid_requeue_stops_taking_and_converges() {
    let fixture = Fixture::new("overflow-mid-requeue");
    std::fs::create_dir(fixture.root.join("overflow")).unwrap();
    let probe = Probe::new([
        LoadingBoundary::StepComplete(Step::Content),
        LoadingBoundary::RequeueStarted,
        LoadingBoundary::StepComplete(Step::Structure),
    ]);
    let ops = fixture.ops(probe.clone());
    let account = ops.account();
    let host = fixture.host_over(ops, norn_host::READ_SETTLE_BOUND);
    let _release = Release(&probe);
    let _demand = host.demand(&name()).unwrap();
    probe.wait(LoadingBoundary::StepComplete(Step::Content));
    fixture.add("doc00.md", "# Requeued\n");
    fact_recorded(&account, 0);
    probe.release();
    probe.wait(LoadingBoundary::RequeueStarted);
    for index in 0..=norn_fs::watch::DIRTY_ROOT_CAP {
        fixture.add(&format!("overflow/file-{index:05}.txt"), "file");
    }
    wait_until("coverage withdrawn during requeue", BUDGET, || {
        if account.read().build_fact_withdrawals > 0 {
            Observed::Met(())
        } else {
            Observed::pending("authority stands")
        }
    })
    .unwrap();
    probe.release();
    probe.wait(LoadingBoundary::StepComplete(Step::Structure));
    assert_eq!(account.read().requeued_roots, 1);
    assert!(host.get(&get("/first")).is_err());
    probe.release();
    ready(&host);
    drop(host);
    project(&fixture.store_path(), &fixture.root).assert_equivalent(
        &from_scratch(&fixture, "reference"),
        "overflow during requeue",
    );
}

#[test]
fn a_symbolic_link_met_on_requeue_keeps_its_advisory() {
    let fixture = Fixture::new("requeue-link-advisory");
    let (advisories, rewalks) = advisories_after_a_change_at(
        &fixture,
        LoadingBoundary::StepComplete(Step::Content),
        |root| std::os::unix::fs::symlink("doc00.md", root.join("link.md")).unwrap(),
    );
    assert!(advises_link(&advisories, "link.md"), "{advisories:?}");
    assert_eq!(rewalks, 0, "an added link never re-walks the vault");
    assert_eq!(advisories, reference_advisories(&fixture));
}

#[test]
fn requeued_link_advisories_keep_the_whole_walk_order() {
    let fixture = Fixture::new("requeue-link-advisory");
    std::fs::create_dir_all(fixture.root.join("a")).unwrap();
    for path in ["a-b.md", "a/x.md"] {
        std::os::unix::fs::symlink(fixture.root.join("doc00.md"), fixture.root.join(path)).unwrap();
    }
    let (advisories, rewalks) = advisories_after_a_change_at(
        &fixture,
        LoadingBoundary::StepComplete(Step::Content),
        |root| std::os::unix::fs::symlink("doc00.md", root.join("link.md")).unwrap(),
    );
    assert!(advises_link(&advisories, "link.md"), "{advisories:?}");
    assert_eq!(rewalks, 0, "an added link never re-walks the vault");
    assert_eq!(advisories, reference_advisories(&fixture));
}

#[test]
fn requeued_link_advisories_retain_the_whole_walk_first_sixteen() {
    let fixture = Fixture::new("requeue-link-advisory");
    std::fs::create_dir_all(fixture.root.join("a")).unwrap();
    for path in std::iter::once("a-b.md".to_owned()).chain((0..16).map(|n| format!("a/x{n:02}.md")))
    {
        std::os::unix::fs::symlink(fixture.root.join("doc00.md"), fixture.root.join(&path))
            .unwrap();
    }
    let (advisories, rewalks) = advisories_after_a_change_at(
        &fixture,
        LoadingBoundary::StepComplete(Step::Content),
        |root| std::os::unix::fs::symlink("doc00.md", root.join("a-c.md")).unwrap(),
    );
    assert!(advises_link(&advisories, "a-c.md"), "{advisories:?}");
    assert_eq!(rewalks, 0, "an added link never re-walks the vault");
    assert_eq!(advisories, reference_advisories(&fixture));
}

#[test]
fn link_advisories_requeued_during_step_four_equal_from_scratch() {
    let fixture = Fixture::new("relations-requeue-link-advisory");
    std::fs::create_dir_all(fixture.root.join("a")).unwrap();
    for path in std::iter::once("a-b.md".to_owned()).chain((0..16).map(|n| format!("a/x{n:02}.md")))
    {
        std::os::unix::fs::symlink(fixture.root.join("doc00.md"), fixture.root.join(&path))
            .unwrap();
    }
    let probe = Probe::new([
        LoadingBoundary::LinkHealthChunk,
        LoadingBoundary::Published(Step::Relations),
    ]);
    let ops = fixture.ops(probe.clone());
    let account = ops.account();
    let host = fixture.host_over(ops, norn_host::READ_SETTLE_BOUND);
    let _release = Release(&probe);
    let _demand = host.demand(&name()).unwrap();
    probe.wait(LoadingBoundary::LinkHealthChunk);
    let before = account.read().build_facts_recorded;
    for path in ["a-b.md", "a/x03.md"] {
        std::fs::remove_file(fixture.root.join(path)).unwrap();
        std::os::unix::fs::symlink(fixture.root.join("doc00.md"), fixture.root.join(path)).unwrap();
    }
    std::os::unix::fs::symlink("doc00.md", fixture.root.join("a-c.md")).unwrap();
    // Every path's fact is accepted before Relations takes them.
    fact_recorded(&account, before + 2);
    probe.release();
    probe.wait(LoadingBoundary::Published(Step::Relations));
    assert!(
        account.read().requeued_roots >= 3,
        "Relations takes every link fact"
    );
    probe.release();
    ready(&host);
    let advisories = host.inspect(&name()).unwrap().advisories;
    assert!(advises_link(&advisories, "a-c.md"), "{advisories:?}");
    drop(host);
    assert_eq!(advisories, reference_advisories(&fixture));
}

/// The advisories a second host's from-zero build over the fixture's tree, as
/// it now stands, reports at Ready.
fn reference_advisories(fixture: &Fixture) -> Vec<norn_host::AttachmentAdvisory> {
    let dirs = ConfigDirs::new(
        fixture.scratch.join("reference-config"),
        fixture.scratch.join("reference-data"),
    )
    .unwrap();
    let reference = fixture.host_over(
        ProductionEntryOps::new(dirs, ProductionPolicy::new(2, 2).unwrap()),
        norn_host::READ_SETTLE_BOUND,
    );
    let _demand = reference.demand(&name()).unwrap();
    ready(&reference);
    reference.inspect(&name()).unwrap().advisories
}

fn advises_link(advisories: &[norn_host::AttachmentAdvisory], link: &str) -> bool {
    advisories.iter().any(|advisory| {
        matches!(
            advisory, norn_host::AttachmentAdvisory::SymlinkSkipped { path } if path == link
        )
    })
}

/// Hold a from-zero build at `held`, make `change` to the vault, and answer
/// the advisories at Ready beside the links-only re-walks the build ran.
///
/// A `held` of [`LoadingBoundary::LinkHealthChunk`] makes the change while
/// Relations judges links, so the build's last requeue is the one that takes
/// it.
fn advisories_after_a_change_at(
    fixture: &Fixture,
    held: LoadingBoundary,
    change: impl FnOnce(&Path),
) -> (Vec<norn_host::AttachmentAdvisory>, u64) {
    let relations = held == LoadingBoundary::LinkHealthChunk;
    let probe = Probe::new(if relations {
        vec![held, LoadingBoundary::Published(Step::Relations)]
    } else {
        vec![held]
    });
    let ops = fixture.ops(probe.clone());
    let account = ops.account();
    let host = fixture.host_over(ops, norn_host::READ_SETTLE_BOUND);
    let _release = Release(&probe);
    let _demand = host.demand(&name()).unwrap();
    probe.wait(held);
    let before = account.read().build_facts_recorded;
    change(&fixture.root);
    fact_recorded(&account, before);
    probe.release();
    if relations {
        probe.wait(LoadingBoundary::Published(Step::Relations));
        probe.release();
    }
    ready(&host);
    let advisories = host.inspect(&name()).unwrap().advisories;
    (advisories, account.read().link_rewalks)
}

#[test]
fn a_link_replaced_by_a_file_on_requeue_drops_its_advisory() {
    let fixture = Fixture::new("requeue-link-replaced");
    std::os::unix::fs::symlink("doc00.md", fixture.root.join("link.md")).unwrap();
    let (advisories, rewalks) = advisories_after_a_change_at(
        &fixture,
        LoadingBoundary::StepComplete(Step::Content),
        |root| {
            std::fs::remove_file(root.join("link.md")).unwrap();
            std::fs::write(root.join("link.md"), "# Now a file\n").unwrap();
        },
    );
    assert!(!advises_link(&advisories, "link.md"), "{advisories:?}");
    assert_eq!(rewalks, 0, "a list the bound never cut is exact as it is");
    assert_eq!(advisories, reference_advisories(&fixture));
}

#[test]
fn a_link_deleted_during_the_names_step_drops_its_advisory() {
    let fixture = Fixture::new("requeue-link-deleted");
    std::os::unix::fs::symlink("doc00.md", fixture.root.join("link.md")).unwrap();
    let (advisories, rewalks) = advisories_after_a_change_at(
        &fixture,
        LoadingBoundary::StepComplete(Step::Names),
        |root| std::fs::remove_file(root.join("link.md")).unwrap(),
    );
    assert!(!advises_link(&advisories, "link.md"), "{advisories:?}");
    assert_eq!(rewalks, 0);
    assert_eq!(advisories, reference_advisories(&fixture));
}

#[test]
fn a_linked_folder_replaced_by_a_folder_advises_only_the_link_inside_it() {
    let fixture = Fixture::new("requeue-linked-folder");
    std::fs::create_dir_all(fixture.root.join("real")).unwrap();
    std::os::unix::fs::symlink(fixture.root.join("real"), fixture.root.join("linked")).unwrap();
    let (advisories, rewalks) = advisories_after_a_change_at(
        &fixture,
        LoadingBoundary::StepComplete(Step::Content),
        |root| {
            std::fs::remove_file(root.join("linked")).unwrap();
            std::fs::create_dir(root.join("linked")).unwrap();
            std::os::unix::fs::symlink(root.join("doc00.md"), root.join("linked/inner.md"))
                .unwrap();
        },
    );
    assert!(!advises_link(&advisories, "linked"), "{advisories:?}");
    assert!(
        advises_link(&advisories, "linked/inner.md"),
        "{advisories:?}"
    );
    assert_eq!(rewalks, 0);
    assert_eq!(advisories, reference_advisories(&fixture));
}

#[test]
fn a_link_inside_a_refused_folder_moved_in_on_requeue_keeps_its_advisory() {
    let fixture = Fixture::new("requeue-refused-folder-link");
    // A backslash refuses every path beneath the folder, so nothing under it
    // derives; a whole-vault walk still passes over the link inside it.
    let staging = fixture.scratch.join("staging");
    std::fs::create_dir_all(&staging).unwrap();
    std::os::unix::fs::symlink(fixture.root.join("doc00.md"), staging.join("l.md")).unwrap();
    let (advisories, rewalks) = advisories_after_a_change_at(
        &fixture,
        LoadingBoundary::StepComplete(Step::Content),
        |root| std::fs::rename(&staging, root.join("q\\y")).unwrap(),
    );
    assert!(advises_link(&advisories, "q\\y/l.md"), "{advisories:?}");
    assert_eq!(rewalks, 0);
    assert_eq!(advisories, reference_advisories(&fixture));
}

#[test]
fn a_link_replaced_while_relations_judges_links_drops_its_advisory() {
    let fixture = Fixture::new("relations-requeue-link-replaced");
    std::os::unix::fs::symlink("doc00.md", fixture.root.join("link.md")).unwrap();
    let (advisories, rewalks) =
        advisories_after_a_change_at(&fixture, LoadingBoundary::LinkHealthChunk, |root| {
            std::fs::remove_file(root.join("link.md")).unwrap();
            std::fs::write(root.join("link.md"), "# Now a file\n").unwrap();
        });
    assert!(!advises_link(&advisories, "link.md"), "{advisories:?}");
    assert_eq!(rewalks, 0);
    assert_eq!(advisories, reference_advisories(&fixture));
}

#[test]
fn a_link_removed_from_a_cut_list_recovers_the_link_the_bound_dropped() {
    let fixture = Fixture::new("requeue-link-overflow");
    std::fs::create_dir_all(fixture.root.join("a")).unwrap();
    for n in 0..=16 {
        std::os::unix::fs::symlink(
            fixture.root.join("doc00.md"),
            fixture.root.join(format!("a/x{n:02}.md")),
        )
        .unwrap();
    }
    let (advisories, rewalks) = advisories_after_a_change_at(
        &fixture,
        LoadingBoundary::StepComplete(Step::Content),
        |root| {
            std::fs::remove_file(root.join("a/x03.md")).unwrap();
            std::fs::write(root.join("a/x03.md"), "# Now a file\n").unwrap();
        },
    );
    assert!(!advises_link(&advisories, "a/x03.md"), "{advisories:?}");
    assert!(advises_link(&advisories, "a/x16.md"), "{advisories:?}");
    assert_eq!(rewalks, 1, "only a hole in a cut list re-walks the vault");
    assert_eq!(advisories, reference_advisories(&fixture));
}

#[test]
fn a_case_respelling_during_each_document_step_equals_from_scratch() {
    use norn_testkit::churn::{folding, runs_where_the_volume_folds};
    for step in [Step::Names, Step::Content, Step::Structure] {
        let fixture = Fixture::new("requeue-folded-respelling");
        if !runs_where_the_volume_folds(folding(&fixture.root).unwrap(), "construction respelling")
        {
            return;
        }
        fixture.add("a-folder/note.md", "# Respelling\n");
        let mut points = vec![LoadingBoundary::CanonicalPage];
        if step != Step::Names {
            points.insert(
                0,
                LoadingBoundary::StepComplete(if step == Step::Content {
                    Step::Names
                } else {
                    Step::Content
                }),
            );
        }
        let probe = Probe::new(points.clone());
        let ops = fixture.ops(probe.clone());
        let account = ops.account();
        let host = fixture.host_over(ops, norn_host::READ_SETTLE_BOUND);
        let _release = Release(&probe);
        let _demand = host.demand(&name()).unwrap();
        if points.len() > 1 {
            probe.wait(points[0]);
            probe.release();
        }
        probe.wait(LoadingBoundary::CanonicalPage);
        std::fs::rename(fixture.root.join("a-folder"), fixture.root.join("A-FOLDER")).unwrap();
        fact_recorded(&account, 0);
        probe.release();
        ready(&host);
        drop(host);
        project(&fixture.store_path(), &fixture.root)
            .assert_equivalent(&from_scratch(&fixture, "reference"), "folded respelling");
    }
}

#[test]
fn a_fact_during_step_four_rejudges_its_reach() {
    for change in ["create", "delete", "heading"] {
        assert_step_four_rejudges_its_reach(change);
    }
}

#[test]
fn a_folder_renamed_during_step_four_rejudges_its_reach() {
    assert_step_four_rejudges_its_reach("folder");
}

#[test]
fn a_path_target_deleted_during_step_four_rejudges_its_reach() {
    assert_step_four_rejudges_its_reach("path-delete");
}

fn assert_step_four_rejudges_its_reach(change: &str) {
    let fixture = Fixture::new("relations-requeue");
    fixture.add(
            "source.md",
            "[[created]]\n[[target#Before]]\n[relative](target.md#Before)\n[folder](/folder/inside.md)\n[path](/asset.png)\n",
        );
    fixture.add("target.md", "# Before\n");
    fixture.add("folder/inside.md", "# Inside\n");
    fixture.add("asset.png", "asset");
    let probe = Probe::new([
        LoadingBoundary::LinkHealthChunk,
        LoadingBoundary::Published(Step::Relations),
    ]);
    let ops = fixture.ops(probe.clone());
    let account = ops.account();
    let host = fixture.host_over(ops, norn_host::READ_SETTLE_BOUND);
    let _release = Release(&probe);
    let _demand = host.demand(&name()).unwrap();
    probe.wait(LoadingBoundary::LinkHealthChunk);
    let before = account.read().build_facts_recorded;
    match change {
        "create" => fixture.add("created.md", "# Created\n[[created#Absent]]\n[[absent]]\n"),
        "delete" => std::fs::remove_file(fixture.root.join("target.md")).unwrap(),
        "folder" => {
            std::fs::rename(fixture.root.join("folder"), fixture.root.join("moved")).unwrap()
        }
        "path-delete" => std::fs::remove_file(fixture.root.join("asset.png")).unwrap(),
        _ => fixture.add("target.md", "# After\n[[target#Absent]]\n[[absent]]\n"),
    }
    fact_recorded(&account, before);
    probe.release();
    probe.wait(LoadingBoundary::Published(Step::Relations));
    assert!(
        account.read().requeued_roots > 0,
        "{change} must requeue before Relations"
    );
    let shape = ValidateParams::new(address());
    let early = host.validate(&shape).unwrap();
    probe.release();
    ready(&host);
    assert_eq!(
        json!(early.answer.report),
        json!(host.validate(&shape).unwrap().answer.report)
    );
    drop(host);
    project(&fixture.store_path(), &fixture.root)
        .assert_equivalent(&from_scratch(&fixture, "reference"), change);
}

#[test]
fn a_directory_relisted_during_step_four_rejudges_the_links_beneath_it() {
    use std::os::unix::fs::PermissionsExt;
    let fixture = Fixture::new("construction-relist");
    fixture.add("locked/inside.md", "# Restored\n[[absent]]\n");
    fixture.add("outside.md", "[inside](locked/inside.md)\n");
    let directory = fixture.root.join("locked");
    let restore = RestoreMode(directory.clone(), 0o755);
    std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o000)).unwrap();
    assert!(std::fs::read_dir(&directory).is_err());
    let probe = Probe::new([
        LoadingBoundary::LinkHealthChunk,
        LoadingBoundary::Published(Step::Relations),
    ]);
    let ops = fixture.ops(probe.clone());
    let account = ops.account();
    let host = fixture.host_over(ops, norn_host::READ_SETTLE_BOUND);
    let _release = Release(&probe);
    let _demand = host.demand(&name()).unwrap();
    probe.wait(LoadingBoundary::LinkHealthChunk);
    drop(restore);
    fact_recorded(&account, 0);
    probe.release();
    probe.wait(LoadingBoundary::Published(Step::Relations));
    let answer = host.get(&get("/locked/inside")).unwrap();
    assert!(
        serde_json::to_string(&answer.answer.report)
            .unwrap()
            .contains("Restored")
    );
    assert!(account.read().requeue_links_rejudged > 0);
    let shape = ValidateParams::new(address());
    let early = host.validate(&shape).unwrap();
    probe.release();
    ready(&host);
    assert_eq!(
        json!(early.answer.report),
        json!(host.validate(&shape).unwrap().answer.report)
    );
    drop(host);
    project(&fixture.store_path(), &fixture.root).assert_equivalent(
        &from_scratch(&fixture, "reference"),
        "directory relisted before Relations",
    );
}

/// A build's requeue calls and takes, counted at their own boundaries rather
/// than by the evidence counters `requeue` keeps, so a test can hold those
/// counters to an independent count.
#[derive(Default)]
struct RequeueCount {
    calls: std::sync::atomic::AtomicU64,
    takes: std::sync::atomic::AtomicU64,
}

impl RequeueCount {
    /// Count `boundary`, returning how many takes began before it.
    fn count(&self, boundary: LoadingBoundary) -> u64 {
        use std::sync::atomic::Ordering;
        match boundary {
            LoadingBoundary::RequeueCalled => {
                self.calls.fetch_add(1, Ordering::SeqCst);
                self.takes.load(Ordering::SeqCst)
            }
            LoadingBoundary::RequeueStarted => self.takes.fetch_add(1, Ordering::SeqCst),
            _ => self.takes.load(Ordering::SeqCst),
        }
    }
    fn read(&self) -> (u64, u64) {
        use std::sync::atomic::Ordering;
        (
            self.calls.load(Ordering::SeqCst),
            self.takes.load(Ordering::SeqCst),
        )
    }
}

impl LoadingProbe for RequeueCount {
    fn at(&self, boundary: LoadingBoundary) {
        self.count(boundary);
    }
}

/// Keeps a fact pending at every requeue call of the build, and lands facts
/// on both sides of each take's read:
///
/// - the first content page seeds `hot`, so the first call has a fact;
/// - inside every take, before it reads, `hot` is rewritten `per_take` times,
///   so a take that loops would never run out of facts;
/// - after every take, past its read and commit, `hot` is rewritten once, a
///   fact only the next call or the reconcile can carry into the store;
/// - once Structure publishes, past every ordinary read of `hot`, it is
///   rewritten once, so a take still runs after no ordinary pass would read
///   `hot` again and the fact it leaves can only reach the store as a fact;
/// - once Relations publishes, past its closing take, `late` is rewritten, a
///   fact only the reconcile can carry.
///
/// Each rewrite waits for the watcher to accept it. Writing inside takes stops
/// after `cap` takes, so a build that takes again within one requeue ends and
/// fails its assertions instead of running forever.
struct HotWriter {
    hot: PathBuf,
    late: PathBuf,
    per_take: u64,
    cap: u64,
    account: std::sync::OnceLock<Arc<norn_host::JobEvidence>>,
    version: std::sync::atomic::AtomicU64,
    count: RequeueCount,
}

impl HotWriter {
    fn rewrite(&self, path: &Path, times: u64) {
        use std::sync::atomic::Ordering;
        let account = self.account.get().expect("the account is bound first");
        for _ in 0..times {
            let before = account.read().build_facts_recorded;
            let version = self.version.fetch_add(1, Ordering::SeqCst) + 1;
            std::fs::write(path, format!("# Version {version}\n")).unwrap();
            fact_recorded(account, before);
        }
    }
}

impl LoadingProbe for HotWriter {
    fn at(&self, boundary: LoadingBoundary) {
        use std::sync::atomic::Ordering;
        let began = self.count.count(boundary);
        match boundary {
            LoadingBoundary::CanonicalPage if self.version.load(Ordering::SeqCst) == 0 => {
                self.rewrite(&self.hot, self.per_take)
            }
            LoadingBoundary::RequeueStarted if began < self.cap => {
                self.rewrite(&self.hot, self.per_take)
            }
            LoadingBoundary::Requeued if began <= self.cap => self.rewrite(&self.hot, 1),
            LoadingBoundary::Published(Step::Structure) => self.rewrite(&self.hot, 1),
            LoadingBoundary::Published(Step::Relations) => self.rewrite(&self.late, 1),
            _ => {}
        }
    }
}

/// The requeue calls a from-zero build of `fixture`'s tree makes with nothing
/// written during it, after checking the build's evidence counts each of them
/// once and took nothing.
fn quiet_requeue_calls(fixture: &Fixture) -> u64 {
    let dirs = ConfigDirs::new(
        fixture.scratch.join("quiet-config"),
        fixture.scratch.join("quiet-data"),
    )
    .unwrap();
    let count = Arc::new(RequeueCount::default());
    let ops = ProductionEntryOps::new(dirs, ProductionPolicy::new(2, 2).unwrap())
        .with_loading_probe(count.clone());
    let account = ops.account();
    {
        let host = fixture.host_over(ops, norn_host::READ_SETTLE_BOUND);
        let _demand = host.demand(&name()).unwrap();
        ready(&host);
    }
    let spent = account.read();
    let (calls, takes) = count.read();
    assert!(calls > 0, "a quiet build makes requeue calls");
    assert_eq!(takes, 0, "a quiet build takes nothing");
    assert_eq!(
        spent.requeue_calls, calls,
        "the evidence counts each quiet requeue call once"
    );
    assert_eq!(spent.requeue_takes, 0, "the evidence counts no quiet take");
    calls
}

/// ADR 0048: each requeue takes once. A build's requeue calls are bounded by
/// the vault's shape, so a writer that keeps a fact pending at every call
/// adds no call, and each call makes exactly the one take; and no fact is
/// lost, whether it lands before a take's read, after it, or after Relations'
/// closing take, so the store reaching `Ready` equals a from-scratch build of
/// the last bytes.
#[test]
fn a_writer_inside_every_take_never_outnumbers_the_requeue_calls() {
    const PER_TAKE: u64 = 20;
    const CAP: u64 = 400;
    let fixture = Fixture::new("hot-writer");
    let quiet = quiet_requeue_calls(&fixture);
    let writer = Arc::new(HotWriter {
        hot: fixture.root.join("doc00.md"),
        late: fixture.root.join("doc01.md"),
        per_take: PER_TAKE,
        cap: CAP,
        account: std::sync::OnceLock::new(),
        version: std::sync::atomic::AtomicU64::new(0),
        count: RequeueCount::default(),
    });
    let ops = fixture.ops(writer.clone());
    let account = ops.account();
    writer.account.set(account.clone()).ok().unwrap();
    let host = fixture.host_over(ops, norn_host::READ_SETTLE_BOUND);
    {
        let _demand = host.demand(&name()).unwrap();
        ready(&host);
    }
    let spent = account.read();
    let (calls, takes) = writer.count.read();
    let facts = spent.build_facts_recorded;
    eprintln!(
        "quiet calls {quiet}; calls {calls} (evidence {}), takes {takes} (evidence {}), \
         facts accepted {facts}",
        spent.requeue_calls, spent.requeue_takes
    );
    // Pulling the hot document ahead takes it out of the ordinary passes, so
    // they can page fewer chunks than the quiet build's, never more.
    assert!(
        calls <= quiet,
        "the writer raised the requeue calls from {quiet} to {calls}"
    );
    assert_eq!(
        spent.requeue_calls, calls,
        "the evidence counts each requeue call once"
    );
    assert!(
        takes <= calls,
        "{takes} takes outnumber {calls} requeue calls"
    );
    assert_eq!(takes, calls, "a call left the fact pending at it untaken");
    assert_eq!(
        spent.requeue_takes, takes,
        "the evidence counts each completed take once"
    );
    assert!(
        facts >= 10 * calls,
        "{facts} facts accepted do not outrun {calls} requeue calls"
    );
    drop(host);
    project(&fixture.store_path(), &fixture.root).assert_equivalent(
        &from_scratch(&fixture, "reference"),
        "a writer inside and after every take",
    );
}
