//! A from-zero build defers every re-decision door until one complete judgment.

use norn_store::{Change, ContentModel, DiscardScope, IncrementProvenance, Store, StoreError};
use norn_wire::FindingKind;

use crate::common::{Scratch, path};
use crate::health::{derived, directory_mark};

fn write(store: &mut Store, documents: &[(&str, &str)], declared: &ContentModel) {
    store
        .begin_request()
        .apply_increment(
            IncrementProvenance::Derived,
            documents
                .iter()
                .map(|(at, body)| Change::Upsert(derived(at, body))),
            &[],
            declared,
        )
        .unwrap();
}

#[test]
fn deferral_over_a_finished_store_is_refused() {
    let scratch = Scratch::new("judgment-finished");
    let mut store = scratch.open();
    store.finish_construction().unwrap();
    assert!(matches!(
        store.defer_link_health(),
        Err(StoreError::Lifecycle { .. })
    ));
    assert!(!store.link_health_deferred());
}

#[test]
fn a_store_still_deferred_cannot_finish_construction() {
    let scratch = Scratch::new("judgment-unfinished");
    let mut store = scratch.open();
    store.defer_link_health().unwrap();
    store.defer_link_health().unwrap();
    assert!(matches!(
        store.finish_construction(),
        Err(StoreError::Lifecycle { .. })
    ));
    assert!(store.under_construction());
    assert!(store.link_health_deferred());
}

#[test]
fn a_deferred_store_redecides_nothing_at_any_door() {
    let scratch = Scratch::new("judgment-doors");
    let mut store = scratch.open();
    store.defer_link_health().unwrap();
    let mut request = store.begin_request();
    request
        .apply_increment(
            IncrementProvenance::Derived,
            [Change::Upsert(derived(
                "ref.md",
                "[gone](/locked/gone.md) [[missing]]\n",
            ))],
            &[],
            &ContentModel::none(),
        )
        .unwrap();
    request.record_finding(&directory_mark("locked")).unwrap();
    request
        .discard_findings_about(
            &path("locked"),
            DiscardScope::Kinds(&[FindingKind::UnreadableDirectory]),
        )
        .unwrap();
    request.record_finding(&directory_mark("locked")).unwrap();
    request.pin_vault_schema(b"schema", "new-schema").unwrap();
    assert!(request.stored_findings(&path("ref.md")).unwrap().is_empty());
    assert_eq!(request.finish().links_redecided(), 0);
}

fn judge(store: &mut Store) -> norn_store::DerivationCounters {
    let mut request = store.begin_request();
    request
        .judge_link_health_once(&ContentModel::none(), &mut || {})
        .unwrap();
    request.finish()
}

#[test]
fn the_judgment_equals_per_changeset_redecision() {
    use norn_testkit::equivalence::StoreProjection;
    let left = Scratch::new("judgment-equivalence-ordinary");
    let right = Scratch::new("judgment-equivalence-deferred");
    let mut ordinary = left.open();
    let mut deferred = right.open();
    deferred.defer_link_health().unwrap();
    for store in [&mut ordinary, &mut deferred] {
        write(
            store,
            &[(
                "ref.md",
                "[[absent]] [[hub]] [[target#missing]] [[broken#missing]] [[vault://target]] [web](https://example.org) [[photo.png]] [gone](/locked/gone.md) [held](/locked/held.md) [[held]]\n",
            )],
            &ContentModel::none(),
        );
        for at in 0..6 {
            write(
                store,
                &[(&format!("d{at}/hub.md"), "hub\n")],
                &ContentModel::none(),
            );
        }
        write(
            store,
            &[("target.md", "# Present\n"), ("locked/held.md", "# Held\n")],
            &ContentModel::none(),
        );
        store
            .begin_request()
            .apply_increment(
                IncrementProvenance::Derived,
                [
                    Change::FileBirth {
                        path: path("photo.png"),
                    },
                    Change::Unreadable {
                        path: path("broken.md"),
                    },
                    Change::BeneathUnlistable {
                        prefix: norn_store::DirectoryPrefix::new("locked").unwrap(),
                    },
                ],
                &[norn_store::DerivedFinding {
                    facts: directory_mark("locked"),
                    replaces: None,
                    bytes_hash: None,
                }],
                &ContentModel::none(),
            )
            .unwrap();
    }
    judge(&mut deferred);
    let expected = StoreProjection::read(&mut ordinary).unwrap();
    let actual = StoreProjection::read(&mut deferred).unwrap();
    assert_eq!(expected, actual);
    let findings = deferred
        .begin_request()
        .stored_findings(&path("ref.md"))
        .unwrap();
    assert_eq!(
        findings
            .iter()
            .map(|f| (f.ordinal.unwrap(), f.kind.as_str()))
            .collect::<Vec<_>>(),
        [
            (0, "link/broken"),
            (1, "link/ambiguous"),
            (2, "link/missing-anchor"),
            (4, "link/vault-scheme")
        ]
    );
    assert_eq!(findings[1].candidates_total, 6);
    assert_eq!(findings[1].candidates.len(), 5);
}

#[test]
fn the_judgment_judges_each_link_once_across_pages() {
    let scratch = Scratch::new("judgment-pages");
    let mut store = scratch.open();
    store.defer_link_health().unwrap();
    write(
        &mut store,
        &[("ref.md", &"[[absent]]\n".repeat(600))],
        &ContentModel::none(),
    );
    let mut checkpoints = 0;
    let mut request = store.begin_request();
    request
        .judge_link_health_once(&ContentModel::none(), &mut || checkpoints += 1)
        .unwrap();
    let counters = request.finish();
    assert_eq!(checkpoints, 3);
    assert_eq!(counters.links_judged(), 600);
    assert_eq!(counters.links_redecided(), 0);
    assert_eq!(counters.link_health_keys_resolved(), 1);
    assert_eq!(
        store
            .begin_request()
            .stored_facts(&path("ref.md"))
            .unwrap()
            .unwrap()
            .links
            .len(),
        600
    );
}

#[test]
fn a_judgment_run_again_files_the_same_findings() {
    let scratch = Scratch::new("judgment-repeat");
    let mut store = scratch.open();
    store.defer_link_health().unwrap();
    write(
        &mut store,
        &[("ref.md", "[[absent]]\n")],
        &ContentModel::none(),
    );
    judge(&mut store);
    let before = norn_testkit::equivalence::StoreProjection::read(&mut store).unwrap();
    assert_eq!(judge(&mut store).links_judged(), 1);
    assert_eq!(
        before,
        norn_testkit::equivalence::StoreProjection::read(&mut store).unwrap()
    );
}

#[test]
fn an_increment_after_the_judgment_redecides_its_set() {
    let scratch = Scratch::new("judgment-followed");
    let mut store = scratch.open();
    store.defer_link_health().unwrap();
    write(
        &mut store,
        &[("ref.md", "[[absent]]\n")],
        &ContentModel::none(),
    );
    judge(&mut store);
    assert!(!store.link_health_deferred());
    let mut request = store.begin_request();
    request
        .apply_increment(
            IncrementProvenance::Derived,
            [Change::Upsert(derived("absent.md", "now present\n"))],
            &[],
            &ContentModel::none(),
        )
        .unwrap();
    assert!(request.stored_findings(&path("ref.md")).unwrap().is_empty());
    assert_eq!(request.finish().links_redecided(), 1);
}

#[test]
fn a_judgment_refuses_an_unpinned_declaration_and_keeps_deferral() {
    let scratch = Scratch::new("judgment-declaration");
    let mut store = scratch.open();
    store.defer_link_health().unwrap();
    assert!(matches!(
        store
            .begin_request()
            .judge_link_health_once(&ContentModel::under("other"), &mut || {}),
        Err(StoreError::UnpinnedDeclaration { .. })
    ));
    assert!(store.link_health_deferred());
}

pub(crate) const JUDGMENT: [norn_store::ExplainedStatement<'static>; 2] = [
    norn_store::ExplainedStatement::LinkHealthEveryLink,
    norn_store::ExplainedStatement::LinkHealthJudgedDiscard,
];

#[test]
fn the_complete_judgment_seeks_each_page_and_its_findings_without_sorting() {
    use norn_store::ExplainedStatement;
    use norn_testkit::explain::{Access, PlanRow, QueryPlan};
    let scratch = Scratch::new("judgment-plans");
    let mut store = scratch.open();
    for statement in JUDGMENT {
        let emitted = store.begin_request().emitted_plan(statement).unwrap();
        let plan = QueryPlan::new(
            emitted.sql,
            emitted
                .steps
                .into_iter()
                .map(|s| PlanRow::new(s.id, s.parent, s.detail))
                .collect(),
        );
        plan.assert_no_full_scan();
        plan.assert_no_temp_btree();
        let seeks = match statement {
            ExplainedStatement::LinkHealthEveryLink => vec![
                ("dw", "documents_path", "(path>?)"),
                ("lw", "links_document_ordinal", "(document=? AND ordinal>?)"),
            ],
            ExplainedStatement::LinkHealthJudgedDiscard => vec![(
                "f",
                "findings_one_per_link",
                "(vault_schema_fingerprint=? AND path=? AND ordinal=?)",
            )],
            _ => unreachable!(),
        };
        for (alias, index, constraint) in seeks {
            assert!(
                plan.rows().iter().any(|row| row.searches() == Some(alias)
                    && row.access() == Some(Access::Index(index))
                    && row.constraint() == Some(constraint)),
                "{statement:?}: {:?}",
                plan.rows()
            );
        }
    }
}
