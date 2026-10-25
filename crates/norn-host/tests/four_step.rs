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
    structured: Mutex<Vec<String>>,
    report: Mutex<Option<FourStepReport>>,
    early: Mutex<Option<serde_json::Value>>,
    find: Mutex<Option<serde_json::Value>>,
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
                        read.get(&params).is_err(),
                        "step 3 alone never admits provisional targets"
                    );
                }
            }
            FourStepEvent::GetReady(path) => {
                if path == "z-requested.md" {
                    let params = GetParams::new(
                        VaultAddress::name(norn_wire::VaultName::new("notes").unwrap()),
                        ResolutionTarget::new("z-requested").unwrap(),
                    );
                    let got = read.get(&params)?;
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
        ("one/dupe.md", "[[hub]]\n"),
        ("two/dupe.md", "[[hub]]\n"),
        ("deep/nested/target.md", "[[hub]] [[../../hub]]\n"),
        (
            "z-requested.md",
            "---\nrating: 7\nrelated: '[[hub]]'\n---\n# Requested\n[[hub#Present]] [[missing]] [[a]]\n#early\n",
        ),
        ("media/picture.png", "name-only"),
    ] {
        std::fs::write(root.join(path), text).unwrap();
    }
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

fn without_findings(mut report: serde_json::Value) -> serde_json::Value {
    report["document"]
        .as_object_mut()
        .unwrap()
        .remove("findings");
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
            ResolutionTarget::new("z-requested").unwrap(),
        );
        let ordinary_get = serde_json::to_value(host.get(&params).unwrap().answer.report).unwrap();
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
        for no_body in [false, true] {
            for pull_ahead in [false, true] {
                let machine = vault.beside(&scratch.join(format!("lab-{no_body}-{pull_ahead}")));
                let probe = Arc::new(Probe::default());
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
                .unwrap();
                let host = machine.host_with_four_step(loading);
                let demand = host.demand(machine.name()).unwrap();
                wait_ready(&host, &machine);
                assert_eq!(
                    format!("{:?}", host.inspect(machine.name()).unwrap().advisories),
                    ordinary_advisories
                );
                let work = host.evidence();
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
                assert_eq!(report.steps["relations"]["links_judged"], 17);
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
                assert_eq!(work.links_redecided, 17);
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
                assert_eq!(
                    without_findings(probe.early.lock().unwrap().clone().unwrap()),
                    without_findings(ordinary_get.clone())
                );
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
                ResolutionTarget::new("z-requested").unwrap(),
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
        assert_eq!(
            probe.1.lock().unwrap().as_ref().unwrap().steps["relations"]["tombstones_discarded"],
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
