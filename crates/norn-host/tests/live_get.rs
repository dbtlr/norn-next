//! Private whole-Get admission stays attached to live watcher observations.
#![cfg(feature = "induced-failure")]
#![allow(clippy::disallowed_methods)]
mod attach;

use norn_host::startup_experiment::{Experiment, LiveGetReader, LiveGetRefusal, Observer, Stage};
use norn_store::{ContentModel, SnapshotReader, startup_experiment::Catalogue};
use norn_wire::{GetParams, ResolutionTarget, VaultAddress};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

struct Probe {
    params: GetParams,
    early: Mutex<Option<serde_json::Value>>,
    retained: Mutex<Option<Arc<LiveGetReader>>>,
}
impl Observer for Probe {
    fn catalogue(&self, _: &Catalogue, _: &ContentModel) -> Result<(), String> {
        Ok(())
    }
    fn indexed(
        &self,
        stage: Stage,
        _: Arc<SnapshotReader>,
        _: Arc<ContentModel>,
    ) -> Result<(), String> {
        assert_ne!(
            stage,
            Stage::CanonicalDemand,
            "live mode must not expose an early raw reader"
        );
        Ok(())
    }
    fn live_get(&self, reader: Arc<LiveGetReader>) -> Result<(), String> {
        let mut foreign = self.params.clone();
        foreign.target = ResolutionTarget::new("later").unwrap();
        assert!(matches!(
            reader.get(&foreign, Instant::now() + Duration::from_secs(5)),
            Err(LiveGetRefusal::Unsupported)
        ));
        let mut anchored = self.params.clone();
        anchored.target = ResolutionTarget::new("first#Details").unwrap();
        assert!(matches!(
            reader.get(&anchored, Instant::now() + Duration::from_secs(5)),
            Err(LiveGetRefusal::Unsupported)
        ));
        let projected = self
            .params
            .clone()
            .with_columns([norn_wire::Column::path()]);
        assert!(matches!(
            reader.get(&projected, Instant::now() + Duration::from_secs(5)),
            Err(LiveGetRefusal::Unsupported)
        ));
        assert!(matches!(
            reader.get(&self.params, Instant::now()),
            Err(LiveGetRefusal::Unavailable)
        ));
        let got = reader
            .get(&self.params, Instant::now() + Duration::from_secs(5))
            .unwrap();
        assert!(got.unsatisfied.is_empty());
        *self.early.lock().unwrap() = Some(serde_json::to_value(got.report).unwrap());
        *self.retained.lock().unwrap() = Some(reader);
        Ok(())
    }
}

#[test]
fn unchanged_live_whole_get_equals_canonical_rich_record_and_retires() {
    let scratch = norn_testkit::scratch::Scratch::under(
        std::path::Path::new(env!("CARGO_TARGET_TMPDIR")),
        "live-get",
    )
    .unwrap();
    let root = scratch.join("vault");
    std::fs::create_dir_all(root.join(".norn")).unwrap();
    std::fs::write(
        root.join(".norn/schema.yaml"),
        "version: 1\nfields:\n  rating: { type: number }\n",
    )
    .unwrap();
    std::fs::write(root.join("first.md"), "---\nstatus: draft\nrating: nope\ntags: [alpha, beta]\n---\n# First\n[[later#Absent]] [[missing]] [[asset.png]]\n## Details\nText ^block\n").unwrap();
    std::fs::write(root.join("later.md"), "# Later\nOther body\n").unwrap();
    std::fs::write(root.join("asset.png"), "asset").unwrap();
    let vault = attach::Vault::adopt(scratch.root());
    let params = GetParams::new(
        VaultAddress::name(vault.name().clone()),
        ResolutionTarget::new("first").unwrap(),
    );
    let probe = Arc::new(Probe {
        params: params.clone(),
        early: Mutex::new(None),
        retained: Mutex::new(None),
    });
    let host = vault.host_with_startup_experiment(
        norn_host::ProductionPolicy::new(32, 32).unwrap(),
        Experiment::live_whole_get("first", probe.clone()),
    );
    let _demand = attach::attach_and_wait(&host, vault.name());
    let early = probe
        .early
        .lock()
        .unwrap()
        .take()
        .expect("early complete Get");
    assert_eq!(
        answer_content(early.clone()),
        answer_content(serde_json::to_value(host.get(&params).unwrap().answer.report).unwrap())
    );
    let encoded = early.to_string();
    assert!(encoded.contains("missing-anchor"), "{encoded}");
    assert!(encoded.contains("broken"), "{encoded}");
    let held = probe.retained.lock().unwrap().take().unwrap();
    assert!(matches!(
        held.get(&params, Instant::now() + Duration::from_secs(5)),
        Err(LiveGetRefusal::Unavailable)
    ));
}

// Finding handles and generations belong to independent disposable/canonical
// stores. Compare every content field, including full findings and link heads.
fn answer_content(mut value: serde_json::Value) -> serde_json::Value {
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

#[derive(Clone, Copy, PartialEq, Eq)]
enum Point {
    Inventory,
    Catalogue,
    Dependency,
    Delayed,
    Root,
}
struct Movement {
    root: std::path::PathBuf,
    point: Point,
    params: GetParams,
    observation: Mutex<Option<norn_fs::watch::Observation>>,
    changed: std::sync::atomic::AtomicBool,
    offered: std::sync::atomic::AtomicBool,
    declined: std::sync::atomic::AtomicBool,
    retained: Mutex<Option<Arc<LiveGetReader>>>,
}
impl Movement {
    fn change(&self) {
        use std::sync::atomic::Ordering;
        if self.changed.swap(true, Ordering::SeqCst) {
            return;
        }
        match self.point {
            Point::Inventory | Point::Catalogue => {
                std::fs::write(self.root.join("folder/first.md"), "Competing name\n").unwrap()
            }
            Point::Dependency => {
                std::fs::write(self.root.join("later.md"), "# Absent\nChanged dependency\n")
                    .unwrap()
            }
            Point::Delayed => {
                std::fs::write(self.root.join("one/first.md"), "Changed target [[later]]\n")
                    .unwrap()
            }
            Point::Root => {
                std::fs::rename(&self.root, self.root.with_file_name("moved-vault")).unwrap()
            }
        }
        let observation = self.observation.lock().unwrap().clone().unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while observation.is_current() {
            assert!(Instant::now() < deadline, "watcher did not accept the edit");
            std::thread::yield_now();
        }
    }
}
impl Observer for Movement {
    fn live_observation(&self, observation: &norn_fs::watch::Observation) -> Result<(), String> {
        assert!(observation.is_current());
        *self.observation.lock().unwrap() = Some(observation.clone());
        Ok(())
    }
    fn catalogue_replay(
        &self,
        path: &std::path::Path,
        _: &mut Catalogue,
        _: &ContentModel,
    ) -> Result<(), String> {
        if self.point == Point::Inventory && path == std::path::Path::new("one/first.md") {
            self.change();
        }
        Ok(())
    }
    fn catalogue(&self, _: &Catalogue, _: &ContentModel) -> Result<(), String> {
        if self.point == Point::Catalogue {
            self.change();
        }
        Ok(())
    }
    fn demand_loaded(&self, path: &str) -> Result<(), String> {
        if self.point == Point::Dependency && path == "later.md" {
            self.change();
        }
        Ok(())
    }
    fn indexed(
        &self,
        stage: Stage,
        _: Arc<SnapshotReader>,
        _: Arc<ContentModel>,
    ) -> Result<(), String> {
        assert_ne!(stage, Stage::CanonicalDemand);
        Ok(())
    }
    fn declined(&self, _: &str) -> Result<(), String> {
        self.declined
            .store(true, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    }
    fn live_get(&self, reader: Arc<LiveGetReader>) -> Result<(), String> {
        self.offered
            .store(true, std::sync::atomic::Ordering::SeqCst);
        assert!(
            matches!(self.point, Point::Delayed | Point::Root),
            "observed movement published early Get"
        );
        assert!(
            reader
                .get(&self.params, Instant::now() + Duration::from_secs(5))
                .is_ok()
        );
        // A retained observer capability must re-admit the actual call.
        *self.retained.lock().unwrap() = Some(reader.clone());
        self.change();
        assert!(matches!(
            reader.get(&self.params, Instant::now() + Duration::from_secs(5)),
            Err(LiveGetRefusal::Unavailable)
        ));
        Ok(())
    }
}

fn movement(point: Point) {
    use std::sync::atomic::{AtomicBool, Ordering};
    let scratch = norn_testkit::scratch::Scratch::under(
        std::path::Path::new(env!("CARGO_TARGET_TMPDIR")),
        "live-get-movement",
    )
    .unwrap();
    let root = scratch.join("vault");
    std::fs::create_dir_all(root.join(".norn")).unwrap();
    std::fs::create_dir_all(root.join("folder")).unwrap();
    std::fs::create_dir_all(root.join("one")).unwrap();
    std::fs::write(root.join(".norn/schema.yaml"), "version: 1\n").unwrap();
    std::fs::write(root.join("one/first.md"), "[[later#Absent]]\n").unwrap();
    std::fs::write(root.join("later.md"), "# Later\n").unwrap();
    let vault = attach::Vault::adopt(scratch.root());
    let params = GetParams::new(
        VaultAddress::name(vault.name().clone()),
        ResolutionTarget::new("first").unwrap(),
    );
    let probe = Arc::new(Movement {
        root,
        point,
        params: params.clone(),
        observation: Mutex::new(None),
        changed: AtomicBool::new(false),
        offered: AtomicBool::new(false),
        declined: AtomicBool::new(false),
        retained: Mutex::new(None),
    });
    let host = vault.host_with_startup_experiment(
        norn_host::ProductionPolicy::new(32, 32).unwrap(),
        Experiment::live_whole_get("first", probe.clone()),
    );
    let _demand = host.demand(vault.name()).unwrap();
    if point == Point::Root {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match host.state(vault.name()) {
                Err(_) => break,
                Ok(norn_wire::TrustState::Ready) => panic!("root loss published Ready"),
                _ => (),
            }
            assert!(Instant::now() < deadline);
            std::thread::yield_now();
        }
    } else {
        attach::wait_for_ready(&host, vault.name(), Duration::from_secs(10));
        if matches!(point, Point::Inventory | Point::Catalogue) {
            let refusal = host
                .get(&params)
                .expect_err("ordinary readiness must reflect the competing name");
            assert_eq!(refusal.code(), &norn_wire::ReasonCode::VaultAmbiguousTarget);
            assert!(probe.declined.load(Ordering::SeqCst));
            assert!(!probe.offered.load(Ordering::SeqCst));
        } else {
            let got = host.get(&params).unwrap();
            let encoded = serde_json::to_string(&got.answer.report).unwrap();
            if point == Point::Dependency {
                assert!(
                    !encoded.contains("missing-anchor"),
                    "ordinary health must see the changed dependency"
                );
                assert!(probe.declined.load(Ordering::SeqCst));
                assert!(!probe.offered.load(Ordering::SeqCst));
            } else {
                assert!(encoded.contains("Changed target"));
            }
        }
    }
    assert!(probe.changed.load(Ordering::SeqCst));
    if let Some(held) = probe.retained.lock().unwrap().as_ref() {
        assert!(matches!(
            held.get(&params, Instant::now() + Duration::from_secs(5)),
            Err(LiveGetRefusal::Unavailable)
        ));
    }
}

#[test]
fn competing_name_during_open_heal_or_inventory_declines_then_ready_is_ambiguous() {
    movement(Point::Catalogue);
    movement(Point::Inventory);
}
#[test]
fn observed_dependency_edit_declines_then_ordinary_health_reflects_it() {
    movement(Point::Dependency);
}
#[test]
fn delayed_live_handle_cannot_serve_after_target_invalidation_or_root_loss() {
    movement(Point::Delayed);
    movement(Point::Root);
}
