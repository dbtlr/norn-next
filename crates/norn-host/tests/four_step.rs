//! Semantic and work gates for the opt-in canonical loading experiment.
#![cfg(all(unix, feature = "induced-failure"))]
#![allow(clippy::disallowed_methods, clippy::disallowed_macros)] // Isolated lab fixtures and evidence.

mod attach;

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use norn_host::{FourStepEvent, FourStepLoading, FourStepObserver, FourStepRead, FourStepReport};
use norn_wire::{GetParams, ResolutionTarget, TrustState, VaultAddress};

#[derive(Default)]
struct Probe {
    project_fields: bool,
    structured: Mutex<Vec<String>>,
    report: Mutex<Option<FourStepReport>>,
    early: Mutex<Option<serde_json::Value>>,
    find: Mutex<Option<serde_json::Value>>,
    search: Mutex<Option<serde_json::Value>>,
}

impl FourStepObserver for Probe {
    fn at(&self, event: FourStepEvent<'_>, read: &FourStepRead<'_>) -> Result<(), String> {
        match event {
            FourStepEvent::Structured(path) => {
                self.structured.lock().unwrap().push(path.into());
                if path == "z-requested.md" && !read.metadata_complete() {
                    let params = GetParams::new(
                        VaultAddress::name(norn_wire::VaultName::new("notes").unwrap()),
                        ResolutionTarget::new("z-requested").unwrap(),
                    );
                    assert!(
                        read.get(&params).is_ok(),
                        "own step 3 admits Get while other documents still owe content"
                    );
                }
            }
            FourStepEvent::GetReady(path) => {
                if path == "z-requested.md" {
                    let params = GetParams::new(
                        VaultAddress::name(norn_wire::VaultName::new("notes").unwrap()),
                        ResolutionTarget::new("z-requested").unwrap(),
                    );
                    if !read.metadata_complete() {
                        assert!(
                            read.get(
                                &params
                                    .clone()
                                    .with_columns([norn_wire::Column::field("missing")])
                            )
                            .unwrap_err()
                            .contains("complete field universe")
                        );
                    }
                    if self.project_fields {
                        let projected = read.get(&params.clone().with_columns([
                            norn_wire::Column::field("rating"),
                            norn_wire::Column::field("related"),
                        ]))?;
                        assert!(projected.unsatisfied.is_empty());
                        let value = serde_json::to_value(projected.report).unwrap();
                        assert_eq!(
                            value["document"]["fields"]["rating"],
                            serde_json::json!({"kind":"scalar", "raw":"7"})
                        );
                    }
                    assert!(read.link_resolution_loading());
                    assert!(read.link_health_loading());
                    let got = read.get(&params)?;
                    assert!(got.unsatisfied.is_empty());
                    *self.early.lock().unwrap() = Some(serde_json::to_value(got.report).unwrap());
                }
            }
            FourStepEvent::ContentComplete => {
                let params = norn_wire::FindParams::new(VaultAddress::name(
                    norn_wire::VaultName::new("notes").unwrap(),
                ))
                .with_columns([
                    norn_wire::Column::path(),
                    norn_wire::Column::field("rating"),
                ])
                .with_predicates([norn_wire::Predicate::equal_to("rating", "7")]);
                let (_, _, report) = read.find(&params)?.into_report();
                *self.find.lock().unwrap() = Some(serde_json::to_value(report).unwrap());
            }
            FourStepEvent::Complete(report) => *self.report.lock().unwrap() = Some(report.clone()),
            _ => {}
        }
        let query =
            norn_store::LexicalQuery::new("Requested").with_columns([norn_wire::Column::path()]);
        if matches!(
            event,
            FourStepEvent::ContentComplete
                | FourStepEvent::StructureComplete
                | FourStepEvent::FullTextComplete
        ) {
            if read.search_complete() {
                let (unsatisfied, advisories, report) = read.search(&query)?.into_report();
                assert!(unsatisfied.is_empty());
                assert!(advisories.is_empty());
                self.search
                    .lock()
                    .unwrap()
                    .get_or_insert_with(|| serde_json::to_value(report).unwrap());
                assert!(
                    read.search(
                        &norn_store::LexicalQuery::new("Requested")
                            .with_columns([norn_wire::Column::tags()])
                    )
                    .is_err(),
                    "the early search hook refuses incomplete structure projections"
                );
            } else {
                assert!(
                    read.search(&query).is_err(),
                    "no-body search waits for stored bodies"
                );
            }
        }
        Ok(())
    }
}

fn fixture(label: &str, undecodable: bool) -> norn_testkit::scratch::Scratch {
    let scratch = norn_testkit::scratch::Scratch::under(
        &std::env::current_dir().unwrap().join(".scratch"),
        label,
    )
    .unwrap();
    let root = scratch.join("vault");
    for dir in [".norn", "one", "two", "deep/nested", "media"] {
        std::fs::create_dir_all(root.join(dir)).unwrap();
    }
    std::fs::write(root.join(".norn/schema.yaml"),
        "version: 1\nfields:\n  rating: { type: number }\ntags:\n  declared: [alpha]\n  undeclared: report\n").unwrap();
    for (path, text) in [
        (
            "b.md",
            "---\nrating: 2\nrelated: '[[hub#Present]]'\ntags: [alpha, beta]\n---\n[[hub]] [[hub#Missing]] [[hub#^block]] [[hub#^absent]] [[dupe]] [[missing]] [[/deep/nested/target]] [file](media/picture.png)\n#inline\n",
        ),
        (
            "odd.md",
            "\u{feff}---\r\nrating: 3\r\n---\r\n# Unicode é\r\nText. ^odd\r\n#beta\r\n",
        ),
        ("malformed.md", "---\nbad: [\n---\n# Body\n#beta\n"),
        ("hub.md", "# Present\nBlock. ^block\n"),
        ("bare-bom.md", "\u{feff}[[hub]] #beta\n"),
        ("cr-only.md", "---\rrating: 4\r---\r# CR\r[[hub]]\r"),
        ("crlf.md", "# CRLF\r\n[[hub]]\r\n"),
        ("one/dupe.md", "[[hub]]\n"),
        ("two/dupe.md", "[[hub]]\n"),
        ("deep/nested/target.md", "[[hub]] [[../../hub]]\n"),
        (
            "z-requested.md",
            "---\nrating: 7\ntags: [beta]\nrelated: '[[hub]]'\n---\n# Requested\n[[hub#Present]] [[missing]] [[a]]\n#early\n",
        ),
        ("media/picture.png", "name-only"),
    ] {
        std::fs::write(root.join(path), text).unwrap();
    }
    // More refused names than one lab chunk exercises the persisted queue;
    // a decodable row at a rendered spelling still withholds its refusal.
    for index in 0..5 {
        std::fs::write(
            root.join(format!("refused\\{index}.md")),
            "Refused spelling\n",
        )
        .unwrap();
    }
    std::fs::write(root.join("rendered\\.md"), "Refused spelling\n").unwrap();
    std::fs::write(root.join("rendered�.md"), "Decoded spelling\n").unwrap();
    std::os::unix::fs::symlink("hub.md", root.join("link.md")).unwrap();
    if undecodable {
        std::fs::write(root.join("a.md"), [0xff, 0xfe]).unwrap();
        // Both spellings render onto x�.md. A provisional name must not
        // withhold the refused spelling after its own bytes quarantine it.
        std::fs::write(root.join("x\\.md"), "Refused spelling\n").unwrap();
        std::fs::write(root.join("x�.md"), [0xff, 0xfe]).unwrap();
    }
    scratch
}

fn wait_ready(host: &attach::ServingHost, vault: &attach::Vault) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while host.state(vault.name()).unwrap() != TrustState::Ready
        || !host.work_in_flight(vault.name()).unwrap().is_quiet()
    {
        assert!(
            Instant::now() < deadline,
            "four-step startup did not converge"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
}

fn ids(path: &std::path::Path) -> BTreeMap<String, i64> {
    let database = norn_db::open_read_only(path).adopted.unwrap();
    database
        .connection()
        .prepare("SELECT path,id FROM documents ORDER BY path")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

fn without_link_findings(mut report: serde_json::Value) -> serde_json::Value {
    let findings = &mut report["document"]["findings"];
    let rows = findings["items"].as_array_mut().unwrap();
    rows.retain(|finding| !finding["kind"].as_str().unwrap().starts_with("link/"));
    findings["total"] = serde_json::json!(rows.len());
    report
}

fn without_loading_link_resolution(mut report: serde_json::Value) -> serde_json::Value {
    for link in report["document"]["links"]["items"].as_array_mut().unwrap() {
        link.as_object_mut().unwrap().remove("health");
        link.as_object_mut().unwrap().remove("targets");
    }
    report
}

fn normalize_finding_provenance(mut report: serde_json::Value) -> serde_json::Value {
    for finding in report["document"]["findings"]["items"]
        .as_array_mut()
        .unwrap()
    {
        finding["id"] = serde_json::json!(0);
        finding["generation"] = serde_json::json!(0);
    }
    report
}

#[test]
fn four_step_matches_ordinary_semantics_and_judges_each_link_once() {
    for undecodable in [false, true] {
        let scratch = fixture("four-step-equivalence", undecodable);
        let vault = attach::Vault::adopt(scratch.root());
        let ordinary = vault.beside(&scratch.join("ordinary"));
        let host = ordinary.host_with_policy(norn_host::ProductionPolicy::new(2, 2).unwrap());
        let demand = host.demand(ordinary.name()).unwrap();
        wait_ready(&host, &ordinary);
        let params = GetParams::new(
            VaultAddress::name(ordinary.name().clone()),
            ResolutionTarget::new("/z-requested.md").unwrap(),
        );
        let ordinary_answer = host.get(&params).unwrap().answer;
        assert!(ordinary_answer.unsatisfied.is_empty());
        let ordinary_get = serde_json::to_value(ordinary_answer.report).unwrap();
        let search_params =
            norn_wire::SearchParams::new(VaultAddress::name(ordinary.name().clone()), "Requested")
                .with_rungs(norn_wire::RungSelection::exactly(
                    norn_wire::RungSet::of([norn_wire::Rung::Lexical]).unwrap(),
                ))
                .with_columns([norn_wire::Column::path()]);
        let ordinary_search =
            serde_json::to_value(host.search(&search_params).unwrap().answer.report).unwrap();
        let find_params = norn_wire::FindParams::new(VaultAddress::name(ordinary.name().clone()))
            .with_columns([
                norn_wire::Column::path(),
                norn_wire::Column::field("rating"),
            ])
            .with_predicates([norn_wire::Predicate::equal_to("rating", "7")]);
        let ordinary_find =
            serde_json::to_value(host.find(&find_params).unwrap().answer.report).unwrap();
        let ordinary_advisories =
            format!("{:?}", host.inspect(ordinary.name()).unwrap().advisories);
        assert!(ordinary_advisories.contains("SymlinkSkipped"));
        let ordinary_work = host.evidence();
        drop(demand);
        drop(host);
        let expected =
            norn_store::startup_audit::audit(&ordinary.database(), |_, _, _| Ok(())).unwrap();
        for (no_body, fts_mode) in [
            (false, norn_store::four_step::FtsBuildMode::Rebuild),
            (false, norn_store::four_step::FtsBuildMode::Batched),
            (true, norn_store::four_step::FtsBuildMode::Rebuild),
            (true, norn_store::four_step::FtsBuildMode::Batched),
        ] {
            for pull_ahead in [false, true] {
                let machine =
                    vault.beside(&scratch.join(format!("lab-{no_body}-{pull_ahead}-{fts_mode:?}")));
                let probe = Arc::new(Probe {
                    project_fields: true,
                    ..Probe::default()
                });
                let loading = FourStepLoading::new(
                    2,
                    no_body,
                    if pull_ahead {
                        vec!["z-requested.md".into()]
                    } else {
                        vec![]
                    },
                    probe.clone(),
                )
                .unwrap()
                .with_fts_mode(fts_mode);
                let host = machine.host_with_four_step(loading);
                let demand = host.demand(machine.name()).unwrap();
                wait_ready(&host, &machine);
                assert_eq!(
                    format!("{:?}", host.inspect(machine.name()).unwrap().advisories),
                    ordinary_advisories
                );
                let work = host.evidence();
                let final_answer = host.get(&params).unwrap().answer;
                assert!(final_answer.unsatisfied.is_empty());
                let final_get = serde_json::to_value(final_answer.report).unwrap();
                drop(demand);
                drop(host);
                let actual =
                    norn_store::startup_audit::audit(&machine.database(), |_, _, _| Ok(()))
                        .unwrap();
                for (label, audit) in [("ordinary", &expected), ("four-step", &actual)] {
                    println!(
                        "{label} no_body={no_body} pull_ahead={pull_ahead} undecodable={undecodable}: {}",
                        serde_json::json!({
                            "semantic_sha256": audit["semantic_sha256"],
                            "canonical_identity_sha256": audit["canonical_identity_sha256"],
                            "page_map_sha256": audit["page_map_sha256"],
                            "document_construction_sha256": audit["document_construction_sha256"],
                        })
                    );
                }
                assert_eq!(actual["semantic_sha256"], expected["semantic_sha256"]);
                if !undecodable {
                    assert_eq!(ids(&machine.database()), ids(&ordinary.database()));
                }
                // Names precede decoding: a.md's provisional id leaves an accepted gap.
                let report = probe
                    .report
                    .lock()
                    .unwrap()
                    .clone()
                    .expect("completion counters");
                assert_eq!(report.steps["relations"]["links_judged"], 20);
                assert_eq!(
                    report.steps["content"]["rows_deleted"],
                    if undecodable { 2 } else { 0 }
                );
                assert_eq!(
                    report
                        .steps
                        .values()
                        .map(|s| s["findings_discarded"])
                        .sum::<u64>(),
                    0
                );
                assert_eq!(work.links_redecided, 20);
                // Final health reports its resolutions in the step and the job.
                let relations = &report.steps["relations"];
                assert!(relations["link_health_keys_resolved"] > 0);
                assert!(relations["link_health_candidates_read"] > 0);
                assert_eq!(
                    work.link_health_keys_resolved,
                    relations["link_health_keys_resolved"]
                );
                assert_eq!(
                    work.link_health_candidates_read,
                    relations["link_health_candidates_read"]
                );
                if pull_ahead {
                    assert!(
                        report.requested_structure_ns["z-requested.md"]
                            <= report.content_complete_ns
                    );
                    assert_eq!(
                        probe.structured.lock().unwrap().first().unwrap(),
                        "z-requested.md"
                    );
                }
                let early = probe.early.lock().unwrap().clone().unwrap();
                assert_eq!(
                    early,
                    without_link_findings(early.clone()),
                    "early link-health findings are absent: health is still loading"
                );
                assert!(
                    early["document"]["findings"]["items"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|finding| finding["kind"] == "document/undeclared-tag"),
                    "the early answer includes its non-link findings"
                );
                assert_eq!(
                    without_loading_link_resolution(early.clone()),
                    without_loading_link_resolution(without_link_findings(final_get)),
                    "same-store non-link finding ids and generations must remain unchanged"
                );
                assert_eq!(
                    normalize_finding_provenance(without_loading_link_resolution(early)),
                    normalize_finding_provenance(without_loading_link_resolution(
                        without_link_findings(ordinary_get.clone())
                    ))
                );
                assert_eq!(
                    probe.search.lock().unwrap().as_ref().unwrap(),
                    &ordinary_search
                );
                assert_eq!(
                    report.steps["structure"]["body_rows_rewritten"],
                    if no_body {
                        report.steps["content"]["rows_rewritten"]
                    } else {
                        0
                    }
                );
                let boundaries = ["names", "content", "structure", "relations", "full-text"]
                    .map(|step| report.boundary_ns[step]);
                assert!(boundaries.windows(2).all(|pair| pair[0] <= pair[1]));
                for step in ["names", "content", "structure", "relations", "full-text"] {
                    assert!(report.busy_ns[step] <= report.boundary_ns[step]);
                }
                assert_eq!(probe.find.lock().unwrap().as_ref().unwrap(), &ordinary_find);
                println!("ordinary={ordinary_work:?}; lab={work:?}; steps={report:?}");
                machine.store().verify_integrity().unwrap();
            }
        }
    }
}

struct Mutation {
    root: std::path::PathBuf,
    changed: std::sync::atomic::AtomicBool,
    stale_refused: std::sync::atomic::AtomicBool,
    report: Mutex<Option<FourStepReport>>,
}

impl FourStepObserver for Mutation {
    fn at(&self, event: FourStepEvent<'_>, read: &FourStepRead<'_>) -> Result<(), String> {
        if matches!(event, FourStepEvent::ContentComplete) {
            std::fs::write(
                self.root.join("z-requested.md"),
                "---\nrating: 9\n---\n# Changed\n[[hub#Missing]]\n",
            )
            .unwrap();
            std::fs::write(self.root.join("new.md"), "[[hub]]\n").unwrap();
            self.changed
                .store(true, std::sync::atomic::Ordering::SeqCst);
            let until = Instant::now() + Duration::from_secs(5);
            while read.is_current() {
                assert!(
                    Instant::now() < until,
                    "watcher did not withdraw lab authority"
                );
                std::thread::sleep(Duration::from_millis(1));
            }
        }
        if matches!(event, FourStepEvent::Structured(_))
            && self.changed.load(std::sync::atomic::Ordering::SeqCst)
        {
            let params = GetParams::new(
                VaultAddress::name(norn_wire::VaultName::new("notes").unwrap()),
                ResolutionTarget::new("/z-requested.md").unwrap(),
            );
            assert!(
                read.get(&params).is_err(),
                "stale lab answer was labelled current"
            );
            self.stale_refused
                .store(true, std::sync::atomic::Ordering::SeqCst);
        }
        if let FourStepEvent::Complete(report) = event {
            *self.report.lock().unwrap() = Some(report.clone());
        }
        Ok(())
    }
}

#[test]
fn source_changes_withdraw_early_reads_and_reconcile_the_final_tree() {
    for no_body in [false, true] {
        let scratch = fixture("four-step-change", false);
        let vault = attach::Vault::adopt(scratch.root());
        let lab = vault.beside(&scratch.join("lab"));
        let probe = Arc::new(Mutation {
            root: scratch.join("vault"),
            changed: false.into(),
            stale_refused: false.into(),
            report: Mutex::new(None),
        });
        let host = lab
            .host_with_four_step(FourStepLoading::new(2, no_body, vec![], probe.clone()).unwrap());
        let demand = host.demand(lab.name()).unwrap();
        wait_ready(&host, &lab);
        drop(demand);
        drop(host);
        assert!(
            probe
                .stale_refused
                .load(std::sync::atomic::Ordering::SeqCst)
        );
        assert!(probe.report.lock().unwrap().as_ref().unwrap().reconciled);
        assert_eq!(
            probe.report.lock().unwrap().as_ref().unwrap().steps["structure"]["rows_deleted"],
            u64::from(no_body)
        );
        let ordinary = vault.beside(&scratch.join("ordinary-final-tree"));
        let host = ordinary.host();
        let demand = host.demand(ordinary.name()).unwrap();
        wait_ready(&host, &ordinary);
        drop(demand);
        drop(host);
        let actual = norn_store::startup_audit::audit(&lab.database(), |_, _, _| Ok(())).unwrap();
        let expected =
            norn_store::startup_audit::audit(&ordinary.database(), |_, _, _| Ok(())).unwrap();
        assert_eq!(actual["semantic_sha256"], expected["semantic_sha256"]);
    }
}

struct Interrupted;
impl FourStepObserver for Interrupted {
    fn at(&self, event: FourStepEvent<'_>, _: &FourStepRead<'_>) -> Result<(), String> {
        if matches!(event, FourStepEvent::NamesComplete) {
            return Err("lab interruption after names".into());
        }
        Ok(())
    }
}

#[test]
fn incomplete_lab_store_is_rebuilt_instead_of_adopted() {
    let scratch = fixture("four-step-interrupted", false);
    let vault = attach::Vault::adopt(scratch.root());
    let host = vault.host_with_four_step(
        FourStepLoading::new(2, false, vec![], Arc::new(Interrupted)).unwrap(),
    );
    let demand = host.demand(vault.name()).unwrap();
    let until = Instant::now() + Duration::from_secs(30);
    loop {
        if let Err(error) = host.state(vault.name()) {
            assert!(format!("{error:?}").contains("lab interruption after names"));
            if host.work_in_flight(vault.name()).unwrap().is_quiet() {
                break;
            }
        }
        assert!(Instant::now() < until, "interrupted lab did not stop");
        std::thread::sleep(Duration::from_millis(1));
    }
    drop(demand);
    drop(host);
    let mut store = vault.store();
    assert!(matches!(
        store.open_outcome(),
        norn_store::OpenOutcome::RebuiltFromZero(_)
    ));
    assert!(
        store
            .begin_request()
            .stored_document(&norn_store::DocumentPath::new("b.md").unwrap())
            .unwrap()
            .is_none()
    );
}

struct DeleteAfterContent(std::path::PathBuf, Mutex<Option<FourStepReport>>);
impl FourStepObserver for DeleteAfterContent {
    fn at(&self, event: FourStepEvent<'_>, read: &FourStepRead<'_>) -> Result<(), String> {
        if matches!(event, FourStepEvent::ContentComplete) {
            std::fs::remove_file(self.0.join("z-requested.md")).unwrap();
            let until = Instant::now() + Duration::from_secs(5);
            while read.is_current() {
                assert!(Instant::now() < until);
                std::thread::sleep(Duration::from_millis(1));
            }
        }
        if let FourStepEvent::Complete(report) = event {
            *self.1.lock().unwrap() = Some(report.clone());
        }
        Ok(())
    }
}

#[test]
fn a_document_removed_after_content_matches_an_ordinary_build_of_the_final_tree() {
    for no_body in [false, true] {
        let scratch = fixture("four-step-delete", false);
        let vault = attach::Vault::adopt(scratch.root());
        let lab = vault.beside(&scratch.join("lab"));
        let probe = Arc::new(DeleteAfterContent(scratch.join("vault"), Mutex::new(None)));
        let host = lab
            .host_with_four_step(FourStepLoading::new(2, no_body, vec![], probe.clone()).unwrap());
        let demand = host.demand(lab.name()).unwrap();
        wait_ready(&host, &lab);
        drop(demand);
        drop(host);
        let report = probe.1.lock().unwrap().clone().unwrap();
        assert_eq!(
            report.steps["structure"]["files_read"],
            if no_body {
                report.steps["content"]["rows_rewritten"]
            } else {
                0
            },
            "a failed no-body reread is still a file-read attempt"
        );
        assert_eq!(
            probe.1.lock().unwrap().as_ref().unwrap().steps["full-text"]["tombstones_discarded"],
            u64::from(!no_body)
        );
        let ordinary = vault.beside(&scratch.join("ordinary-final"));
        let host = ordinary.host();
        let demand = host.demand(ordinary.name()).unwrap();
        wait_ready(&host, &ordinary);
        drop(demand);
        drop(host);
        let actual = norn_store::startup_audit::audit(&lab.database(), |_, _, _| Ok(())).unwrap();
        let expected =
            norn_store::startup_audit::audit(&ordinary.database(), |_, _, _| Ok(())).unwrap();
        assert_eq!(
            actual["semantic_sha256"], expected["semantic_sha256"],
            "removed document, no_body={no_body}: tombstones lab={} ordinary={}",
            actual["tables"]["tombstones"]["rows"], expected["tables"]["tombstones"]["rows"]
        );
    }
}

#[test]
fn refused_only_tail_drains_after_the_only_document_was_pulled_ahead() {
    for no_body in [false, true] {
        let scratch = norn_testkit::scratch::Scratch::under(
            &std::env::current_dir().unwrap().join(".scratch"),
            "four-step-refused-tail",
        )
        .unwrap();
        let root = scratch.join("vault");
        std::fs::create_dir_all(root.join(".norn")).unwrap();
        std::fs::write(root.join(".norn/schema.yaml"), "version: 1\n").unwrap();
        std::fs::write(root.join("z-requested.md"), "# Requested\n").unwrap();
        for index in 0..5 {
            std::fs::write(root.join(format!("refused\\{index}.md")), "Refused\n").unwrap();
        }
        let vault = attach::Vault::adopt(scratch.root());
        let probe = Arc::new(Probe::default());
        let host = vault.host_with_four_step(
            FourStepLoading::new(2, no_body, vec!["z-requested.md".into()], probe.clone()).unwrap(),
        );
        let demand = host.demand(vault.name()).unwrap();
        wait_ready(&host, &vault);
        let report = probe.report.lock().unwrap().clone().unwrap();
        assert_eq!(report.steps["structure"]["findings_written"], 5);
        assert_eq!(
            report.steps["structure"]["commits"], 2,
            "one requested transaction and one transaction for all refused-only chunks"
        );
        assert_eq!(report.timing_commits["structure"], 0);
        assert!(report.timing_ns["structure"] > 0);
        assert_eq!(report.steps["content"]["rows_rewritten"], 1);
        drop(demand);
        drop(host);
        vault.store().verify_integrity().unwrap();
    }
}

#[derive(Default)]
struct DynamicProbe {
    requests: Mutex<Option<norn_host::FourStepRequests>>,
    content_seen: std::sync::atomic::AtomicU64,
    delivered: Mutex<Vec<(String, u64)>>,
    report: Mutex<Option<FourStepReport>>,
}

impl FourStepObserver for DynamicProbe {
    fn at(&self, event: FourStepEvent<'_>, read: &FourStepRead<'_>) -> Result<(), String> {
        use std::sync::atomic::Ordering;
        let find = norn_wire::FindParams::new(VaultAddress::name(
            norn_wire::VaultName::new("notes").unwrap(),
        ));
        match event {
            FourStepEvent::Batch {
                step: "content",
                completed,
                total,
            } => {
                self.content_seen.store(completed, Ordering::SeqCst);
                if completed >= 2 && completed < total && self.requests.lock().unwrap().is_some() {
                    self.requests
                        .lock()
                        .unwrap()
                        .take()
                        .unwrap()
                        .request("z-requested.md".into())?;
                }
            }
            FourStepEvent::GetReady(path) if path == "z-requested.md" => {
                assert!(!read.metadata_complete());
                assert!(read.link_resolution_loading());
                assert!(read.link_health_loading());
                let params = GetParams::new(
                    VaultAddress::name(norn_wire::VaultName::new("notes").unwrap()),
                    ResolutionTarget::new("/z-requested.md").unwrap(),
                );
                assert!(read.get(&params)?.unsatisfied.is_empty());
                assert!(
                    read.find(&find)
                        .unwrap_err()
                        .contains("vault still indexing for the first time")
                );
                self.delivered
                    .lock()
                    .unwrap()
                    .push((path.into(), self.content_seen.load(Ordering::SeqCst)));
            }
            FourStepEvent::ContentComplete => {
                assert!(read.find(&find).is_ok());
            }
            FourStepEvent::FullTextComplete => {
                assert!(
                    read.search(&norn_store::LexicalQuery::new("Requested"))
                        .is_ok()
                );
                assert!(read.admit_write().is_ok());
            }
            FourStepEvent::Complete(report) => {
                *self.report.lock().unwrap() = Some(report.clone());
            }
            _ => {}
        }
        if !read.search_complete() {
            assert!(
                read.search(&norn_store::LexicalQuery::new("Requested"))
                    .unwrap_err()
                    .contains("vault still indexing for the first time")
            );
            assert!(
                read.admit_write()
                    .unwrap_err()
                    .contains("vault still indexing for the first time")
            );
        }
        Ok(())
    }
}

#[test]
fn a_mid_content_request_finishes_own_structure_before_the_remaining_content_queue() {
    let scratch = fixture("dynamic-request", false);
    let vault = attach::Vault::adopt(scratch.root());
    let probe = Arc::new(DynamicProbe::default());
    let loading = FourStepLoading::new(2, false, vec![], probe.clone()).unwrap();
    *probe.requests.lock().unwrap() = Some(loading.requests());
    let host = vault.host_with_four_step(loading);
    let demand = host.demand(vault.name()).unwrap();
    wait_ready(&host, &vault);
    assert_eq!(
        *probe.delivered.lock().unwrap(),
        vec![("z-requested.md".into(), 2)]
    );
    let report = probe.report.lock().unwrap();
    let report = report.as_ref().unwrap();
    assert_eq!(
        report.build_commits["content"], 2,
        "the first two-document chunk is forced visible before requested work; the remaining content is one group"
    );
    assert!(report.request_ns["z-requested.md"] <= report.requested_structure_ns["z-requested.md"]);
    assert!(
        report.requested_structure_ns["z-requested.md"] <= report.delivery_ns["z-requested.md"]
    );
    assert!(report.delivery_ns["z-requested.md"] < report.boundary_ns["content"]);
    drop(demand);
}

#[test]
fn pending_get_requests_are_bounded_and_duplicate_paths_share_a_slot() {
    let requests = norn_host::FourStepRequests::default();
    for n in 0..norn_host::FourStepRequests::MAX_PENDING {
        requests.request(format!("{n}.md")).unwrap();
    }
    requests.request("0.md".into()).unwrap();
    assert!(
        requests
            .request("overflow.md".into())
            .unwrap_err()
            .contains("queue is full (256)")
    );
}

#[derive(Default)]
struct DoneProbe {
    requests: Mutex<Option<norn_host::FourStepRequests>>,
    report: Mutex<Option<FourStepReport>>,
    redelivered: std::sync::atomic::AtomicBool,
}
impl FourStepObserver for DoneProbe {
    fn at(&self, event: FourStepEvent<'_>, read: &FourStepRead<'_>) -> Result<(), String> {
        match event {
            FourStepEvent::StructureComplete => {
                self.requests
                    .lock()
                    .unwrap()
                    .as_ref()
                    .unwrap()
                    .request("z-requested.md".into())?;
            }
            FourStepEvent::GetReady("z-requested.md")
                if self.requests.lock().unwrap().is_some() && read.metadata_complete() =>
            {
                let params = GetParams::new(
                    VaultAddress::name(norn_wire::VaultName::new("notes").unwrap()),
                    ResolutionTarget::new("/z-requested.md").unwrap(),
                );
                assert!(read.get(&params)?.unsatisfied.is_empty());
                self.redelivered
                    .store(true, std::sync::atomic::Ordering::SeqCst);
            }
            FourStepEvent::Complete(report) => {
                *self.report.lock().unwrap() = Some(report.clone());
            }
            _ => {}
        }
        Ok(())
    }
}
#[test]
fn a_request_for_an_already_structured_document_needs_no_second_derivation() {
    let scratch = fixture("done-request", false);
    let vault = attach::Vault::adopt(scratch.root());
    let probe = Arc::new(DoneProbe::default());
    let loading =
        FourStepLoading::new(2, false, vec!["z-requested.md".into()], probe.clone()).unwrap();
    *probe.requests.lock().unwrap() = Some(loading.requests());
    let host = vault.host_with_four_step(loading);
    let demand = host.demand(vault.name()).unwrap();
    wait_ready(&host, &vault);
    let guard = probe.report.lock().unwrap();
    let report = guard.as_ref().unwrap();
    assert!(probe.redelivered.load(std::sync::atomic::Ordering::SeqCst));
    assert!(report.requested_structure_ns["z-requested.md"] < report.request_ns["z-requested.md"]);
    assert_eq!(report.steps["content"]["rows_rewritten"], 12);
    assert_eq!(report.steps["content"]["files_read"], 12);
    assert_eq!(report.steps["structure"]["files_read"], 0);
    drop(demand);
}

#[derive(Default)]
struct TailProbe {
    requests: Mutex<Option<norn_host::FourStepRequests>>,
    tail: std::sync::atomic::AtomicBool,
    deliveries: Mutex<Vec<String>>,
    report: Mutex<Option<FourStepReport>>,
}
impl FourStepObserver for TailProbe {
    fn at(&self, event: FourStepEvent<'_>, _: &FourStepRead<'_>) -> Result<(), String> {
        use std::sync::atomic::Ordering;
        match event {
            FourStepEvent::Batch {
                step: "full-text",
                completed,
                total,
            } if completed == total => {
                self.tail.store(true, Ordering::SeqCst);
                self.requests
                    .lock()
                    .unwrap()
                    .as_ref()
                    .unwrap()
                    .request("hub.md".into())?;
            }
            FourStepEvent::GetReady(path) if self.tail.load(Ordering::SeqCst) => {
                self.deliveries.lock().unwrap().push(path.into());
                if path == "hub.md" {
                    self.requests
                        .lock()
                        .unwrap()
                        .as_ref()
                        .unwrap()
                        .request("z-requested.md".into())?;
                }
            }
            FourStepEvent::Complete(report) => {
                *self.report.lock().unwrap() = Some(report.clone());
            }
            _ => {}
        }
        Ok(())
    }
}
#[test]
fn requests_accepted_while_delivering_the_final_batch_are_drained_before_ready() {
    for mode in [
        norn_store::four_step::FtsBuildMode::Rebuild,
        norn_store::four_step::FtsBuildMode::Batched,
    ] {
        let scratch = fixture("tail-request", false);
        let vault = attach::Vault::adopt(scratch.root());
        let probe = Arc::new(TailProbe::default());
        let loading = FourStepLoading::new(2, false, vec![], probe.clone())
            .unwrap()
            .with_fts_mode(mode);
        *probe.requests.lock().unwrap() = Some(loading.requests());
        let host = vault.host_with_four_step(loading);
        let demand = host.demand(vault.name()).unwrap();
        wait_ready(&host, &vault);
        assert_eq!(
            *probe.deliveries.lock().unwrap(),
            vec!["hub.md", "z-requested.md"]
        );
        let guard = probe.report.lock().unwrap();
        let report = guard.as_ref().unwrap();
        assert!(report.delivery_ns.contains_key("z-requested.md"));
        assert!(report.delivery_ns["z-requested.md"] <= report.boundary_ns["full-text"]);
        drop(demand);
    }
}

struct MissingProbe {
    requests: Mutex<Option<norn_host::FourStepRequests>>,
    root: std::path::PathBuf,
    refusals: Mutex<Vec<String>>,
}
impl FourStepObserver for MissingProbe {
    fn at(&self, event: FourStepEvent<'_>, read: &FourStepRead<'_>) -> Result<(), String> {
        match event {
            FourStepEvent::ContentComplete => {
                std::fs::write(self.root.join("new.md"), "# Reconciled\n").unwrap();
                let until = Instant::now() + Duration::from_secs(5);
                while read.is_current() {
                    assert!(Instant::now() < until);
                    std::thread::sleep(Duration::from_millis(1));
                }
                let requests = self.requests.lock().unwrap();
                requests.as_ref().unwrap().request("absent.md".into())?;
                requests.as_ref().unwrap().request("a.md".into())?;
            }
            FourStepEvent::RelationsComplete => {
                self.requests
                    .lock()
                    .unwrap()
                    .as_ref()
                    .unwrap()
                    .request("new.md".into())?;
            }
            FourStepEvent::RequestRefused { path, reason } => {
                assert!(reason.contains("vault still indexing for the first time"));
                self.refusals.lock().unwrap().push(path.into());
            }
            FourStepEvent::Complete(report) => {
                for path in ["absent.md", "a.md", "new.md"] {
                    assert!(
                        report.request_refusals[path]
                            .contains("vault still indexing for the first time")
                    );
                    assert!(report.request_ns[path] <= report.refused_ns[path]);
                    assert!(!report.delivery_ns.contains_key(path));
                }
            }
            _ => {}
        }
        Ok(())
    }
}
#[test]
fn absent_quarantined_and_reconciled_requests_receive_explicit_refusals() {
    let scratch = fixture("missing-request", true);
    let vault = attach::Vault::adopt(scratch.root());
    let probe = Arc::new(MissingProbe {
        requests: Mutex::new(None),
        root: scratch.join("vault"),
        refusals: Mutex::new(vec![]),
    });
    let loading = FourStepLoading::new(2, false, vec![], probe.clone()).unwrap();
    *probe.requests.lock().unwrap() = Some(loading.requests());
    let host = vault.host_with_four_step(loading);
    let demand = host.demand(vault.name()).unwrap();
    wait_ready(&host, &vault);
    assert_eq!(
        *probe.refusals.lock().unwrap(),
        vec!["absent.md", "a.md", "new.md"]
    );
    drop(demand);
}

#[derive(Default)]
struct GroupProbe(Mutex<Option<FourStepReport>>, Mutex<Vec<(u64, u64)>>);
impl FourStepObserver for GroupProbe {
    fn at(&self, event: FourStepEvent<'_>, _: &FourStepRead<'_>) -> Result<(), String> {
        if let FourStepEvent::Complete(report) = event {
            *self.0.lock().unwrap() = Some(report.clone());
        }
        if let FourStepEvent::Batch {
            step: "relations",
            completed,
            total,
        } = event
        {
            self.1.lock().unwrap().push((completed, total));
        }
        Ok(())
    }
}

#[test]
fn absent_requests_group_host_chunks_into_at_most_2048_document_commits_in_every_pass() {
    for mode in [
        norn_store::four_step::FtsBuildMode::Rebuild,
        norn_store::four_step::FtsBuildMode::Batched,
    ] {
        let scratch = norn_testkit::scratch::Scratch::under(
            &std::env::current_dir().unwrap().join(".scratch"),
            "four-step-commit-groups",
        )
        .unwrap();
        let root = scratch.join("vault");
        std::fs::create_dir_all(root.join(".norn")).unwrap();
        std::fs::write(root.join(".norn/schema.yaml"), "version: 1\n").unwrap();
        let count: u64 = 2051;
        for n in 0..count {
            std::fs::write(root.join(format!("d{n:04}.md")), "# Body\n[[missing]]\n").unwrap();
        }
        let vault = attach::Vault::adopt(scratch.root());
        let probe = Arc::new(GroupProbe::default());
        let loading = FourStepLoading::new(128, false, vec![], probe.clone())
            .unwrap()
            .with_fts_mode(mode);
        let host = vault.host_with_four_step(loading);
        let demand = host.demand(vault.name()).unwrap();
        wait_ready(&host, &vault);
        let report = probe.0.lock().unwrap().clone().unwrap();
        for step in ["names", "content", "structure", "relations", "full-text"] {
            assert!(
                report.build_commits[step] <= count.div_ceil(2048),
                "{step}: {:?}",
                report.build_commits
            );
            assert!(report.build_commits[step] > 0);
        }
        let progress = probe.1.lock().unwrap();
        assert_eq!(progress.first(), Some(&(128, count)));
        assert_eq!(progress.last(), Some(&(count, count)));
        assert!(progress.windows(2).all(|pair| pair[0].0 < pair[1].0));
        assert_eq!(report.steps["content"]["files_read"], count);
        assert_eq!(report.steps["content"]["rows_rewritten"], count);
        assert_eq!(report.steps["full-text"]["fts_rows_inserted"], count);
        drop(demand);
        drop(host);
    }
}

#[derive(Default)]
struct ProvisionalAmbiguity(Mutex<Option<serde_json::Value>>);
impl FourStepObserver for ProvisionalAmbiguity {
    fn at(&self, event: FourStepEvent<'_>, read: &FourStepRead<'_>) -> Result<(), String> {
        if matches!(event, FourStepEvent::GetReady("one/topic.md")) {
            assert!(!read.metadata_complete());
            let params = GetParams::new(
                VaultAddress::name(norn_wire::VaultName::new("notes").unwrap()),
                ResolutionTarget::new("topic").unwrap(),
            );
            assert!(
                read.get(&params)
                    .unwrap_err()
                    .contains("vault still indexing")
            );
            let other = GetParams::new(
                params.vault.clone(),
                ResolutionTarget::new("two/topic").unwrap(),
            );
            assert!(
                read.get(&other)
                    .unwrap_err()
                    .contains("vault still indexing")
            );
            let complete =
                GetParams::new(params.vault, ResolutionTarget::new("one/topic").unwrap());
            *self.0.lock().unwrap() =
                Some(serde_json::to_value(read.get(&complete)?.report).unwrap());
        }
        Ok(())
    }
}

#[test]
fn a_provisional_undecodable_name_withholds_ambiguity_until_content_is_complete() {
    let scratch = norn_testkit::scratch::Scratch::under(
        &std::env::current_dir().unwrap().join(".scratch"),
        "four-step-provisional-name",
    )
    .unwrap();
    let root = scratch.join("vault");
    for directory in [".norn", "one", "two"] {
        std::fs::create_dir_all(root.join(directory)).unwrap();
    }
    std::fs::write(root.join(".norn/schema.yaml"), "version: 1\n").unwrap();
    std::fs::write(root.join("one/topic.md"), "# Topic\n").unwrap();
    std::fs::write(root.join("two/topic.md"), [0xff, 0xfe]).unwrap();
    let vault = attach::Vault::adopt(scratch.root());
    let probe = Arc::new(ProvisionalAmbiguity::default());
    let host = vault.host_with_four_step(
        FourStepLoading::new(2, false, vec!["one/topic.md".into()], probe.clone()).unwrap(),
    );
    let demand = host.demand(vault.name()).unwrap();
    wait_ready(&host, &vault);
    let params = GetParams::new(
        VaultAddress::name(vault.name().clone()),
        ResolutionTarget::new("topic").unwrap(),
    );
    let final_answer = serde_json::to_value(host.get(&params).unwrap().answer.report).unwrap();
    assert_eq!(probe.0.lock().unwrap().as_ref().unwrap(), &final_answer);
    drop(demand);
}
