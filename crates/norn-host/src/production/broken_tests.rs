use super::*;

const CHILD_ROOT: &str = "NORN_463_BROKEN_ROOT";
const CHILD_STORE: &str = "NORN_463_BROKEN_STORE";

fn assert_broken(store: &mut Store, spelling: &str) {
    let path = DocumentPath::new(spelling).unwrap();
    let facts = store
        .begin_request()
        .stored_facts(&path)
        .unwrap()
        .expect("the name stands");
    assert!(facts.document.content_hash.is_none());
    assert!(facts.document.byte_length.is_none() && facts.document.body_offset.is_none());
    assert!(facts.document.frontmatter.is_none() && facts.body.is_none());
    assert_eq!(facts.document.frontmatter_diagnostic_count, 0);
    assert!(
        facts.links.is_empty()
            && facts.headings.is_empty()
            && facts.blocks.is_empty()
            && facts.tags.is_empty()
            && facts.fields.is_empty()
    );
    let findings = findings_at(store, spelling);
    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0].kind, "document/unreadable");
    assert_eq!(findings[0].severity, "error");
    assert_eq!(
        findings[0].detail.as_deref(),
        Some("Permission denied (os error 13)")
    );
    assert!(
        store
            .begin_request()
            .stored_tombstone(&path)
            .unwrap()
            .is_none()
    );
}

fn under_denied_open(test: &str, scope: &str) {
    if let Some(root) = std::env::var_os(CHILD_ROOT) {
        let root = PathBuf::from(root);
        let database = PathBuf::from(std::env::var_os(CHILD_STORE).unwrap());
        let order = stored_path_order(
            norn_fs::Vault::open(&root, &[PathBuf::from(".norn")])
                .unwrap()
                .case_sensitivity(),
        );
        let mut store = Store::open(database, order, crate::DERIVATION_VERSION).unwrap();
        let progress = ProgressReporter::disconnected();
        let policy = ProductionPolicy::new(8, 2).unwrap();
        match scope {
            "full" | "stat" => {
                heal_documents(
                    &mut store,
                    &root,
                    &[PathBuf::from(".norn")],
                    policy,
                    &progress.healing(),
                )
                .unwrap();
            }
            "scoped" => scoped_increment(
                &mut store,
                &root,
                &dirty_path(&root, "notes/a.md"),
                policy,
                &progress.healing(),
                &[PathBuf::from(".norn")],
            )
            .unwrap(),
            "subtree" => scoped_increment(
                &mut store,
                &root,
                &dirty_path(&root, "notes"),
                policy,
                &progress.healing(),
                &[PathBuf::from(".norn")],
            )
            .unwrap(),
            _ => unreachable!(),
        }
        assert_broken(&mut store, "notes/a.md");
        if scope == "stat" {
            assert!(
                store
                    .begin_request()
                    .stored_document(&DocumentPath::new("notes/a.md/held.md").unwrap())
                    .unwrap()
                    .is_some(),
                "a denied stat pruned a row beneath its name"
            );
            assert!(
                store
                    .begin_request()
                    .stored_tombstone(&DocumentPath::new("notes/a.md/held.md").unwrap())
                    .unwrap()
                    .is_none()
            );
        }
        // The one-shot fault is spent. Identical bytes owe a fresh derivation.
        heal_documents(
            &mut store,
            &root,
            &[PathBuf::from(".norn")],
            policy,
            &progress.healing(),
        )
        .unwrap();
        assert!(
            findings_at(&mut store, "notes/a.md")
                .iter()
                .all(|finding| finding.kind != "document/unreadable")
        );
        let recovered = norn_testkit::equivalence::StoreProjection::read(&mut store).unwrap();
        let mut oracle = Store::open(
            root.parent().unwrap().join("oracle.sqlite3"),
            order,
            crate::DERIVATION_VERSION,
        )
        .unwrap();
        heal_from_zero(&mut oracle, &root, &[PathBuf::from(".norn")]).unwrap();
        recovered.assert_equivalent(
            &norn_testkit::equivalence::StoreProjection::read(&mut oracle).unwrap(),
            "identical-byte recovery",
        );
        return;
    }
    let f = Fixture::watcherless(test);
    fs::create_dir_all(f.vault().join("notes")).unwrap();
    if scope == "stat" {
        fs::create_dir_all(f.vault().join("notes/a.md")).unwrap();
        fs::write(
            f.vault().join("notes/a.md/held.md"),
            "held below an unknown name\n",
        )
        .unwrap();
    } else {
        fs::write(
            f.vault().join("notes/a.md"),
            "---\ntitle: original\n---\n# Heading\n\n[[absent]] #tag\n",
        )
        .unwrap();
    }
    let database = f.root.join("broken.sqlite3");
    let mut store = Store::open(&database, proven_order(&f), crate::DERIVATION_VERSION).unwrap();
    heal_from_zero(&mut store, &f.vault(), &[PathBuf::from(".norn")]).unwrap();
    drop(store);
    if scope == "stat" {
        fs::remove_dir_all(f.vault().join("notes/a.md")).unwrap();
        fs::write(f.vault().join("notes/a.md"), "now a file\n").unwrap();
    }
    let child = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            &format!("production::tests::broken_documents::{test}"),
            "--nocapture",
        ])
        .env(CHILD_ROOT, f.vault())
        .env(CHILD_STORE, database)
        .env(
            "NORN_FS_WALK_ARMED_STAGES",
            if scope == "stat" {
                "page=denied"
            } else {
                "open=denied"
            },
        )
        .output()
        .unwrap();
    assert!(
        child.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&child.stdout),
        String::from_utf8_lossy(&child.stderr)
    );
}

#[test]
fn a_warm_full_heal_marks_an_unreadable_document_broken() {
    under_denied_open(
        "a_warm_full_heal_marks_an_unreadable_document_broken",
        "full",
    );
}
#[test]
fn a_warm_scoped_increment_marks_an_unreadable_document_broken() {
    under_denied_open(
        "a_warm_scoped_increment_marks_an_unreadable_document_broken",
        "scoped",
    );
}
#[test]
fn a_failing_final_name_stat_in_a_scoped_increment_marks_the_document_broken() {
    let f = Fixture::watcherless("scoped-failing-stat");
    let root = f.vault();
    fs::create_dir_all(root.join("notes")).unwrap();
    fs::write(root.join("notes/a.md"), "# Heading\n\n[[absent]]\n").unwrap();
    let mut store = Store::open(
        f.root.join("broken.sqlite3"),
        proven_order(&f),
        crate::DERIVATION_VERSION,
    )
    .unwrap();
    heal_from_zero(&mut store, &root, &[]).unwrap();
    let _failing = norn_fs::fail_document_stats(&root.join("notes/a.md"));
    let progress = ProgressReporter::disconnected();
    scoped_increment(
        &mut store,
        &root,
        &dirty_path(&root, "notes/a.md"),
        ProductionPolicy::new(8, 2).unwrap(),
        &progress.healing(),
        &[],
    )
    .expect("a path failure at the name's stat does not refuse the job");
    let stored = store
        .begin_request()
        .stored_document(&DocumentPath::new("notes/a.md").unwrap())
        .unwrap()
        .expect("the name stands");
    assert!(stored.content_hash.is_none());
    let findings = findings_at(&mut store, "notes/a.md");
    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0].kind, "document/unreadable");
}

#[test]
fn a_warm_subtree_heal_marks_an_unreadable_document_broken() {
    under_denied_open(
        "a_warm_subtree_heal_marks_an_unreadable_document_broken",
        "subtree",
    );
}

#[test]
fn a_still_broken_document_writes_nothing() {
    let f = Fixture::watcherless("still-broken");
    let mut store = Store::open(
        f.root.join("broken.sqlite3"),
        proven_order(&f),
        crate::DERIVATION_VERSION,
    )
    .unwrap();
    let mut account = Account::default();
    let root = f.vault();
    let mut pending = Pending::new(&mut store, 8, &root, &[], &mut account).unwrap();
    pending
        .unreadable(
            Path::new("a.md"),
            "opening a document",
            &std::io::Error::from_raw_os_error(13),
        )
        .unwrap();
    pending.flush().unwrap();
    let generation = pending.store.begin_request().write_generation().unwrap();
    let before = pending.counters.clone();
    pending
        .unreadable(
            Path::new("a.md"),
            "opening a document",
            &std::io::Error::from_raw_os_error(5),
        )
        .unwrap();
    assert!(pending.changes.is_empty() && pending.queued.is_empty());
    pending.flush().unwrap();
    assert_eq!(
        pending.store.begin_request().write_generation().unwrap(),
        generation
    );
    assert_eq!(pending.counters, before);
    assert_broken(pending.store, "a.md");
}

#[test]
fn an_unopenable_refused_spelling_is_quarantined_by_spelling() {
    let f = Fixture::watcherless("unopenable-spelling");
    let mut store = Store::open(
        f.root.join("broken.sqlite3"),
        proven_order(&f),
        crate::DERIVATION_VERSION,
    )
    .unwrap();
    let mut account = Account::default();
    let root = f.vault();
    let mut pending = Pending::new(&mut store, 8, &root, &[], &mut account).unwrap();
    pending
        .unreadable(
            Path::new("bad\\name.md"),
            "opening a document",
            &std::io::Error::from_raw_os_error(13),
        )
        .unwrap();
    assert!(pending.changes.is_empty());
    assert_eq!(pending.queued.len(), 1);
    assert_eq!(pending.queued[0].cause.decided(), Decided::BySpelling);
    pending.flush().unwrap();
    assert_eq!(finding_total(&mut store), 1);
    assert!(stored_paths(&mut store).is_empty());
}

#[test]
fn a_stat_denied_md_name_takes_the_document_posture() {
    under_denied_open("a_stat_denied_md_name_takes_the_document_posture", "stat");
}

#[test]
fn a_first_attach_marks_an_unreadable_document_broken() {
    let f = Fixture::new("first-broken");
    let path = f.vault().join("a.md");
    fs::write(&path, "# Heading\n\nbody\n").unwrap();
    let denied = norn_fs::deny_document_opens(&path);
    let (ops, name) = f.ops(8);
    let progress = ProgressReporter::disconnected();
    let mut attachment = ops
        .attach(&f.registration(), &progress)
        .expect("a broken document does not refuse attach");
    assert_broken(&mut attachment.store, "a.md");
    assert_eq!(ops.evidence.read().rebuilds_run, 0);
    let before = ops.evidence.read();
    let generation = attachment.store.begin_request().write_generation().unwrap();
    let finding = findings_at(&mut attachment.store, "a.md");
    heal_the_vault(&ops, &name, &mut attachment, &progress);
    let spent = ops.evidence.read().since(before);
    assert_eq!(spent.documents_derived, 0);
    assert_eq!(spent.changesets_applied, 0);
    assert_eq!(spent.findings_written, 0);
    assert_eq!(
        attachment.store.begin_request().write_generation().unwrap(),
        generation
    );
    assert_eq!(findings_at(&mut attachment.store, "a.md"), finding);
    drop(denied);
    heal_the_vault(&ops, &name, &mut attachment, &progress);
    assert!(
        attachment
            .store
            .begin_request()
            .stored_document(&DocumentPath::new("a.md").unwrap())
            .unwrap()
            .unwrap()
            .content_hash
            .is_some()
    );
    assert!(findings_at(&mut attachment.store, "a.md").is_empty());
    ops.detach(&name, attachment);
}

#[test]
fn an_appliers_post_write_reread_marks_an_unreadable_document_broken() {
    let f = Fixture::watcherless("apply-reread-broken");
    let mut store = Store::open(
        f.root.join("broken.sqlite3"),
        proven_order(&f),
        crate::DERIVATION_VERSION,
    )
    .unwrap();
    let bytes = b"# Heading\n";
    let path = f.vault().join("a.md");
    fs::write(&path, bytes).unwrap();
    heal_from_zero(&mut store, &f.vault(), &[]).unwrap();
    let _denied = norn_fs::deny_document_opens(&path);
    let outcome = commit_plan_changeset(
        &mut store,
        &f.vault(),
        &[],
        &[PlanEffect {
            path: DocumentPath::new("a.md").unwrap(),
            holds: Some(norn_fs::ContentHash::of(bytes)),
        }],
        IncrementProvenance::Composed,
    )
    .expect("a post-write path failure does not refuse the changeset");
    assert_eq!(outcome.get("documents_upserted"), Some(1));
    assert_broken(&mut store, "a.md");
}

#[test]
fn an_unreadable_respelling_retires_the_old_spelling_atomically() {
    let f = Fixture::watcherless("broken-respelling");
    let root = f.vault();
    let mut store = Store::open(
        f.root.join("broken.sqlite3"),
        StoredPathOrder::AsciiCaseInsensitive,
        crate::DERIVATION_VERSION,
    )
    .unwrap();
    fs::write(root.join("a.md"), "old body\n").unwrap();
    let facts = norn_store::DocumentFacts::new(
        DocumentPath::new("a.md").unwrap(),
        "old-hash",
        "old body\n",
        9,
    );
    store
        .begin_request()
        .apply_increment(
            IncrementProvenance::Derived,
            [Change::Upsert(facts)],
            &[],
            &ContentModel::none(),
        )
        .unwrap();
    fs::rename(root.join("a.md"), root.join("A.md")).unwrap();
    let _denied = norn_fs::deny_document_opens(&root.join("A.md"));
    let progress = ProgressReporter::disconnected();
    let mut account = Account::default();
    merge_walk(
        &mut store,
        &root,
        &[PathBuf::from(".norn")],
        norn_fs::walk(&root, &[PathBuf::from(".norn")]).unwrap(),
        norn_fs::CaseSensitivity::Insensitive,
        HealScope::Vault,
        ProductionPolicy::new(1, 1).unwrap(),
        &progress.healing(),
        &mut account,
    )
    .unwrap();
    assert_eq!(stored_paths(&mut store), ["A.md"]);
    assert_broken(&mut store, "A.md");
    assert!(
        account.withheld.roots.is_empty(),
        "a broken row accounts for its own name"
    );
}

#[test]
fn a_rolling_heal_quarantines_an_unopenable_refused_spelling_once() {
    let f = Fixture::new("rolling-unopenable-spelling");
    let path = f.vault().join("bad\\name.md");
    fs::write(&path, "body\n").unwrap();
    let (ops, name) = f.ops(1);
    let progress = ProgressReporter::disconnected();
    let mut attachment = ops.attach(&f.registration(), &progress).unwrap();
    attachment.subscription.take();
    let subject = DocumentPath::rendered(Path::new("bad\\name.md"));
    assert_eq!(
        findings_at(&mut attachment.store, subject.as_str()).len(),
        1
    );
    let _denied = norn_fs::deny_document_opens(&path);
    attachment.rolling.due = Instant::now();
    for _ in 0..1000 {
        rolling::slice(&mut attachment, ops.policy).unwrap();
        if attachment.rolling.last_completed.is_some() {
            break;
        }
    }
    assert!(attachment.rolling.last_completed.is_some());
    let findings = findings_at(&mut attachment.store, subject.as_str());
    assert_eq!(
        findings.len(),
        1,
        "a rolling pass filed the spelling twice: {findings:?}"
    );
    assert_eq!(findings[0].kind, "document/path-names-no-document");
    ops.detach(&name, attachment);
}

#[test]
fn a_broken_file_does_not_withhold_its_loading_name() {
    let f = Fixture::watcherless("loading-broken-name");
    let root = f.vault();
    fs::write(root.join("a.md"), "body\n").unwrap();
    let _denied = norn_fs::deny_document_opens(&root.join("a.md"));
    let source = norn_fs::Vault::open(&root, &[]).unwrap();
    let fact = source.fact(Path::new("a.md")).unwrap();
    let mut store = Store::open(
        f.root.join("broken.sqlite3"),
        proven_order(&f),
        crate::DERIVATION_VERSION,
    )
    .unwrap();
    let mut account = Account::default();
    let mut pending = Pending::new(&mut store, 8, &root, &[], &mut account).unwrap();
    assert!(
        loading::facts::load_fact(&mut pending, fact, loading::facts::Depth::Metadata, false)
            .unwrap()
            .is_none()
    );
    pending.flush().unwrap();
    assert!(pending.account.withheld.roots.is_empty() && !pending.account.withheld.rendered);
    assert_broken(pending.store, "a.md");
}
