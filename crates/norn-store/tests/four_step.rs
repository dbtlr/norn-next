#![cfg(feature = "induced-failure")]

use norn_store::{
    ContentModel, DerivationVersion, DocumentPath, OpenOutcome, Store, StoredPathOrder, four_step,
};

fn scratch() -> norn_testkit::scratch::Scratch {
    norn_testkit::scratch::Scratch::under(
        &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.scratch"),
        "four-step",
    )
    .unwrap()
}
fn open(path: &std::path::Path) -> Store {
    Store::open(path, StoredPathOrder::Sensitive, DerivationVersion::new(1)).unwrap()
}

fn complete_structure(
    store: &mut Store,
    documents: &[norn_store::DocumentFacts],
    model: &ContentModel,
    no_body: bool,
) -> Result<four_step::StepCounters, norn_store::StoreError> {
    let pending = documents
        .iter()
        .filter_map(|facts| {
            four_step::pending_at(store, four_step::Step::Structure, &facts.path).unwrap()
        })
        .collect::<Vec<_>>();
    four_step::structure(store, documents, &pending, &[], model, no_body)
}

#[test]
fn content_retains_body_column_and_frontmatter_tag_names_for_structure() {
    let scratch = scratch();
    let mut store = open(&scratch.join("store.sqlite3"));
    let mut facts = mixed_facts();
    facts.tags[0].name = "front".into();
    facts.tags[1].name = "body".into();
    four_step::begin(&mut store).unwrap();
    four_step::names(
        &mut store,
        std::slice::from_ref(&facts.path),
        &[],
        &ContentModel::none(),
    )
    .unwrap();
    four_step::content(
        &mut store,
        std::slice::from_ref(&facts),
        &[],
        &ContentModel::none(),
        false,
        &[(0, 3)],
    )
    .unwrap();
    let pending = four_step::pending_at(&store, four_step::Step::Structure, &facts.path)
        .unwrap()
        .unwrap();
    assert_eq!(pending.body_line_offset, 0);
    assert_eq!(pending.body_column_offset, 3);
    assert_eq!(pending.frontmatter_tag_names, vec!["front"]);
}

#[test]
fn each_name_or_content_chunk_shares_one_write_generation() {
    let scratch = scratch();
    let mut store = open(&scratch.join("store.sqlite3"));
    let documents = [
        norn_store::DocumentFacts::new(DocumentPath::new("a.md").unwrap(), "a", "a", 1),
        norn_store::DocumentFacts::new(DocumentPath::new("z.md").unwrap(), "z", "z", 1),
    ];
    four_step::begin(&mut store).unwrap();
    let before = store.begin_request().write_generation().unwrap();
    four_step::names(
        &mut store,
        &documents
            .iter()
            .map(|facts| facts.path.clone())
            .collect::<Vec<_>>(),
        &[DocumentPath::new("image.png").unwrap()],
        &ContentModel::none(),
    )
    .unwrap();
    assert_eq!(
        store.begin_request().write_generation().unwrap(),
        before + 1
    );
    for facts in &documents {
        assert_eq!(
            store
                .begin_request()
                .stored_document(&facts.path)
                .unwrap()
                .unwrap()
                .generation,
            before + 1
        );
    }
    four_step::content(
        &mut store,
        &documents,
        &[],
        &ContentModel::none(),
        false,
        &[(0, 0), (0, 0)],
    )
    .unwrap();
    assert_eq!(
        store.begin_request().write_generation().unwrap(),
        before + 2
    );
    for facts in &documents {
        assert_eq!(
            store
                .begin_request()
                .stored_document(&facts.path)
                .unwrap()
                .unwrap()
                .generation,
            before + 2
        );
    }
}

#[test]
fn requested_structure_can_precede_remaining_content_but_health_waits_for_all_structure() {
    let scratch = scratch();
    let mut store = open(&scratch.join("store.sqlite3"));
    let a = norn_store::DocumentFacts::new(DocumentPath::new("a.md").unwrap(), "a", "a", 1);
    let z = norn_store::DocumentFacts::new(DocumentPath::new("z.md").unwrap(), "z", "z", 1);
    four_step::begin(&mut store).unwrap();
    four_step::names(
        &mut store,
        &[a.path.clone(), z.path.clone()],
        &[],
        &ContentModel::none(),
    )
    .unwrap();
    assert!(
        complete_structure(
            &mut store,
            std::slice::from_ref(&z),
            &ContentModel::none(),
            false
        )
        .is_err()
    );
    for facts in [&z, &a] {
        four_step::content(
            &mut store,
            std::slice::from_ref(facts),
            &[],
            &ContentModel::none(),
            false,
            &[(0, 0)],
        )
        .unwrap();
        assert!(
            four_step::names(
                &mut store,
                &[DocumentPath::new("zz.md").unwrap()],
                &[],
                &ContentModel::none()
            )
            .is_err()
        );
        assert!(four_step::health(&mut store, 1, &ContentModel::none()).is_err());
        complete_structure(
            &mut store,
            std::slice::from_ref(facts),
            &ContentModel::none(),
            false,
        )
        .unwrap();
    }
    assert_eq!(
        four_step::health(&mut store, 1, &ContentModel::none())
            .unwrap()
            .commits,
        1
    );
    assert!(four_step::content(&mut store, &[], &[], &ContentModel::none(), false, &[]).is_err());
    assert!(complete_structure(&mut store, &[], &ContentModel::none(), false).is_err());
    four_step::health(&mut store, 1, &ContentModel::none()).unwrap();
    four_step::finish(&mut store).unwrap();
}

#[test]
fn names_and_pending_follow_the_store_path_order_across_chunks() {
    for (order, spellings) in [
        (StoredPathOrder::Sensitive, ["A.md", "B.md", "a.md"]),
        (
            StoredPathOrder::AsciiCaseInsensitive,
            ["A.md", "a.md", "B.md"],
        ),
    ] {
        let scratch = scratch();
        let mut store = Store::open(
            scratch.join("store.sqlite3"),
            order,
            DerivationVersion::new(1),
        )
        .unwrap();
        four_step::begin(&mut store).unwrap();
        let paths: Vec<_> = spellings
            .iter()
            .map(|path| DocumentPath::new(path).unwrap())
            .collect();
        for path in &paths {
            let file = DocumentPath::new(&path.as_str().replace(".md", ".png")).unwrap();
            let counters = four_step::names(
                &mut store,
                std::slice::from_ref(path),
                &[file],
                &ContentModel::none(),
            )
            .unwrap();
            assert_eq!(counters.rows_inserted, 2);
        }
        assert!(
            four_step::names(
                &mut store,
                std::slice::from_ref(&paths[1]),
                &[],
                &ContentModel::none()
            )
            .is_err()
        );
        assert!(
            four_step::names(
                &mut store,
                &[],
                &[DocumentPath::new(&paths[1].as_str().replace(".md", ".png")).unwrap()],
                &ContentModel::none()
            )
            .is_err()
        );
        let pending = four_step::pending(&store, four_step::Step::Content, 3).unwrap();
        assert_eq!(
            pending.iter().map(|row| &row.path).collect::<Vec<_>>(),
            paths.iter().collect::<Vec<_>>()
        );
        assert_eq!(
            four_step::pending_at(&store, four_step::Step::Content, &paths[1]).unwrap(),
            Some(pending[1].clone())
        );
        assert!(
            four_step::pending_at(
                &store,
                four_step::Step::Content,
                &DocumentPath::new("absent.md").unwrap()
            )
            .unwrap()
            .is_none()
        );
    }
}

#[test]
fn incomplete_names_are_never_adopted_after_a_restart() {
    let scratch = scratch();
    let path = scratch.join("store.sqlite3");
    let mut store = open(&path);
    assert_eq!(four_step::begin(&mut store).unwrap().commits, 1);
    let document = DocumentPath::new("a.md").unwrap();
    let counters = four_step::names(
        &mut store,
        &[document],
        &[DocumentPath::new("image.png").unwrap()],
        &ContentModel::none(),
    )
    .unwrap();
    assert_eq!(counters.rows_inserted, 2);
    assert_eq!(counters.commits, 1);
    drop(store);
    let mut store = open(&path);
    assert!(matches!(
        store.open_outcome(),
        OpenOutcome::RebuiltFromZero(_)
    ));
    assert!(
        store
            .begin_request()
            .stored_document(&DocumentPath::new("a.md").unwrap())
            .unwrap()
            .is_none()
    );
    assert!(
        four_step::begin(&mut store).is_err(),
        "only Created admits the experiment"
    );
}

#[test]
fn content_updates_once_and_carries_body_position_to_structure() {
    let scratch = scratch();
    let mut store = open(&scratch.join("store.sqlite3"));
    four_step::begin(&mut store).unwrap();
    let path = DocumentPath::new("a.md").unwrap();
    four_step::names(
        &mut store,
        std::slice::from_ref(&path),
        &[],
        &ContentModel::none(),
    )
    .unwrap();
    let mut facts = norn_store::DocumentFacts::new(path.clone(), "hash", "# Body\n", 17);
    facts.body_offset = 10;
    let counters = four_step::content(
        &mut store,
        std::slice::from_ref(&facts),
        &[],
        &ContentModel::none(),
        false,
        &[(3, 0)],
    )
    .unwrap();
    assert_eq!(counters.rows_rewritten, 1);
    assert_eq!(counters.links_judged, 0);
    let pending = four_step::pending(&store, four_step::Step::Structure, 1).unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].path, path);
    assert_eq!(pending[0].content_hash, "hash");
    assert_eq!(pending[0].body, "# Body\n");
    assert_eq!(pending[0].body_offset, 10);
    assert_eq!(pending[0].body_line_offset, 3);
    assert!(
        four_step::pending(&store, four_step::Step::Content, 1)
            .unwrap()
            .is_empty()
    );
    assert!(
        four_step::content(
            &mut store,
            &[facts],
            &[],
            &ContentModel::none(),
            false,
            &[(3, 0)]
        )
        .is_err()
    );
}

fn mixed_facts() -> norn_store::DocumentFacts {
    use norn_store::{DocumentFacts, FrontmatterValue, LinkFact, LinkFamily, TagFact, TagSource};
    let mut facts = DocumentFacts::new(DocumentPath::new("a.md").unwrap(), "hash", "# Body\n", 7)
        .with_frontmatter(
            Some(FrontmatterValue::Map(vec![(
                "status".into(),
                FrontmatterValue::String("open".into()),
            )])),
            &ContentModel::none(),
        );
    for field in [Some("related".into()), None] {
        facts.links.push(LinkFact {
            field,
            family: LinkFamily::Wikilink,
            embed: false,
            protocol: None,
            target: "missing".into(),
            title: None,
            anchor: None,
            span: None,
        });
    }
    for source in [TagSource::Frontmatter, TagSource::Body] {
        facts.tags.push(TagFact {
            name: "tag".into(),
            source,
            span: None,
        });
    }
    facts
}

fn missing_field(path: &DocumentPath, field: usize) -> norn_store::DerivedFinding<'static> {
    norn_store::DerivedFinding {
        facts: norn_store::FindingFacts {
            kind: norn_wire::FindingKind::RequiredMissing,
            severity: norn_wire::Severity::Error,
            path: path.clone(),
            class_keys: Default::default(),
            path_keys: Default::default(),
            target: Some(format!("field-{field}")),
            span: None,
            ordinal: None,
            candidates: vec![],
            candidates_total: 0,
            message: "required field is missing".into(),
            detail: None,
            rules: Default::default(),
            value: None,
        },
        replaces: None,
        bytes_hash: None,
    }
}

#[test]
fn a_single_content_document_can_have_more_schema_findings_than_the_document_chunk_bound() {
    let scratch = scratch();
    let mut store = open(&scratch.join("store.sqlite3"));
    let facts = mixed_facts();
    let findings: Vec<_> = (0..=norn_store::MAX_PAGE)
        .map(|field| missing_field(&facts.path, field))
        .collect();
    four_step::begin(&mut store).unwrap();
    four_step::names(
        &mut store,
        std::slice::from_ref(&facts.path),
        &[],
        &ContentModel::none(),
    )
    .unwrap();
    let counters = four_step::content(
        &mut store,
        &[facts],
        &findings,
        &ContentModel::none(),
        false,
        &[(0, 0)],
    )
    .unwrap();
    assert_eq!(counters.findings_written, findings.len() as u64);
    assert_eq!(counters.commits, 1);
    assert_eq!(
        store
            .begin_request()
            .stored_findings(&findings[0].facts.path)
            .unwrap()
            .len(),
        findings.len()
    );
}

#[test]
fn forgetting_a_vanished_name_or_changed_no_body_source_discards_only_its_own_facts_and_findings() {
    let scratch = scratch();
    let mut store = open(&scratch.join("store.sqlite3"));
    let source = mixed_facts();
    let vanished = DocumentPath::new("vanished.md").unwrap();
    let other = norn_store::DocumentFacts::new(DocumentPath::new("z.md").unwrap(), "z", "z", 1);
    assert!(four_step::forget(&mut store, &vanished).is_err());
    four_step::begin(&mut store).unwrap();
    four_step::names(
        &mut store,
        &[source.path.clone(), vanished.clone(), other.path.clone()],
        &[],
        &ContentModel::none(),
    )
    .unwrap();
    let forgotten = four_step::forget(&mut store, &vanished).unwrap();
    assert_eq!(forgotten.commits, 1);
    assert_eq!(forgotten.findings_discarded, 0);
    assert!(
        four_step::pending_at(&store, four_step::Step::Content, &vanished)
            .unwrap()
            .is_none()
    );
    assert!(
        store
            .begin_request()
            .stored_tombstone(&vanished)
            .unwrap()
            .is_none()
    );
    assert!(
        four_step::pending_at(&store, four_step::Step::Structure, &source.path)
            .unwrap()
            .is_none()
    );
    four_step::content(
        &mut store,
        &[source.clone(), other.clone()],
        &[
            missing_field(&source.path, 0),
            missing_field(&source.path, 1),
            missing_field(&other.path, 0),
        ],
        &ContentModel::none(),
        true,
        &[(3, 0), (0, 0)],
    )
    .unwrap();
    let row = four_step::pending_at(&store, four_step::Step::Structure, &source.path)
        .unwrap()
        .unwrap();
    assert_eq!(
        row,
        four_step::pending(&store, four_step::Step::Structure, 2).unwrap()[0]
    );
    assert_eq!(
        (
            row.body.as_str(),
            row.content_hash.as_str(),
            row.body_line_offset
        ),
        ("", "hash", 3)
    );
    assert_eq!(
        (
            row.frontmatter_links,
            row.frontmatter_tags,
            row.frontmatter_fields
        ),
        (1, 1, 2)
    );
    let facts = store
        .begin_request()
        .stored_facts(&source.path)
        .unwrap()
        .unwrap();
    assert_eq!(
        (
            facts.links.len(),
            facts.tags.len(),
            facts.fields.rows().len()
        ),
        (1, 1, 2)
    );
    let forgotten = four_step::forget(&mut store, &source.path).unwrap();
    assert_eq!(forgotten.commits, 1);
    assert_eq!(forgotten.findings_discarded, 2);
    assert!(
        store
            .begin_request()
            .stored_facts(&source.path)
            .unwrap()
            .is_none()
    );
    assert!(
        store
            .begin_request()
            .stored_findings(&source.path)
            .unwrap()
            .is_empty()
    );
    assert!(
        store
            .begin_request()
            .stored_tombstone(&source.path)
            .unwrap()
            .is_none()
    );
    assert_eq!(
        store
            .begin_request()
            .stored_findings(&other.path)
            .unwrap()
            .len(),
        1
    );
    complete_structure(&mut store, &[other], &ContentModel::none(), true).unwrap();
    four_step::health(&mut store, 1, &ContentModel::none()).unwrap();
    assert!(four_step::forget(&mut store, &source.path).is_err());
    four_step::finish(&mut store).unwrap();
    store.verify_integrity().unwrap();
}

fn refused_spelling(path: &DocumentPath) -> norn_store::FindingFacts {
    let mut facts = missing_field(path, 0).facts;
    facts.kind = norn_wire::FindingKind::PathNamesNoDocument;
    facts.severity = facts.kind.default_severity();
    facts.target = None;
    facts.message = "path names no document".into();
    facts.detail = Some("refused spelling".into());
    facts
}

#[test]
fn refused_spellings_are_withheld_after_content_and_consumed_in_bounded_structure_chunks() {
    let scratch = scratch();
    let mut store = open(&scratch.join("store.sqlite3"));
    let model = ContentModel::none();
    let valid = norn_store::DocumentFacts::new(DocumentPath::new("a.md").unwrap(), "a", "a", 1);
    let bad = DocumentPath::new("b.md").unwrap();
    let refused = [
        valid.path.clone(),
        bad.clone(),
        DocumentPath::new("c.md").unwrap(),
        DocumentPath::new("d.md").unwrap(),
    ]
    .iter()
    .map(refused_spelling)
    .collect::<Vec<_>>();
    four_step::begin(&mut store).unwrap();
    let names = four_step::names_with_refused(
        &mut store,
        &[valid.path.clone(), bad.clone()],
        &[],
        &refused,
        &model,
    )
    .unwrap();
    assert_eq!(
        (names.commits, names.rows_inserted, names.findings_written),
        (1, 2, 0)
    );
    assert!(four_step::has_refused(&store).unwrap());
    assert!(
        four_step::structure_with_refused(&mut store, &[], &[], &[], &model, false, 1).is_err()
    );
    let mut quarantine = norn_store::DerivedFinding {
        facts: refused_spelling(&bad),
        replaces: None,
        bytes_hash: Some("bad".into()),
    };
    quarantine.facts.kind = norn_wire::FindingKind::BodyBytesNotUtf8;
    four_step::content(
        &mut store,
        std::slice::from_ref(&valid),
        &[quarantine],
        &model,
        false,
        &[(0, 0)],
    )
    .unwrap();
    let pending = four_step::pending(&store, four_step::Step::Structure, 1).unwrap();
    let first =
        four_step::structure_with_refused(&mut store, &[valid], &pending, &[], &model, false, 1)
            .unwrap();
    assert_eq!(
        (first.commits, first.findings_written),
        (1, 0),
        "a decoded row withholds its queued spelling finding"
    );
    assert!(four_step::has_refused(&store).unwrap());
    assert!(
        four_step::health(&mut store, 1, &model).is_err(),
        "final health cannot seal an undrained queue"
    );
    let second =
        four_step::structure_with_refused(&mut store, &[], &[], &[], &model, false, 2).unwrap();
    assert_eq!(
        (second.commits, second.findings_written, second.rows_deleted),
        (1, 2, 0)
    );
    assert!(four_step::has_refused(&store).unwrap());
    let third =
        four_step::structure_with_refused(&mut store, &[], &[], &[], &model, false, 2).unwrap();
    assert_eq!((third.commits, third.findings_written), (1, 1));
    assert!(!four_step::has_refused(&store).unwrap());
    assert!(
        store
            .begin_request()
            .stored_findings(&refused[0].path)
            .unwrap()
            .is_empty()
    );
    for finding in &refused[1..] {
        let stored = store
            .begin_request()
            .stored_findings(&finding.path)
            .unwrap();
        let actual = stored
            .iter()
            .find(|stored| stored.kind == finding.kind.as_str())
            .unwrap();
        assert_eq!(actual.message, finding.message);
        assert_eq!(actual.detail, finding.detail);
        assert_eq!(actual.severity, finding.severity.as_str());
    }
    four_step::health(&mut store, 1, &model).unwrap();
    four_step::finish(&mut store).unwrap();
    store.verify_integrity().unwrap();
}

#[test]
fn structure_refuses_another_stores_pending_source_and_edited_public_metadata() {
    let scratch = scratch();
    let mut a = open(&scratch.join("a.sqlite3"));
    let mut b = open(&scratch.join("b.sqlite3"));
    let path = DocumentPath::new("a.md").unwrap();
    let model = ContentModel::none();
    let facts_a = norn_store::DocumentFacts::new(path.clone(), "hash-a", "aaa", 3);
    let facts_b = norn_store::DocumentFacts::new(path.clone(), "hash-b", "bbb", 3);
    for (store, facts) in [(&mut a, &facts_a), (&mut b, &facts_b)] {
        four_step::begin(store).unwrap();
        four_step::names(store, std::slice::from_ref(&path), &[], &model).unwrap();
        four_step::content(
            store,
            std::slice::from_ref(facts),
            &[],
            &model,
            true,
            &[(0, 0)],
        )
        .unwrap();
    }
    let pending_a = four_step::pending_at(&a, four_step::Step::Structure, &path)
        .unwrap()
        .unwrap();
    let pending_b = four_step::pending_at(&b, four_step::Step::Structure, &path)
        .unwrap()
        .unwrap();
    assert!(
        four_step::structure(
            &mut a,
            std::slice::from_ref(&facts_b),
            &[pending_b],
            &[],
            &model,
            true
        )
        .is_err()
    );
    let mut forged = pending_a.clone();
    forged.content_hash = "hash-b".into();
    assert!(
        four_step::structure(
            &mut a,
            std::slice::from_ref(&facts_b),
            &[forged],
            &[],
            &model,
            true
        )
        .is_err()
    );
    let mut forged = pending_a.clone();
    forged.frontmatter_links = 99;
    assert!(
        four_step::structure(
            &mut a,
            std::slice::from_ref(&facts_a),
            &[forged],
            &[],
            &model,
            true
        )
        .is_err()
    );
    let unchanged = a.begin_request().stored_facts(&path).unwrap().unwrap();
    assert_eq!(unchanged.document.content_hash, "hash-a");
    assert_eq!(unchanged.body, "");
    four_step::structure(&mut a, &[facts_a], &[pending_a], &[], &model, true).unwrap();
    assert_eq!(
        a.begin_request().stored_facts(&path).unwrap().unwrap().body,
        "aaa"
    );
}

#[test]
fn structure_commits_host_findings_atomically_and_flushes_without_touching_pending_rows() {
    let scratch = scratch();
    let mut store = open(&scratch.join("store.sqlite3"));
    let source = mixed_facts();
    let other = norn_store::DocumentFacts::new(DocumentPath::new("z.md").unwrap(), "z", "z", 1);
    let documents = [source.clone(), other.clone()];
    let model = ContentModel::none();
    four_step::begin(&mut store).unwrap();
    four_step::names(
        &mut store,
        &[source.path.clone(), other.path.clone()],
        &[],
        &model,
    )
    .unwrap();
    four_step::content(&mut store, &documents, &[], &model, true, &[(0, 0), (0, 0)]).unwrap();
    let pending = four_step::pending(&store, four_step::Step::Structure, 2).unwrap();
    let finding = missing_field(&source.path, 0);
    let mut changed = other.clone();
    changed.content_hash = "changed".into();
    assert!(
        four_step::structure(
            &mut store,
            &[source.clone(), changed],
            &pending,
            std::slice::from_ref(&finding),
            &model,
            true
        )
        .is_err()
    );
    let unchanged = store
        .begin_request()
        .stored_facts(&source.path)
        .unwrap()
        .unwrap();
    assert_eq!(unchanged.body, "");
    assert_eq!(unchanged.links.len(), 1);
    assert!(
        store
            .begin_request()
            .stored_findings(&source.path)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        four_step::pending(&store, four_step::Step::Structure, 2)
            .unwrap()
            .len(),
        2
    );

    let counters = four_step::structure(
        &mut store,
        std::slice::from_ref(&source),
        &pending[..1],
        std::slice::from_ref(&finding),
        &model,
        true,
    )
    .unwrap();
    assert_eq!(
        (
            counters.commits,
            counters.rows_rewritten,
            counters.body_rows_rewritten,
            counters.findings_written
        ),
        (1, 0, 1, 1)
    );
    let stored = store
        .begin_request()
        .stored_facts(&source.path)
        .unwrap()
        .unwrap();
    assert_eq!(stored.body, "# Body\n");
    assert_eq!(stored.links.len(), 2);
    assert_eq!(
        store
            .begin_request()
            .stored_findings(&source.path)
            .unwrap()
            .len(),
        1
    );

    let refused = DocumentPath::new("refused.md").unwrap();
    let mut deferred = missing_field(&refused, 0);
    deferred.facts.kind = norn_wire::FindingKind::PathBytesNotUtf8;
    let flushed = four_step::structure(&mut store, &[], &[], &[deferred], &model, true).unwrap();
    assert_eq!(
        (
            flushed.commits,
            flushed.rows_rewritten,
            flushed.body_rows_rewritten,
            flushed.rows_deleted,
            flushed.findings_written
        ),
        (1, 0, 0, 0, 1)
    );
    assert_eq!(
        store
            .begin_request()
            .stored_findings(&refused)
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        four_step::pending(&store, four_step::Step::Structure, 2).unwrap(),
        pending[1..]
    );
    assert!(
        store
            .begin_request()
            .stored_document(&other.path)
            .unwrap()
            .is_some()
    );
}

#[test]
fn structure_appends_body_facts_without_replacing_frontmatter_and_fills_omitted_body() {
    let scratch = scratch();
    let mut store = open(&scratch.join("store.sqlite3"));
    let facts = mixed_facts();
    four_step::begin(&mut store).unwrap();
    four_step::names(
        &mut store,
        std::slice::from_ref(&facts.path),
        &[],
        &ContentModel::none(),
    )
    .unwrap();
    four_step::content(
        &mut store,
        std::slice::from_ref(&facts),
        &[],
        &ContentModel::none(),
        true,
        &[(0, 0)],
    )
    .unwrap();
    let content = store
        .begin_request()
        .stored_facts(&facts.path)
        .unwrap()
        .unwrap();
    assert_eq!(content.body, "");
    assert_eq!(content.links.len(), 1);
    assert_eq!(content.tags.len(), 1);
    let counters = complete_structure(
        &mut store,
        std::slice::from_ref(&facts),
        &ContentModel::none(),
        true,
    )
    .unwrap();
    assert_eq!(counters.rows_rewritten, 0);
    assert_eq!(counters.body_rows_rewritten, 1);
    assert_eq!(counters.links_judged, 0);
    let structure = store
        .begin_request()
        .stored_facts(&facts.path)
        .unwrap()
        .unwrap();
    assert_eq!(structure.body, "# Body\n");
    assert_eq!(structure.links.len(), 2);
    assert_eq!(structure.links[0].fact.field.as_deref(), Some("related"));
    assert_eq!(structure.links[1].fact.field, None);
    assert_eq!(structure.tags.len(), 2);
    assert_eq!(structure.fields, content.fields);
    assert_eq!(structure.document.frontmatter, content.document.frontmatter);
    assert!(
        store
            .begin_request()
            .stored_findings(&facts.path)
            .unwrap()
            .is_empty()
    );
    assert!(complete_structure(&mut store, &[facts], &ContentModel::none(), true).is_err());
}

#[test]
fn final_health_judges_eligible_links_once_across_link_pages_and_seals_for_reuse() {
    let scratch = scratch();
    let path = scratch.join("store.sqlite3");
    let mut store = open(&path);
    let mut facts = mixed_facts();
    facts.links.clear();
    for external in [true, false] {
        for _ in 0..300 {
            facts.links.push(norn_store::LinkFact {
                field: None,
                family: norn_store::LinkFamily::Markdown,
                embed: false,
                protocol: external.then(|| "https".into()),
                target: if external { "example.com" } else { "missing" }.into(),
                title: None,
                anchor: None,
                span: None,
            });
        }
    }
    four_step::begin(&mut store).unwrap();
    four_step::names(
        &mut store,
        std::slice::from_ref(&facts.path),
        &[],
        &ContentModel::none(),
    )
    .unwrap();
    four_step::content(
        &mut store,
        std::slice::from_ref(&facts),
        &[],
        &ContentModel::none(),
        false,
        &[(0, 0)],
    )
    .unwrap();
    assert!(four_step::finish(&mut store).is_err());
    complete_structure(
        &mut store,
        std::slice::from_ref(&facts),
        &ContentModel::none(),
        false,
    )
    .unwrap();
    assert!(
        store
            .begin_request()
            .stored_findings(&facts.path)
            .unwrap()
            .is_empty()
    );
    let counters = four_step::health(&mut store, 1, &ContentModel::none()).unwrap();
    assert_eq!(counters.links_judged, 300);
    assert_eq!(counters.findings_written, 300);
    assert_eq!(counters.commits, 1);
    let again = four_step::health(&mut store, 1, &ContentModel::none()).unwrap();
    assert_eq!(again.links_judged, 0);
    assert_eq!(again.commits, 0);
    assert_eq!(
        store
            .begin_request()
            .stored_findings(&facts.path)
            .unwrap()
            .len(),
        300
    );
    assert_eq!(four_step::finish(&mut store).unwrap().commits, 1);
    store.verify_integrity().unwrap();
    drop(store);
    let store = open(&path);
    assert!(matches!(store.open_outcome(), OpenOutcome::Reused));
}

#[test]
fn provisional_quarantine_has_the_same_semantics_as_an_ordinary_build_from_zero() {
    use norn_store::{DerivedFinding, DiscardScope, FindingFacts, IncrementProvenance};
    use norn_wire::{FindingKind, Severity};
    let scratch = scratch();
    let mut store = open(&scratch.join("lab.sqlite3"));
    let mut ordinary = open(&scratch.join("ordinary.sqlite3"));
    let path = DocumentPath::new("bad.md").unwrap();
    let finding = DerivedFinding {
        facts: FindingFacts {
            kind: FindingKind::BodyBytesNotUtf8,
            severity: Severity::Error,
            path: path.clone(),
            class_keys: Default::default(),
            path_keys: Default::default(),
            target: None,
            span: None,
            ordinal: None,
            candidates: vec![],
            candidates_total: 0,
            message: "invalid UTF-8".into(),
            detail: None,
            rules: Default::default(),
            value: None,
        },
        replaces: Some(DiscardScope::Kinds(&[FindingKind::BodyBytesNotUtf8])),
        bytes_hash: Some("bad-hash".into()),
    };
    ordinary
        .begin_request()
        .apply_increment(
            IncrementProvenance::Derived,
            [],
            std::slice::from_ref(&finding),
            &ContentModel::none(),
        )
        .unwrap();
    four_step::begin(&mut store).unwrap();
    four_step::names(
        &mut store,
        std::slice::from_ref(&path),
        &[],
        &ContentModel::none(),
    )
    .unwrap();
    let counters = four_step::content(
        &mut store,
        &[],
        &[finding],
        &ContentModel::none(),
        false,
        &[],
    )
    .unwrap();
    assert_eq!(counters.findings_written, 1);
    assert!(
        four_step::pending(&store, four_step::Step::Content, 8)
            .unwrap()
            .is_empty()
    );
    four_step::health(&mut store, 8, &ContentModel::none()).unwrap();
    four_step::finish(&mut store).unwrap();
    let request = store.begin_request();
    assert!(request.stored_document(&path).unwrap().is_none());
    assert!(request.stored_tombstone(&path).unwrap().is_none());
    assert_eq!(
        request.content_quarantine_hash(&path).unwrap().as_deref(),
        Some("bad-hash")
    );
    let ordinary = ordinary.begin_request();
    let mut actual = request.stored_findings(&path).unwrap();
    let mut expected = ordinary.stored_findings(&path).unwrap();
    actual[0].generation = 0;
    expected[0].generation = 0;
    assert_eq!(actual, expected);
    assert_eq!(
        request.content_quarantine_hash(&path).unwrap(),
        ordinary.content_quarantine_hash(&path).unwrap()
    );
    store.verify_integrity().unwrap();
}

#[test]
fn four_step_finding_ids_follow_ordinary_first_build_order() {
    use norn_store::{
        Change, DocumentFacts, IncrementProvenance, LinkAnchor, LinkFact, LinkFamily, Span,
    };

    fn sequence(store: &mut Store) -> Vec<(DocumentPath, Option<Span>, String)> {
        // This public reader follows finding IDs, rather than sorting findings
        // by their subjects and hiding a different insertion order.
        store
            .begin_request()
            .stored_findings_after(None, 8)
            .unwrap()
            .into_iter()
            .map(|(_, finding)| (finding.path, finding.span, finding.kind))
            .collect()
    }

    let scratch = scratch();
    let mut ordinary = Store::open(
        scratch.join("ordinary.sqlite3"),
        StoredPathOrder::AsciiCaseInsensitive,
        DerivationVersion::new(1),
    )
    .unwrap();
    // Folded path order differs from byte order, and both documents' links
    // fit in one health page, so the reader's byte order must be restored.
    let documents: Vec<_> = ["a.md", "B.md"]
        .into_iter()
        .map(|path| {
            let mut facts = DocumentFacts::new(DocumentPath::new(path).unwrap(), path, "", 0);
            for (target, anchor, line) in [
                ("missing", None, 1),
                (
                    path,
                    Some(LinkAnchor::Block {
                        id: "absent".into(),
                    }),
                    2,
                ),
            ] {
                facts.links.push(LinkFact {
                    field: None,
                    family: LinkFamily::Wikilink,
                    embed: false,
                    protocol: None,
                    target: target.into(),
                    title: None,
                    anchor,
                    span: Some(Span {
                        line,
                        column: 1,
                        byte_offset: line - 1,
                    }),
                });
            }
            facts
        })
        .collect();
    // Ordinary first construction with one document per changeset follows
    // the walk's stored path order, with link positions ordered within it.
    for facts in &documents {
        ordinary
            .begin_request()
            .apply_increment(
                IncrementProvenance::Derived,
                [Change::Upsert(facts.clone())],
                &[],
                &ContentModel::none(),
            )
            .unwrap();
    }
    let expected = sequence(&mut ordinary);
    assert_eq!(
        expected
            .iter()
            .map(|(path, span, kind)| (path.as_str(), span.unwrap().line, kind.as_str()))
            .collect::<Vec<_>>(),
        vec![
            ("a.md", 1, "link/broken"),
            ("a.md", 2, "link/missing-anchor"),
            ("B.md", 1, "link/broken"),
            ("B.md", 2, "link/missing-anchor"),
        ]
    );
    for bound in [1, 2] {
        let mut lab = Store::open(
            scratch.join(format!("lab-{bound}.sqlite3")),
            StoredPathOrder::AsciiCaseInsensitive,
            DerivationVersion::new(1),
        )
        .unwrap();
        four_step::begin(&mut lab).unwrap();
        let paths = documents
            .iter()
            .map(|facts| facts.path.clone())
            .collect::<Vec<_>>();
        four_step::names(&mut lab, &paths, &[], &ContentModel::none()).unwrap();
        four_step::content(
            &mut lab,
            &documents,
            &[],
            &ContentModel::none(),
            false,
            &[(0, 0), (0, 0)],
        )
        .unwrap();
        complete_structure(&mut lab, &documents, &ContentModel::none(), false).unwrap();
        while four_step::health(&mut lab, bound, &ContentModel::none())
            .unwrap()
            .commits
            != 0
        {}
        four_step::finish(&mut lab).unwrap();
        assert_eq!(
            sequence(&mut lab),
            expected,
            "health document bound {bound}"
        );
    }
}

#[test]
fn complete_four_steps_equal_ordinary_derivation_including_late_target_anchors() {
    use norn_store::{
        AnchorReadings, BlockFact, Change, DocumentFacts, HeadingFact, IncrementProvenance,
        LinkAnchor, LinkFact, LinkFamily, Span,
    };
    use norn_testkit::equivalence::StoreProjection;
    for order in [
        StoredPathOrder::Sensitive,
        StoredPathOrder::AsciiCaseInsensitive,
    ] {
        for no_body in [false, true] {
            let scratch = scratch();
            let mut lab = Store::open(
                scratch.join("lab.sqlite3"),
                order,
                DerivationVersion::new(1),
            )
            .unwrap();
            let mut ordinary = Store::open(
                scratch.join("ordinary.sqlite3"),
                order,
                DerivationVersion::new(1),
            )
            .unwrap();
            let mut source = mixed_facts();
            for anchor in [
                LinkAnchor::Heading {
                    written: "Present".into(),
                    readings: AnchorReadings {
                        text: "Present".into(),
                        marked: None,
                    },
                },
                LinkAnchor::Block { id: "id".into() },
                LinkAnchor::Heading {
                    written: "Absent".into(),
                    readings: AnchorReadings {
                        text: "Absent".into(),
                        marked: None,
                    },
                },
            ] {
                source.links.push(LinkFact {
                    field: None,
                    family: LinkFamily::Wikilink,
                    embed: false,
                    protocol: None,
                    target: "z".into(),
                    title: None,
                    anchor: Some(anchor),
                    span: None,
                });
            }
            let body = "# Present\nblock ^id\n";
            let mut target = DocumentFacts::new(
                DocumentPath::new("z.md").unwrap(),
                "z-hash",
                body,
                body.len() as u64,
            );
            target.headings.push(HeadingFact {
                level: 1,
                text: "Present".into(),
                reading: "Present".into(),
                slug: "present".into(),
                span: Span {
                    line: 1,
                    column: 1,
                    byte_offset: 0,
                },
                body_offset: 10,
                inside_container: false,
            });
            target.blocks.push(BlockFact {
                block_id: "id".into(),
                span: Some(Span {
                    line: 2,
                    column: 7,
                    byte_offset: 16,
                }),
            });
            let docs = [source, target];
            let file = DocumentPath::new("image.png").unwrap();
            ordinary
                .begin_request()
                .apply_increment(
                    IncrementProvenance::Derived,
                    docs.iter()
                        .cloned()
                        .map(Change::Upsert)
                        .chain([Change::FileBirth { path: file.clone() }]),
                    &[],
                    &ContentModel::none(),
                )
                .unwrap();
            four_step::begin(&mut lab).unwrap();
            four_step::names(
                &mut lab,
                &[docs[0].path.clone(), docs[1].path.clone()],
                &[file],
                &ContentModel::none(),
            )
            .unwrap();
            for facts in &docs {
                let counters = four_step::content(
                    &mut lab,
                    std::slice::from_ref(facts),
                    &[],
                    &ContentModel::none(),
                    no_body,
                    &[(0, 0)],
                )
                .unwrap();
                assert_eq!(counters.rows_rewritten, 1);
            }
            for facts in &docs {
                let counters = complete_structure(
                    &mut lab,
                    std::slice::from_ref(facts),
                    &ContentModel::none(),
                    no_body,
                )
                .unwrap();
                assert_eq!(counters.rows_rewritten, 0);
                assert_eq!(counters.body_rows_rewritten, u64::from(no_body));
            }
            let health = four_step::health(&mut lab, 1, &ContentModel::none()).unwrap();
            assert_eq!(health.links_judged, 5);
            assert_eq!(health.findings_written, 3);
            four_step::health(&mut lab, 1, &ContentModel::none()).unwrap();
            four_step::finish(&mut lab).unwrap();
            StoreProjection::read(&mut lab).unwrap().assert_equivalent(
                &StoreProjection::read(&mut ordinary).unwrap(),
                "four-step ordinary equality",
            );
        }
    }
}

#[test]
fn requested_document_can_complete_structure_before_other_content_and_be_forgotten_on_churn() {
    let scratch = scratch();
    let mut store = open(&scratch.join("lab.sqlite3"));
    let source = mixed_facts();
    let other = norn_store::DocumentFacts::new(DocumentPath::new("z.md").unwrap(), "z", "other", 5);
    four_step::begin(&mut store).unwrap();
    four_step::names(
        &mut store,
        &[source.path.clone(), other.path.clone()],
        &[],
        &ContentModel::none(),
    )
    .unwrap();
    assert!(
        four_step::pending_at(&store, four_step::Step::Content, &source.path)
            .unwrap()
            .is_some()
    );
    four_step::content(
        &mut store,
        std::slice::from_ref(&source),
        &[],
        &ContentModel::none(),
        false,
        &[(0, 0)],
    )
    .unwrap();
    complete_structure(
        &mut store,
        std::slice::from_ref(&source),
        &ContentModel::none(),
        false,
    )
    .unwrap();
    assert!(
        four_step::pending_at(&store, four_step::Step::Structure, &source.path)
            .unwrap()
            .is_none()
    );
    assert!(four_step::health(&mut store, 8, &ContentModel::none()).is_err());
    four_step::content(
        &mut store,
        &[other],
        &[],
        &ContentModel::none(),
        false,
        &[(0, 0)],
    )
    .unwrap();
    let forgotten = four_step::forget(&mut store, &source.path).unwrap();
    assert_eq!(forgotten.commits, 1);
    assert!(
        store
            .begin_request()
            .stored_document(&source.path)
            .unwrap()
            .is_none()
    );
    assert!(
        store
            .begin_request()
            .stored_tombstone(&source.path)
            .unwrap()
            .is_none()
    );
}
