//! Frozen-copy prefix admission uses ordinary Find and withholds false final pages.
#![cfg(feature = "induced-failure")]
#![allow(clippy::disallowed_methods)]
mod attach;
use norn_host::startup_experiment::{Experiment, FrozenInput, Observer, PrefixReader, Stage};
use norn_store::{ContentModel, SnapshotReader, startup_experiment::Catalogue};
use norn_wire::{FindParams, Predicate, VaultAddress};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

struct Probe {
    address: VaultAddress,
    result: Mutex<Option<norn_host::startup_experiment::PrefixAttempt>>,
}
impl Observer for Probe {
    fn catalogue(&self, _: &Catalogue, _: &ContentModel) -> Result<(), String> {
        Ok(())
    }
    fn indexed(
        &self,
        _: Stage,
        _: Arc<SnapshotReader>,
        _: Arc<ContentModel>,
    ) -> Result<(), String> {
        Ok(())
    }
    fn prefix(&self, reader: &PrefixReader) -> Result<(), String> {
        *self.result.lock().unwrap() = Some(reader.find(&query(&self.address)));
        Ok(())
    }
}
fn query(address: &VaultAddress) -> FindParams {
    FindParams::new(address.clone())
        .with_limit(25)
        .with_predicates([Predicate::equal_to("perf_bucket", "active")])
}
#[test]
fn capped_find_requires_lookahead_or_real_eof() {
    for matches in [0, 1, 24, 25, 26] {
        for eof in [false, true] {
            let scratch = norn_testkit::scratch::Scratch::new("capped-boundary");
            let root = scratch.join("vault");
            std::fs::create_dir_all(root.join(".norn")).unwrap();
            std::fs::write(
                root.join(".norn/schema.yaml"),
                "version: 1\nfields:\n  perf_bucket: { type: text }\n",
            )
            .unwrap();
            // Keep the Get target after all prefix candidates; its non-match does not affect the page.
            std::fs::write(root.join("zz-small.md"), "# Small\n").unwrap();
            for i in 0..if eof { matches } else { 140 } {
                let value = if i < matches { "active" } else { "inactive" };
                std::fs::write(
                    root.join(format!("a-{i:03}.md")),
                    format!("---\nperf_bucket: {value}\n---\nBody\n"),
                )
                .unwrap();
            }
            let vault = attach::Vault::adopt(scratch.root());
            let address = VaultAddress::name(vault.name().clone());
            let probe = Arc::new(Probe {
                address: address.clone(),
                result: Mutex::new(None),
            });
            let host = vault.host_with_startup_experiment(
                norn_host::ProductionPolicy::new(32, 32).unwrap(),
                Experiment::capped_prefix_frozen(
                    "zz-small",
                    probe.clone(),
                    FrozenInput::assume_immutable(),
                ),
            );
            let demand = host.demand(vault.name()).unwrap();
            let deadline = Instant::now() + Duration::from_secs(30);
            while host.state(vault.name()).unwrap() != norn_wire::TrustState::Ready {
                assert!(Instant::now() < deadline);
                std::thread::sleep(Duration::from_millis(1));
            }
            let attempt = probe
                .result
                .lock()
                .unwrap()
                .take()
                .expect("prefix attempted");
            assert_eq!(attempt.certificate.eof, eof);
            assert!(attempt.certificate.consumed <= 128);
            if eof || matches >= 26 {
                let found = attempt.answer.expect("certified page");
                let ordinary = host.find(&query(&address)).unwrap().answer;
                let (_, _, report) = found.into_report();
                let mut early = serde_json::to_value(report).unwrap();
                let mut ordinary = serde_json::to_value(ordinary.report).unwrap();
                assert_eq!(
                    early["page"]["next"].is_null(),
                    ordinary["page"]["next"].is_null()
                );
                early["page"]["next"] = serde_json::Value::Null;
                ordinary["page"]["next"] = serde_json::Value::Null;
                assert_eq!(early, ordinary);
            } else {
                assert!(
                    attempt.answer.is_none(),
                    "a sparse prefix cannot publish a terminal answer"
                );
                assert!(attempt.decline.is_some());
            }
            drop(demand);
            drop(host);
        }
    }
}

struct Gates {
    queries: Vec<FindParams>,
    attempts: Mutex<Vec<norn_host::startup_experiment::PrefixAttempt>>,
    root: std::path::PathBuf,
}
impl Observer for Gates {
    fn catalogue(&self, _: &Catalogue, _: &ContentModel) -> Result<(), String> {
        Ok(())
    }
    fn indexed(
        &self,
        _: Stage,
        _: Arc<SnapshotReader>,
        _: Arc<ContentModel>,
    ) -> Result<(), String> {
        Ok(())
    }
    fn prefix(&self, reader: &PrefixReader) -> Result<(), String> {
        let mut attempts: Vec<_> = self.queries.iter().map(|q| reader.find(q)).collect();
        if let Some(cursor) = attempts[0].answer.as_ref().and_then(|a| a.next.clone()) {
            let continuation = reader.find(&self.queries[0].clone().with_after(cursor));
            assert!(
                continuation.answer.is_none(),
                "an insufficient prefix cannot publish page two"
            );
            assert!(continuation.decline.is_some());
            attempts.push(continuation);
        }
        self.attempts.lock().unwrap().extend(attempts);
        Ok(())
    }
    fn prefix_retired(&self) -> Result<(), String> {
        no_prefix_files(&self.root);
        Ok(())
    }
}
fn no_prefix_files(root: &std::path::Path) {
    for entry in std::fs::read_dir(root).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            no_prefix_files(&path);
        } else {
            assert!(
                !path
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with("intake-prefix-"),
                "prefix retained at pre-heal seam: {}",
                path.display()
            );
        }
    }
}

#[test]
fn capped_gates_global_diagnostics_and_preserves_canonical_cursor_handoff() {
    let scratch = norn_testkit::scratch::Scratch::new("capped-gates");
    let root = scratch.join("vault");
    std::fs::create_dir_all(root.join(".norn")).unwrap();
    std::fs::write(
        root.join(".norn/schema.yaml"),
        "version: 1\nfields:\n  due: { type: date }\n",
    )
    .unwrap();
    std::fs::write(root.join("small.md"), "# Small\n").unwrap();
    for i in 0..150 {
        std::fs::write(
            root.join(format!("a-{i:03}.md")),
            format!(
                "---\nperf_bucket: active\ndue: '2026-10-24T12:00:00Z'\n{}---\nBody\n",
                if i == 149 { "late_key: value\n" } else { "" }
            ),
        )
        .unwrap();
    }
    let vault = attach::Vault::adopt(scratch.root());
    let address = VaultAddress::name(vault.name().clone());
    let base = query(&address);
    let mut queries = vec![base.clone()];
    queries.push(
        base.clone()
            .with_columns([norn_wire::Column::field("late_key")]),
    );
    queries.push(
        base.clone()
            .with_predicates([Predicate::missing("late_key")]),
    );
    queries.push(
        base.clone()
            .with_predicates([Predicate::equal_to("perf_buket", "active")]),
    );
    queries.push(
        base.clone()
            .with_predicates([Predicate::equal_to("due", "2026-10-24T12:00:00Z")]),
    );
    queries.push(base.clone().with_predicates([Predicate::path("folder")]));
    queries.push(base.clone().with_sort(norn_wire::Sort::new(
        norn_wire::SortKey::field("perf_bucket"),
        norn_wire::Direction::Ascending,
    )));
    queries.push(base.clone().with_predicates([Predicate::matches("Body")]));
    queries.push(base.clone().with_limit(24));
    let observer = Arc::new(Gates {
        queries,
        attempts: Mutex::new(Vec::new()),
        root: scratch.root().to_path_buf(),
    });
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
    while host.state(vault.name()).unwrap() != norn_wire::TrustState::Ready {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    }
    let mut attempts = observer.attempts.lock().unwrap();
    let first = attempts[0].answer.take().unwrap();
    assert_eq!(
        first.snapshot.epoch,
        attempts[0].reading.as_ref().unwrap().epoch()
    );
    assert!(attempts[0].query_executed);
    for attempt in &attempts[1..] {
        assert!(attempt.answer.is_none());
        assert!(attempt.decline.is_some());
        assert!(!attempt.query_executed);
    }
    let cursor = first.next.clone().unwrap();
    assert_eq!(cursor.snapshot().epoch, first.snapshot.epoch);
    assert_eq!(cursor.snapshot().generation, first.snapshot.generation);
    // The unchanged prefix cursor carries its actual projection epoch into Host.
    let mut paths = first
        .rows
        .iter()
        .map(|row| row.path.clone())
        .collect::<Vec<_>>();
    let handoff = host
        .find(&base.clone().with_after(cursor))
        .unwrap()
        .answer
        .report
        .page;
    assert_eq!(handoff.moved, [norn_wire::Moved::Epoch]);
    paths.extend(handoff.rows.iter().map(|row| row.path.clone()));
    let mut next = handoff.next;
    while let Some(cursor) = next {
        let page = host
            .find(&base.clone().with_after(cursor))
            .unwrap()
            .answer
            .report
            .page;
        assert!(page.moved.is_empty());
        paths.extend(page.rows.iter().map(|row| row.path.clone()));
        next = page.next;
    }
    assert_eq!(paths.len(), 150);
    let unique: std::collections::BTreeSet<_> = paths.iter().collect();
    assert_eq!(unique.len(), 150);
    assert_eq!(paths.first().unwrap().as_str(), "a-000.md");
    assert_eq!(paths.last().unwrap().as_str(), "a-149.md");
    drop(demand);
    drop(host);
}

#[allow(dead_code)]
#[path = "performance/staged.rs"]
mod staged;
static REPORTED: Mutex<Vec<serde_json::Value>> = Mutex::new(Vec::new());
fn emit(value: serde_json::Value) {
    REPORTED.lock().unwrap().push(value);
}

#[test]
fn capped_operator_protocol_publishes_success_or_decline_and_retires_before_heal() {
    for dense in [true, false] {
        REPORTED.lock().unwrap().clear();
        let scratch = norn_testkit::scratch::Scratch::new("capped-protocol");
        let root = scratch.join("vault");
        std::fs::create_dir_all(root.join(".norn")).unwrap();
        std::fs::create_dir(scratch.join("operator-prefix-acks")).unwrap();
        std::fs::write(root.join(".norn/schema.yaml"), "version: 1\n").unwrap();
        std::fs::write(root.join("small.md"), "# Small\nBody\n").unwrap();
        for i in 0..150 {
            let value = if dense || i >= 128 {
                "active"
            } else {
                "inactive"
            };
            std::fs::write(
                root.join(format!("a-{i:03}.md")),
                format!("---\nperf_bucket: {value}\n---\nBody\n"),
            )
            .unwrap();
        }
        let vault = attach::Vault::adopt(scratch.root());
        let observer = Arc::new(staged::CappedPrefix::new(
            Instant::now(),
            vault.name().clone(),
            scratch.root().to_path_buf(),
        ));
        let host = vault.host_with_startup_experiment(
            norn_host::ProductionPolicy::new(32, 32).unwrap(),
            Experiment::capped_prefix_frozen(
                "small",
                observer.clone(),
                FrozenInput::assume_immutable(),
            ),
        );
        let demand = host.demand(vault.name()).unwrap();
        let address = VaultAddress::name(vault.name().clone());
        let deadline = Instant::now() + Duration::from_secs(30);
        let mut acked = 0;
        while host.state(vault.name()).unwrap() != norn_wire::TrustState::Ready {
            let events = REPORTED.lock().unwrap().clone();
            for row in events.iter().skip(acked) {
                assert!(
                    host.find(&query(&address)).is_err(),
                    "initial public reads must still refuse"
                );
                assert_eq!(row["operator_prefix_sequence"], acked + 1);
                let ack_path = scratch.join(format!("operator-prefix-acks/{}.json", acked + 1));
                let pending = ack_path.with_extension("pending");
                std::fs::write(
                    &pending,
                    serde_json::json!({"sequence":acked+1,"probe":row["probe"]}).to_string(),
                )
                .unwrap();
                std::fs::rename(pending, ack_path).unwrap();
                acked += 1;
            }
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(acked, 2);
        let events = REPORTED.lock().unwrap().clone();
        assert_eq!(events.len(), 2, "no final observer query");
        assert_eq!(events[1]["early_published"], dense);
        assert_eq!(events[1]["query_executed"], true);
        assert_eq!(events[1]["answer"].is_object(), dense);
        assert_eq!(observer.receipt()["fallback_required"], !dense);
        assert_eq!(observer.receipt()["queries_executed"], 2);
        // The fallback is this same request through Host after Ready, with its full response.
        let ordinary = host.find(&query(&address)).unwrap().answer;
        assert_eq!(ordinary.report.page.rows.len(), if dense { 25 } else { 22 });
        if dense {
            assert_eq!(
                serde_json::to_value(ordinary.report.page.rows).unwrap(),
                events[1]["answer"]["report"]["page"]["rows"]
            );
        }
        no_prefix_files(scratch.root());
        drop(demand);
        drop(host);
    }
}

#[test]
fn capped_eof_uses_ordinary_parser_quarantine_and_answer_order() {
    let scratch = norn_testkit::scratch::Scratch::new("capped-content");
    let root = scratch.join("vault");
    std::fs::create_dir_all(root.join(".norn")).unwrap();
    std::fs::write(
        root.join(".norn/schema.yaml"),
        "version: 1\nfields:\n  perf_bucket: { type: text }\n",
    )
    .unwrap();
    std::fs::write(root.join("small.md"), "# Small\n").unwrap();
    let distinct_case_names = norn_fs::Vault::open(&root, &[]).unwrap().case_sensitivity()
        == norn_fs::CaseSensitivity::Sensitive;
    for (name, bytes) in [
        ("B.md", "---\nperf_bucket: active\n---\nBody\n".to_string()),
        (
            "C.md",
            "---\nperf_bucket: [active, other]\n---\nBody\n".into(),
        ),
        ("a.md", "---\nperf_bucket: active\n---\nBody\n".into()),
        ("A.md", "---\nperf_bucket: active\n---\nBody\n".into()),
        ("é.md", "---\nperf_bucket: active\n---\nBody\n".into()),
        (
            "malformed.md",
            "---\nperf_bucket: [active\n---\nBody\n".into(),
        ),
        ("unclosed.md", "---\nperf_bucket: active\nBody\n".into()),
        (
            "oversized.md",
            format!(
                "---\nperf_bucket: active\nother: {}\n---\nBody\n",
                "x".repeat(20_000)
            ),
        ),
        ("null.md", "---\nperf_bucket: null\n---\nBody\n".into()),
        (
            "map.md",
            "---\nperf_bucket: {nested: active}\n---\nBody\n".into(),
        ),
        ("empty-list.md", "---\nperf_bucket: []\n---\nBody\n".into()),
        ("wrong-shape.md", "---\nperf_bucket: 17\n---\nBody\n".into()),
        (
            "aliases.md",
            "---\nbase: &base active\nperf_bucket: *base\n---\nBody\n".into(),
        ),
    ] {
        if name != "A.md" || distinct_case_names {
            std::fs::write(root.join(name), bytes).unwrap();
        }
    }
    std::fs::write(root.join("undecodable.md"), [0xff, 0xfe]).unwrap();
    let vault = attach::Vault::adopt(scratch.root());
    let address = VaultAddress::name(vault.name().clone());
    let probe = Arc::new(Probe {
        address: address.clone(),
        result: Mutex::new(None),
    });
    let host = vault.host_with_startup_experiment(
        norn_host::ProductionPolicy::new(32, 32).unwrap(),
        Experiment::capped_prefix_frozen("small", probe.clone(), FrozenInput::assume_immutable()),
    );
    let demand = host.demand(vault.name()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    while host.state(vault.name()).unwrap() != norn_wire::TrustState::Ready {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    }
    let attempt = probe.result.lock().unwrap().take().unwrap();
    assert!(attempt.certificate.eof);
    assert_eq!(
        attempt.certificate.consumed,
        if distinct_case_names { 15 } else { 14 }
    );
    let answer = attempt.answer.expect("EOF answer");
    let content_hashes = answer.content_hashes.clone();
    let (_, _, early) = answer.into_report();
    let ordinary = host.find(&query(&address)).unwrap().answer;
    assert_eq!(early, ordinary.report);
    assert_eq!(
        early
            .page
            .rows
            .iter()
            .map(|r| r.path.as_str())
            .collect::<Vec<_>>(),
        if distinct_case_names {
            vec!["A.md", "a.md", "aliases.md", "B.md", "C.md", "é.md"]
        } else {
            vec!["a.md", "aliases.md", "B.md", "C.md", "é.md"]
        }
    );
    drop(demand);
    drop(host);
    let mut store = vault.store();
    for (row, hash) in early.page.rows.iter().zip(content_hashes) {
        let path = norn_store::DocumentPath::new(row.path.as_str()).unwrap();
        assert_eq!(
            store
                .begin_request()
                .stored_document(&path)
                .unwrap()
                .unwrap()
                .content_hash,
            hash
        );
    }
}

struct BrokenFrozen {
    probe: Probe,
    frozen: Arc<FrozenInput>,
    root: std::path::PathBuf,
    mutation: &'static str,
}
impl Observer for BrokenFrozen {
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
            // The fixture owner withdraws its assumption. This is not a live dirty detector.
            self.frozen.invalidate();
            match self.mutation {
                "edit" => std::fs::write(
                    self.root.join("a.md"),
                    "---\nperf_bucket: active\n---\nChanged\n",
                )
                .unwrap(),
                "delete" => std::fs::remove_file(self.root.join("a.md")).unwrap(),
                "rename" => {
                    std::fs::rename(self.root.join("a.md"), self.root.join("earlier.md")).unwrap()
                }
                "new-earlier" => std::fs::write(
                    self.root.join("A.md"),
                    "---\nperf_bucket: active\n---\nNew\n",
                )
                .unwrap(),
                "schema" => std::fs::write(
                    self.root.join(".norn/schema.yaml"),
                    "version: 1\nfields:\n  perf_bucket: {type: text}\n",
                )
                .unwrap(),
                _ => unreachable!(),
            }
        }
        Ok(())
    }
    fn prefix(&self, reader: &PrefixReader) -> Result<(), String> {
        self.probe.prefix(reader)
    }
}
#[test]
fn mutation_probes_explicitly_break_frozen_input_certificate() {
    for mutation in ["edit", "delete", "rename", "new-earlier", "schema"] {
        let scratch = norn_testkit::scratch::Scratch::new("capped-broken-frozen");
        let root = scratch.join("vault");
        std::fs::create_dir_all(root.join(".norn")).unwrap();
        std::fs::write(root.join(".norn/schema.yaml"), "version: 1\n").unwrap();
        std::fs::write(root.join("small.md"), "Small\n").unwrap();
        std::fs::write(root.join("a.md"), "---\nperf_bucket: active\n---\nBody\n").unwrap();
        let vault = attach::Vault::adopt(scratch.root());
        let address = VaultAddress::name(vault.name().clone());
        let frozen = FrozenInput::assume_immutable();
        let observer = Arc::new(BrokenFrozen {
            probe: Probe {
                address: address.clone(),
                result: Mutex::new(None),
            },
            frozen: frozen.clone(),
            root,
            mutation,
        });
        let host = vault.host_with_startup_experiment(
            norn_host::ProductionPolicy::new(32, 32).unwrap(),
            Experiment::capped_prefix_frozen("small", observer.clone(), frozen),
        );
        let demand = host.demand(vault.name()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(30);
        while host.state(vault.name()).unwrap() != norn_wire::TrustState::Ready {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(1));
        }
        let attempt = observer.probe.result.lock().unwrap().take().unwrap();
        assert!(attempt.answer.is_none());
        assert!(!attempt.query_executed);
        assert_eq!(
            attempt.decline.as_deref(),
            Some("frozen-input certificate broken")
        );
        assert!(
            host.find(&query(&address)).is_ok(),
            "same query available at ordinary Ready"
        );
        drop(demand);
        drop(host);
    }
}

struct RejectPrefix;
impl Observer for RejectPrefix {
    fn catalogue(&self, _: &Catalogue, _: &ContentModel) -> Result<(), String> {
        Ok(())
    }
    fn indexed(
        &self,
        _: Stage,
        _: Arc<SnapshotReader>,
        _: Arc<ContentModel>,
    ) -> Result<(), String> {
        Ok(())
    }
    fn prefix(&self, _: &PrefixReader) -> Result<(), String> {
        Err("observer rejected prefix".into())
    }
}
#[test]
fn rejected_prefix_abandons_temporary_files_and_never_publishes_ready() {
    let scratch = norn_testkit::scratch::Scratch::new("capped-rejected");
    let root = scratch.join("vault");
    std::fs::create_dir_all(root.join(".norn")).unwrap();
    std::fs::write(root.join(".norn/schema.yaml"), "version: 1\n").unwrap();
    std::fs::write(
        root.join("small.md"),
        "---\nperf_bucket: active\n---\nSmall\n",
    )
    .unwrap();
    let vault = attach::Vault::adopt(scratch.root());
    let host = vault.host_with_startup_experiment(
        norn_host::ProductionPolicy::new(32, 32).unwrap(),
        Experiment::capped_prefix_frozen(
            "small",
            Arc::new(RejectPrefix),
            FrozenInput::assume_immutable(),
        ),
    );
    let demand = host.demand(vault.name()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    while let Ok(state) = host.state(vault.name()) {
        assert_ne!(state, norn_wire::TrustState::Ready);
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    }
    drop(demand);
    drop(host);
    no_prefix_files(scratch.root());
}

#[test]
fn late_unknown_and_dated_perf_keys_decline_without_fake_empty_answer() {
    for dated in [false, true] {
        let scratch = norn_testkit::scratch::Scratch::new("capped-unsettled-key");
        let root = scratch.join("vault");
        std::fs::create_dir_all(root.join(".norn")).unwrap();
        std::fs::write(
            root.join(".norn/schema.yaml"),
            if dated {
                "version: 1\nfields:\n  perf_bucket: {type: date}\n"
            } else {
                "version: 1\n"
            },
        )
        .unwrap();
        std::fs::write(root.join("small.md"), "Small\n").unwrap();
        for i in 0..150 {
            let fields = if dated {
                if i < 128 {
                    "perf_bucket: '2026-10-24T12:00:00Z'"
                } else {
                    "perf_bucket: '2026-10-24T12:00:00'"
                }
            } else if i < 128 {
                "other: active"
            } else {
                "perf_bucket: active"
            };
            std::fs::write(
                root.join(format!("a-{i:03}.md")),
                format!("---\n{fields}\n---\nBody\n"),
            )
            .unwrap();
        }
        let vault = attach::Vault::adopt(scratch.root());
        let address = VaultAddress::name(vault.name().clone());
        let probe = Arc::new(Probe {
            address: address.clone(),
            result: Mutex::new(None),
        });
        let host = vault.host_with_startup_experiment(
            norn_host::ProductionPolicy::new(32, 32).unwrap(),
            Experiment::capped_prefix_frozen(
                "small",
                probe.clone(),
                FrozenInput::assume_immutable(),
            ),
        );
        let demand = host.demand(vault.name()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(30);
        while host.state(vault.name()).unwrap() != norn_wire::TrustState::Ready {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(1));
        }
        let attempt = probe.result.lock().unwrap().take().unwrap();
        assert!(attempt.answer.is_none());
        assert!(
            attempt
                .decline
                .unwrap()
                .contains(if dated { "dated key" } else { "unknown key" })
        );
        assert_eq!(attempt.query_executed, !dated);
        let fallback = host.find(&query(&address));
        if dated {
            assert_eq!(
                fallback.unwrap_err().code(),
                &norn_wire::ReasonCode::VaultUnreadableBound
            );
        } else {
            assert_eq!(fallback.unwrap().answer.report.page.rows.len(), 22);
        }
        drop(demand);
        drop(host);
    }
}
