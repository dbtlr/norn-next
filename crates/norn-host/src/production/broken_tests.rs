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
    let child = crate::child_gate::output_of_test_binary(std::process::Command::new(std::env::current_exe().unwrap())
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
        )
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
fn a_rolling_revisit_marks_a_document_broken_that_turned_unreadable_since_healing() {
    let f = Fixture::new("rolling-revisit-recovered");
    let path = f.vault().join("a.md");
    fs::write(&path, "body\n").unwrap();
    let (ops, name) = f.ops(1);
    let progress = ProgressReporter::disconnected();
    let failing = norn_fs::deny_document_opens(&path);
    let mut attachment = ops.attach(&f.registration(), &progress).unwrap();
    attachment.subscription.take();
    let broken = findings_at(&mut attachment.store, "a.md");
    assert_eq!(broken.len(), 1);
    assert_eq!(broken[0].kind, "document/unreadable");
    attachment.rolling.due = Instant::now();
    for _ in 0..1000 {
        rolling::slice(&mut attachment, ops.policy).unwrap();
        if attachment
            .rolling
            .status()
            .pass
            .is_some_and(|pass| pass.phase == norn_wire::RollingHealPhase::Finishing)
        {
            break;
        }
    }
    // Healing recorded the broken row; a watcher turn then derives the file.
    drop(failing);
    ops.reconcile(
        &name,
        &mut attachment,
        ReconcileWork {
            batch: dirty_batch(f.vault().as_path(), &["a.md"]),
        },
        &progress,
    )
    .unwrap();
    let derived = |store: &mut Store| {
        store
            .begin_request()
            .stored_facts(&DocumentPath::new("a.md").unwrap())
            .unwrap()
            .expect("the name stands")
            .document
            .content_hash
            .is_some()
    };
    assert!(derived(&mut attachment.store));
    // The revisit re-observes each name as it replays it, so an open denied
    // now is newer evidence than the watcher turn's read: the store keeps
    // nothing derived from bytes it can no longer read.
    let _failing = norn_fs::deny_document_opens(&path);
    for _ in 0..1000 {
        rolling::slice(&mut attachment, ops.policy).unwrap();
        if attachment.rolling.last_completed.is_some() {
            break;
        }
    }
    assert!(attachment.rolling.last_completed.is_some());
    assert!(!derived(&mut attachment.store));
    assert_broken(&mut attachment.store, "a.md");
    let broken = findings_at(&mut attachment.store, "a.md");
    assert_eq!(broken.len(), 1);
    assert_eq!(broken[0].kind, "document/unreadable");
    ops.detach(&name, attachment);
}

#[cfg(unix)]
#[test]
fn an_unlistable_directory_is_withheld_and_marked_in_a_full_heal() {
    use std::os::unix::fs::PermissionsExt;

    let f = Fixture::watcherless("directory-full-heal");
    let root = f.vault();
    fs::create_dir(root.join("locked")).unwrap();
    fs::write(
        root.join("locked/inside.md"),
        "# Heading\n\n[[gone]] #tag\n",
    )
    .unwrap();
    fs::write(
        root.join("kept.md"),
        "[path](/locked/gone.md) [[locked/gone.md]] [[inside]]\n",
    )
    .unwrap();
    let mut store = Store::open(
        f.root.join("directory.sqlite3"),
        proven_order(&f),
        crate::DERIVATION_VERSION,
    )
    .unwrap();
    heal_from_zero(&mut store, &root, &[]).unwrap();
    let locked = root.join("locked");
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
    if fs::read_dir(&locked).is_ok() {
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
        eprintln!("skipped: this account reads mode-000 directories; Stage::List carries denial");
        return;
    }
    let progress = ProgressReporter::disconnected();
    let healed = heal_documents(
        &mut store,
        &root,
        &[],
        ProductionPolicy::new(8, 2).unwrap(),
        &progress.healing(),
    );
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
    healed.expect("one unlistable directory does not refuse the heal");
    let facts = store
        .begin_request()
        .stored_facts(&DocumentPath::new("locked/inside.md").unwrap())
        .unwrap()
        .expect("the name stands");
    assert!(facts.document.content_hash.is_none() && facts.body.is_none());
    assert!(facts.links.is_empty() && facts.headings.is_empty() && facts.tags.is_empty());
    assert!(findings_at(&mut store, "locked/inside.md").is_empty());
    let findings = findings_at(&mut store, "locked");
    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0].kind, "directory/unreadable");
    assert_eq!(findings[0].severity, "error");
    assert!(
        findings[0]
            .detail
            .as_deref()
            .unwrap()
            .contains("Permission denied")
    );
    assert!(
        store
            .begin_request()
            .stored_tombstone(&DocumentPath::new("locked/inside.md").unwrap())
            .unwrap()
            .is_none()
    );
    let broken = findings_at(&mut store, "kept.md");
    assert_eq!(
        broken.len(),
        1,
        "only the suffix link remains broken: {broken:?}"
    );
    assert_eq!(broken[0].target.as_deref(), Some("locked/gone.md"));
}

#[test]
fn a_still_unlistable_directory_writes_nothing() {
    let f = Fixture::watcherless("still-unlistable");
    let mut store = Store::open(
        f.root.join("directory.sqlite3"),
        proven_order(&f),
        crate::DERIVATION_VERSION,
    )
    .unwrap();
    let root = f.vault();
    let mut account = Account::default();
    let mut pending = Pending::new(&mut store, 8, &root, &[], &mut account).unwrap();
    pending
        .unlistable(Path::new("locked"), &std::io::Error::from_raw_os_error(13))
        .unwrap();
    pending.flush().unwrap();
    let generation = pending.store.begin_request().write_generation().unwrap();
    let counters = pending.counters.clone();
    let findings = findings_at(pending.store, "locked");
    assert_eq!(findings.len(), 1);
    pending
        .unlistable(Path::new("locked"), &std::io::Error::from_raw_os_error(5))
        .unwrap();
    assert!(pending.changes.is_empty() && pending.queued.is_empty());
    pending.flush().unwrap();
    assert_eq!(
        pending.store.begin_request().write_generation().unwrap(),
        generation
    );
    assert_eq!(pending.counters, counters);
    assert_eq!(findings_at(pending.store, "locked"), findings);
}

fn directory_under_the_list_arm(test: &str, scope: &str) {
    const ROOT: &str = "NORN_464_DIRECTORY_ROOT";
    const DATABASE: &str = "NORN_464_DIRECTORY_DATABASE";
    if let Some(root) = std::env::var_os(ROOT) {
        let root = PathBuf::from(root);
        let order = stored_path_order(norn_fs::Vault::open(&root, &[]).unwrap().case_sensitivity());
        let mut store = Store::open(
            PathBuf::from(std::env::var_os(DATABASE).unwrap()),
            order,
            crate::DERIVATION_VERSION,
        )
        .unwrap();
        let progress = ProgressReporter::disconnected();
        let policy = ProductionPolicy::new(8, 2).unwrap();
        match scope {
            "dirty" => scoped_increment(
                &mut store,
                &root,
                &dirty_path(&root, "locked/inside.md"),
                policy,
                &progress.healing(),
                &[],
            )
            .unwrap(),
            "subtree" | "recovery" => scoped_increment(
                &mut store,
                &root,
                &dirty_path(&root, "locked"),
                policy,
                &progress.healing(),
                &[],
            )
            .unwrap(),
            "applier" => {
                commit_plan_changeset(
                    &mut store,
                    &root,
                    &[],
                    &[PlanEffect {
                        path: DocumentPath::new("locked/inside.md").unwrap(),
                        holds: Some(norn_fs::ContentHash::of(b"# Heading\n\n[[absent]] #tag\n")),
                    }],
                    IncrementProvenance::Composed,
                )
                .unwrap();
            }
            "fact" => {
                let source = norn_fs::Vault::open(&root, &[]).unwrap();
                let fact = source.fact(Path::new("locked/inside.md")).unwrap();
                let mut account = Account::default();
                let mut pending = Pending::new(&mut store, 8, &root, &[], &mut account).unwrap();
                pending
                    .unreadable_fact(
                        match &fact {
                            norn_fs::WalkFact::Unreadable(fact) => fact,
                            other => panic!("expected unreadable folder: {other:?}"),
                        },
                        None,
                    )
                    .unwrap();
                pending.flush().unwrap();
            }
            "stat" | "known" => {
                heal_documents(&mut store, &root, &[], policy, &progress.healing()).unwrap();
            }
            _ => unreachable!(),
        }
        let directory = if scope == "known" {
            "locked.md"
        } else {
            "locked"
        };
        let findings = findings_at(&mut store, directory);
        assert_eq!(findings.len(), 1, "{scope}: {findings:?}");
        assert_eq!(findings[0].kind, "directory/unreadable");
        if scope == "known" {
            assert!(
                store
                    .begin_request()
                    .stored_document(&DocumentPath::new(directory).unwrap())
                    .unwrap()
                    .is_none()
            );
        } else {
            let held = store
                .begin_request()
                .stored_facts(&DocumentPath::new("locked/inside.md").unwrap())
                .unwrap()
                .expect("the name is held");
            assert!(held.document.content_hash.is_none() && held.body.is_none());
            assert!(findings_at(&mut store, "locked/inside.md").is_empty());
            assert!(
                store
                    .begin_request()
                    .stored_tombstone(&DocumentPath::new("locked/inside.md").unwrap())
                    .unwrap()
                    .is_none()
            );
        }
        // The one-shot arm is spent. A scoped listing ends its mark at job end.
        scoped_increment(
            &mut store,
            &root,
            &dirty_path(&root, directory),
            policy,
            &progress.healing(),
            &[],
        )
        .unwrap();
        assert!(findings_at(&mut store, directory).is_empty());
        if scope != "known" {
            assert!(
                store
                    .begin_request()
                    .stored_document(&DocumentPath::new("locked/inside.md").unwrap())
                    .unwrap()
                    .unwrap()
                    .content_hash
                    .is_some()
            );
        }
        let mut oracle = Store::open(
            root.parent().unwrap().join("directory-oracle.sqlite3"),
            order,
            crate::DERIVATION_VERSION,
        )
        .unwrap();
        heal_from_zero(&mut oracle, &root, &[]).unwrap();
        norn_testkit::equivalence::StoreProjection::read(&mut store)
            .unwrap()
            .assert_equivalent(
                &norn_testkit::equivalence::StoreProjection::read(&mut oracle).unwrap(),
                "directory recovery at identical bytes",
            );
        return;
    }
    let f = Fixture::watcherless(test);
    let root = f.vault();
    if scope == "stat" {
        // The paging arm selects the first directory. This fixture needs no control files.
        fs::remove_dir_all(root.join(".norn")).unwrap();
    }
    if scope == "known" {
        fs::write(root.join("locked.md"), "formerly a document\n").unwrap();
    } else {
        fs::create_dir(root.join("locked")).unwrap();
        fs::write(
            root.join("locked/inside.md"),
            "# Heading\n\n[[absent]] #tag\n",
        )
        .unwrap();
    }
    fs::write(root.join("kept.md"), "[path](/locked/gone.md)\n").unwrap();
    let database = f.root.join("directory.sqlite3");
    let mut store = Store::open(&database, proven_order(&f), crate::DERIVATION_VERSION).unwrap();
    heal_from_zero(&mut store, &root, &[]).unwrap();
    drop(store);
    if scope == "known" {
        fs::remove_file(root.join("locked.md")).unwrap();
        fs::create_dir(root.join("locked.md")).unwrap();
        fs::write(root.join("locked.md/inside.md"), "new child\n").unwrap();
    }
    let output = crate::child_gate::output_of_test_binary(std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            &format!("production::tests::broken_documents::{test}"),
            "--nocapture",
        ])
        .env(ROOT, &root)
        .env(DATABASE, database)
        .env(
            "NORN_FS_WALK_ARMED_STAGES",
            if scope == "stat" {
                "page=denied"
            } else {
                "list=denied"
            },
        )
        .env("NORN_FS_WALK_REACH", "directory")
        .env(
            "NORN_FS_WALK_LIST_AT",
            if scope == "known" {
                "locked.md"
            } else {
                "locked"
            },
        )
        )
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn an_unlistable_directory_is_withheld_and_marked_in_a_scoped_subtree_heal() {
    directory_under_the_list_arm(
        "an_unlistable_directory_is_withheld_and_marked_in_a_scoped_subtree_heal",
        "subtree",
    );
}

#[test]
fn a_dirty_path_under_a_denied_folder_marks_the_folder() {
    directory_under_the_list_arm(
        "a_dirty_path_under_a_denied_folder_marks_the_folder",
        "dirty",
    );
}

#[test]
fn a_stored_only_row_confirmation_marks_its_denied_folder() {
    directory_under_the_list_arm(
        "a_stored_only_row_confirmation_marks_its_denied_folder",
        "fact",
    );
}

#[test]
fn an_appliers_post_write_reread_marks_its_denied_folder() {
    directory_under_the_list_arm(
        "an_appliers_post_write_reread_marks_its_denied_folder",
        "applier",
    );
}

#[test]
fn a_stat_denied_other_name_takes_the_directory_posture() {
    directory_under_the_list_arm(
        "a_stat_denied_other_name_takes_the_directory_posture",
        "stat",
    );
}

#[test]
fn a_relisted_directory_ends_its_mark_at_the_scoped_leg() {
    directory_under_the_list_arm(
        "a_relisted_directory_ends_its_mark_at_the_scoped_leg",
        "recovery",
    );
}

#[test]
fn a_directory_row_at_a_known_directory_name_dies() {
    directory_under_the_list_arm("a_directory_row_at_a_known_directory_name_dies", "known");
}

#[test]
fn an_unlistable_directory_is_withheld_and_marked_at_first_build() {
    const CHILD: &str = "NORN_464_FIRST_DIRECTORY_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let output = crate::child_gate::output_of_test_binary(std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "production::tests::broken_documents::an_unlistable_directory_is_withheld_and_marked_at_first_build", "--nocapture"])
            .env(CHILD, "1").env("NORN_FS_WALK_ARMED_STAGES", "list=denied")
            .env("NORN_FS_WALK_LIST_AT", "locked")).unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    let f = Fixture::new("directory-first-build");
    fs::create_dir(f.vault().join("locked")).unwrap();
    fs::write(f.vault().join("locked/inside.md"), "# Held\n").unwrap();
    fs::write(f.vault().join("kept.md"), "kept\n").unwrap();
    let (ops, name) = f.ops(8);
    let progress = ProgressReporter::disconnected();
    let mut attachment = ops
        .attach(&f.registration(), &progress)
        .expect("an unlistable directory does not refuse attach");
    assert_eq!(ops.evidence.read().rebuilds_run, 0);
    assert_eq!(
        findings_at(&mut attachment.store, "locked")[0].kind,
        "directory/unreadable"
    );
    assert_eq!(stored_paths(&mut attachment.store), ["kept.md"]);
    ops.detach(&name, attachment);
}

#[cfg(unix)]
#[test]
fn marking_a_directory_leaves_its_readable_case_twin_untouched() {
    const ROOT: &str = "NORN_464_TWIN_ROOT";
    const DATABASE: &str = "NORN_464_TWIN_DATABASE";
    if let Some(root) = std::env::var_os(ROOT) {
        let root = PathBuf::from(root);
        let order = stored_path_order(norn_fs::Vault::open(&root, &[]).unwrap().case_sensitivity());
        let mut store = Store::open(PathBuf::from(std::env::var_os(DATABASE).unwrap()), order, crate::DERIVATION_VERSION).unwrap();
        let sibling = DocumentPath::new("locked/b.md").unwrap();
        let before = store.begin_request().stored_facts(&sibling).unwrap();
        let findings = findings_at(&mut store, sibling.as_str());
        let already_marked = store.begin_request().unreadable_directory_stands(&DocumentPath::new("Locked").unwrap()).unwrap();
        let generation = store.begin_request().write_generation().unwrap();
        let progress = ProgressReporter::disconnected();
        heal_documents(&mut store, &root, &[], ProductionPolicy::new(8, 2).unwrap(), &progress.healing()).unwrap();
        assert_eq!(store.begin_request().stored_facts(&sibling).unwrap(), before,
            "the readable sibling's bytes remain known");
        assert_eq!(findings_at(&mut store, sibling.as_str()), findings);
        if already_marked {
            assert_eq!(store.begin_request().write_generation().unwrap(), generation,
                "the still-denied directory writes nothing");
        }
        assert_eq!(findings_at(&mut store, "Locked")[0].kind, "directory/unreadable");
        assert!(store.begin_request().stored_document(&DocumentPath::new("Locked/inside.md").unwrap()).unwrap().unwrap().content_hash.is_none());
        return;
    }
    let f = Fixture::watcherless("unlistable-case-twin");
    let root = f.vault();
    if !norn_testkit::churn::runs_where_the_volume_tells_case_apart(
        norn_testkit::churn::folding(&root).unwrap(), "unlistable case twins",
    ) {
        return;
    }
    fs::create_dir(root.join("Locked")).unwrap();
    fs::create_dir(root.join("locked")).unwrap();
    fs::write(root.join("Locked/inside.md"), "unread later\n").unwrap();
    fs::write(root.join("locked/b.md"), "# Heading\n\n#tag [[nowhere]]\n").unwrap();
    let database = f.root.join("twins.sqlite3");
    let mut store = Store::open(&database, proven_order(&f), crate::DERIVATION_VERSION).unwrap();
    heal_from_zero(&mut store, &root, &[]).unwrap();
    drop(store);
    // Each enumerating heal gets one directory failure, including under root.
    for _ in 0..2 {
        let child = crate::child_gate::output_of_test_binary(std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "production::tests::broken_documents::marking_a_directory_leaves_its_readable_case_twin_untouched", "--nocapture"])
            .env(ROOT, &root).env(DATABASE, &database)
            .env("NORN_FS_WALK_ARMED_STAGES", "list=denied")
            .env("NORN_FS_WALK_LIST_AT", "Locked")).unwrap();
        assert!(child.status.success(), "{}\n{}", String::from_utf8_lossy(&child.stdout), String::from_utf8_lossy(&child.stderr));
    }
}

/// **A directory step 1 listed and step 2 cannot is a held directory that
/// stopped listing.** Every descendant leaves the progress queue as a
/// names-only row with no tombstone, beneath the directory's finding: what
/// ADR 0046 gives a store that held those rows when the mark is first filed.
/// That is the ordinary build to compare with — the tree as step 1 listed it,
/// then the heal that meets the mark — not a build from zero over the final
/// tree, which never saw the names and so holds no rows beneath it.
#[test]
fn a_mark_found_when_content_opens() {
    const CHILD: &str = "NORN_STEP_CONTENT_DIRECTORY_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let output = crate::child_gate::output_of_test_binary(std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "production::tests::broken_documents::a_mark_found_when_content_opens", "--nocapture"])
            .env(CHILD, "1").env("NORN_FS_WALK_ARMED_STAGES", "list=denied")
            .env("NORN_FS_WALK_LIST_AT", "locked")).unwrap();
        assert!(output.status.success(), "{}\n{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
        return;
    }
    let f = Fixture::watcherless("content-open-unlistable");
    fs::create_dir(f.vault().join("locked")).unwrap();
    for path in ["locked/a.md", "locked/b.md"] { fs::write(f.vault().join(path), "# Heading\n[[missing]] #tag\n").unwrap(); }
    let mut store = Store::open(f.root.join("steps.sqlite3"), proven_order(&f), crate::DERIVATION_VERSION).unwrap();
    let paths: Vec<_> = ["locked/a.md", "locked/b.md"].map(|p| DocumentPath::new(p).unwrap()).into();
    store.defer_link_health().unwrap(); store.begin_steps().unwrap();
    store.begin_request().apply_increment(IncrementProvenance::Derived, paths.iter().cloned().map(|path| Change::Named { path }), &[], &ContentModel::none()).unwrap();
    store.finish_step(norn_store::Step::Names).unwrap();
    let (ops, _) = f.ops(2);
    let source = norn_fs::Vault::open(&f.vault(), &[]).unwrap();
    let mut account = Account::default();
    let mut group = store.step_group().unwrap();
    let root = f.vault();
    let mut pending = Pending::new(group.store(), 2, &root, &[], &mut account).unwrap();
    loading::steps::content_chunk(&ops, &mut pending, &source, &paths).unwrap();
    pending.flush().unwrap(); drop(pending); group.commit().unwrap();
    assert!(store.begin_request().content_page(2).unwrap().is_empty());
    assert!(store.begin_request().structure_page(2).unwrap().is_empty());
    assert!(store.begin_request().unreadable_directory_stands(&DocumentPath::new("locked").unwrap()).unwrap());
    for path in &paths {
        let facts = store.begin_request().stored_facts(path).unwrap().unwrap();
        assert!(facts.body.is_none()); assert!(facts.links.is_empty()); assert!(facts.headings.is_empty());
        assert!(store.begin_request().stored_tombstone(path).unwrap().is_none());
    }
}

#[test]
fn a_broken_content_open_finishes_without_structure() {
    const CHILD: &str = "NORN_STEP_CONTENT_FILE_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let output = crate::child_gate::output_of_test_binary(std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "production::tests::broken_documents::a_broken_content_open_finishes_without_structure", "--nocapture"])
            .env(CHILD, "1").env("NORN_FS_WALK_ARMED_STAGES", "open=denied")).unwrap();
        assert!(output.status.success(), "{}\n{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
        return;
    }
    let f = Fixture::watcherless("content-open-broken");
    for path in ["a.md", "b.md"] { fs::write(f.vault().join(path), "# Heading\n[[missing]] #tag\n").unwrap(); }
    let mut store = Store::open(f.root.join("steps.sqlite3"), proven_order(&f), crate::DERIVATION_VERSION).unwrap();
    let paths: Vec<_> = ["a.md", "b.md"].map(|p| DocumentPath::new(p).unwrap()).into();
    store.defer_link_health().unwrap(); store.begin_steps().unwrap();
    store.begin_request().apply_increment(IncrementProvenance::Derived, paths.iter().cloned().map(|path| Change::Named { path }), &[], &ContentModel::none()).unwrap();
    store.finish_step(norn_store::Step::Names).unwrap();
    let (ops, _) = f.ops(2);
    let source = norn_fs::Vault::open(&f.vault(), &[]).unwrap();
    let root = f.vault();
    let mut account = Account::default();
    let mut group = store.step_group().unwrap();
    let mut pending = Pending::new(group.store(), 2, &root, &[], &mut account).unwrap();
    loading::steps::content_chunk(&ops, &mut pending, &source, &paths).unwrap();
    pending.flush().unwrap(); drop(pending); group.commit().unwrap();
    assert_broken(&mut store, "a.md");
    assert!(store.begin_request().content_page(2).unwrap().is_empty());
    let rows = store.begin_request().structure_page(2).unwrap();
    assert_eq!(rows.len(), 1); assert_eq!(rows[0].facts.path.as_str(), "b.md");
}

/// **A flush that holds content rows and a change writes both.** The content
/// queued for one document lands, and the change filed beside it for another
/// is applied after it: neither is dropped for sharing the flush.
#[test]
fn a_flush_holding_content_and_a_change_writes_both() {
    let f = Fixture::watcherless("content-beside-a-change");
    for path in ["a.md", "b.md"] { fs::write(f.vault().join(path), "# Heading\n[[missing]] #tag\n").unwrap(); }
    let mut store = Store::open(f.root.join("steps.sqlite3"), proven_order(&f), crate::DERIVATION_VERSION).unwrap();
    let paths: Vec<_> = ["a.md", "b.md"].map(|p| DocumentPath::new(p).unwrap()).into();
    store.defer_link_health().unwrap(); store.begin_steps().unwrap();
    store.begin_request().apply_increment(IncrementProvenance::Derived, paths.iter().cloned().map(|path| Change::Named { path }), &[], &ContentModel::none()).unwrap();
    store.finish_step(norn_store::Step::Names).unwrap();
    let (ops, _) = f.ops(4);
    let source = norn_fs::Vault::open(&f.vault(), &[]).unwrap();
    let root = f.vault();
    let mut account = Account::default();
    let mut group = store.step_group().unwrap();
    let mut pending = Pending::new(group.store(), 4, &root, &[], &mut account).unwrap();
    loading::steps::content_chunk(&ops, &mut pending, &source, &paths[..1]).unwrap();
    pending.unreadable(Path::new("b.md"), "opening a document", &std::io::Error::from_raw_os_error(13)).unwrap();
    pending.flush().unwrap(); drop(pending); group.commit().unwrap();
    assert_broken(&mut store, "b.md");
    assert!(store.begin_request().content_page(2).unwrap().is_empty(), "a queued content row was dropped");
    let rows = store.begin_request().structure_page(2).unwrap();
    assert_eq!(rows.len(), 1); assert_eq!(rows[0].facts.path.as_str(), "a.md");
}

/// **A directory mark filed beside a content row beneath it leaves no content
/// finding standing.** The mark makes the row names-only and ends what stands
/// beneath it, so the frontmatter finding derived from the bytes it no longer
/// holds goes with them, as when the mark is filed in a flush of its own.
#[test]
fn a_mark_filed_beside_content_beneath_it_ends_that_content_s_findings() {
    let f = Fixture::watcherless("mark-beside-content");
    fs::create_dir(f.vault().join("dir")).unwrap();
    fs::write(f.vault().join("dir/a.md"), "---\nbad: [\n---\nbody\n").unwrap();
    let mut store = Store::open(f.root.join("steps.sqlite3"), proven_order(&f), crate::DERIVATION_VERSION).unwrap();
    let paths: Vec<_> = ["dir/a.md"].map(|p| DocumentPath::new(p).unwrap()).into();
    store.defer_link_health().unwrap(); store.begin_steps().unwrap();
    store.begin_request().apply_increment(IncrementProvenance::Derived, paths.iter().cloned().map(|path| Change::Named { path }), &[], &ContentModel::none()).unwrap();
    store.finish_step(norn_store::Step::Names).unwrap();
    let (ops, _) = f.ops(4);
    let source = norn_fs::Vault::open(&f.vault(), &[]).unwrap();
    let root = f.vault();
    let mut account = Account::default();
    let mut group = store.step_group().unwrap();
    let mut pending = Pending::new(group.store(), 4, &root, &[], &mut account).unwrap();
    loading::steps::content_chunk(&ops, &mut pending, &source, &paths).unwrap();
    pending.unlistable(Path::new("dir"), &std::io::Error::from_raw_os_error(13)).unwrap();
    pending.flush().unwrap(); drop(pending); group.commit().unwrap();
    assert!(store.begin_request().content_page(2).unwrap().is_empty());
    let filed: Vec<String> = norn_db::open_read_only(store.path()).adopted.unwrap().connection()
        .prepare("SELECT kind FROM findings WHERE path = 'dir/a.md'").unwrap()
        .query_map([], |row| row.get(0)).unwrap().map(Result::unwrap).collect();
    assert!(filed.is_empty(), "a names-only row kept its content's findings: {filed:?}");
}

/// **A mark filed beside a withdrawn document's finding ends it.** An
/// undecodable document beneath a directory leaves no content row, only its
/// queued content finding; the mark filed beside it ends what stands beneath
/// the directory, that finding included, as in a flush of its own.
#[test]
fn a_mark_filed_beside_a_withdrawn_document_s_finding_ends_it() {
    let f = Fixture::watcherless("mark-beside-withdrawn");
    fs::create_dir(f.vault().join("dir")).unwrap();
    fs::write(f.vault().join("dir/a.md"), b"\xff\xfe\x00bad").unwrap();
    let mut store = Store::open(f.root.join("steps.sqlite3"), proven_order(&f), crate::DERIVATION_VERSION).unwrap();
    let paths: Vec<_> = ["dir/a.md"].map(|p| DocumentPath::new(p).unwrap()).into();
    store.defer_link_health().unwrap(); store.begin_steps().unwrap();
    store.begin_request().apply_increment(IncrementProvenance::Derived, paths.iter().cloned().map(|path| Change::Named { path }), &[], &ContentModel::none()).unwrap();
    store.finish_step(norn_store::Step::Names).unwrap();
    let (ops, _) = f.ops(4);
    let source = norn_fs::Vault::open(&f.vault(), &[]).unwrap();
    let root = f.vault();
    let mut account = Account::default();
    let mut group = store.step_group().unwrap();
    let mut pending = Pending::new(group.store(), 4, &root, &[], &mut account).unwrap();
    loading::steps::content_chunk(&ops, &mut pending, &source, &paths).unwrap();
    pending.unlistable(Path::new("dir"), &std::io::Error::from_raw_os_error(13)).unwrap();
    pending.flush().unwrap(); drop(pending); group.commit().unwrap();
    let filed: Vec<String> = norn_db::open_read_only(store.path()).adopted.unwrap().connection()
        .prepare("SELECT kind FROM findings WHERE path = 'dir/a.md'").unwrap()
        .query_map([], |row| row.get(0)).unwrap().map(Result::unwrap).collect();
    assert!(filed.is_empty(), "a finding beneath the mark survived it: {filed:?}");
}

/// **A named document that became a directory the walk cannot list leaves the
/// content step.** Its provisional row owns no death, so it is withdrawn and
/// the directory is marked: a build from zero over the final tree holds no row
/// at that path and the mark at it. The content queue empties, so the step
/// ends rather than paging the same name again.
#[test]
fn a_named_document_turned_unlistable_directory_leaves_the_content_step() {
    use std::os::unix::fs::PermissionsExt;
    let f = Fixture::watcherless("named-turned-directory");
    for path in ["a.md", "b.md"] { fs::write(f.vault().join(path), "# Heading\n").unwrap(); }
    let mut store = Store::open(f.root.join("steps.sqlite3"), proven_order(&f), crate::DERIVATION_VERSION).unwrap();
    let paths: Vec<_> = ["a.md", "b.md"].map(|p| DocumentPath::new(p).unwrap()).into();
    store.defer_link_health().unwrap(); store.begin_steps().unwrap();
    store.begin_request().apply_increment(IncrementProvenance::Derived, paths.iter().cloned().map(|path| Change::Named { path }), &[], &ContentModel::none()).unwrap();
    store.finish_step(norn_store::Step::Names).unwrap();
    let turned = f.vault().join("a.md");
    fs::remove_file(&turned).unwrap();
    fs::create_dir(&turned).unwrap();
    fs::write(turned.join("inside.md"), "# Inside\n").unwrap();
    fs::set_permissions(&turned, fs::Permissions::from_mode(0o000)).unwrap();
    let (ops, _) = f.ops(4);
    let source = norn_fs::Vault::open(&f.vault(), &[]).unwrap();
    let root = f.vault();
    let mut account = Account::default();
    let mut group = store.step_group().unwrap();
    let mut pending = Pending::new(group.store(), 4, &root, &[], &mut account).unwrap();
    loading::steps::content_chunk(&ops, &mut pending, &source, &paths).unwrap();
    pending.flush().unwrap(); drop(pending); group.commit().unwrap();
    fs::set_permissions(&turned, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(store.begin_request().content_page(4).unwrap().is_empty(), "the content step pages a name it can never settle");
    assert!(store.begin_request().stored_facts(&paths[0]).unwrap().is_none(), "the withdrawn name still stands");
    assert!(store.begin_request().stored_tombstone(&paths[0]).unwrap().is_none());
    assert!(store.begin_request().unreadable_directory_stands(&paths[0]).unwrap());
}

#[test]
fn a_findings_only_flush_outside_a_group_takes_the_increment_path() {
    let f = Fixture::watcherless("findings-only-construction");
    let mut store = Store::open(f.root.join("steps.sqlite3"), proven_order(&f), crate::DERIVATION_VERSION).unwrap();
    store.defer_link_health().unwrap();
    store.begin_steps().unwrap();
    store.finish_step(norn_store::Step::Names).unwrap();
    let root = f.vault();
    let mut account = Account::default();
    let mut pending = Pending::new(&mut store, 2, &root, &[], &mut account).unwrap();
    pending.file(plan_quarantine(Path::new("..md"), document_path(Path::new("..md")).unwrap_err()), None);
    pending.flush().unwrap();
}

#[test]
fn ordinary_document_steps_keep_the_configured_findings_flush_bound() {
    for step in [norn_store::Step::Content, norn_store::Step::Structure] {
        let f = Fixture::watcherless("ordinary-findings-bound");
        let mut store = Store::open(f.root.join("steps.sqlite3"), proven_order(&f), crate::DERIVATION_VERSION).unwrap();
        store.defer_link_health().unwrap();
        store.begin_steps().unwrap();
        store.finish_step(norn_store::Step::Names).unwrap();
        if step == norn_store::Step::Structure {
            store.finish_step(norn_store::Step::Content).unwrap();
        }
        let root = f.vault();
        let mut account = Account::default();
        let mut pending = Pending::new(&mut store, 2, &root, &[], &mut account).unwrap();
        for spelling in ["..md", "...md"] {
            pending.file(plan_quarantine(Path::new(spelling), document_path(Path::new(spelling)).unwrap_err()), None);
        }
        assert!(pending.is_full(), "{step:?} must flush at the configured findings bound");
    }
}
