//! Immutable first-index sources use ordinary builders and real file ownership.
#![allow(clippy::disallowed_methods)]
use norn_store::{
    Change, ContentModel, DerivationVersion, DocumentFacts, DocumentPath, IncrementProvenance,
    ReadBudget, ReadBudgetPolicy, ReadNeed, Store, StoredPathOrder, loading::GetBuilder,
};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};

#[test]
fn a_rich_sealed_source_survives_retirement_and_refuses_ordinary_adoption() {
    let scratch = norn_testkit::scratch::Scratch::new("rich-loading-source");
    let canonical = Store::open(
        scratch.join("store.sqlite3"),
        StoredPathOrder::Sensitive,
        DerivationVersion::new(1),
    )
    .unwrap();
    let directory = Store::loading_directory(&scratch.join("store.sqlite3"));
    let mut builder = GetBuilder::create_in(
        &directory,
        StoredPathOrder::Sensitive,
        DerivationVersion::new(1),
    )
    .unwrap();
    let receipt = builder.cleanup_receipt();
    let path = receipt.paths()[0].clone();
    builder
        .write(|store| {
            assert!(
                store.open_reader().reader.is_err(),
                "construction cannot mint readers"
            );
            store.begin_request().apply_increment(
                IncrementProvenance::Derived,
                [Change::Upsert(DocumentFacts::new(
                    DocumentPath::new("hello.md").unwrap(),
                    "hash",
                    "rich sealed body",
                    16,
                ))],
                &[],
                &ContentModel::none(),
            )
        })
        .unwrap();
    let budget = ReadBudget::new(ReadBudgetPolicy::default());
    let reader = Arc::new(
        builder
            .finish_in_group(&budget, &canonical.read_group())
            .unwrap(),
    );
    assert!(
        matches!(
            Store::open_throwaway(&path, StoredPathOrder::Sensitive, DerivationVersion::new(1)),
            Err(norn_store::StoreError::Lifecycle {
                operation: "opening a sealed loading source",
                ..
            })
        ),
        "the loading-source marker survives sealing"
    );
    let snapshot = reader
        .acquire(ReadNeed::Source, Instant::now() + Duration::from_secs(5))
        .unwrap()
        .unwrap()
        .establish()
        .unwrap();
    reader.retire();
    drop(reader);
    assert!(path.exists());
    assert_eq!(budget.census().live_connections, 1);
    let params = norn_wire::GetParams::new(
        norn_wire::VaultAddress::name(norn_wire::VaultName::new("test").unwrap()),
        norn_wire::ResolutionTarget::new("hello").unwrap(),
    );
    let answer = snapshot
        .get(&params, &ContentModel::none(), &WholeText)
        .unwrap();
    let norn_wire::GetReport::Record { document, .. } = answer.report else {
        panic!("whole record")
    };
    let norn_wire::Content::Known { value: body, .. } = document.body.unwrap() else {
        panic!("the sealed source body must be known");
    };
    assert_eq!(body.text(), "rich sealed body");
    drop(snapshot);
    budget.trim(Instant::now() + Duration::from_secs(60));
    assert_eq!(budget.census().live_connections, 0);
    assert!(!path.exists());
    assert!(matches!(
        receipt.status(),
        norn_db::TemporaryCleanupStatus::Removed
    ));
}

struct WholeText;
impl norn_store::DocumentText for WholeText {
    fn section(
        &self,
        _: &[norn_store::HeadingFact],
        _: &str,
        _: usize,
        _: &str,
    ) -> Option<norn_store::SectionAt> {
        panic!("whole Get resolves no section")
    }
    fn block(&self, _: &str, _: usize, _: u64) -> std::ops::Range<usize> {
        panic!("whole Get resolves no block")
    }
}

#[test]
fn relative_dependency_names_require_complete_inventory() {
    let scratch = norn_testkit::scratch::Scratch::new("loading-complete-names");
    let mut catalogue = norn_store::loading::Catalogue::open(&scratch.join("loading")).unwrap();
    let holder = DocumentPath::new("notes/holder.md").unwrap();
    let link = norn_store::LinkFact {
        field: None,
        family: norn_store::LinkFamily::Wikilink,
        embed: false,
        protocol: None,
        target: "./target".into(),
        title: None,
        anchor: None,
        span: None,
    };
    catalogue
        .record_document(
            &DocumentPath::new("notes/target.md").unwrap(),
            &norn_store::AmbiguityIgnore::none(),
        )
        .unwrap();
    assert!(
        catalogue
            .demand_link_names(&link, &holder, 256)
            .unwrap()
            .is_none(),
        "relative links cannot certify an incomplete namespace"
    );
    catalogue.finish_inventory().unwrap();
    assert_eq!(
        catalogue
            .demand_link_names(&link, &holder, 256)
            .unwrap()
            .unwrap(),
        ["notes/target.md"]
    );
}

#[test]
fn multiple_loading_files_share_one_canonical_fair_turn() {
    let scratch = norn_testkit::scratch::Scratch::new("loading-fair-turns");
    let a = Store::open(
        scratch.join("a.sqlite3"),
        StoredPathOrder::Sensitive,
        DerivationVersion::new(1),
    )
    .unwrap();
    let b = Store::open(
        scratch.join("b.sqlite3"),
        StoredPathOrder::Sensitive,
        DerivationVersion::new(1),
    )
    .unwrap();
    let budget = ReadBudget::new(ReadBudgetPolicy::new(2, Duration::from_secs(5)).unwrap());
    let canonical = Arc::new(a.reader_pool(&budget));
    let other = Arc::new(b.reader_pool(&budget));
    let directory = Store::loading_directory(&scratch.join("a.sqlite3"));
    let get1 = Arc::new(
        GetBuilder::create_in(
            &directory,
            StoredPathOrder::Sensitive,
            DerivationVersion::new(1),
        )
        .unwrap()
        .finish_in_group(&budget, &a.read_group())
        .unwrap(),
    );
    let get2 = Arc::new(
        GetBuilder::create_in(
            &directory,
            StoredPathOrder::Sensitive,
            DerivationVersion::new(1),
        )
        .unwrap()
        .finish_in_group(&budget, &a.read_group())
        .unwrap(),
    );
    let metadata = Arc::new(
        norn_store::loading::IntakeBuilder::create_in(
            &directory,
            StoredPathOrder::Sensitive,
            DerivationVersion::new(1),
        )
        .unwrap()
        .finish_in_group(&budget, &a.read_group())
        .unwrap(),
    );
    let take = |reader: &Arc<norn_store::SnapshotReader>| {
        reader
            .acquire(ReadNeed::Source, Instant::now() + Duration::from_secs(5))
            .unwrap()
            .unwrap()
            .establish()
            .unwrap()
    };
    let one = take(&canonical);
    let two = take(&canonical);
    let (send, receive) = std::sync::mpsc::channel();
    std::thread::scope(|scope| {
        for (position, (reader, label)) in [
            (get1, "get1"),
            (get2, "get2"),
            (other, "other"),
            (metadata, "metadata"),
            (Arc::clone(&canonical), "canonical"),
        ]
        .into_iter()
        .enumerate()
        {
            let send = send.clone();
            scope.spawn(move || {
                let held = take(&reader);
                send.send(label).unwrap();
                drop(held);
            });
            let deadline = Instant::now() + Duration::from_secs(5);
            while budget.census().waiting_reads != position + 1 {
                assert!(Instant::now() < deadline, "waiters: {:?}", budget.census());
                std::thread::yield_now();
            }
        }
        drop(one);
        for expected in ["get1", "other", "get2", "metadata", "canonical"] {
            assert_eq!(
                receive.recv_timeout(Duration::from_secs(5)).unwrap(),
                expected
            );
        }
    });
    drop(two);
    assert_eq!(budget.census().peak_connections, 2);
}

#[test]
fn a_rich_projection_refuses_payload_omitting_writes() {
    let scratch = norn_testkit::scratch::Scratch::new("rich-only-derivation");
    let mut rich = GetBuilder::create_in(
        &scratch.join("loading"),
        StoredPathOrder::Sensitive,
        DerivationVersion::new(1),
    )
    .unwrap();
    rich.write(|store| {
        let result = store.begin_request().apply_intake_increment(
            [Change::Upsert(DocumentFacts::new(
                DocumentPath::new("hello.md").unwrap(),
                "hash",
                "body",
                4,
            ))],
            &[],
            &ContentModel::none(),
        );
        assert!(
            result.is_err(),
            "rich sources must retain ordinary payload and health"
        );
        Ok::<_, norn_store::StoreError>(())
    })
    .unwrap();
}

#[test]
fn a_sealed_rich_get_contains_ordinary_links_anchor_context_and_findings() {
    use norn_store::{
        AnchorReadings, BlockFact, HeadingFact, LinkAnchor, LinkFact, LinkFamily, Span,
    };
    let scratch = norn_testkit::scratch::Scratch::new("rich-loading-health");
    let canonical = Store::open(
        scratch.join("store.sqlite3"),
        StoredPathOrder::Sensitive,
        DerivationVersion::new(1),
    )
    .unwrap();
    let mut builder = GetBuilder::create_in(
        &scratch.join("loading"),
        StoredPathOrder::Sensitive,
        DerivationVersion::new(1),
    )
    .unwrap();
    let body = "[[target#Actual]] [[target#^yes]] [[absent]]";
    let mut source = DocumentFacts::new(
        DocumentPath::new("source.md").unwrap(),
        "source-hash",
        body,
        body.len() as u64,
    );
    for (target, anchor, offset) in [
        (
            "target",
            Some(LinkAnchor::Heading {
                written: "Actual".into(),
                readings: AnchorReadings {
                    text: norn_text::heading_reading("Actual"),
                    marked: None,
                },
            }),
            0,
        ),
        ("target", Some(LinkAnchor::Block { id: "yes".into() }), 18),
        ("absent", None, 34),
    ] {
        source.links.push(LinkFact {
            field: None,
            family: LinkFamily::Wikilink,
            embed: false,
            protocol: None,
            target: target.into(),
            title: None,
            anchor,
            span: Some(Span {
                line: 1,
                column: offset + 1,
                byte_offset: offset,
            }),
        });
    }
    let target_body = "# Actual\n\nblock ^yes\n";
    let mut target = DocumentFacts::new(
        DocumentPath::new("target.md").unwrap(),
        "target-hash",
        target_body,
        target_body.len() as u64,
    );
    target.headings.push(HeadingFact {
        level: 1,
        text: "Actual".into(),
        reading: norn_text::heading_reading("Actual"),
        slug: "actual".into(),
        span: Span {
            line: 1,
            column: 1,
            byte_offset: 0,
        },
        body_offset: 9,
        inside_container: false,
    });
    target.blocks.push(BlockFact {
        block_id: "yes".into(),
        span: Some(Span {
            line: 3,
            column: 7,
            byte_offset: 16,
        }),
    });
    // The target arrives later: ordinary health must repair the source's first judgment.
    for facts in [source, target] {
        builder
            .write(|store| {
                store.begin_request().apply_increment(
                    IncrementProvenance::Derived,
                    [Change::Upsert(facts)],
                    &[],
                    &ContentModel::none(),
                )
            })
            .unwrap();
    }
    let budget = ReadBudget::new(ReadBudgetPolicy::default());
    let reader = Arc::new(
        builder
            .finish_in_group(&budget, &canonical.read_group())
            .unwrap(),
    );
    let held = reader
        .acquire(ReadNeed::Source, Instant::now() + Duration::from_secs(5))
        .unwrap()
        .unwrap()
        .establish()
        .unwrap();
    let params = norn_wire::GetParams::new(
        norn_wire::VaultAddress::name(norn_wire::VaultName::new("test").unwrap()),
        norn_wire::ResolutionTarget::new("source").unwrap(),
    );
    let norn_wire::GetReport::Record { document, .. } = held
        .get(&params, &ContentModel::none(), &WholeText)
        .unwrap()
        .report
    else {
        panic!("whole record")
    };
    let norn_wire::Content::Known { value: links, .. } = document.links.unwrap() else {
        panic!("the sealed source links must be known");
    };
    assert_eq!(links.total, 3);
    assert_eq!(
        links
            .items
            .iter()
            .map(|link| link.health())
            .collect::<Vec<_>>(),
        [
            norn_wire::LinkHealth::Healthy,
            norn_wire::LinkHealth::Healthy,
            norn_wire::LinkHealth::Broken
        ]
    );
    let findings = document.findings.unwrap();
    assert_eq!(findings.total, 1);
    assert_eq!(findings.items[0].kind, norn_wire::FindingKind::Broken);
}

#[test]
fn complete_metadata_seals_before_field_find_and_admits_source_only() {
    use norn_store::{
        FrontmatterValue,
        loading::{IntakeBuilder, supports_find},
    };
    let scratch = norn_testkit::scratch::Scratch::new("loading-complete-metadata");
    let canonical = Store::open(
        scratch.join("store.sqlite3"),
        StoredPathOrder::Sensitive,
        DerivationVersion::new(1),
    )
    .unwrap();
    let mut builder = IntakeBuilder::create_in(
        &Store::loading_directory(&scratch.join("store.sqlite3")),
        StoredPathOrder::Sensitive,
        DerivationVersion::new(1),
    )
    .unwrap();
    let cleanup = builder.cleanup_receipt();
    for (name, status) in [("a.md", "open"), ("b.md", "closed"), ("c.md", "open")] {
        let facts = DocumentFacts::new(
            DocumentPath::new(name).unwrap(),
            "hash",
            "omitted payload",
            15,
        )
        .with_frontmatter(
            Some(FrontmatterValue::Map(vec![(
                "status".into(),
                FrontmatterValue::String(status.into()),
            )])),
            &ContentModel::none(),
        );
        builder
            .write(|store| {
                assert!(store.open_reader().reader.is_err());
                store.begin_request().apply_intake_increment(
                    [Change::Upsert(facts)],
                    &[],
                    &ContentModel::none(),
                )
            })
            .unwrap();
    }
    let budget = ReadBudget::new(ReadBudgetPolicy::default());
    let reader = Arc::new(
        builder
            .finish_in_group(&budget, &canonical.read_group())
            .unwrap(),
    );
    assert!(
        reader
            .acquire(
                ReadNeed::SourceAndScratch,
                Instant::now() + Duration::from_secs(5)
            )
            .is_err()
    );
    assert_eq!(budget.census().live_connections, 0);
    let held = reader
        .acquire(ReadNeed::Source, Instant::now() + Duration::from_secs(5))
        .unwrap()
        .unwrap()
        .establish()
        .unwrap();
    let params = norn_wire::FindParams::new(norn_wire::VaultAddress::name(
        norn_wire::VaultName::new("test").unwrap(),
    ))
    .with_predicates([norn_wire::Predicate::equal_to("status", "open")])
    .with_columns([norn_wire::Column::path(), norn_wire::Column::fields()])
    .with_limit(1);
    assert!(supports_find(&params));
    assert!(!supports_find(
        &params.clone().with_columns([norn_wire::Column::body()])
    ));
    let first = held.find(&params, &ContentModel::none()).unwrap();
    assert_eq!(first.rows[0].path.as_str(), "a.md");
    let last = held
        .find(
            &params.with_after(first.next.unwrap()),
            &ContentModel::none(),
        )
        .unwrap();
    assert_eq!(last.rows[0].path.as_str(), "c.md");
    assert!(last.next.is_none());
    reader.retire();
    drop(reader);
    drop(held);
    assert!(matches!(
        cleanup.status(),
        norn_db::TemporaryCleanupStatus::Removed
    ));
}

#[test]
fn sealed_disposable_cannot_be_rebuilt_into_an_ordinary_store() {
    let scratch = norn_testkit::scratch::Scratch::new("sealed-adoption-version");
    let canonical = Store::open(
        scratch.join("store.sqlite3"),
        StoredPathOrder::Sensitive,
        DerivationVersion::new(1),
    )
    .unwrap();
    let builder = GetBuilder::create_in(
        &scratch.join("loading"),
        StoredPathOrder::Sensitive,
        DerivationVersion::new(1),
    )
    .unwrap();
    let cleanup = builder.cleanup_receipt();
    let budget = ReadBudget::new(ReadBudgetPolicy::default());
    let reader = builder
        .finish_in_group(&budget, &canonical.read_group())
        .unwrap();
    let result = Store::open_throwaway(
        &cleanup.paths()[0],
        StoredPathOrder::AsciiCaseInsensitive,
        DerivationVersion::new(2),
    );
    assert!(
        matches!(
            result,
            Err(norn_store::StoreError::Lifecycle {
                operation: "opening a sealed loading source",
                ..
            })
        ),
        "disposable adoption refuses before client-side rebuild decisions: {result:?}"
    );
    drop(reader);
}

#[test]
fn sealed_source_grants_follow_the_finish_contract_on_cold_and_warm_reads() {
    for kind in ["metadata", "get", "legacy"] {
        let scratch = norn_testkit::scratch::Scratch::new("sealed-grant-policy");
        let canonical = Store::open(
            scratch.join("canonical.sqlite3"),
            StoredPathOrder::Sensitive,
            DerivationVersion::new(1),
        )
        .unwrap();
        let budget = ReadBudget::new(ReadBudgetPolicy::default());
        let directory = Store::loading_directory(&scratch.join("canonical.sqlite3"));
        let (reader, receipt) = if kind == "get" {
            let builder = GetBuilder::create_in(
                &directory,
                StoredPathOrder::Sensitive,
                DerivationVersion::new(1),
            )
            .unwrap();
            let receipt = builder.cleanup_receipt();
            (
                builder
                    .finish_in_group(&budget, &canonical.read_group())
                    .unwrap(),
                receipt,
            )
        } else {
            let builder = norn_store::loading::IntakeBuilder::create_in(
                &directory,
                StoredPathOrder::Sensitive,
                DerivationVersion::new(1),
            )
            .unwrap();
            let receipt = builder.cleanup_receipt();
            let reader = if kind == "legacy" {
                builder.finish(&budget).unwrap()
            } else {
                builder
                    .finish_in_group(&budget, &canonical.read_group())
                    .unwrap()
            };
            (reader, receipt)
        };
        let reader = Arc::new(reader);
        let path = receipt.paths()[0].clone();
        for warm in [false, true] {
            let deadline = || Instant::now() + Duration::from_secs(5);
            for need in [
                ReadNeed::SourceAndScratch,
                ReadNeed::SourceAndOptionalScratch,
            ] {
                if kind != "legacy" {
                    assert!(reader.try_take_for(need, deadline()).is_none());
                    assert!(reader.acquire(need, deadline()).is_err());
                    assert_eq!(budget.census().live_connections, usize::from(warm));
                } else {
                    let turn = reader
                        .try_take_for(need, deadline())
                        .or_else(|| reader.acquire(need, deadline()).unwrap())
                        .unwrap();
                    let snapshot = turn.establish().unwrap();
                    assert_eq!(budget.census().live_connections, 2);
                    drop(snapshot);
                }
            }
            let snapshot = reader
                .acquire(ReadNeed::Source, deadline())
                .unwrap()
                .unwrap()
                .establish()
                .unwrap();
            drop(snapshot);
        }
        let need = if kind == "legacy" {
            ReadNeed::SourceAndScratch
        } else {
            ReadNeed::Source
        };
        let snapshot = reader
            .acquire(need, Instant::now() + Duration::from_secs(5))
            .unwrap()
            .unwrap()
            .establish()
            .unwrap();
        reader.retire();
        drop(reader);
        assert!(path.exists());
        assert_eq!(
            budget.census().live_connections,
            if kind == "legacy" { 2 } else { 1 }
        );
        drop(snapshot);
        assert_eq!(budget.census().live_connections, 0);
        assert!(!path.exists());
    }
}
