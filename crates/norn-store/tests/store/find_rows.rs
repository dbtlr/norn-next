//! A find's rows: the cursor a page mints and continues, the columns a row
//! carries and what they cost, the bounds a row is cut to, and the parts of a
//! request that are reported rather than applied.
//!
//! The fixture is [`crate::find`]'s: five documents whose fields part the raw
//! and typed orders.

use norn_store::{
    BODY_ROW_CEILING, BlockFact, DeclaredFields, FindRefusal, FindStatement, FindWork, Found,
    HeadingFact, NESTED_ROW_CEILING, Nested, NestedRows, Span, TagFact, TagSource,
};
use norn_wire::{
    BlockRow, Column, Cursor, CursorKey, CursorOrderChanged, Direction, FieldValue, FindParams,
    Moved, Predicate, Snapshot as WireSnapshot, Sort, SortKey, TagRow, Unsatisfied,
};

use crate::common::{document, write_documents};
use crate::find::{Seeded, declared, map, request, sorted, string};

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
/// fingerprint exactly where the page ran in a typed order — here the empty
/// one, since the fixture pins no schema.
#[test]
fn a_cursor_continues_exactly_against_the_snapshot_it_was_minted_on() {
    let seeded = Seeded::new("find-cursor-exact");
    for params in orders() {
        let whole = paths(&seeded.found(&params));
        assert_eq!(whole.len(), 5);

        let mut snapshot = seeded.snapshot();
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
                typed.then(String::new),
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
/// survives the re-pin.** The refusal is the wire's, naming both fingerprints.
/// A raw cursor over `count` — minted where `count` carried no type — goes on
/// in the raw order after a declaration and a pin that give it one, because
/// the order it names a position in has not moved.
#[test]
fn a_typed_cursor_refuses_after_a_re_pin_and_a_raw_one_survives_it() {
    let mut seeded = Seeded::new("find-cursor-re-pin");
    seeded.pin("schema-1");
    let by_count = sorted(SortKey::field("count"), Direction::Ascending).with_limit(2);
    let typed = seeded.found(&by_count);
    assert_eq!(
        typed.snapshot.schema_fingerprint.as_deref(),
        Some("schema-1")
    );
    let typed_cursor = typed.next.expect("a next page");

    let untyped = DeclaredFields::none().declare("status").declare("count");
    let raw = seeded.found_under(&by_count, &untyped);
    assert_eq!(raw.snapshot.schema_fingerprint, None);
    // Raw ascending: the documents with no value first, then "10", "3", "nine".
    assert_eq!(paths(&raw), ["other/glossary.md", "other/v1.2.md"]);
    let raw_cursor = raw.next.expect("a next page");

    seeded.pin("schema-2");
    let refusal = seeded
        .snapshot()
        .find(&by_count.clone().with_after(typed_cursor), &declared())
        .expect_err("a typed cursor under a fingerprint that no longer stands");
    assert_eq!(
        refusal,
        FindRefusal::OrderChanged(CursorOrderChanged::new(
            "schema-1",
            Some("schema-2".to_string())
        ))
    );

    let survived = seeded.found(&by_count.with_limit(10).with_after(raw_cursor));
    assert_eq!(survived.moved, [Moved::Generation]);
    assert_eq!(
        paths(&survived),
        ["notes/B.md", "notes/a.md", "notes/c.md"],
        "the continuation left the raw order it was minted in"
    );
    assert_eq!(survived.snapshot.schema_fingerprint, None);
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
        FindRefusal::NotADocumentCursor
    );
}

// ---- what a row costs ----

/// **A page of `limit` rows hydrates exactly `limit` document rows, and reads
/// no table whose column was not named.** The page statement reads one key
/// past the bound, which is how it knows a next page exists; the hydration
/// reads the bound. A path alone is carried by the keys and hydrates nothing.
/// Each nested collection reads its own table and no other, and the work
/// instrument's statements are the snapshot counter's.
#[test]
fn a_page_of_limit_rows_hydrates_limit_rows_and_reads_no_unnamed_table() {
    let seeded = Seeded::new("find-hydration-work");
    let work = |params: &FindParams| {
        let mut snapshot = seeded.snapshot();
        let before = snapshot.counters().statements_executed();
        let found = snapshot.find(params, &declared()).expect("a page");
        assert_eq!(
            found.work.statements,
            snapshot.counters().statements_executed() - before
        );
        (found.rows.len(), found.work)
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
                statements: 1,
                keys_read: 3,
                documents_hydrated: 0,
                nested_rows: nested(0, 0, 0),
            }
        )
    );
    assert_eq!(
        work(&request().with_limit(2).with_columns([Column::body()])),
        (
            2,
            FindWork {
                statements: 2,
                keys_read: 3,
                documents_hydrated: 2,
                nested_rows: nested(0, 0, 0),
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
                statements: 2,
                keys_read: 4,
                documents_hydrated: 3,
                nested_rows: nested(0, 0, 0),
            }
        )
    );
    // `notes/a.md` carries the one tag; the page is it and `notes/B.md`.
    assert_eq!(
        work(&request().with_limit(2).with_columns([Column::tags()])),
        (
            2,
            FindWork {
                statements: 2,
                keys_read: 3,
                documents_hydrated: 0,
                nested_rows: nested(1, 0, 0),
            }
        )
    );

    // The plans name the same statements: a named collection's head and
    // total, and nothing of a table no column named.
    let statements: Vec<FindStatement> = seeded
        .plans(&request().with_columns([Column::tags()]), None)
        .into_iter()
        .map(|plan| plan.statement)
        .collect();
    assert!(statements.contains(&FindStatement::NestedHead(Nested::Tags)));
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

/// **A link or finding column is refused by name**, as a `links_to` part is:
/// the indexes those columns read land with the task that builds them.
#[test]
fn a_link_or_finding_column_is_refused_by_name() {
    let seeded = Seeded::new("find-dormant-columns");
    for (column, fact) in [
        (Column::links(), "a document's links"),
        (Column::findings(), "the findings standing over a document"),
    ] {
        assert_eq!(
            seeded
                .snapshot()
                .find(
                    &request().with_columns([Column::body(), column]),
                    &declared()
                )
                .expect_err("a dormant column"),
            FindRefusal::NotIndexed {
                fact,
                consumer: "NORN-229",
            }
        );
    }
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
            .find_plans(params, &declaring_due(), None)
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
