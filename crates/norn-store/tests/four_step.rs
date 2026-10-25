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
        four_step::structure(
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
            &[0],
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
        four_step::structure(
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
    assert!(four_step::structure(&mut store, &[], &ContentModel::none(), false).is_err());
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
        &[3],
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
            &[3]
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
        &[0],
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
        &[3, 0],
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
    four_step::structure(&mut store, &[other], &ContentModel::none(), true).unwrap();
    four_step::health(&mut store, 1, &ContentModel::none()).unwrap();
    assert!(four_step::forget(&mut store, &source.path).is_err());
    four_step::finish(&mut store).unwrap();
    store.verify_integrity().unwrap();
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
        &[0],
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
    let counters = four_step::structure(
        &mut store,
        std::slice::from_ref(&facts),
        &ContentModel::none(),
        true,
    )
    .unwrap();
    assert_eq!(counters.rows_rewritten, 1);
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
    assert!(four_step::structure(&mut store, &[facts], &ContentModel::none(), true).is_err());
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
        &[0],
    )
    .unwrap();
    assert!(four_step::finish(&mut store).is_err());
    four_step::structure(
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
fn complete_four_steps_equal_ordinary_derivation_including_late_target_anchors() {
    use norn_store::{
        AnchorReadings, BlockFact, Change, DocumentFacts, HeadingFact, IncrementProvenance,
        LinkAnchor, LinkFact, LinkFamily, Span,
    };
    use norn_testkit::equivalence::StoreProjection;
    for no_body in [false, true] {
        let scratch = scratch();
        let mut lab = open(&scratch.join("lab.sqlite3"));
        let mut ordinary = open(&scratch.join("ordinary.sqlite3"));
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
                &[0],
            )
            .unwrap();
            assert_eq!(counters.rows_rewritten, 1);
        }
        for facts in &docs {
            let counters = four_step::structure(
                &mut lab,
                std::slice::from_ref(facts),
                &ContentModel::none(),
                no_body,
            )
            .unwrap();
            assert_eq!(counters.rows_rewritten, u64::from(no_body));
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
        &[0],
    )
    .unwrap();
    four_step::structure(
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
        &[0],
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
