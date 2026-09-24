//! A find's rows: the cursor a page mints and continues, the columns a row
//! carries and what they cost, the bounds a row is cut to, and the parts of a
//! request that are reported rather than applied.
//!
//! The fixture is [`crate::find`]'s: five documents whose fields part the raw
//! and typed orders.

use norn_store::{
    BODY_ROW_CEILING, BlockFact, DeclaredFields, FindStatement, FindWork, Found, HeadingFact,
    NESTED_ROW_CEILING, Nested, NestedRows, PageRefusal, SnapshotReader, Span, Store, TagFact,
    TagSource, Validation,
};
use norn_wire::{
    BlockRow, Column, Cursor, CursorKey, CursorOrderChanged, Direction, FieldValue, FindParams,
    FindingKind, Moved, Predicate, Snapshot as WireSnapshot, Sort, SortKey, TagRow, Unsatisfied,
    ValidateParams, VaultAddress, VaultName,
};

use crate::common::{Scratch, ambiguity, document, unread_block, violation, write_documents};
use crate::find::{SEED_SCHEMA, Seeded, declared, integer_order, map, request, sorted, string};

/// The fixture's declaration, read from the schema pinned under `schema`.
fn declared_under(schema: &str) -> DeclaredFields {
    DeclaredFields::under(schema)
        .declare("status")
        .declare_typed("count", integer_order())
}

/// The fixture's keys, both ordered by their text, under `schema`.
fn declared_raw_under(schema: &str) -> DeclaredFields {
    DeclaredFields::under(schema)
        .declare("status")
        .declare("count")
}

/// A store with no schema pinned, holding three documents under no
/// declaration, `notes/a.md` with a finding.
struct Unpinned {
    _scratch: Scratch,
    _store: Store,
    reader: std::sync::Arc<SnapshotReader>,
}

impl Unpinned {
    fn new(label: &str) -> Self {
        let scratch = Scratch::new(label);
        let mut store = scratch.open();
        let mut request = store.begin_request();
        let counted = |at: &str, hash: &str, count: &str| {
            document(at, hash, "a body\n").with_frontmatter(
                Some(map(vec![("count", string(count))])),
                &DeclaredFields::none(),
            )
        };
        write_documents(
            &mut request,
            &[
                counted("notes/a.md", "hash-a", "3"),
                counted("notes/b.md", "hash-b", "10"),
                document("notes/c.md", "hash-c", "a body\n"),
            ],
        );
        request
            .record_finding(&violation("notes/a.md"))
            .expect("recording a finding");
        let reader = std::sync::Arc::new(
            store
                .open_reader()
                .reader
                .expect("a live store mints a reader"),
        );
        Unpinned {
            _scratch: scratch,
            _store: store,
            reader,
        }
    }

    fn snapshot(&self) -> norn_store::Snapshot {
        self.reader
            .try_take()
            .expect("a handle nothing is reading holds its connection")
            .establish(norn_store::StoredPathOrder::Sensitive)
            .snapshot
            .expect("a snapshot")
    }
}

impl Seeded {
    /// One find on a fresh snapshot, under the fixture's declaration.
    fn found(&self, params: &FindParams) -> Found {
        self.found_under(params, &declared())
    }

    /// One find on a fresh snapshot, under `declared`.
    fn found_under(&self, params: &FindParams, declared: &DeclaredFields) -> Found {
        self.snapshot()
            .find(params, declared)
            .unwrap_or_else(|refusal| panic!("a find of {params:?}: {refusal}"))
    }

    /// Write `facts` in one changeset.
    fn write(&mut self, facts: &[norn_store::DocumentFacts]) {
        let mut request = self.store.begin_request();
        write_documents(&mut request, facts);
    }

    /// Pin a schema under `fingerprint`, which is a new generation.
    fn pin(&mut self, fingerprint: &str) {
        self.store
            .begin_request()
            .pin_vault_schema(fingerprint.as_bytes(), fingerprint)
            .expect("pinning a schema");
    }
}

fn paths(found: &Found) -> Vec<String> {
    found
        .rows
        .iter()
        .map(|row| row.path.as_str().to_string())
        .collect()
}

// ---- the cursor ----

/// The orders a cursor is judged under: the path both ways, a raw field sort
/// both ways, and a typed one both ways.
fn orders() -> Vec<FindParams> {
    [
        SortKey::path(),
        SortKey::field("status"),
        SortKey::field("count"),
    ]
    .into_iter()
    .flat_map(|key| {
        [Direction::Ascending, Direction::Descending]
            .into_iter()
            .map(move |direction| sorted(key.clone(), direction))
    })
    .collect()
}

/// **A page's cursor continues exactly against the same snapshot.** Every
/// order, drained two rows at a time on one snapshot, yields the rows one page
/// holds, with nothing moved at any step; the last page mints no cursor. A
/// cursor carries the reading it was minted under, and names a schema
/// fingerprint exactly where the page ran in a typed order — here the one the
/// fixture pins.
#[test]
fn a_cursor_continues_exactly_against_the_snapshot_it_was_minted_on() {
    let seeded = Seeded::new("find-cursor-exact");
    for params in orders() {
        let whole = paths(&seeded.found(&params));
        assert_eq!(whole.len(), 5);

        let snapshot = seeded.snapshot();
        let mut drained = Vec::new();
        let mut after: Option<Cursor> = None;
        let typed = matches!(&params.sort, Some(sort) if sort.key == SortKey::field("count"));
        loop {
            let mut page = params.clone().with_limit(2);
            if let Some(cursor) = after.take() {
                page = page.with_after(cursor);
            }
            let found = snapshot.find(&page, &declared()).expect("a page");
            assert!(found.moved.is_empty(), "{params:?}: {:?}", found.moved);
            assert_eq!(
                found.snapshot.schema_fingerprint,
                typed.then(|| SEED_SCHEMA.to_string()),
                "{params:?} mints under the wrong order"
            );
            drained.extend(paths(&found));
            match found.next {
                Some(cursor) => {
                    assert_eq!(cursor.snapshot(), &found.snapshot);
                    after = Some(cursor);
                }
                None => break,
            }
        }
        assert_eq!(
            drained, whole,
            "{params:?} drained other rows than one page holds"
        );
    }
}

/// **A continuation after a write reports the generation moved, and resumes
/// after its own position.** A document written before the position is not
/// read again and one written after it is.
#[test]
fn a_continuation_after_a_write_reports_the_generation_moved() {
    let mut seeded = Seeded::new("find-cursor-moved");
    let first = seeded.found(&request().with_limit(2));
    assert_eq!(paths(&first), ["notes/a.md", "notes/B.md"]);
    let cursor = first.next.expect("a next page");

    seeded.write(&[
        document("notes/aa.md", "hash-aa", "before the position\n"),
        document("notes/d.md", "hash-d", "after the position\n"),
    ]);
    let second = seeded.found(&request().with_limit(2).with_after(cursor));
    assert_eq!(second.moved, [Moved::Generation]);
    assert_eq!(paths(&second), ["notes/c.md", "notes/d.md"]);
}

/// **A typed cursor refuses once its fingerprint no longer stands; a raw one
/// survives a re-pin that leaves its key untyped.** The refusal is the wire's,
/// naming both fingerprints. A raw cursor over `count` goes on in the raw order
/// after a re-pin whose declaration still orders `count` by its text, because
/// the order it names a position in has not moved; continued under a
/// declaration that gives `count` a type, it is refused, because the request's
/// order is now the typed one.
#[test]
fn a_typed_cursor_refuses_after_a_re_pin_and_a_raw_one_survives_it() {
    let mut seeded = Seeded::new("find-cursor-re-pin");
    seeded.pin("schema-1");
    let by_count = sorted(SortKey::field("count"), Direction::Ascending).with_limit(2);
    let typed = seeded.found_under(&by_count, &declared_under("schema-1"));
    assert_eq!(
        typed.snapshot.schema_fingerprint.as_deref(),
        Some("schema-1")
    );
    let typed_cursor = typed.next.expect("a next page");

    let raw = seeded.found_under(&by_count, &declared_raw_under("schema-1"));
    assert_eq!(raw.snapshot.schema_fingerprint, None);
    // Raw ascending: the documents with no value first, then "10", "3", "nine".
    assert_eq!(paths(&raw), ["other/glossary.md", "other/v1.2.md"]);
    let raw_cursor = raw.next.expect("a next page");

    seeded.pin("schema-2");
    let refused = |cursor: &Cursor, declared: &DeclaredFields| {
        seeded
            .snapshot()
            .find(&by_count.clone().with_after(cursor.clone()), declared)
            .expect_err("a cursor that is no position in the request's order")
    };
    assert_eq!(
        refused(&typed_cursor, &declared_under("schema-2")),
        PageRefusal::OrderChanged(CursorOrderChanged::new(
            "schema-1",
            Some("schema-2".to_string())
        ))
    );
    assert_eq!(
        refused(&raw_cursor, &declared_under("schema-2")),
        PageRefusal::OrderChanged(CursorOrderChanged::minted_raw(Some("schema-2".to_string())))
    );

    let survived = seeded.found_under(
        &by_count.with_limit(10).with_after(raw_cursor),
        &declared_raw_under("schema-2"),
    );
    assert_eq!(survived.moved, [Moved::Generation]);
    assert_eq!(
        paths(&survived),
        ["notes/B.md", "notes/a.md", "notes/c.md"],
        "the continuation left the raw order it was minted in"
    );
    assert_eq!(survived.snapshot.schema_fingerprint, None);
}

/// **A cursor is judged against the order its request names, on one
/// snapshot.** The fixture's declaration orders `count` by a type and `status`
/// by its text. A raw cursor continued in a typed order, a typed one continued
/// in a raw order or a path order, and a cursor carrying a sort value continued
/// in a path order are each refused with the wire's order change; a cursor
/// carrying no sort value continued in a raw field order stands in its missing
/// section.
#[test]
fn a_cursor_minted_in_another_order_than_the_requests_is_refused() {
    let seeded = Seeded::new("find-cursor-other-order");
    let cursor = |params: FindParams| {
        seeded
            .found(&params.with_limit(1))
            .next
            .expect("a next page")
    };
    let by_path = request().with_sort(Sort::new(SortKey::path(), Direction::Ascending));
    let by_status = sorted(SortKey::field("status"), Direction::Descending);
    let by_count = sorted(SortKey::field("count"), Direction::Ascending);
    let path_cursor = cursor(by_path.clone());
    let raw_cursor = cursor(by_status.clone());
    let typed_cursor = cursor(by_count.clone());
    assert!(path_cursor.snapshot().schema_fingerprint.is_none());
    assert!(matches!(
        raw_cursor.key(),
        CursorKey::Document { sort: Some(_), .. }
    ));
    assert_eq!(raw_cursor.snapshot().schema_fingerprint, None);
    assert_eq!(
        typed_cursor.snapshot().schema_fingerprint.as_deref(),
        Some(SEED_SCHEMA)
    );

    let refusal = |params: &FindParams, cursor: &Cursor| {
        seeded
            .snapshot()
            .find(&params.clone().with_after(cursor.clone()), &declared())
            .expect_err("a cursor that is no position in the request's order")
    };
    let seed = || Some(SEED_SCHEMA.to_string());
    for (params, cursor, changed) in [
        (
            &by_count,
            &raw_cursor,
            CursorOrderChanged::minted_raw(seed()),
        ),
        (
            &by_count,
            &path_cursor,
            CursorOrderChanged::minted_raw(seed()),
        ),
        (
            &by_status,
            &typed_cursor,
            CursorOrderChanged::new(SEED_SCHEMA, None),
        ),
        (
            &by_path,
            &typed_cursor,
            CursorOrderChanged::new(SEED_SCHEMA, None),
        ),
        (&by_path, &raw_cursor, CursorOrderChanged::minted_raw(None)),
    ] {
        assert_eq!(
            refusal(params, cursor),
            PageRefusal::OrderChanged(changed),
            "{params:?} continuing {cursor:?}"
        );
    }

    // A path order's cursor carries no sort value, which a raw field order
    // reads as a position in its missing section: the encoding names no key.
    let continued = seeded.found(&by_status.clone().with_after(path_cursor));
    assert!(continued.moved.is_empty());
}

/// **A find's declaration is the one the snapshot pins.** A declaration read
/// from another schema than the pinned one — or from any schema where none is
/// pinned, or from none where one is — is refused, naming both fingerprints,
/// before any page is read.
#[test]
fn a_declaration_read_from_another_schema_than_the_pinned_one_is_refused() {
    let mut seeded = Seeded::new("find-declaration-not-pinned");
    let refusal = |seeded: &Seeded, declared: &DeclaredFields| {
        seeded
            .snapshot()
            .find(&request(), declared)
            .expect_err("a declaration the snapshot does not pin")
    };
    assert_eq!(
        refusal(&seeded, &DeclaredFields::none()),
        PageRefusal::DeclarationNotPinned {
            declared_under: None,
            pinned: Some(SEED_SCHEMA.to_string()),
        }
    );
    seeded.pin("schema-1");
    assert_eq!(
        refusal(&seeded, &declared()),
        PageRefusal::DeclarationNotPinned {
            declared_under: Some(SEED_SCHEMA.to_string()),
            pinned: Some("schema-1".to_string()),
        }
    );

    let unpinned = Unpinned::new("find-declaration-unpinned");
    assert_eq!(
        unpinned
            .snapshot()
            .find(&request(), &declared())
            .expect_err("a declaration where no schema is pinned"),
        PageRefusal::DeclarationNotPinned {
            declared_under: Some(SEED_SCHEMA.to_string()),
            pinned: None,
        }
    );
}

/// **A store with no schema pinned mints every cursor under no fingerprint.**
/// Its declaration declares nothing, so no key has a typed order: a field sort
/// runs in the raw order, and neither it nor a path order names a fingerprint.
/// A finding recorded there is stamped with the empty fingerprint, and a
/// finding part still finds it.
#[test]
fn a_store_with_no_schema_pinned_mints_cursors_under_no_fingerprint() {
    let unpinned = Unpinned::new("find-no-pin");
    for key in [SortKey::path(), SortKey::field("count")] {
        for direction in [Direction::Ascending, Direction::Descending] {
            let params = sorted(key.clone(), direction).with_limit(1);
            let found = unpinned
                .snapshot()
                .find(&params, &DeclaredFields::none())
                .expect("a page");
            assert_eq!(found.snapshot.schema_fingerprint, None, "{params:?}");
            let next = found.next.expect("a next page");
            assert_eq!(next.snapshot().schema_fingerprint, None, "{params:?}");
            unpinned
                .snapshot()
                .find(&params.with_after(next), &DeclaredFields::none())
                .expect("a continuation under no schema");
        }
    }
    let with_finding = unpinned
        .snapshot()
        .find(
            &request().with_predicates([Predicate::has_finding(FindingKind::BodyBytesNotUtf8)]),
            &DeclaredFields::none(),
        )
        .expect("a page");
    assert_eq!(paths(&with_finding), ["notes/a.md"]);
}

/// A cursor among rows that are not documents names no place in a find.
#[test]
fn a_cursor_among_other_rows_is_refused() {
    let seeded = Seeded::new("find-cursor-other-rows");
    let cursor = Cursor::new(
        WireSnapshot::new(seeded.snapshot().epoch(), 1, None, None),
        CursorKey::ordinal(3),
    );
    assert_eq!(
        seeded
            .snapshot()
            .find(&request().with_after(cursor), &declared())
            .expect_err("an ordinal cursor"),
        PageRefusal::NotADocumentCursor
    );
}

// ---- what a row costs ----

/// **A page of `limit` rows hydrates exactly `limit` document rows, and reads
/// no table whose column was not named.** The page statement reads one key
/// past the bound, which is how it knows a next page exists; the hydration
/// reads the bound, a path part narrowing the page included: the glob is a
/// filter inside the page statement, so it narrows the keys the bound counts
/// rather than voiding the bound. A path alone is carried by the keys and
/// hydrates nothing.
/// Each nested collection reads its own table and no other, and the work
/// instrument's statements are the snapshot counter's, among them the one
/// point read of the active fingerprint every find judges its declaration by.
#[test]
fn a_page_of_limit_rows_hydrates_limit_rows_and_reads_no_unnamed_table() {
    let seeded = Seeded::new("find-hydration-work");
    let work = |params: &FindParams| {
        let snapshot = seeded.snapshot();
        let before = snapshot.counters().statements_executed();
        let found = snapshot.find(params, &declared()).expect("a page");
        assert_eq!(
            found.work.statements,
            snapshot.counters().statements_executed() - before
        );
        // The virtual-machine steps are SQLite's own count of its operations,
        // which no case here states; the size-independence pair reads them.
        let work = FindWork {
            page_vm_steps: 0,
            ..found.work
        };
        (found.rows.len(), work)
    };
    let nested = |tags, headings, blocks| NestedRows {
        tags,
        headings,
        blocks,
    };

    assert_eq!(
        work(&request().with_limit(2)),
        (
            2,
            FindWork {
                statements: 2,
                keys_read: 3,
                page_full_scan_steps: 0,
                page_sorts: 0,
                page_vm_steps: 0,
                documents_hydrated: 0,
                nested_rows: nested(0, 0, 0),
                finding_rows: 0,
            }
        )
    );
    assert_eq!(
        work(&request().with_limit(2).with_columns([Column::body()])),
        (
            2,
            FindWork {
                statements: 3,
                keys_read: 3,
                page_full_scan_steps: 0,
                page_sorts: 0,
                page_vm_steps: 0,
                documents_hydrated: 2,
                nested_rows: nested(0, 0, 0),
                finding_rows: 0,
            }
        )
    );
    // Three documents stand under `notes/`; the page holds two of them.
    assert_eq!(
        work(
            &request()
                .with_predicates([Predicate::path("notes/*.md")])
                .with_limit(2)
                .with_columns([Column::body()])
        ),
        (
            2,
            FindWork {
                statements: 3,
                keys_read: 3,
                page_full_scan_steps: 0,
                page_sorts: 1,
                page_vm_steps: 0,
                documents_hydrated: 2,
                nested_rows: nested(0, 0, 0),
                finding_rows: 0,
            }
        )
    );
    assert_eq!(
        work(
            &sorted(SortKey::field("status"), Direction::Descending)
                .with_limit(3)
                .with_columns([Column::field("status"), Column::fields()])
        ),
        (
            3,
            FindWork {
                statements: 3,
                keys_read: 4,
                page_full_scan_steps: 0,
                page_sorts: 0,
                page_vm_steps: 0,
                documents_hydrated: 3,
                nested_rows: nested(0, 0, 0),
                finding_rows: 0,
            }
        )
    );
    // `notes/a.md` carries the one tag; the page is it and `notes/B.md`.
    assert_eq!(
        work(&request().with_limit(2).with_columns([Column::tags()])),
        (
            2,
            FindWork {
                statements: 3,
                keys_read: 3,
                page_full_scan_steps: 0,
                page_sorts: 0,
                page_vm_steps: 0,
                documents_hydrated: 0,
                nested_rows: nested(1, 0, 0),
                finding_rows: 0,
            }
        )
    );

    // The plans name the same statements: a named collection's head, no
    // total where no head filled the ceiling, and nothing of a table no column
    // named.
    let statements: Vec<FindStatement> = seeded
        .plans(&request().with_columns([Column::tags()]))
        .into_iter()
        .map(|plan| plan.statement)
        .collect();
    assert!(statements.contains(&FindStatement::NestedHead(Nested::Tags)));
    assert!(
        !statements.contains(&FindStatement::NestedTotal(Nested::Tags)),
        "a total ran where no head filled the ceiling: {statements:?}"
    );
    for unnamed in [
        FindStatement::HydrateDocuments,
        FindStatement::NestedHead(Nested::Headings),
        FindStatement::NestedTotal(Nested::Headings),
        FindStatement::NestedHead(Nested::Blocks),
        FindStatement::NestedTotal(Nested::Blocks),
    ] {
        assert!(
            !statements.contains(&unnamed),
            "{unnamed:?} in {statements:?}"
        );
    }
}

/// **A find's work reads out whole, each count under its own name.** A
/// harness compares two finds name by name, so every count is present at
/// whatever value it holds, the nested rows one name per table.
#[test]
fn a_finds_work_reads_out_every_count_by_name() {
    let work = FindWork {
        statements: 1,
        keys_read: 2,
        page_full_scan_steps: 3,
        page_sorts: 4,
        page_vm_steps: 5,
        documents_hydrated: 6,
        nested_rows: NestedRows {
            tags: 7,
            headings: 0,
            blocks: 9,
        },
        finding_rows: 10,
    };
    assert_eq!(
        work.readings().collect::<Vec<_>>(),
        vec![
            ("find_statements", 1),
            ("find_keys_read", 2),
            ("find_page_full_scan_steps", 3),
            ("find_page_sorts", 4),
            ("find_page_vm_steps", 5),
            ("find_documents_hydrated", 6),
            ("find_tag_rows", 7),
            ("find_heading_rows", 0),
            ("find_block_rows", 9),
            ("find_finding_rows", 10),
        ]
    );
}

/// **A row carries the columns it names, and a field is read off the
/// projection.** A scalar crosses as the text the pillar holds for it — a
/// number as its digits, a string without quotes — a sequence as its items,
/// and a key the document does not carry as absent. A row that names no
/// column but its path carries nothing else.
#[test]
fn a_row_carries_the_columns_it_names_read_off_the_projection() {
    let seeded = Seeded::new("find-projection");
    let found = seeded.found(&request().with_columns([
        Column::fields(),
        Column::field("status"),
        Column::body(),
        Column::tags(),
    ]));
    let row = |path: &str| {
        found
            .rows
            .iter()
            .find(|row| row.path.as_str() == path)
            .unwrap_or_else(|| panic!("{path} in the page"))
    };
    let fields = |path: &str| row(path).fields.clone().expect("fields projected");

    assert_eq!(
        fields("notes/a.md"),
        [
            ("count".to_string(), FieldValue::scalar("3")),
            ("status".to_string(), FieldValue::scalar("open")),
        ]
        .into_iter()
        .collect()
    );
    assert_eq!(
        fields("notes/B.md")["count"],
        FieldValue::sequence([FieldValue::scalar("10"), FieldValue::scalar("9")])
    );
    assert_eq!(fields("other/v1.2.md")["count"], FieldValue::sequence([]));
    assert_eq!(
        fields("other/glossary.md"),
        [("status".to_string(), FieldValue::absent())]
            .into_iter()
            .collect()
    );
    let body = row("notes/a.md").body.clone().expect("body projected");
    assert_eq!(body.text(), "the interloper walked in\n");
    assert_eq!(body.byte_length(), 25);
    assert!(!body.is_truncated());
    let tags = row("notes/a.md").tags.clone().expect("tags projected");
    assert_eq!(
        tags.items,
        [TagRow::new("draft", norn_wire::TagSource::Body, None)]
    );
    assert_eq!(tags.total, 1);
    assert_eq!(
        row("notes/c.md").tags.as_ref().map(|tags| tags.total),
        Some(0)
    );
    assert!(row("notes/a.md").headings.is_none());

    let bare = seeded.found(&request().with_limit(1));
    assert_eq!(
        bare.rows[0],
        norn_wire::DocumentRow::new(bare.rows[0].path.clone())
    );
}

/// **A link column is refused by name** as a column a find does not project
/// yet, and its refusal reads as that fact.
#[test]
fn a_link_column_is_refused_by_name() {
    let seeded = Seeded::new("find-dormant-columns");
    let refusal = seeded
        .snapshot()
        .find(
            &request().with_columns([Column::body(), Column::links()]),
            &declared(),
        )
        .expect_err("a dormant column");
    assert_eq!(
        refusal,
        PageRefusal::NotProjected {
            column: "the links column"
        }
    );
    assert_eq!(
        refusal.to_string(),
        "the links column is not yet projected onto a find's row"
    );
}

/// **A row's findings column carries the findings standing at its path, each
/// the row `validate` answers for it**: the same head, total and hint, in the
/// `(kind, id)` order a validate narrowed to that path answers. A document
/// with no finding carries an empty collection of none. **The column is
/// bounded by the per-row ceiling and says how many stood**: a document with
/// more findings than [`NESTED_ROW_CEILING`] carries the first that-many and
/// the whole count, and the head statement read no more than it kept. Under
/// no schema pinned, a row carries the findings recorded under none.
#[test]
fn a_rows_findings_column_carries_what_validate_answers_at_its_path() {
    let mut seeded = Seeded::new("find-findings-column");
    seeded.write(&[document("long/doc.md", "hash-long", "a body\n")]);
    let mut writing = seeded.store.begin_request();
    for finding in [
        ambiguity(
            "notes/a.md",
            "glossary",
            "glossary/",
            &["other/glossary.md"],
            3,
        ),
        unread_block("notes/a.md"),
    ] {
        writing
            .record_finding(&finding)
            .expect("recording a finding");
    }
    for _ in 0..NESTED_ROW_CEILING + 3 {
        writing
            .record_finding(&violation("long/doc.md"))
            .expect("recording a finding");
    }

    let found = seeded.found(&request().with_columns([Column::findings()]));
    let vault = VaultAddress::name(VaultName::new("notes").expect("a vault name"));
    let mut kept = 0;
    for row in &found.rows {
        let findings = row.findings.clone().expect("the findings column");
        let validated = match seeded
            .snapshot()
            .validate(
                &ValidateParams::new(vault.clone())
                    .with_predicates([Predicate::path(row.path.as_str())])
                    .with_limit(1000),
                &declared(),
            )
            .expect("a validate")
            .answer
        {
            Validation::Findings { rows, .. } => rows,
            Validation::Summary { .. } => panic!("a page answered a summary"),
        };
        assert_eq!(
            findings.total,
            validated.len() as u64,
            "{}",
            row.path.as_str()
        );
        assert_eq!(
            findings.items,
            validated
                .into_iter()
                .take(NESTED_ROW_CEILING)
                .collect::<Vec<_>>(),
            "{}",
            row.path.as_str()
        );
        kept += findings.items.len() as u64;
    }
    let by_path = |at: &str| {
        found
            .rows
            .iter()
            .find(|row| row.path.as_str() == at)
            .and_then(|row| row.findings.clone())
            .expect("a row")
    };
    let long = by_path("long/doc.md");
    assert_eq!(
        (long.items.len(), long.total),
        (NESTED_ROW_CEILING, NESTED_ROW_CEILING as u64 + 3)
    );
    assert!(long.is_truncated());
    let a = by_path("notes/a.md");
    assert_eq!(
        a.items.iter().map(|item| item.kind).collect::<Vec<_>>(),
        vec![
            FindingKind::FrontmatterUnreadable,
            FindingKind::PathNamesNoDocument
        ]
    );
    assert_eq!(a.items[1].head.total(), 3);
    assert!(a.items[1].hint.is_some());
    assert_eq!(by_path("notes/B.md").items, Vec::new());
    assert_eq!(by_path("notes/B.md").total, 0);
    assert_eq!(
        found.work.finding_rows, kept,
        "the head read past the ceiling"
    );

    let unpinned = Unpinned::new("find-findings-column-unpinned");
    let bare = unpinned
        .snapshot()
        .find(
            &FindParams::new(VaultAddress::name(
                VaultName::new("notes").expect("a vault name"),
            ))
            .with_columns([Column::findings()]),
            &DeclaredFields::none(),
        )
        .expect("a find under no schema");
    let carried: Vec<(String, u64)> = bare
        .rows
        .iter()
        .map(|row| {
            (
                row.path.as_str().to_string(),
                row.findings.as_ref().expect("the findings column").total,
            )
        })
        .collect();
    assert_eq!(
        carried,
        vec![
            ("notes/a.md".to_string(), 1),
            ("notes/b.md".to_string(), 0),
            ("notes/c.md".to_string(), 0),
        ]
    );
}

/// **A row cut by a ceiling says how much the whole held.** A collection
/// longer than [`NESTED_ROW_CEILING`] carries its first that-many items in
/// document order and its whole count; one exactly at the ceiling carries all
/// of it and says so. A body longer than [`BODY_ROW_CEILING`] is cut back to
/// the last whole character — here a two-byte character straddles the
/// ceiling, so the head is one byte short of it — and carries the whole
/// body's length.
#[test]
fn a_row_cut_by_a_ceiling_says_how_much_the_whole_held() {
    let mut seeded = Seeded::new("find-ceilings");
    let body = format!(
        "{}é{}",
        "a".repeat(BODY_ROW_CEILING - 1),
        "b".repeat(70_000 - BODY_ROW_CEILING - 1)
    );
    assert_eq!(body.len(), 70_000);
    let mut long = document("long/doc.md", "hash-long", &body);
    long.tags = (0..300)
        .map(|index| TagFact {
            name: format!("t{index:03}"),
            source: TagSource::Frontmatter,
            span: None,
        })
        .collect();
    long.headings = (0..NESTED_ROW_CEILING + 1)
        .map(|index| HeadingFact {
            level: 2,
            text: format!("heading {index}"),
            slug: format!("heading-{index}"),
            span: Span {
                line: index as u64 + 1,
                column: 1,
                byte_offset: 0,
            },
            body_offset: 0,
            inside_container: false,
        })
        .collect();
    long.blocks = (0..NESTED_ROW_CEILING)
        .map(|index| BlockFact {
            block_id: format!("b{index}"),
            span: None,
        })
        .collect();
    seeded.write(&[long]);

    let found = seeded.found(
        &request()
            .with_predicates([Predicate::path("long/**")])
            .with_columns([
                Column::body(),
                Column::tags(),
                Column::headings(),
                Column::blocks(),
            ]),
    );
    let [row] = &found.rows[..] else {
        panic!("one row: {:?}", paths(&found));
    };
    let tags = row.tags.clone().expect("tags");
    assert_eq!((tags.items.len(), tags.total), (NESTED_ROW_CEILING, 300));
    assert_eq!(tags.items[0].name, "t000");
    assert_eq!(tags.items[NESTED_ROW_CEILING - 1].name, "t255");
    let headings = row.headings.clone().expect("headings");
    assert_eq!(
        (headings.items.len(), headings.total),
        (NESTED_ROW_CEILING, NESTED_ROW_CEILING as u64 + 1)
    );
    assert!(headings.is_truncated());
    let blocks = row.blocks.clone().expect("blocks");
    assert_eq!(
        (blocks.items.len(), blocks.total),
        (NESTED_ROW_CEILING, NESTED_ROW_CEILING as u64)
    );
    assert!(!blocks.is_truncated());
    assert_eq!(blocks.items[1], BlockRow::new("b1", None));

    let text = row.body.clone().expect("body");
    assert_eq!(text.byte_length(), 70_000);
    assert_eq!(text.text().len(), BODY_ROW_CEILING - 1);
    assert!(text.text().bytes().all(|byte| byte == b'a'));
    assert!(text.is_truncated());
    // The nested rows read are the heads, never the whole.
    assert_eq!(
        found.work.nested_rows,
        NestedRows {
            tags: NESTED_ROW_CEILING as u64,
            headings: NESTED_ROW_CEILING as u64,
            blocks: NESTED_ROW_CEILING as u64,
        }
    );
}

// ---- what a request cannot apply ----

/// The fixture's declaration with `due` declared too, which no document
/// carries.
fn declaring_due() -> DeclaredFields {
    declared().declare("due")
}

/// **Every unknown key is reported with the keys near it, and never applied in
/// silence.** An unknown sort key orders the rows by path, ascending, whatever
/// direction was asked; an unknown projected key carries nothing; an unknown
/// predicate key's part filters nothing. The suggestions are drawn from the
/// field universe — `priority` only a document carries, `due` only the
/// declaration names — by the one rule: within two edits, case folded.
#[test]
fn every_unknown_key_is_reported_with_the_keys_near_it() {
    let mut seeded = Seeded::new("find-unknown-keys");
    seeded.write(&[document("notes/d.md", "hash-d", "a body\n")
        .with_frontmatter(Some(map(vec![("priority", string("high"))])), &declared())]);
    let every = [
        "notes/a.md",
        "notes/B.md",
        "notes/c.md",
        "notes/d.md",
        "other/glossary.md",
        "other/v1.2.md",
    ];
    let found = |params: FindParams| seeded.found_under(&params, &declaring_due());

    let unknown_sort = found(sorted(SortKey::field("Stauts"), Direction::Descending));
    assert_eq!(paths(&unknown_sort), every);
    assert_eq!(
        unknown_sort.unsatisfied,
        [Unsatisfied::unknown_sort_key(
            "Stauts",
            vec!["status".to_string()]
        )]
    );
    assert_eq!(unknown_sort.snapshot.schema_fingerprint, None);

    let unknown_part = found(request().with_predicates([
        Predicate::equal_to("priorty", "high"),
        Predicate::has("dew"),
    ]));
    assert_eq!(paths(&unknown_part), every);
    assert_eq!(
        unknown_part.unsatisfied,
        [
            Unsatisfied::unknown_predicate_key("priorty", vec!["priority".to_string()]),
            Unsatisfied::unknown_predicate_key("dew", vec!["due".to_string()]),
        ]
    );

    let unknown_column = found(
        request()
            .with_limit(1)
            .with_columns([Column::field("cont"), Column::field("status")]),
    );
    assert_eq!(
        unknown_column.rows[0].fields,
        Some(
            [("status".to_string(), FieldValue::scalar("open"))]
                .into_iter()
                .collect()
        )
    );
    assert_eq!(
        unknown_column.unsatisfied,
        [Unsatisfied::unknown_projection_key(
            "cont",
            vec!["count".to_string()]
        )]
    );

    // A key only a document carries, and one only the declaration names, are
    // known: applied, and not reported.
    let known = found(
        sorted(SortKey::field("due"), Direction::Ascending)
            .with_predicates([Predicate::equal_to("priority", "high")]),
    );
    assert_eq!(paths(&known), ["notes/d.md"]);
    assert!(known.unsatisfied.is_empty());

    // Every place at once: reported in the request's order, sort then parts
    // then columns, and the universe read once for all of them.
    let all = request()
        .with_sort(Sort::new(SortKey::field("stat"), Direction::Ascending))
        .with_predicates([Predicate::missing("nothing")])
        .with_columns([Column::field("xyzzy")]);
    assert_eq!(
        found(all.clone()).unsatisfied,
        [
            Unsatisfied::unknown_sort_key("stat", vec!["status".to_string()]),
            Unsatisfied::unknown_predicate_key("nothing", Vec::new()),
            Unsatisfied::unknown_projection_key("xyzzy", Vec::new()),
        ]
    );
    let probes = |params: &FindParams| {
        let statements: Vec<FindStatement> = seeded
            .snapshot()
            .find_plans(params, &declaring_due())
            .expect("plans")
            .into_iter()
            .map(|plan| plan.statement)
            .collect();
        (
            statements
                .iter()
                .filter(|statement| **statement == FindStatement::KnownKey)
                .count(),
            statements
                .iter()
                .filter(|statement| **statement == FindStatement::FieldUniverse)
                .count(),
        )
    };
    assert_eq!(probes(&all), (3, 1));
    // The happy path asks one seek for the key only a document carries, and
    // never walks the universe.
    assert_eq!(
        probes(&request().with_predicates([Predicate::equal_to("priority", "high")])),
        (1, 0)
    );
    assert_eq!(
        probes(&sorted(SortKey::field("due"), Direction::Ascending)),
        (0, 0)
    );
}

/// **A path part that cannot match by construction is reported, and one that
/// matches nothing is answered with nothing.** A glob with no wildcard at which
/// no document stands and under which some do is a bare directory; a glob no
/// document path can spell is impossible. A literal path with nothing at it
/// and nothing under it can be applied, and matches no document: an empty
/// page, and no report.
#[test]
fn a_path_part_that_cannot_match_is_reported_and_one_matching_nothing_is_not() {
    let seeded = Seeded::new("find-path-parts");
    let with = |glob: &str| seeded.found(&request().with_predicates([Predicate::path(glob)]));

    let bare = with("notes");
    assert!(bare.rows.is_empty());
    assert_eq!(bare.unsatisfied, [Unsatisfied::bare_directory("notes")]);

    for impossible in ["notes//*.md", "/notes/*.md", "notes/./*.md", "*/../x.md"] {
        let found = with(impossible);
        assert!(found.rows.is_empty());
        assert_eq!(
            found.unsatisfied,
            [Unsatisfied::impossible_path(impossible)],
            "{impossible}"
        );
    }

    for nothing in ["nowhere/x.md", "archive", "notes/a.md/deeper"] {
        let found = with(nothing);
        assert!(found.rows.is_empty(), "{nothing}");
        assert!(
            found.unsatisfied.is_empty(),
            "{nothing}: {:?}",
            found.unsatisfied
        );
    }
    let exact = with("notes/a.md");
    assert_eq!(paths(&exact), ["notes/a.md"]);
    assert!(exact.unsatisfied.is_empty());
}

/// A find hands a handler the unsatisfied parts and the page it wraps.
#[test]
fn a_find_is_the_report_a_handler_wraps() {
    let seeded = Seeded::new("find-report");
    let found = seeded.found(&sorted(SortKey::field("nope"), Direction::Ascending).with_limit(1));
    let next = found.next.clone();
    let rows = found.rows.clone();
    let (unsatisfied, report) = found.into_report();
    assert_eq!(
        unsatisfied,
        [Unsatisfied::unknown_sort_key("nope", Vec::new())]
    );
    assert_eq!(report.rows, rows);
    assert_eq!(report.next, next);
    assert!(report.moved.is_empty());
}
