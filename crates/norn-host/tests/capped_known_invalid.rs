#![cfg(feature = "induced-failure")]
#![allow(clippy::disallowed_methods)]
mod attach;
use norn_host::startup_experiment::{Experiment, FrozenInput, Observer, PrefixReader, Stage};
use norn_store::{ContentModel, ReadNeed, SnapshotReader, startup_experiment::Catalogue};
use norn_wire::{FindParams, GetParams, Predicate, ResolutionTarget, TrustState, VaultAddress};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

struct Probe {
    frozen: Arc<FrozenInput>,
    invalidate: bool,
    address: VaultAddress,
    get_answered: Mutex<bool>,
    get_notifications: Mutex<usize>,
    prefix_invalid: Mutex<Option<String>>,
}
impl Observer for Probe {
    fn catalogue(&self, _: &Catalogue, _: &ContentModel) -> Result<(), String> {
        if self.invalidate {
            self.frozen.invalidate();
        }
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
            let params = GetParams::new(
                self.address.clone(),
                ResolutionTarget::new("small").unwrap(),
            );
            *self.get_answered.lock().unwrap() =
                norn_host::startup_experiment::get(&snapshot, &params, &model).is_ok();
        }
        Ok(())
    }
    fn prefix(&self, reader: &PrefixReader) -> Result<(), String> {
        let attempt = reader.find(
            &FindParams::new(self.address.clone())
                .with_limit(25)
                .with_predicates([Predicate::equal_to("perf_bucket", "active")]),
        );
        *self.prefix_invalid.lock().unwrap() = attempt.certificate.invalid;
        Ok(())
    }
}
fn run(invalidate: bool) -> (usize, bool, Option<String>) {
    let scratch = norn_testkit::scratch::Scratch::new("review-known-invalid");
    let root = scratch.join("vault");
    std::fs::create_dir_all(root.join(".norn")).unwrap();
    std::fs::write(root.join(".norn/schema.yaml"), "version: 1\n").unwrap();
    std::fs::write(
        root.join("small.md"),
        "---\nperf_bucket: active\n---\nSmall\n",
    )
    .unwrap();
    let vault = attach::Vault::adopt(scratch.root());
    let frozen = FrozenInput::assume_immutable();
    let probe = Arc::new(Probe {
        frozen: frozen.clone(),
        invalidate,
        address: VaultAddress::name(vault.name().clone()),
        get_answered: Mutex::new(false),
        get_notifications: Mutex::new(0),
        prefix_invalid: Mutex::new(None),
    });
    let host = vault.host_with_startup_experiment(
        norn_host::ProductionPolicy::new(32, 32).unwrap(),
        Experiment::capped_prefix_frozen("small", probe.clone(), frozen),
    );
    let demand = host.demand(vault.name()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    while host.state(vault.name()).unwrap() != TrustState::Ready {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    }
    drop(demand);
    drop(host);
    let answered = *probe.get_answered.lock().unwrap();
    let invalid = probe.prefix_invalid.lock().unwrap().clone();
    eprintln!("early_get_answered={answered}; prefix_invalid={invalid:?}; host_finished=true");
    let notifications = *probe.get_notifications.lock().unwrap();
    (notifications, answered, invalid)
}
#[test]
fn known_invalid_frozen_never_emits_get() {
    let (notifications, answered, invalid) = run(true);
    assert_eq!(notifications, 0);
    assert!(
        invalid
            .as_deref()
            .unwrap()
            .contains("frozen-input certificate broken")
    );
    assert!(
        !answered,
        "private early Get emitted after explicit certificate withdrawal"
    );
}

struct ChangedCandidate {
    address: VaultAddress,
    root: std::path::PathBuf,
    phase: &'static str,
    attempt: Mutex<Option<norn_host::startup_experiment::PrefixAttempt>>,
}
impl Observer for ChangedCandidate {
    fn catalogue(&self, _: &Catalogue, _: &ContentModel) -> Result<(), String> {
        Ok(())
    }
    fn indexed(
        &self,
        stage: Stage,
        _: Arc<SnapshotReader>,
        _: Arc<ContentModel>,
    ) -> Result<(), String> {
        if stage == Stage::CanonicalDemand {
            match self.phase {
                "vanish" => std::fs::remove_file(self.root.join("a.md")).unwrap(),
                "rename" => {
                    std::fs::rename(self.root.join("a.md"), self.root.join("z.md")).unwrap()
                }
                _ => (),
            }
        }
        Ok(())
    }
    fn prefix_loaded(&self, path: &str) -> Result<(), String> {
        if self.phase == "rehash" && path == "a.md" {
            std::fs::write(
                self.root.join(path),
                "---\nperf_bucket: inactive\n---\nChanged\n",
            )
            .unwrap();
        }
        Ok(())
    }
    fn prefix(&self, reader: &PrefixReader) -> Result<(), String> {
        *self.attempt.lock().unwrap() = Some(
            reader.find(
                &FindParams::new(self.address.clone())
                    .with_limit(25)
                    .with_predicates([Predicate::equal_to("perf_bucket", "active")]),
            ),
        );
        Ok(())
    }
}
#[test]
fn consumed_candidates_that_vanish_rename_or_change_before_rehash_never_publish() {
    for phase in ["vanish", "rename", "rehash"] {
        let scratch = norn_testkit::scratch::Scratch::new("capped-consumed-change");
        let root = scratch.join("vault");
        std::fs::create_dir_all(root.join(".norn")).unwrap();
        std::fs::write(root.join(".norn/schema.yaml"), "version: 1\n").unwrap();
        std::fs::write(root.join("small.md"), "Small\n").unwrap();
        std::fs::write(root.join("a.md"), "---\nperf_bucket: active\n---\nBody\n").unwrap();
        let vault = attach::Vault::adopt(scratch.root());
        let address = VaultAddress::name(vault.name().clone());
        let observer = Arc::new(ChangedCandidate {
            address: address.clone(),
            root,
            phase,
            attempt: Mutex::new(None),
        });
        // Deliberately do not withdraw authority: these probes exercise the
        // consumed-candidate checks, not a general live freshness contract.
        let host = vault.host_with_startup_experiment(
            norn_host::ProductionPolicy::new(32, 32).unwrap(),
            Experiment::capped_prefix_frozen(
                "small",
                observer.clone(),
                FrozenInput::assume_immutable(),
            ),
        );
        let demand = host.demand(vault.name()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(30);
        while host.state(vault.name()).unwrap() != TrustState::Ready {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(1));
        }
        let attempt = observer.attempt.lock().unwrap().take().unwrap();
        assert!(
            attempt.answer.is_none(),
            "{phase} published an unsafe answer"
        );
        assert!(!attempt.query_executed);
        let invalid = attempt.certificate.invalid.unwrap();
        assert!(
            invalid.contains(if phase == "rehash" {
                "changed/vanished bytes"
            } else {
                "changed/vanished candidate"
            }),
            "{invalid}"
        );
        assert!(
            host.find(
                &FindParams::new(address)
                    .with_limit(25)
                    .with_predicates([Predicate::equal_to("perf_bucket", "active")])
            )
            .is_ok()
        );
        drop(demand);
        drop(host);
    }
}

#[test]
fn intact_frozen_input_preserves_the_complete_get_notification() {
    let (notifications, answered, invalid) = run(false);
    assert_eq!(notifications, 1);
    assert!(answered);
    assert!(invalid.is_none());
}
