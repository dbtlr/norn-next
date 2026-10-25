//! Measurement receipts must describe the actual guarded call, without extra reads.
#![cfg(feature = "induced-failure")]
#![allow(clippy::disallowed_methods)]
mod attach;

use norn_host::startup_experiment::{
    Experiment, LiveFindReader, LiveGetReader, LivePhase, Observer, Stage,
};
use norn_store::{ContentModel, SnapshotReader, startup_experiment::Catalogue};
use norn_wire::{FindParams, GetParams, ResolutionTarget, VaultAddress};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

struct Probe {
    address: VaultAddress,
    declines: Mutex<Vec<LivePhase>>,
    served: Mutex<bool>,
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
        assert_eq!(stage, Stage::CanonicalHealth);
        Ok(())
    }
    fn live_declined(&self, phase: LivePhase, _: &str) -> Result<(), String> {
        self.declines.lock().unwrap().push(phase);
        Ok(())
    }
    fn live_get(&self, _: Arc<LiveGetReader>) -> Result<(), String> {
        panic!("missing Get must decline construction")
    }
    fn live_find(&self, reader: Arc<LiveFindReader>) -> Result<(), String> {
        let params = FindParams::new(self.address.clone());
        let unavailable = reader.find_measured(&params, Instant::now());
        assert!(!unavailable.evidence.builder_invoked);
        assert!(unavailable.evidence.reading.is_none());
        assert!(unavailable.evidence.counters.is_none());
        let attempt = reader.find_measured(&params, Instant::now() + Duration::from_secs(5));
        assert!(attempt.evidence.builder_invoked);
        assert!(attempt.withdrawn.is_none());
        let answer = attempt.result.unwrap();
        let reading = attempt.evidence.reading.unwrap();
        assert_eq!(answer.snapshot.epoch, reading.epoch());
        assert_eq!(
            answer.snapshot.generation,
            u64::try_from(reading.write_generation()).unwrap()
        );
        let counters = attempt.evidence.counters.unwrap();
        assert_eq!(counters.snapshots_opened(), 1);
        assert_eq!(
            answer.work.statements,
            counters.statements_executed()
                - attempt
                    .evidence
                    .counters_before_builder
                    .unwrap()
                    .statements_executed()
        );
        assert!(counters.vm_steps() > 0);
        assert_eq!(answer.rows.len(), 1);
        *self.served.lock().unwrap() = true;
        Ok(())
    }
}
#[test]
fn get_construction_decline_is_phase_tagged_and_find_still_serves_with_actual_evidence() {
    let scratch = norn_testkit::scratch::Scratch::under(
        std::path::Path::new(env!("CARGO_TARGET_TMPDIR")),
        "live-measurement",
    )
    .unwrap();
    std::fs::create_dir_all(scratch.join("vault")).unwrap();
    std::fs::write(scratch.join("vault/first.md"), "# First\n").unwrap();
    let vault = attach::Vault::adopt(scratch.root());
    let probe = Arc::new(Probe {
        address: VaultAddress::name(vault.name().clone()),
        declines: Mutex::new(Vec::new()),
        served: Mutex::new(false),
    });
    let host = vault.host_with_startup_experiment(
        norn_host::ProductionPolicy::new(32, 32).unwrap(),
        Experiment::live_metadata("missing", probe.clone()),
    );
    let _demand = attach::attach_and_wait(&host, vault.name());
    assert_eq!(*probe.declines.lock().unwrap(), [LivePhase::Get]);
    assert!(*probe.served.lock().unwrap());
    // Ordinary public Get keeps its ordinary missing-result semantics.
    let params = GetParams::new(
        probe.address.clone(),
        ResolutionTarget::new("missing").unwrap(),
    );
    assert!(host.get(&params).is_err());
}
