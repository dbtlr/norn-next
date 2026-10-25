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
    BeforeCatalogue,
    CatalogueRecord,
    CatalogueComplete,
    GetLoaded,
    GetPublished,
    MetadataRecord,
    MetadataComplete,
    CanonicalPage,
    CanonicalComplete,
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
            LoadingBoundary::BeforeCatalogue => Point::BeforeCatalogue,
            LoadingBoundary::CatalogueRecord(_) => Point::CatalogueRecord,
            LoadingBoundary::CatalogueComplete => Point::CatalogueComplete,
            LoadingBoundary::GetLoaded(_) => Point::GetLoaded,
            LoadingBoundary::GetPublished(_) => Point::GetPublished,
            LoadingBoundary::MetadataRecord(_) => Point::MetadataRecord,
            LoadingBoundary::MetadataComplete => Point::MetadataComplete,
            LoadingBoundary::CanonicalPage => Point::CanonicalPage,
            LoadingBoundary::CanonicalComplete => Point::CanonicalComplete,
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
    fn host(&self, probe: Arc<Probe>) -> Host<ProductionEntryOps> {
        self.host_bound(probe, norn_host::READ_SETTLE_BOUND)
    }
    fn host_bound(&self, probe: Arc<Probe>, bound: Duration) -> Host<ProductionEntryOps> {
        let registration = Registration::new(name(), VaultRoot::new(&self.root).unwrap());
        Host::new(
            RegistryRead::from_entries([registration]),
            ProductionEntryOps::new(self.dirs.clone(), ProductionPolicy::new(2, 2).unwrap())
                .with_loading_probe(probe),
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
    wait_until("canonical Ready", BUDGET, || {
        let state = host.state(&name()).unwrap();
        if state == TrustState::Ready {
            Observed::Met(())
        } else {
            Observed::pending(format!("{state:?}"))
        }
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
    let host = fixture.host(Arc::clone(&probe));
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
            TrustState::Warming { .. }
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
