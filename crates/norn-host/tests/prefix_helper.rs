//! Private native lifecycle controls. Every clock in this target is nonqualifying.
#![cfg(feature = "induced-failure")]
#![allow(clippy::disallowed_methods)]
mod attach;
use norn_host::startup_experiment::{Experiment, FrozenInput, Observer, PrefixReader, Stage};
use norn_store::{ContentModel, SnapshotReader, startup_experiment::Catalogue};
use norn_wire::{FindParams, GetParams, Predicate, VaultAddress};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

struct Transcript {
    address: VaultAddress,
    stages: Mutex<Vec<Stage>>,
    find_ran: Mutex<bool>,
}
impl Observer for Transcript {
    fn catalogue(&self, _: &Catalogue, _: &ContentModel) -> Result<(), String> {
        Ok(())
    }
    fn indexed(
        &self,
        stage: Stage,
        reader: Arc<SnapshotReader>,
        model: Arc<ContentModel>,
    ) -> Result<(), String> {
        self.stages.lock().unwrap().push(stage);
        if stage == Stage::CanonicalDemand {
            let snapshot = reader
                .acquire(
                    norn_store::ReadNeed::Source,
                    Instant::now() + Duration::from_secs(5),
                )
                .unwrap()
                .unwrap()
                .establish()
                .unwrap();
            let params = GetParams::new(
                self.address.clone(),
                norn_wire::ResolutionTarget::new("first").unwrap(),
            );
            let (unsatisfied, report) =
                norn_host::startup_experiment::get(&snapshot, &params, &model)
                    .unwrap()
                    .into_report();
            assert!(unsatisfied.is_empty());
            let norn_wire::GetReport::Record { document, .. } = report else {
                panic!("whole record required")
            };
            assert_eq!(document.body.unwrap().text(), "# First\n");
        }
        Ok(())
    }
    fn prefix(&self, reader: &PrefixReader) -> Result<(), String> {
        let attempt = reader.find(
            &FindParams::new(self.address.clone())
                .with_limit(25)
                .with_predicates([Predicate::equal_to("perf_bucket", "active")]),
        );
        assert!(attempt.answer.is_some());
        *self.find_ran.lock().unwrap() = attempt.query_executed;
        Ok(())
    }
}

#[test]
fn successful_prefix_helper_stops_before_canonical_heal_or_ready() {
    let scratch = norn_testkit::scratch::Scratch::under(
        std::path::Path::new(env!("CARGO_TARGET_TMPDIR")),
        "prefix-helper",
    )
    .unwrap();
    std::fs::create_dir_all(scratch.join("vault/.norn")).unwrap();
    std::fs::write(
        scratch.join("vault/.norn/schema.yaml"),
        "version: 1\nfields:\n  perf_bucket: { type: text }\n",
    )
    .unwrap();
    std::fs::write(
        scratch.join("vault/first.md"),
        "---\nperf_bucket: active\n---\n# First\n",
    )
    .unwrap();
    let vault = attach::Vault::adopt(scratch.root());
    let probe = Arc::new(Transcript {
        address: VaultAddress::name(vault.name().clone()),
        stages: Mutex::new(Vec::new()),
        find_ran: Mutex::new(false),
    });
    let experiment =
        Experiment::capped_prefix_frozen("first", probe.clone(), FrozenInput::assume_immutable());
    let completed = vault
        .prefix_helper(&experiment, &Default::default())
        .unwrap();
    assert_eq!(completed.budget().live_connections, 0);
    assert_eq!(completed.budget().waiting_reads, 0);
    assert!(!completed.retired_paths().is_empty());
    for path in completed.retired_paths() {
        assert!(!path.exists(), "{}", path.display());
    }
    assert!(*probe.find_ran.lock().unwrap());
    assert_eq!(
        *probe.stages.lock().unwrap(),
        [Stage::CanonicalDemand],
        "successful helper must stop before canonical health and ordinary Ready"
    );
}
