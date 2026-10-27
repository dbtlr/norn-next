//! Real public requests at initial construction boundaries, without private readers.
#![cfg(all(unix, feature = "induced-failure"))]
#![allow(clippy::disallowed_methods)] // Every path is inside this test's owned fixture.

use norn_config::{ConfigDirs, registry::VaultRoot};
use norn_host::{
    Host, LifecyclePolicy, LoadingBoundary, LoadingProbe, ProductionEntryOps, ProductionPolicy,
    Registration, RegistryRead,
};
use norn_testkit::{
    isolation::{self, Lease},
    scratch::Scratch,
    wait::{Budget, Observed, wait_until},
};
use norn_wire::{
    Column, CountParams, FindParams, GetParams, Predicate, ReasonCode, ResolutionTarget,
    TrustState, VaultAddress, VaultName,
};
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, mpsc};
use std::time::Duration;

const BUDGET: Budget = Budget::new(Duration::from_secs(20), Duration::from_millis(100));
type GetAnswer = Result<
    norn_host::Answered<norn_wire::GetReport, norn_store::GetWork>,
    norn_wire::ErrorEnvelope,
>;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Point {
    AuthorityCaptured,
    BeforeCatalogue,
    CatalogueRecord,
    CatalogueComplete,
    GetLoaded,
    GetPublished,
    MetadataRecord,
    MetadataComplete,
    CanonicalPage,
    CanonicalComplete,
    QueryEstablished,
    QueryBuilt,
}

#[derive(Default)]
struct GateState {
    next: VecDeque<Point>,
    current: Option<Point>,
    released: bool,
    stop: bool,
    files: Vec<(String, Vec<PathBuf>)>,
}
struct Probe {
    state: Mutex<GateState>,
    moved: Condvar,
}
impl Probe {
    fn new(points: impl IntoIterator<Item = Point>) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(GateState {
                next: points.into_iter().collect(),
                ..GateState::default()
            }),
            moved: Condvar::new(),
        })
    }
    fn wait(&self, point: Point) {
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
    fn files(&self) -> Vec<(String, Vec<PathBuf>)> {
        self.state.lock().unwrap().files.clone()
    }
}
impl LoadingProbe for Probe {
    fn at(&self, boundary: LoadingBoundary<'_>) {
        let point = match boundary {
            LoadingBoundary::AuthorityCaptured => Point::AuthorityCaptured,
            LoadingBoundary::BeforeCatalogue => Point::BeforeCatalogue,
            LoadingBoundary::CatalogueRecord(_) => Point::CatalogueRecord,
            LoadingBoundary::CatalogueComplete => Point::CatalogueComplete,
            LoadingBoundary::GetLoaded(_) => Point::GetLoaded,
            LoadingBoundary::GetPublished(_) => Point::GetPublished,
            LoadingBoundary::MetadataRecord(_) => Point::MetadataRecord,
            LoadingBoundary::MetadataComplete => Point::MetadataComplete,
            LoadingBoundary::CanonicalPage => Point::CanonicalPage,
            LoadingBoundary::CanonicalComplete => Point::CanonicalComplete,
            LoadingBoundary::QueryEstablished => Point::QueryEstablished,
            LoadingBoundary::QueryBuilt => Point::QueryBuilt,
            LoadingBoundary::SourceCreated { role, paths } => {
                self.state
                    .lock()
                    .unwrap()
                    .files
                    .push((role.into(), paths.into()));
                return;
            }
        };
        let mut state = self.state.lock().unwrap();
        if state.stop || state.next.front() != Some(&point) {
            return;
        }
        state.next.pop_front();
        state.current = Some(point);
        state.released = false;
        self.moved.notify_all();
        let (mut state, timeout) = self
            .moved
            .wait_timeout_while(state, Duration::from_secs(60), |s| !s.released && !s.stop)
            .unwrap();
        state.current = None;
        assert!(
            !timeout.timed_out(),
            "construction boundary {point:?} was never released"
        );
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
        let scratch = Scratch::under(Path::new(env!("CARGO_TARGET_TMPDIR")), label).unwrap();
        let root = scratch.join("vault");
        std::fs::create_dir_all(root.join(".norn")).unwrap();
        std::fs::write(
            root.join(".norn/schema.yaml"),
            "version: 1\nfields:\n  rank: { type: number }\n",
        )
        .unwrap();
        for n in 0..12 {
            std::fs::write(
                root.join(format!("doc{n:02}.md")),
                format!("---\nrank: {n}\n---\n# Title {n}\nBody {n}.\n"),
            )
            .unwrap();
        }
        std::fs::write(root.join("first.md"), "---\nrank: nope\ntags: [one, two]\n---\n# First\n[[doc01#Missing]] [[absent]] [[asset.png]]\nText ^block\n").unwrap();
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
    fn host(&self, probe: Arc<dyn LoadingProbe>) -> Host<ProductionEntryOps> {
        self.host_bound(probe, norn_host::READ_SETTLE_BOUND)
    }
    fn host_bound(
        &self,
        probe: Arc<dyn LoadingProbe>,
        bound: Duration,
    ) -> Host<ProductionEntryOps> {
        self.host_policy(probe, bound, ProductionPolicy::new(2, 2).unwrap())
    }
    fn host_policy(
        &self,
        probe: Arc<dyn LoadingProbe>,
        bound: Duration,
        policy: ProductionPolicy,
    ) -> Host<ProductionEntryOps> {
        let registration = Registration::new(name(), VaultRoot::new(&self.root).unwrap());
        norn_fs::watch::settle_prior_changes(self.scratch.root(), BUDGET.work()).unwrap();
        Host::new(
            RegistryRead::from_entries([registration]),
            ProductionEntryOps::new(self.dirs.clone(), policy).with_loading_probe(probe),
            LifecyclePolicy {
                idle_after: Duration::from_secs(7200),
                worker_slots: 1,
                watch_poll_interval: Duration::from_millis(25),
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
fn spawn_get<'scope, 'env>(
    scope: &'scope std::thread::Scope<'scope, 'env>,
    host: &'scope Host<ProductionEntryOps>,
    target: &str,
) -> mpsc::Receiver<GetAnswer> {
    let (send, receive) = mpsc::sync_channel(1);
    let params = get(target);
    scope.spawn(move || {
        let _ = send.send(host.get(&params));
    });
    receive
}
fn content(report: norn_wire::GetReport) -> serde_json::Value {
    let mut value = serde_json::to_value(report).unwrap();
    for finding in value["document"]["findings"]["items"]
        .as_array_mut()
        .unwrap()
    {
        let finding = finding.as_object_mut().unwrap();
        assert!(finding.remove("id").unwrap().as_u64().unwrap() > 0);
        assert!(finding.remove("generation").unwrap().as_u64().unwrap() > 0);
    }
    value
}

#[test]
fn incoming_targets_and_complete_find_remain_available_through_later_construction() {
    let fixture = Fixture::new("public-stage-arrivals");
    let probe = Probe::new([
        Point::BeforeCatalogue,
        Point::CatalogueRecord,
        Point::CatalogueComplete,
        Point::MetadataRecord,
        Point::MetadataComplete,
        Point::CanonicalPage,
        Point::CanonicalComplete,
    ]);
    let host = fixture.host(probe.clone());
    std::thread::scope(|scope| {
        let _release = Release(&probe);
        let first = spawn_get(scope, &host, "first");
        probe.wait(Point::BeforeCatalogue);
        probe.release();
        probe.wait(Point::CatalogueRecord);
        let before = host.read_evidence().settle_waits;
        let second = spawn_get(scope, &host, "doc02");
        waits(&host, before + 1);
        probe.release();
        probe.wait(Point::CatalogueComplete);
        let before = host.read_evidence().settle_waits;
        let third = spawn_get(scope, &host, "doc03");
        waits(&host, before + 1);
        probe.release();
        probe.wait(Point::MetadataRecord);
        let first = first.recv_timeout(BUDGET.work()).unwrap().unwrap();
        assert!(matches!(
            first.answer.reading.trust,
            TrustState::Warming {
                phase: norn_wire::WarmingPhase::Healing,
                ..
            }
        ));
        let original = content(first.answer.report);
        assert!(original.to_string().contains("missing-anchor"));
        assert!(original.to_string().contains("broken"));
        assert_eq!(
            content(host.get(&get("first")).unwrap().answer.report),
            original
        );
        probe.release();
        probe.wait(Point::MetadataComplete);
        second.recv_timeout(BUDGET.work()).unwrap().unwrap();
        third.recv_timeout(BUDGET.work()).unwrap().unwrap();
        let early_find = host.find(&find().with_limit(3)).unwrap();
        assert_eq!(early_find.answer.report.page.rows.len(), 3);
        assert!(matches!(
            early_find.answer.reading.trust,
            TrustState::Warming { .. }
        ));
        let cursor = early_find
            .answer
            .report
            .page
            .next
            .clone()
            .expect("a full suffix remains");
        let early_epoch = early_find.answer.reading.epoch;
        assert_eq!(
            content(host.get(&get("first")).unwrap().answer.report),
            original
        );
        assert_eq!(
            host.count(&CountParams::new(address())).unwrap_err().code(),
            &ReasonCode::HostEntryNotReady
        );
        assert!(host.begin_read(&name()).is_err());
        assert_eq!(
            host.validate(&norn_wire::ValidateParams::new(address()))
                .unwrap_err()
                .code(),
            &ReasonCode::HostEntryNotReady
        );
        assert_eq!(
            host.describe(&norn_wire::DescribeParams::new(address()))
                .unwrap_err()
                .code(),
            &ReasonCode::HostEntryNotReady
        );
        assert_eq!(
            host.search(&norn_wire::SearchParams::new(address(), "Body"))
                .unwrap_err()
                .code(),
            &ReasonCode::HostEntryNotReady
        );
        let untouched = std::fs::read(fixture.root.join("first.md")).unwrap();
        let mutation = norn_wire::SetParams::new(
            address(),
            norn_wire::ApplyMode::Preview,
            norn_wire::WriteTarget::path(norn_wire::DocumentPath::new("first.md").unwrap()),
            vec![norn_wire::FieldChange::set(
                "rank",
                norn_wire::AuthoredValue::string("99"),
            )],
        );
        let refusal = host
            .set(mutation)
            .expect("the preview is answered")
            .wait()
            .unwrap_err();
        assert_eq!(refusal.code(), &ReasonCode::HostEntryNotReady);
        assert_eq!(
            std::fs::read(fixture.root.join("first.md")).unwrap(),
            untouched
        );
        assert_eq!(
            host.get(&get("first#First")).unwrap_err().code(),
            &ReasonCode::HostEntryNotReady
        );
        assert_eq!(
            host.get(&get("first").with_columns([Column::path()]))
                .unwrap_err()
                .code(),
            &ReasonCode::HostEntryNotReady
        );
        assert_eq!(
            host.find(&find().with_columns([Column::body()]))
                .unwrap_err()
                .code(),
            &ReasonCode::HostEntryNotReady
        );
        assert_eq!(
            host.find(&find().with_predicates([Predicate::matches("Body")]))
                .unwrap_err()
                .code(),
            &ReasonCode::HostEntryNotReady
        );
        probe.release();
        probe.wait(Point::CanonicalPage);
        assert_eq!(
            content(host.get(&get("first")).unwrap().answer.report),
            original
        );
        assert_eq!(
            host.find(&find()).unwrap().answer.report.page.rows.len(),
            13
        );
        let before = host.read_evidence().settle_waits;
        let fourth = spawn_get(scope, &host, "doc11");
        waits(&host, before + 1);
        probe.release();
        probe.wait(Point::CanonicalComplete);
        fourth.recv_timeout(BUDGET.work()).unwrap().unwrap();
        assert_eq!(
            content(host.get(&get("first")).unwrap().answer.report),
            original
        );
        probe.release();
        ready(&host);
        assert_eq!(
            content(host.get(&get("first")).unwrap().answer.report),
            original
        );
        let mut params = find().with_limit(3).with_after(cursor);
        let mut suffix = Vec::new();
        loop {
            let page = host.find(&params).unwrap();
            assert_ne!(page.answer.reading.epoch, early_epoch);
            if suffix.is_empty() {
                assert_eq!(page.answer.report.page.moved, [norn_wire::Moved::Epoch]);
            }
            suffix.extend(page.answer.report.page.rows);
            let Some(next) = page.answer.report.page.next else {
                break;
            };
            params = params.with_after(next);
        }
        let all = host.find(&find()).unwrap().answer.report.page.rows;
        assert_eq!(suffix, all[3..]);
        assert_eq!(
            host.read_evidence().gate_retakes_within_establishing_holds,
            0
        );
        for (_, paths) in probe.files() {
            for path in paths {
                assert!(
                    !path.exists(),
                    "promoted temporary source still exists: {}",
                    path.display()
                );
            }
        }
    });
    drop(host);
    let _ = fixture.scratch.root();
}

#[test]
fn an_early_source_available_at_the_deadline_gets_first_index_expiry() {
    let fixture = Fixture::new("public-stage-publication-deadline");
    let probe = Probe::new([Point::MetadataComplete]);
    let host = fixture.host_bound(probe.clone(), Duration::ZERO);
    let _release = Release(&probe);
    let _demand = host.demand(&name()).unwrap();
    probe.wait(Point::MetadataComplete);
    let error = host.find(&find()).unwrap_err();
    assert_eq!(error.code(), &ReasonCode::HostEntryNotReady);
    assert_eq!(
        error.message(),
        "this vault is still indexing for the first time"
    );
    assert_eq!(host.read_evidence().reader_waits, 0);
    probe.release();
    ready(&host);
}

#[test]
fn expired_interests_release_reservations_without_renewing_the_original_deadline() {
    let fixture = Fixture::new("public-stage-expired");
    let probe = Probe::new([Point::BeforeCatalogue, Point::MetadataRecord]);
    let host = fixture.host_bound(probe.clone(), Duration::from_secs(1));
    std::thread::scope(|scope| {
        let _release = Release(&probe);
        let first = spawn_get(scope, &host, "doc00");
        probe.wait(Point::BeforeCatalogue);
        let mut expired = vec![first];
        for n in 1..8 {
            expired.push(spawn_get(scope, &host, &format!("doc{n:02}")));
        }
        for receiver in expired {
            let error = receiver.recv_timeout(BUDGET.work()).unwrap().unwrap_err();
            assert_eq!(error.code(), &ReasonCode::HostEntryNotReady);
            assert_eq!(
                error.message(),
                "this vault is still indexing for the first time"
            );
        }
        let before = host.read_evidence().settle_waits;
        let later = spawn_get(scope, &host, "doc11");
        waits(&host, before + 1);
        probe.release();
        probe.wait(Point::MetadataRecord);
        assert!(matches!(
            later
                .recv_timeout(BUDGET.work())
                .unwrap()
                .unwrap()
                .answer
                .reading
                .trust,
            TrustState::Warming { .. }
        ));
        let sources: Vec<_> = probe
            .files()
            .into_iter()
            .filter(|(role, _)| role.starts_with("Get "))
            .collect();
        assert_eq!(
            sources.len(),
            1,
            "expired requests spent no preparation work"
        );
        assert_eq!(sources[0].0, "Get doc11");
        probe.release();
        ready(&host);
    });
}

#[test]
fn distinct_requests_cannot_evict_published_sources_or_buy_unbounded_preparation() {
    let fixture = Fixture::new("public-stage-bound");
    let probe = Probe::new([
        Point::BeforeCatalogue,
        Point::MetadataComplete,
        Point::CanonicalComplete,
    ]);
    let host = fixture.host(probe.clone());
    std::thread::scope(|scope| {
        let _release = Release(&probe);
        let mut callers = vec![spawn_get(scope, &host, "doc00")];
        probe.wait(Point::BeforeCatalogue);
        for n in 1..8 {
            let before = host.read_evidence().settle_waits;
            callers.push(spawn_get(scope, &host, &format!("doc{n:02}")));
            waits(&host, before + 1);
        }
        assert_eq!(
            host.get(&get("doc08")).unwrap_err().code(),
            &ReasonCode::HostEntryNotReady
        );
        probe.release();
        probe.wait(Point::MetadataComplete);
        for caller in callers {
            caller.recv_timeout(BUDGET.work()).unwrap().unwrap();
        }
        assert_eq!(
            probe
                .files()
                .iter()
                .filter(|(role, _)| role.starts_with("Get "))
                .count(),
            8
        );
        assert!(host.get(&get("doc00")).is_ok());
        assert_eq!(
            host.get(&get("doc09")).unwrap_err().code(),
            &ReasonCode::HostEntryNotReady
        );
        probe.release();
        probe.wait(Point::CanonicalComplete);
        assert_eq!(
            probe
                .files()
                .iter()
                .filter(|(role, _)| role.starts_with("Get "))
                .count(),
            8
        );
        assert!(host.get(&get("doc00")).is_ok());
        probe.release();
        ready(&host);
        assert!(host.get(&get("doc09")).is_ok());
    });
}

#[test]
fn early_field_find_uses_ordinary_order_projection_unsatisfied_and_cursor_judgment() {
    let fixture = Fixture::new("public-stage-find-shapes");
    let probe = Probe::new([Point::MetadataComplete]);
    let host = fixture.host(probe.clone());
    let _release = Release(&probe);
    let _demand = host.demand(&name()).unwrap();
    probe.wait(Point::MetadataComplete);
    let query = find()
        .with_predicates([Predicate::equal_to("rank", "7")])
        .with_sort(norn_wire::Sort::new(
            norn_wire::SortKey::field("rank"),
            norn_wire::Direction::Descending,
        ));
    let early = host.find(&query).unwrap();
    assert_eq!(early.answer.report.page.rows.len(), 1);
    assert_eq!(early.answer.report.page.rows[0].path.as_str(), "doc07.md");
    let unknown = find().with_predicates([Predicate::equal_to("unknown", "x")]);
    let early_unknown = host.find(&unknown).unwrap();
    let page = host.find(&find().with_limit(2)).unwrap();
    let cursor = page.answer.report.page.next.unwrap();
    let changed_order = find().with_after(cursor).with_sort(norn_wire::Sort::new(
        norn_wire::SortKey::field("rank"),
        norn_wire::Direction::Descending,
    ));
    let refused = host.find(&changed_order).unwrap_err();
    assert_eq!(refused.code(), &ReasonCode::VaultCursorOrderChanged);
    probe.release();
    ready(&host);
    assert_eq!(
        host.find(&query).unwrap().answer.report,
        early.answer.report
    );
    let canonical_unknown = host.find(&unknown).unwrap();
    assert_eq!(
        canonical_unknown.answer.unsatisfied,
        early_unknown.answer.unsatisfied
    );
    assert_eq!(canonical_unknown.answer.report, early_unknown.answer.report);
}

#[test]
fn construction_contradictions_decline_whole_get_and_canonical_still_converges() {
    for point in [
        Point::CatalogueRecord,
        Point::GetLoaded,
        Point::MetadataRecord,
    ] {
        let fixture = Fixture::new("public-stage-construction-change");
        let probe = Probe::new([point]);
        let host = fixture.host(probe.clone());
        std::thread::scope(|scope| {
            let _release = Release(&probe);
            let caller = spawn_get(scope, &host, "first");
            probe.wait(point);
            std::fs::write(
                fixture.root.join("first.md"),
                "---\nrank: 99\n---\nChanged body.\n",
            )
            .unwrap();
            probe.release();
            let first = caller.recv_timeout(BUDGET.work()).unwrap();
            // A read that already completed before metadata was reached is valid.
            if point == Point::GetLoaded {
                assert!(
                    first.is_err(),
                    "a re-observed construction contradiction cannot publish old content"
                );
            } else if point == Point::CatalogueRecord
                && let Ok(first) = first
            {
                assert!(
                    serde_json::to_value(first.answer.report)
                        .unwrap()
                        .to_string()
                        .contains("Changed body.")
                );
            }
            ready(&host);
            let value =
                serde_json::to_value(host.get(&get("first")).unwrap().answer.report).unwrap();
            assert!(value.to_string().contains("Changed body."), "{value}");
        });
    }
}

#[test]
fn known_competing_name_schema_and_root_changes_withdraw_published_sources() {
    for kind in ["name", "schema", "root"] {
        let fixture = Fixture::new("public-stage-published-change");
        if kind == "name" {
            std::fs::create_dir(fixture.root.join("original")).unwrap();
            std::fs::rename(
                fixture.root.join("first.md"),
                fixture.root.join("original/first.md"),
            )
            .unwrap();
        }
        let probe = Probe::new([Point::MetadataComplete]);
        let host = fixture.host(probe.clone());
        std::thread::scope(|scope| {
            let _release = Release(&probe);
            let first = spawn_get(scope, &host, "first");
            probe.wait(Point::MetadataComplete);
            first.recv_timeout(BUDGET.work()).unwrap().unwrap();
            assert!(host.get(&get("first")).is_ok());
            match kind {
                "name" => {
                    std::fs::create_dir(fixture.root.join("duplicate")).unwrap();
                    std::fs::write(fixture.root.join("duplicate/first.md"), "Duplicate.\n")
                        .unwrap();
                }
                "schema" => std::fs::write(
                    fixture.root.join(".norn/schema.yaml"),
                    "version: 1\nfields:\n  rank: { type: text }\n",
                )
                .unwrap(),
                "root" => {
                    std::fs::rename(&fixture.root, fixture.scratch.join("moved-vault")).unwrap()
                }
                _ => unreachable!(),
            }
            wait_until(
                "known fact withdraws public Get and Find",
                BUDGET,
                || match (host.get(&get("first")), host.find(&find())) {
                    (Err(_), Err(_)) => Observed::Met(()),
                    _ => Observed::pending("published source still admits"),
                },
            )
            .unwrap();
            let _state = host.state(&name()).unwrap();
            for (role, paths) in probe.files() {
                if role != "catalogue" {
                    assert!(
                        paths.iter().all(|path| !path.exists()),
                        "withdrawn {role} kept idle physical resources"
                    );
                }
            }
            probe.release();
            if kind != "root" {
                ready(&host);
            }
            if kind == "name" {
                assert_eq!(
                    host.get(&get("first")).unwrap_err().code(),
                    &ReasonCode::VaultAmbiguousTarget
                );
            }
        });
    }
}

#[test]
fn loading_orphans_and_sidecars_are_swept_only_after_winning_the_maintainer_lock() {
    let fixture = Fixture::new("public-stage-sweep");
    let probe = Probe::new([Point::MetadataComplete]);
    let host = fixture.host(probe.clone());
    let _release = Release(&probe);
    let _demand = host.demand(&name()).unwrap();
    probe.wait(Point::MetadataComplete);
    let live: Vec<_> = probe
        .files()
        .into_iter()
        .flat_map(|(_, paths)| paths)
        .filter(|path| path.exists())
        .collect();
    assert!(!live.is_empty());
    let other = ProductionEntryOps::new(fixture.dirs.clone(), ProductionPolicy::new(2, 2).unwrap());
    assert!(other.last_reader_cleanup_error().is_none());
    for path in live {
        assert!(path.exists(), "another maintainer swept a live source");
    }
    probe.release();
    ready(&host);
    drop(_demand);
    drop(host);
    let directory = norn_store::Store::loading_directory(
        &fixture.dirs.derived_dir(&name()).join("store.sqlite3"),
    );
    std::fs::create_dir_all(&directory).unwrap();
    let base = directory.join("result-0123456789abcdef0123456789abcdef.sqlite3");
    let names: Vec<_> = ["", "-wal", "-shm", "-journal"]
        .into_iter()
        .map(|suffix| PathBuf::from(format!("{}{suffix}", base.display())))
        .collect();
    for path in &names {
        std::fs::write(path, "orphan").unwrap();
    }
    let unrelated = directory.join("keep-me");
    std::fs::write(&unrelated, "unrelated").unwrap();
    let lock = fixture.dirs.derived_dir(&name()).join("maintainer.lock");
    let norn_fs::Acquisition::Acquired(held) = norn_fs::try_acquire(&lock).unwrap() else {
        panic!("fixture lock");
    };
    let skipped =
        ProductionEntryOps::new(fixture.dirs.clone(), ProductionPolicy::new(2, 2).unwrap());
    assert!(skipped.last_reader_cleanup_error().is_none());
    assert!(names.iter().all(|path| path.exists()));
    drop(held);
    let swept = ProductionEntryOps::new(fixture.dirs.clone(), ProductionPolicy::new(2, 2).unwrap());
    assert!(swept.last_reader_cleanup_error().is_none());
    assert!(names.iter().all(|path| !path.exists()));
    assert!(unrelated.exists());
}

struct SplitProbe {
    construction: Arc<Probe>,
    reads: Arc<Probe>,
}
impl LoadingProbe for SplitProbe {
    fn at(&self, boundary: LoadingBoundary<'_>) {
        match boundary {
            LoadingBoundary::QueryEstablished | LoadingBoundary::QueryBuilt => {
                self.reads.at(boundary)
            }
            _ => self.construction.at(boundary),
        }
    }
}

#[test]
fn a_healthy_established_public_get_survives_promotion_charged_until_its_query_finishes() {
    let fixture = Fixture::new("public-stage-held-promotion");
    let construction = Probe::new([Point::MetadataComplete]);
    let reads = Probe::new([]);
    let host = fixture.host(Arc::new(SplitProbe {
        construction: construction.clone(),
        reads: reads.clone(),
    }));
    std::thread::scope(|scope| {
        let _release_construction = Release(&construction);
        let _release_reads = Release(&reads);
        let first = spawn_get(scope, &host, "first");
        construction.wait(Point::MetadataComplete);
        let early = first.recv_timeout(BUDGET.work()).unwrap().unwrap();
        reads
            .state
            .lock()
            .unwrap()
            .next
            .push_back(Point::QueryEstablished);
        let held = spawn_get(scope, &host, "first");
        reads.wait(Point::QueryEstablished);
        let paths = construction
            .files()
            .into_iter()
            .find(|(role, _)| role == "Get first")
            .unwrap()
            .1;
        assert!(paths[0].exists());
        construction.release();
        ready(&host);
        assert!(
            paths[0].exists(),
            "an established read keeps the physical loading file"
        );
        assert_eq!(
            host.read_budget_census().live_connections,
            1,
            "retired held source remains charged"
        );
        assert!(
            host.last_reader_cleanup_error().is_none(),
            "a legitimate owner is not a cleanup error"
        );
        reads.release();
        let held = held.recv_timeout(BUDGET.work()).unwrap().unwrap();
        assert_eq!(content(held.answer.report), content(early.answer.report));
        assert!(matches!(
            held.answer.reading.trust,
            TrustState::Warming { .. }
        ));
        assert!(paths.iter().all(|path| !path.exists()));
        assert_eq!(host.read_budget_census().live_connections, 0);
        assert!(host.last_reader_cleanup_error().is_none());
    });
}

#[test]
fn physical_loading_source_damage_is_reported_without_condemning_canonical_storage() {
    let fixture = Fixture::new("public-stage-physical-damage");
    let construction = Probe::new([Point::MetadataComplete]);
    let mut policy = ProductionPolicy::new(2, 2).unwrap();
    policy.read_budget = norn_store::ReadBudgetPolicy::new(2, Duration::ZERO).unwrap();
    let host = fixture.host_policy(construction.clone(), norn_host::READ_SETTLE_BOUND, policy);
    std::thread::scope(|scope| {
        let _release = Release(&construction);
        let first = spawn_get(scope, &host, "first");
        construction.wait(Point::MetadataComplete);
        first.recv_timeout(BUDGET.work()).unwrap().unwrap();
        wait_until(
            "idle early connection closes before physical damage",
            BUDGET,
            || {
                if host.read_budget_census().live_connections == 0 {
                    Observed::Met(())
                } else {
                    Observed::pending("idle source is still open")
                }
            },
        )
        .unwrap();
        let paths = construction
            .files()
            .into_iter()
            .find(|(role, _)| role == "Get first")
            .unwrap()
            .1;
        std::fs::write(&paths[0], "damaged disposable database").unwrap();
        let error = host.get(&get("first")).unwrap_err();
        assert_eq!(error.code(), &ReasonCode::HostReaderUnavailable);
        assert!(
            !serde_json::to_string(&error)
                .unwrap()
                .contains(paths[0].to_str().unwrap()),
            "wire refusals omit physical storage paths"
        );
        assert!(matches!(
            host.state(&name()).unwrap(),
            TrustState::Warming { .. }
        ));
        assert!(
            host.find(&find()).is_ok(),
            "a different healthy source remains available"
        );
        let diagnostic = host
            .last_reader_cleanup_error()
            .expect("physical operation failure remains observable");
        assert!(
            diagnostic.contains(paths[0].to_str().unwrap()),
            "{diagnostic}"
        );
        assert!(diagnostic.contains("Get first"), "{diagnostic}");
        construction.release();
        ready(&host);
        assert!(host.get(&get("first")).is_ok());
        assert!(
            host.last_reader_cleanup_error().is_some(),
            "Ready must not erase the earlier physical failure"
        );
    });
}

#[test]
fn final_authority_check_covers_success_and_data_derived_refusals() {
    for refusal in [false, true] {
        let fixture = Fixture::new("public-stage-final-check");
        if refusal {
            std::fs::create_dir(fixture.root.join("one")).unwrap();
            std::fs::create_dir(fixture.root.join("two")).unwrap();
            std::fs::rename(
                fixture.root.join("first.md"),
                fixture.root.join("one/first.md"),
            )
            .unwrap();
            std::fs::write(fixture.root.join("two/first.md"), "Other first.\n").unwrap();
        }
        let construction = Probe::new([Point::MetadataComplete]);
        let reads = Probe::new([Point::QueryBuilt]);
        let host = fixture.host(Arc::new(SplitProbe {
            construction: construction.clone(),
            reads: reads.clone(),
        }));
        std::thread::scope(|scope| {
            let _release_construction = Release(&construction);
            let _release_reads = Release(&reads);
            let result = spawn_get(scope, &host, "first");
            reads.wait(Point::QueryBuilt);
            construction.wait(Point::MetadataComplete);
            // Establish a known contradiction after the ordinary builder completed.
            std::fs::rename(&fixture.root, fixture.scratch.join("moved-root")).unwrap();
            assert!(
                host.find(&find()).is_err(),
                "the synchronous root proof records the contradiction"
            );
            reads.release();
            let error = result.recv_timeout(BUDGET.work()).unwrap().unwrap_err();
            assert_eq!(
                error.code(),
                &ReasonCode::HostEntryNotReady,
                "neither success nor an already-built ambiguity may escape withdrawn authority"
            );
            construction.release();
        });
    }
}

#[test]
fn final_authority_check_covers_unreadable_find_bounds() {
    let fixture = Fixture::new("public-stage-final-bound-check");
    let construction = Probe::new([Point::MetadataComplete]);
    let reads = Probe::new([Point::QueryBuilt]);
    let host = fixture.host(Arc::new(SplitProbe {
        construction: construction.clone(),
        reads: reads.clone(),
    }));
    std::thread::scope(|scope| {
        let _release_construction = Release(&construction);
        let _release_reads = Release(&reads);
        let _demand = host.demand(&name()).unwrap();
        construction.wait(Point::MetadataComplete);
        let params = find().with_predicates([Predicate::equal_to("rank", "not-a-number")]);
        let host_ref = &host;
        let result = scope.spawn(move || host_ref.find(&params));
        reads.wait(Point::QueryBuilt);
        std::fs::rename(&fixture.root, fixture.scratch.join("moved-root")).unwrap();
        assert!(host.find(&find()).is_err());
        reads.release();
        let error = result.join().unwrap().unwrap_err();
        assert_eq!(
            error.code(),
            &ReasonCode::HostEntryNotReady,
            "a bound interpreted under a withdrawn declaration cannot escape"
        );
        construction.release();
    });
}

#[test]
fn late_cleanup_failure_reports_the_source_role_after_the_last_held_owner_releases() {
    let fixture = Fixture::new("public-stage-late-cleanup");
    let construction = Probe::new([Point::MetadataComplete]);
    let reads = Probe::new([]);
    let host = fixture.host(Arc::new(SplitProbe {
        construction: construction.clone(),
        reads: reads.clone(),
    }));
    std::thread::scope(|scope| {
        let _release_construction = Release(&construction);
        let _release_reads = Release(&reads);
        let first = spawn_get(scope, &host, "first");
        construction.wait(Point::MetadataComplete);
        first.recv_timeout(BUDGET.work()).unwrap().unwrap();
        reads
            .state
            .lock()
            .unwrap()
            .next
            .push_back(Point::QueryEstablished);
        let held = spawn_get(scope, &host, "first");
        reads.wait(Point::QueryEstablished);
        let paths = construction
            .files()
            .into_iter()
            .find(|(role, _)| role == "Get first")
            .unwrap()
            .1;
        construction.release();
        ready(&host);
        assert!(host.last_reader_cleanup_error().is_none());
        let retained = fixture.scratch.join("retained-loading.sqlite3");
        std::fs::rename(&paths[0], &retained).unwrap();
        std::fs::create_dir(&paths[0]).unwrap();
        assert!(
            host.last_reader_cleanup_error().is_none(),
            "Pending is still a legitimate owner"
        );
        reads.release();
        // Whatever SQLite met keeps its actual result type. Final cleanup is separate.
        let _result = held.recv_timeout(BUDGET.work()).unwrap();
        let diagnostic = host
            .last_reader_cleanup_error()
            .expect("final cleanup failure is retained");
        assert!(diagnostic.contains("Get first"), "{diagnostic}");
        assert!(
            diagnostic.contains(paths[0].to_str().unwrap()),
            "{diagnostic}"
        );
        assert_eq!(host.read_budget_census().live_connections, 0);
        assert!(
            host.get(&get("first")).is_ok(),
            "canonical data remains healthy"
        );
        std::fs::remove_dir(&paths[0]).unwrap();
        std::fs::remove_file(retained).unwrap();
    });
}

#[test]
fn a_missing_cold_loading_source_withdraws_it_once_without_poisoning_metadata() {
    let fixture = Fixture::new("public-stage-missing-source");
    let construction = Probe::new([Point::MetadataComplete]);
    let mut policy = ProductionPolicy::new(2, 2).unwrap();
    policy.read_budget = norn_store::ReadBudgetPolicy::new(2, Duration::ZERO).unwrap();
    let host = fixture.host_policy(construction.clone(), norn_host::READ_SETTLE_BOUND, policy);
    std::thread::scope(|scope| {
        let _release = Release(&construction);
        let first = spawn_get(scope, &host, "first");
        construction.wait(Point::MetadataComplete);
        first.recv_timeout(BUDGET.work()).unwrap().unwrap();
        wait_until("idle early source closes", BUDGET, || {
            if host.read_budget_census().live_connections == 0 {
                Observed::Met(())
            } else {
                Observed::pending("idle source is still open")
            }
        })
        .unwrap();
        let paths = construction
            .files()
            .into_iter()
            .find(|(role, _)| role == "Get first")
            .unwrap()
            .1;
        std::fs::remove_file(&paths[0]).unwrap();
        assert_eq!(
            host.get(&get("first")).unwrap_err().code(),
            &ReasonCode::HostReaderUnavailable
        );
        assert_eq!(
            host.get(&get("first")).unwrap_err().code(),
            &ReasonCode::HostEntryNotReady,
            "known physical source loss permanently withdraws this capability"
        );
        assert!(host.find(&find()).is_ok());
        construction.release();
        ready(&host);
        assert!(host.get(&get("first")).is_ok());
    });
}

#[test]
fn complete_canonical_store_matches_an_ordinary_reused_empty_store_after_public_early_reads() {
    use norn_testkit::equivalence::{StoreProjection, assert_operationally_valid};
    let fixture = Fixture::new("public-stage-final-index");
    std::fs::create_dir_all(fixture.root.join("one")).unwrap();
    std::fs::create_dir_all(fixture.root.join("two")).unwrap();
    std::fs::write(fixture.root.join("one/shared.md"), "# Shared\n[[first]]\n").unwrap();
    std::fs::write(fixture.root.join("two/shared.md"), "Other shared.\n").unwrap();
    std::fs::write(fixture.root.join("broken-utf8.md"), b"\xff\xfe").unwrap();
    std::fs::write(
        fixture.root.join("bad-frontmatter.md"),
        "---\n[invalid yaml\n---\nBody still derives.\n",
    )
    .unwrap();
    std::os::unix::fs::symlink("first.md", fixture.root.join("linked.md")).unwrap();
    let probe = Probe::new([Point::MetadataComplete]);
    let host = fixture.host(probe.clone());
    std::thread::scope(|scope| {
        let _release = Release(&probe);
        let request = spawn_get(scope, &host, "first");
        probe.wait(Point::MetadataComplete);
        request.recv_timeout(BUDGET.work()).unwrap().unwrap();
        host.find(&find()).unwrap();
        probe.release();
        ready(&host);
    });
    let reference_dirs = ConfigDirs::new(
        fixture.scratch.join("reference-config"),
        fixture.scratch.join("reference-data"),
    )
    .unwrap();
    let reference_path = reference_dirs.derived_dir(&name()).join("store.sqlite3");
    let seed = norn_store::Store::open(
        &reference_path,
        norn_store::StoredPathOrder::Sensitive,
        norn_host::DERIVATION_VERSION,
    )
    .unwrap();
    assert!(matches!(
        seed.open_outcome(),
        norn_store::OpenOutcome::Created
    ));
    drop(seed);
    let reference_probe = Probe::new([]);
    let reference = Host::new(
        RegistryRead::from_entries([Registration::new(
            name(),
            VaultRoot::new(&fixture.root).unwrap(),
        )]),
        ProductionEntryOps::new(reference_dirs, ProductionPolicy::new(2, 2).unwrap())
            .with_loading_probe(reference_probe.clone()),
        LifecyclePolicy {
            idle_after: Duration::from_secs(7200),
            worker_slots: 1,
            watch_poll_interval: Duration::from_millis(25),
            read_settle_bound: norn_host::READ_SETTLE_BOUND,
        },
    )
    .unwrap();
    let _reference_demand = reference.demand(&name()).unwrap();
    ready(&reference);
    assert!(
        reference_probe.files().is_empty(),
        "a Reused store never mints an early source"
    );
    let mut actual = norn_store::Store::open(
        fixture.dirs.derived_dir(&name()).join("store.sqlite3"),
        norn_store::StoredPathOrder::Sensitive,
        norn_host::DERIVATION_VERSION,
    )
    .unwrap();
    let mut ordinary = norn_store::Store::open(
        &reference_path,
        norn_store::StoredPathOrder::Sensitive,
        norn_host::DERIVATION_VERSION,
    )
    .unwrap();
    assert_operationally_valid(&mut actual, "public staged final index");
    assert_operationally_valid(&mut ordinary, "ordinary final index");
    let actual = StoreProjection::read(&mut actual).unwrap();
    let ordinary = StoreProjection::read(&mut ordinary).unwrap();
    assert!(actual.documents().len() >= 16);
    assert!(!actual.findings().is_empty());
    assert!(!actual.indexed_terms().is_empty());
    actual.assert_equivalent(
        &ordinary,
        "public staged construction preserves every final derived column",
    );
}

struct BrokenConstructor {
    inner: Arc<Probe>,
    broke: std::sync::atomic::AtomicBool,
}
impl LoadingProbe for BrokenConstructor {
    fn at(&self, boundary: LoadingBoundary<'_>) {
        self.inner.at(boundary);
        if let LoadingBoundary::SourceCreated { role, paths } = boundary
            && role == "Get first"
            && !self.broke.swap(true, std::sync::atomic::Ordering::SeqCst)
        {
            std::fs::remove_file(&paths[0]).unwrap();
            std::fs::create_dir(&paths[0]).unwrap();
        }
    }
}

#[test]
fn a_failed_constructor_waits_for_first_index_expiry_and_keeps_failure_receipts() {
    let fixture = Fixture::new("public-stage-constructor-failure");
    let construction = Probe::new([Point::MetadataComplete]);
    let failure = Arc::new(BrokenConstructor {
        inner: construction.clone(),
        broke: std::sync::atomic::AtomicBool::new(false),
    });
    let host = fixture.host_bound(failure.clone(), Duration::from_millis(200));
    std::thread::scope(|scope| {
        let _release = Release(&construction);
        let first = spawn_get(scope, &host, "first");
        construction.wait(Point::MetadataComplete);
        assert!(failure.broke.load(std::sync::atomic::Ordering::SeqCst));
        let error = first.recv_timeout(BUDGET.work()).unwrap().unwrap_err();
        assert_eq!(
            error.code(),
            &ReasonCode::HostEntryNotReady,
            "missing completeness stays on the ordinary loading path"
        );
        let paths = construction
            .files()
            .into_iter()
            .find(|(role, _)| role == "Get first")
            .unwrap()
            .1;
        let diagnostic = host
            .last_reader_cleanup_error()
            .expect("failed construction retained a receipt");
        assert!(diagnostic.contains("construction"), "{diagnostic}");
        assert!(diagnostic.contains("Get first"), "{diagnostic}");
        assert!(
            diagnostic.contains(paths[0].to_str().unwrap()),
            "{diagnostic}"
        );
        assert!(host.find(&find()).is_ok());
        std::fs::remove_dir(&paths[0]).unwrap();
        construction.release();
        ready(&host);
        assert!(host.get(&get("first")).is_ok());
        assert!(
            host.last_reader_cleanup_error().is_some(),
            "successful final publication does not erase either failure"
        );
    });
}

#[test]
fn a_failed_constructor_waits_for_canonical_ready_within_the_original_bound() {
    let fixture = Fixture::new("public-stage-constructor-ready");
    let construction = Probe::new([Point::MetadataComplete]);
    let failure = Arc::new(BrokenConstructor {
        inner: construction.clone(),
        broke: std::sync::atomic::AtomicBool::new(false),
    });
    let host = fixture.host(failure.clone());
    std::thread::scope(|scope| {
        let _release = Release(&construction);
        let result = spawn_get(scope, &host, "first");
        construction.wait(Point::MetadataComplete);
        assert!(failure.broke.load(std::sync::atomic::Ordering::SeqCst));
        assert!(
            matches!(result.try_recv(), Err(mpsc::TryRecvError::Empty)),
            "a construction failure must leave the request waiting for completeness"
        );
        construction.release();
        assert!(result.recv_timeout(BUDGET.work()).unwrap().is_ok());
        ready(&host);
        assert!(host.last_reader_cleanup_error().is_some());
    });
}

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
        wait_until("public early queries established", BUDGET, || {
            let count_now = self.state.lock().unwrap().1;
            if count_now >= count {
                Observed::Met(())
            } else {
                Observed::pending(format!("{count_now} < {count}"))
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
        assert!(
            !timeout.timed_out(),
            "public read barrier was never released"
        );
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
    fn at(&self, boundary: LoadingBoundary<'_>) {
        match boundary {
            LoadingBoundary::QueryEstablished => self.readers.established(),
            LoadingBoundary::QueryBuilt => {}
            _ => self.construction.at(boundary),
        }
    }
}

#[test]
fn early_capacity_waits_keep_the_budget_deadline_and_reselect_after_promotion_or_withdrawal() {
    for transition in ["deadline", "promotion", "withdrawal"] {
        let fixture = Fixture::new("public-stage-capacity");
        let construction = Probe::new([Point::MetadataComplete]);
        let readers = Arc::new(Readers::default());
        let mut policy = ProductionPolicy::new(2, 2).unwrap();
        policy.read_budget = norn_store::ReadBudgetPolicy::new(2, Duration::ZERO).unwrap();
        let bound = if transition == "deadline" {
            Duration::from_millis(500)
        } else {
            norn_host::READ_SETTLE_BOUND
        };
        let host = fixture.host_policy(
            Arc::new(CapacityProbe {
                construction: construction.clone(),
                readers: readers.clone(),
            }),
            bound,
            policy,
        );
        std::thread::scope(|scope| {
            let _release_construction = Release(&construction);
            let _release_reads = ReleaseReaders(&readers);
            let first = spawn_get(scope, &host, "first");
            construction.wait(Point::MetadataComplete);
            let initial = first.recv_timeout(BUDGET.work()).unwrap().unwrap();
            readers.arm();
            let held_one = spawn_get(scope, &host, "first");
            let held_two = spawn_get(scope, &host, "first");
            readers.wait(2);
            assert_eq!(host.read_budget_census().live_connections, 2);
            let waiting = spawn_get(scope, &host, "first");
            wait_until(
                "third public read waits for shared capacity",
                BUDGET,
                || {
                    if host.read_budget_census().waiting_reads == 1 {
                        Observed::Met(())
                    } else {
                        Observed::pending("third read has not queued")
                    }
                },
            )
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
                    assert_ne!(promoted.answer.reading.epoch, initial.answer.reading.epoch);
                    assert_eq!(
                        content(promoted.answer.report),
                        content(initial.answer.report)
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
                        "retirement wakes capacity acquisition before held reads release"
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

#[test]
fn a_real_first_index_overflow_withdraws_early_sources_and_still_reaches_complete_ready() {
    let fixture = Fixture::new("public-stage-native-overflow");
    std::fs::create_dir(fixture.root.join("overflow")).unwrap();
    let construction = Probe::new([Point::MetadataComplete]);
    let host = fixture.host_policy(
        construction.clone(),
        norn_host::READ_SETTLE_BOUND,
        ProductionPolicy::new(128, 128).unwrap(),
    );
    std::thread::scope(|scope| {
        let _release = Release(&construction);
        let first = spawn_get(scope, &host, "first");
        construction.wait(Point::MetadataComplete);
        first.recv_timeout(BUDGET.work()).unwrap().unwrap();
        // This is the watcher's authored cardinality boundary, not a timing load.
        // The initial heal window retains accepted facts while construction pauses.
        for index in 0..=norn_fs::watch::DIRTY_ROOT_CAP {
            std::fs::write(
                fixture.root.join(format!("overflow/file-{index:05}.txt")),
                "file",
            )
            .unwrap();
        }
        wait_until(
            "accepted overflow withdraws first-index queries",
            BUDGET,
            || {
                if host.get(&get("first")).is_err() && host.find(&find()).is_err() {
                    Observed::Met(())
                } else {
                    Observed::pending("early authority still stands")
                }
            },
        )
        .unwrap();
        construction.release();
        wait_until(
            "ordinary overflow recovery is published",
            BUDGET,
            || match host.state(&name()) {
                Err(error)
                    if matches!(
                        error.detail(),
                        norn_wire::ErrorDetail::EntryUntrusted {
                            reason: norn_wire::UntrustedReason::WatcherOverflow,
                            ..
                        }
                    ) =>
                {
                    Observed::Met(())
                }
                reading => Observed::pending(format!("{reading:?}")),
            },
        )
        .unwrap();
        ready(&host);
        assert!(host.get(&get("first")).is_ok());
        let mut store = norn_store::Store::open(
            fixture.dirs.derived_dir(&name()).join("store.sqlite3"),
            norn_store::StoredPathOrder::Sensitive,
            norn_host::DERIVATION_VERSION,
        )
        .unwrap();
        store.verify_integrity().unwrap();
        let mut after = None;
        let mut files = 0;
        loop {
            let page = store
                .begin_request()
                .stored_targets_in_range_after_ordered(
                    norn_store::SubjectScope::Vault,
                    after.as_ref(),
                    128,
                    norn_store::StoredPathOrder::Sensitive,
                )
                .unwrap();
            if page.is_empty() {
                break;
            }
            files += page
                .iter()
                .filter(|target| target.document.is_none())
                .count();
            after = page.last().map(|target| target.path.clone());
        }
        assert_eq!(files, norn_fs::watch::DIRTY_ROOT_CAP + 2);
    });
}

#[test]
fn an_established_public_get_discovers_root_loss_only_at_final_exposure() {
    let mut fixture = Fixture::new("public-stage-final-root-check");
    let ancestor = fixture.scratch.join("ancestor");
    std::fs::create_dir(&ancestor).unwrap();
    let registered = ancestor.join("vault");
    std::fs::rename(&fixture.root, &registered).unwrap();
    fixture.root = registered;
    let construction = Probe::new([Point::MetadataComplete]);
    let reads = Probe::new([Point::QueryBuilt]);
    let host = fixture.host(Arc::new(SplitProbe {
        construction: construction.clone(),
        reads: reads.clone(),
    }));
    std::thread::scope(|scope| {
        let _release_construction = Release(&construction);
        let _release_reads = Release(&reads);
        let result = spawn_get(scope, &host, "first");
        reads.wait(Point::QueryBuilt);
        construction.wait(Point::MetadataComplete);
        // Moving an unwatched ancestor leaves the covered directory and all its
        // files untouched. No other request or construction poll checks the root.
        std::fs::rename(&ancestor, fixture.scratch.join("moved-ancestor")).unwrap();
        reads.release();
        let error = result.recv_timeout(BUDGET.work()).unwrap().unwrap_err();
        assert_eq!(error.code(), &ReasonCode::HostEntryNotReady);
        construction.release();
    });
}

#[test]
fn a_later_attach_sweep_preserves_an_earlier_physical_cleanup_cause() {
    for residue in ["loading", "count"] {
        let fixture = Fixture::new("public-stage-cleanup-causes");
        let earlier = fixture
            .dirs
            .derived_dir(&VaultName::new("earlier").unwrap());
        std::fs::create_dir_all(earlier.join("store.count-results")).unwrap();
        std::fs::create_dir(earlier.join("maintainer.lock")).unwrap();
        let ops =
            ProductionEntryOps::new(fixture.dirs.clone(), ProductionPolicy::new(2, 2).unwrap());
        let prior = ops
            .last_reader_cleanup_error()
            .expect("actual startup lock failure");
        assert!(prior.contains("earlier"), "{prior}");
        let store = fixture.dirs.derived_dir(&name()).join("store.sqlite3");
        let directory = if residue == "loading" {
            norn_store::Store::loading_directory(&store)
        } else {
            store.with_extension("count-results")
        };
        std::fs::create_dir_all(directory.parent().unwrap()).unwrap();
        std::fs::write(&directory, "a file cannot be swept as a residue namespace").unwrap();
        let host = Host::new(
            RegistryRead::from_entries([Registration::new(
                name(),
                VaultRoot::new(&fixture.root).unwrap(),
            )]),
            ops,
            LifecyclePolicy {
                idle_after: Duration::from_secs(7200),
                worker_slots: 1,
                watch_poll_interval: Duration::from_millis(25),
                read_settle_bound: norn_host::READ_SETTLE_BOUND,
            },
        )
        .unwrap();
        let _demand = host.demand(&name()).unwrap();
        ready(&host);
        let diagnostic = host
            .last_reader_cleanup_error()
            .expect("both physical failures retained");
        assert!(
            diagnostic.starts_with(&prior),
            "earlier cause erased: {diagnostic}"
        );
        assert!(
            diagnostic.contains(&directory.display().to_string()),
            "later failure absent: {diagnostic}"
        );
    }
}

#[test]
fn root_loss_before_binding_declines_provisional_waiters_while_canonical_work_is_held() {
    let fixture = Fixture::new("public-stage-declined-binding");
    let registered = fixture.scratch.join("registered");
    std::os::unix::fs::symlink(&fixture.root, &registered).unwrap();
    let probe = Probe::new([Point::AuthorityCaptured, Point::CanonicalPage]);
    let host = Host::new(
        RegistryRead::from_entries([Registration::new(
            name(),
            VaultRoot::new(&registered).unwrap(),
        )]),
        ProductionEntryOps::new(fixture.dirs.clone(), ProductionPolicy::new(2, 2).unwrap())
            .with_loading_probe(probe.clone()),
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
        let result = spawn_get(scope, &host, "first");
        probe.wait(Point::AuthorityCaptured);
        waits(&host, 1);
        // Coverage keeps its physical root; only the registered spelling is lost.
        std::fs::remove_file(&registered).unwrap();
        probe.release();
        probe.wait(Point::CanonicalPage);
        let error = result.recv_timeout(BUDGET.work()).unwrap().unwrap_err();
        assert_eq!(error.code(), &ReasonCode::HostEntryNotReady);
        assert!(!error.message().contains("first time"));
        assert_eq!(host.read_evidence().settle_expiries, 0);
        probe.release();
    });
}
