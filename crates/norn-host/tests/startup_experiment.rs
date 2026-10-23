//! Early query checkpoints must carry real answers before rich derivation.
#![cfg(feature = "induced-failure")]
#![allow(clippy::disallowed_methods)] // Isolated fixture arrangement.

mod attach;

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use norn_host::startup_experiment::{Experiment, Observer, Stage};
use norn_store::{ContentModel, ReadNeed, SnapshotReader, startup_experiment::Catalogue};
use norn_wire::{Column, FindParams, Predicate, VaultAddress, VaultName};

#[derive(Default)]
struct Probe {
    stages: Mutex<Vec<Stage>>,
}

impl Observer for Probe {
    fn catalogue(&self, catalogue: &Catalogue, model: &ContentModel) -> Result<(), String> {
        let candidates = catalogue
            .candidates("first", model.ambiguity_ignore())
            .unwrap()
            .unwrap();
        assert_eq!(candidates.paths, ["first.md"]);
        Ok(())
    }

    fn indexed(
        &self,
        stage: Stage,
        reader: Arc<SnapshotReader>,
        model: Arc<ContentModel>,
    ) -> Result<(), String> {
        let turn = reader
            .acquire(ReadNeed::Source, Instant::now() + Duration::from_secs(5))
            .unwrap()
            .unwrap();
        let snapshot = turn.establish().unwrap();
        let get = norn_wire::GetParams::new(
            VaultAddress::name(VaultName::new("notes").unwrap()),
            norn_wire::ResolutionTarget::new("first").unwrap(),
        )
        .with_columns([
            Column::path(),
            Column::body(),
            Column::fields(),
            Column::links(),
        ]);
        let (unsatisfied, report) = norn_host::startup_experiment::get(&snapshot, &get, &model)
            .unwrap()
            .into_report();
        assert!(unsatisfied.is_empty());
        let norn_wire::GetReport::Record { document, .. } = report else {
            panic!("record expected")
        };
        assert_eq!(document.path.as_str(), "first.md");
        assert_eq!(document.body.unwrap().text(), "[[later]] #first\n");
        let expected_links = u64::from(matches!(stage, Stage::RichFacts | Stage::LinkHealth));
        assert_eq!(document.links.unwrap().total, expected_links);
        if stage == Stage::Frontmatter {
            let params = FindParams::new(VaultAddress::name(VaultName::new("notes").unwrap()))
                .with_predicates([Predicate::equal_to("status", "active")])
                .with_columns([Column::path()]);
            let result = snapshot.find(&params, &model).unwrap();
            let (unsatisfied, _, report) = result.into_report();
            assert!(unsatisfied.is_empty());
            let encoded = serde_json::to_value(report).unwrap();
            assert!(encoded.to_string().contains("later.md"));
            assert!(!encoded.to_string().contains("first.md"));
        }
        self.stages.lock().unwrap().push(stage);
        Ok(())
    }
}

#[test]
fn staged_queries_precede_the_same_final_healthy_index() {
    let scratch = norn_testkit::scratch::Scratch::new("norn-startup-stages");
    let root = scratch.join("vault");
    std::fs::create_dir_all(root.join(".norn")).unwrap();
    std::fs::write(root.join(".norn/schema.yaml"), "version: 1\n").unwrap();
    std::fs::write(
        root.join("first.md"),
        "---\nstatus: draft\n---\n[[later]] #first\n",
    )
    .unwrap();
    std::fs::write(
        root.join("later.md"),
        "---\nstatus: active\n---\n# Target\n",
    )
    .unwrap();
    let vault = attach::Vault::adopt(scratch.root());
    let probe = Arc::new(Probe::default());
    let host = vault.host_with_startup_experiment(
        norn_host::ProductionPolicy::new(128, 128).unwrap(),
        Experiment::new("first", probe.clone()),
    );
    let _demand = host.demand(vault.name()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(15);
    while host.state(vault.name()).unwrap() != norn_wire::TrustState::Ready {
        assert!(Instant::now() < deadline, "experiment did not become ready");
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(
        *probe.stages.lock().unwrap(),
        [
            Stage::RequestedDocument,
            Stage::Frontmatter,
            Stage::RichFacts,
            Stage::LinkHealth
        ]
    );
    drop(_demand);
    drop(host);
    let mut store = vault.store();
    assert!(
        store
            .begin_request()
            .stored_findings(&norn_store::DocumentPath::new("first.md").unwrap())
            .unwrap()
            .is_empty()
    );
    store.verify_integrity().unwrap();
}
