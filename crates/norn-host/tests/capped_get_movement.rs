//! Observed Get closure movement must also close the subsequent capped Find.
#![cfg(feature = "induced-failure")]
#![allow(clippy::disallowed_methods)]
mod attach;
use norn_host::startup_experiment::{Experiment, FrozenInput, Observer, PrefixReader, Stage};
use norn_store::{ContentModel, ReadNeed, SnapshotReader, startup_experiment::Catalogue};
use norn_wire::{FindParams, GetParams, Predicate, ResolutionTarget, TrustState, VaultAddress};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

struct FindObservation {
    published: bool,
    query_executed: bool,
    decline: Option<String>,
    invalid: Option<String>,
    rows: Vec<String>,
}

struct Moved {
    address: VaultAddress,
    root: std::path::PathBuf,
    mode: &'static str,
    get_notifications: Mutex<usize>,
    declined: Mutex<Vec<String>>,
    attempt: Mutex<Option<FindObservation>>,
}
impl Observer for Moved {
    fn catalogue(&self, _: &Catalogue, _: &ContentModel) -> Result<(), String> {
        // Deliberately unannounced: no FrozenInput::invalidate. The Get's own
        // closure preparation is the component that observes this movement.
        match self.mode {
            "rename-get-target-earlier" => {
                std::fs::rename(self.root.join("zz-target.md"), self.root.join("0-early.md"))
                    .unwrap()
            }
            "vanish-get-target" => std::fs::remove_file(self.root.join("zz-target.md")).unwrap(),
            _ => (),
        }
        Ok(())
    }
    fn demand_loaded(&self, path: &str) -> Result<(), String> {
        if path == "zz-target.md" {
            match self.mode {
                "write-before-rehash" => std::fs::write(
                    self.root.join(path),
                    "---\nperf_bucket: inactive\n---\nChanged\n",
                )
                .unwrap(),
                "vanish-before-rehash" => std::fs::remove_file(self.root.join(path)).unwrap(),
                _ => (),
            }
        }
        Ok(())
    }
    fn declined(&self, reason: &str) -> Result<(), String> {
        self.declined.lock().unwrap().push(reason.to_string());
        Ok(())
    }
    fn indexed(
        &self,
        stage: Stage,
        reader: Arc<SnapshotReader>,
        model: Arc<ContentModel>,
    ) -> Result<(), String> {
        if stage == Stage::CanonicalDemand {
            *self.get_notifications.lock().unwrap() += 1;
            let turn = reader
                .acquire(ReadNeed::Source, Instant::now() + Duration::from_secs(5))
                .unwrap()
                .unwrap();
            let snapshot = turn.establish().unwrap();
            let get = GetParams::new(
                self.address.clone(),
                ResolutionTarget::new("zz-target").unwrap(),
            );
            assert!(norn_host::startup_experiment::get(&snapshot, &get, &model).is_ok());
        }
        Ok(())
    }
    fn prefix(&self, reader: &PrefixReader) -> Result<(), String> {
        let attempt = reader.find(&query(&self.address));
        let published = attempt.answer.is_some();
        let rows = attempt
            .answer
            .map(|found| {
                let (_, _, report) = found.into_report();
                report
                    .page
                    .rows
                    .iter()
                    .map(|r| r.path.as_str().to_string())
                    .collect()
            })
            .unwrap_or_default();
        *self.attempt.lock().unwrap() = Some(FindObservation {
            published,
            query_executed: attempt.query_executed,
            decline: attempt.decline,
            invalid: attempt.certificate.invalid,
            rows,
        });
        Ok(())
    }
}
fn query(address: &VaultAddress) -> FindParams {
    FindParams::new(address.clone())
        .with_limit(25)
        .with_predicates([Predicate::equal_to("perf_bucket", "active")])
}

fn run(mode: &'static str) {
    let scratch = norn_testkit::scratch::Scratch::new("capped-get-movement");
    let root = scratch.join("vault");
    std::fs::create_dir_all(root.join(".norn")).unwrap();
    std::fs::write(root.join(".norn/schema.yaml"), "version: 1\n").unwrap();
    for i in 0..150 {
        std::fs::write(
            root.join(format!("a-{i:03}.md")),
            "---\nperf_bucket: active\n---\nBody\n",
        )
        .unwrap();
    }
    std::fs::write(
        root.join("zz-target.md"),
        "---\nperf_bucket: active\n---\nTarget\n",
    )
    .unwrap();
    if mode == "bound" {
        for i in 0..257 {
            let directory = root.join(format!("group-{i:03}"));
            std::fs::create_dir(&directory).unwrap();
            std::fs::write(directory.join("crowded.md"), "Crowded\n").unwrap();
        }
    }
    let target = match mode {
        "unsupported" => "zz-target#Heading",
        "missing" => "absent",
        "bound" => "crowded",
        _ => "zz-target",
    };
    let vault = attach::Vault::adopt(scratch.root());
    let address = VaultAddress::name(vault.name().clone());
    let observer = Arc::new(Moved {
        address: address.clone(),
        root,
        mode,
        get_notifications: Mutex::new(0),
        declined: Mutex::new(Vec::new()),
        attempt: Mutex::new(None),
    });
    let host = vault.host_with_startup_experiment(
        norn_host::ProductionPolicy::new(32, 32).unwrap(),
        Experiment::capped_prefix_frozen(target, observer.clone(), FrozenInput::assume_immutable()),
    );
    let demand = host.demand(vault.name()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(60);
    while host.state(vault.name()).unwrap() != TrustState::Ready {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    }
    let canonical: Vec<String> = host
        .find(&query(&address))
        .unwrap()
        .answer
        .report
        .page
        .rows
        .iter()
        .map(|r| r.path.as_str().to_string())
        .collect();
    drop(demand);
    drop(host);
    let FindObservation {
        published,
        query_executed,
        decline,
        invalid,
        rows: early,
    } = observer.attempt.lock().unwrap().take().unwrap();
    let declined = observer.declined.lock().unwrap().clone();
    let notifications = *observer.get_notifications.lock().unwrap();
    eprintln!(
        "mode={mode} get_notifications={notifications} declined={declined:?} \
         find_published={published} find_decline={decline:?} find_invalid={invalid:?} \
         early_first={:?} canonical_first={:?} early_eq_canonical={}",
        early.first(),
        canonical.first(),
        early == canonical
    );
    if matches!(mode, "none" | "unsupported" | "missing" | "bound") {
        assert_eq!(notifications, usize::from(mode == "none"));
        assert!(invalid.is_none());
        assert!(query_executed);
        assert!(published && early == canonical);
        return;
    }
    assert_eq!(
        notifications, 0,
        "Get must have declined on observed movement"
    );
    assert!(
        !published,
        "{mode}: early Find published after the Get closure observed source movement; \
         early_first={:?} canonical_first={:?}",
        early.first(),
        canonical.first()
    );
    assert!(!query_executed);
    assert_eq!(invalid.as_deref(), Some("frozen-input certificate broken"));
}

#[test]
fn get_closure_movement_rename_earlier() {
    run("rename-get-target-earlier");
}

#[test]
fn get_closure_movement_vanish() {
    run("vanish-get-target");
}

#[test]
fn intact_get_and_find_match_ready() {
    run("none");
}

#[test]
fn get_closure_write_before_rehash_closes_find() {
    run("write-before-rehash");
}

#[test]
fn get_closure_vanish_before_rehash_closes_find() {
    run("vanish-before-rehash");
}

#[test]
fn legitimate_get_nonclaims_preserve_valid_find() {
    for mode in ["unsupported", "missing", "bound"] {
        run(mode);
    }
}
