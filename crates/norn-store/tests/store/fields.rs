//! The field pillar: what a document's frontmatter derives, how it is written
//! and replaced, and what a schema pin does to its typed half.
//!
//! Each case writes through the store's own increment and reads back through
//! [`norn_store::Request::stored_facts`], so what is asserted is the rows at
//! rest rather than the value the derivation handed over.

use crate::common::{Scratch, document, number, path, record_death, write_document};
use norn_store::{
    Change, DeclaredFields, DocumentFacts, FieldContainer, FieldRow, FieldRows, FrontmatterValue,
    IncrementProvenance, Provenance, StoreError, TypedOrder, induced_failure,
};

/// A presence row, as the rows are compared.
fn presence(key: &str, container: FieldContainer) -> FieldRow {
    FieldRow::Presence {
        key: key.to_string(),
        container,
    }
}

/// A value row carrying no typed value, marked least under the raw order where
/// `least` says so.
fn raw(key: &str, ordinal: u32, text: Option<&str>, least: bool) -> FieldRow {
    FieldRow::Value {
        key: key.to_string(),
        ordinal,
        raw: text.map(str::to_string),
        typed: None,
        least_raw: least,
        least_typed: false,
    }
}

fn string(text: &str) -> FrontmatterValue {
    FrontmatterValue::String(text.to_string())
}

fn map(entries: Vec<(&str, FrontmatterValue)>) -> FrontmatterValue {
    FrontmatterValue::Map(
        entries
            .into_iter()
            .map(|(key, value)| (key.to_string(), value))
            .collect(),
    )
}

/// An order that reads a raw value as an integer and sorts by it, which is
/// enough of a typed order to part from the raw one: `"10"` stands before
/// `"9"` as text and after it as a number.
fn integer_order() -> TypedOrder {
    TypedOrder::new(|raw| {
        raw.parse::<i64>()
            .ok()
            .map(|number| format!("{:020}", i128::from(number) - i128::from(i64::MIN)))
    })
}

/// One document at `at` whose frontmatter is `value`, its rows derived under
/// `declared`.
fn fielded(
    at: &str,
    hash: &str,
    value: FrontmatterValue,
    declared: &DeclaredFields,
) -> DocumentFacts {
    document(at, hash, "a body\n").with_frontmatter(Some(value), declared)
}

fn stored_fields(request: &mut norn_store::Request<'_>, at: &str) -> FieldRows {
    request
        .stored_facts(&path(at))
        .expect("reading a document")
        .expect("a document")
        .fields
}

/// **Every key carries a presence row, and every scalar a value row.** The
/// container a key's value sits in is recorded on its presence row, so an empty
/// sequence and an empty map are present and told apart; a scalar, and each
/// scalar element of a sequence in element order, is one value row carrying its
/// canonical text unquoted. A map yields no value rows, and neither does a
/// sequence or a map nested inside a sequence. A null and a float JSON cannot
/// spell are scalars with no raw text — present, and matched by no value. A
/// repeated key keeps its last value.
#[test]
fn a_documents_rows_are_a_presence_row_per_key_and_a_value_row_per_scalar() {
    let value = map(vec![
        ("title", string("first")),
        ("count", FrontmatterValue::Int(42)),
        ("ratio", FrontmatterValue::Float(1.0)),
        ("draft", FrontmatterValue::Bool(false)),
        ("empty", FrontmatterValue::Null),
        ("infinite", FrontmatterValue::Float(f64::INFINITY)),
        (
            "tags",
            FrontmatterValue::Sequence(vec![
                string("b"),
                FrontmatterValue::Sequence(vec![string("nested")]),
                string("a"),
                map(vec![("inner", string("x"))]),
                FrontmatterValue::Null,
            ]),
        ),
        ("none", FrontmatterValue::Sequence(Vec::new())),
        ("nothing", FrontmatterValue::Map(Vec::new())),
        ("author", map(vec![("name", string("Ada"))])),
        ("title", string("second")),
    ]);
    let expected = vec![
        presence("author", FieldContainer::Map),
        presence("count", FieldContainer::Scalar),
        raw("count", 1, Some("42"), true),
        presence("draft", FieldContainer::Scalar),
        raw("draft", 1, Some("false"), true),
        presence("empty", FieldContainer::Scalar),
        raw("empty", 1, None, false),
        presence("infinite", FieldContainer::Scalar),
        raw("infinite", 1, None, false),
        presence("none", FieldContainer::Sequence),
        presence("nothing", FieldContainer::Map),
        presence("ratio", FieldContainer::Scalar),
        raw("ratio", 1, Some("1.0"), true),
        presence("tags", FieldContainer::Sequence),
        raw("tags", 1, Some("b"), false),
        raw("tags", 2, Some("a"), true),
        raw("tags", 3, None, false),
        presence("title", FieldContainer::Scalar),
        raw("title", 1, Some("second"), true),
    ];
    let derived = FieldRows::derive(Some(&value), &DeclaredFields::none());
    assert_eq!(derived.rows(), expected.as_slice());

    let scratch = Scratch::new("field-rows");
    let mut store = scratch.open();
    let mut request = store.begin_request();
    write_document(
        &mut request,
        &fielded("docs/shape.md", "hash-1", value, &DeclaredFields::none()),
    );
    assert_eq!(
        stored_fields(&mut request, "docs/shape.md"),
        derived,
        "the rows at rest are not the rows the frontmatter derives"
    );
    assert_eq!(request.counters().get("field_rows_written"), Some(19));

    // Only a map derives rows: a scalar or a sequence at the top has no keys.
    for top in [
        string("a scalar"),
        FrontmatterValue::Sequence(vec![string("x")]),
    ] {
        assert!(FieldRows::derive(Some(&top), &DeclaredFields::none()).is_empty());
    }
    assert!(FieldRows::derive(None, &DeclaredFields::none()).is_empty());
}

/// **The raw and the typed order each mark their own least value.** A key
/// ordered by a type carries that type's sort key beside the raw text, and the
/// two orders can disagree about which value is least: `"10"` is the least text
/// of `["9", "10", "x"]` and nine the least integer. A value that does not read
/// as the type carries no typed key and is never the typed least. A key the
/// declaration does not order by a type carries no typed value at all.
#[test]
fn the_raw_and_the_typed_order_mark_their_own_least_value() {
    let declared = DeclaredFields::under("schema-1")
        .declare_field("rank", number(), Some(integer_order()))
        .declare("title");
    let value = map(vec![
        (
            "rank",
            FrontmatterValue::Sequence(vec![string("9"), string("10"), string("x")]),
        ),
        ("title", string("10")),
    ]);
    let rows = FieldRows::derive(Some(&value), &declared);
    let values: Vec<(&str, u32, Option<&str>, bool, bool)> = rows
        .rows()
        .iter()
        .filter_map(|row| match row {
            FieldRow::Value {
                key,
                ordinal,
                typed,
                least_raw,
                least_typed,
                ..
            } => Some((
                key.as_str(),
                *ordinal,
                typed.as_deref(),
                *least_raw,
                *least_typed,
            )),
            FieldRow::Presence { .. } => None,
        })
        .collect();
    let nine = integer_order().sort_key("9");
    let ten = integer_order().sort_key("10");
    assert_eq!(
        values,
        vec![
            ("rank", 1, nine.as_deref(), false, true),
            ("rank", 2, ten.as_deref(), true, false),
            ("rank", 3, None, false, false),
            ("title", 1, None, true, false),
        ]
    );

    let scratch = Scratch::new("field-markers");
    let mut store = scratch.open();
    let mut request = store.begin_request();
    request
        .pin_vault_schema(b"version: 1\n", "schema-1")
        .expect("pinning a schema");
    write_document(
        &mut request,
        &fielded("docs/ranked.md", "hash-1", value, &declared),
    );
    assert_eq!(stored_fields(&mut request, "docs/ranked.md"), rows);
}

/// **A tie for the least value goes to the earliest element.** A sequence can
/// hold its least value more than once; one marker stands per document and
/// order, and it is on the first element holding that value, under the raw
/// order and the typed order alike.
#[test]
fn a_tie_for_the_least_value_marks_the_earliest_element() {
    let declared =
        DeclaredFields::under("schema-1").declare_field("rank", number(), Some(integer_order()));
    let value = map(vec![(
        "rank",
        FrontmatterValue::Sequence(vec![string("3"), string("5"), string("3")]),
    )]);
    let markers: Vec<(u32, bool, bool)> = FieldRows::derive(Some(&value), &declared)
        .rows()
        .iter()
        .filter_map(|row| match row {
            FieldRow::Value {
                ordinal,
                least_raw,
                least_typed,
                ..
            } => Some((*ordinal, *least_raw, *least_typed)),
            FieldRow::Presence { .. } => None,
        })
        .collect();
    assert_eq!(
        markers,
        vec![(1, true, true), (2, false, false), (3, false, false)]
    );
}

/// **A re-derivation replaces a document's field rows wholesale.** The rows the
/// first frontmatter derived are gone, and the rows at rest are exactly the ones
/// the second derives — a key the second value dropped leaves nothing behind.
#[test]
fn a_re_derivation_replaces_the_field_rows_wholesale() {
    let scratch = Scratch::new("field-replace");
    let mut store = scratch.open();
    let mut request = store.begin_request();
    let none = DeclaredFields::none();
    let first = map(vec![
        ("status", string("draft")),
        (
            "tags",
            FrontmatterValue::Sequence(vec![string("a"), string("b")]),
        ),
    ]);
    write_document(
        &mut request,
        &fielded("docs/moving.md", "hash-1", first, &none),
    );

    let second = map(vec![("owner", string("ada"))]);
    write_document(
        &mut request,
        &fielded("docs/moving.md", "hash-2", second.clone(), &none),
    );
    assert_eq!(
        stored_fields(&mut request, "docs/moving.md"),
        FieldRows::derive(Some(&second), &none)
    );
}

/// **A document's field rows are written in its entry, or the document is not
/// written.** A field row the store refuses fails the changeset, and the
/// document row written before it in the same entry does not stand: a reader
/// never meets a document whose field rows are missing. The control is a
/// document with no frontmatter, which writes no field row and stands under the
/// same refusal.
#[test]
fn a_refused_field_row_leaves_no_document_row() {
    let scratch = Scratch::new("field-atomic");
    let mut store = scratch.open();
    induced_failure::execute_out_of_band(
        &mut store,
        "CREATE TRIGGER refuse_field_rows BEFORE INSERT ON document_fields
         BEGIN SELECT RAISE(ABORT, 'field rows refused'); END",
    )
    .expect("installing the refusal");
    let mut request = store.begin_request();

    write_document(
        &mut request,
        &document("docs/plain.md", "hash-1", "a body\n"),
    );

    let fielded = fielded(
        "docs/fielded.md",
        "hash-2",
        map(vec![("status", string("draft"))]),
        &DeclaredFields::none(),
    );
    let refused =
        request.apply_increment(IncrementProvenance::Derived, [Change::Upsert(fielded)], &[]);
    assert!(
        refused.is_err(),
        "a changeset whose field row was refused committed"
    );
    assert!(
        request
            .stored_facts(&path("docs/fielded.md"))
            .expect("reading a document")
            .is_none(),
        "the document stands without the field rows its entry wrote"
    );
    assert!(
        request
            .stored_facts(&path("docs/plain.md"))
            .expect("reading a document")
            .is_some(),
        "a document with no field rows did not stand under the refusal"
    );
}

/// **Typed values stand only under the schema the store pins.** A document
/// whose typed field values were derived under another schema than the pinned
/// one — any schema where none is pinned, or the one a re-pin replaced — is
/// refused in its entry, naming both fingerprints, and nothing of it stands. A
/// document whose rows carry no typed value needs no agreement: raw text and
/// presence are the document's alone, so a declaration that types none of its
/// keys, or no declaration at all, writes what a pin leaves.
#[test]
fn typed_values_derived_under_a_schema_the_store_does_not_pin_are_refused() {
    let scratch = Scratch::new("field-unpinned-declaration");
    let mut store = scratch.open();
    let mut request = store.begin_request();
    let ranked = |declared: &DeclaredFields| {
        fielded(
            "docs/ranked.md",
            "hash-1",
            map(vec![("rank", string("3"))]),
            declared,
        )
    };
    let typed_under = |schema: &str| {
        DeclaredFields::under(schema).declare_field("rank", number(), Some(integer_order()))
    };
    let refusal = |request: &mut norn_store::Request<'_>, declared: &DeclaredFields| {
        let refused = request
            .apply_increment(
                IncrementProvenance::Derived,
                [Change::Upsert(ranked(declared))],
                &[],
            )
            .expect_err("typed values derived under a schema the store does not pin");
        assert!(
            request
                .stored_facts(&path("docs/ranked.md"))
                .expect("reading a document")
                .is_none(),
            "a refused entry's document stands"
        );
        match refused {
            StoreError::Entry { problem, .. } => *problem,
            other => panic!("a refusal outside the entry: {other}"),
        }
    };

    assert_eq!(
        refusal(&mut request, &typed_under("schema-1")),
        StoreError::UnpinnedDeclaration {
            derived_under: Some("schema-1".to_string()),
            pinned: None,
        }
    );

    request
        .pin_vault_schema(b"version: 1\n", "schema-2")
        .expect("pinning a schema");
    assert_eq!(
        refusal(&mut request, &typed_under("schema-1")),
        StoreError::UnpinnedDeclaration {
            derived_under: Some("schema-1".to_string()),
            pinned: Some("schema-2".to_string()),
        }
    );

    for untyped in [
        DeclaredFields::none(),
        DeclaredFields::under("schema-1").declare("rank"),
    ] {
        write_document(&mut request, &ranked(&untyped));
    }
    write_document(&mut request, &ranked(&typed_under("schema-2")));
    assert_eq!(
        stored_fields(&mut request, "docs/ranked.md"),
        FieldRows::derive(
            Some(&map(vec![("rank", string("3"))])),
            &typed_under("schema-2")
        ),
    );
}

/// **A document's field rows die with it.** A death takes the rows through the
/// cascade: the delete succeeds with foreign keys enforced, the store holds no
/// row referencing a document that is not there, and the typed values the dead
/// document carried are gone, which a later pin that clears every typed value
/// counts as none.
#[test]
fn a_documents_field_rows_die_with_it() {
    let scratch = Scratch::new("field-cascade");
    let mut store = scratch.open();
    let mut request = store.begin_request();
    request
        .pin_vault_schema(b"version: 1\n", "schema-1")
        .expect("pinning a schema");
    let declared =
        DeclaredFields::under("schema-1").declare_field("rank", number(), Some(integer_order()));
    write_document(
        &mut request,
        &fielded(
            "docs/doomed.md",
            "hash-1",
            map(vec![("rank", string("3"))]),
            &declared,
        ),
    );
    record_death(
        &mut request,
        &path("docs/doomed.md"),
        Provenance::PlanDelete,
    );
    request.finish();
    store
        .verify_integrity()
        .expect("a store whose document died");

    let pin = store
        .begin_request()
        .pin_vault_schema(b"version: 1\nfields: {}\n", "schema-2")
        .expect("re-pinning a schema");
    assert!(pin.repinned);
    assert_eq!(
        pin.invalidated.typed_values_discarded, 0,
        "a dead document's typed value outlived it"
    );
}

/// **A pin that moves the schema fingerprint clears every typed value, and
/// nothing else of the pillar.** The typed values were derived under the schema
/// the pin replaces, so the pin clears them and their markers in its own
/// transaction and reports how many it cleared; the rows and their raw text and
/// raw markers stand, because they are a function of the document alone. A pin
/// of the schema already pinned is not a schema change and clears nothing.
#[test]
fn a_moved_pin_clears_every_typed_value_and_nothing_else() {
    let scratch = Scratch::new("field-pin");
    let mut store = scratch.open();
    let mut request = store.begin_request();
    let declared =
        DeclaredFields::under("schema-1").declare_field("rank", number(), Some(integer_order()));
    request
        .pin_vault_schema(b"version: 1\n", "schema-1")
        .expect("pinning a schema");
    let value = map(vec![
        (
            "rank",
            FrontmatterValue::Sequence(vec![string("9"), string("10")]),
        ),
        ("title", string("ranked")),
    ]);
    write_document(
        &mut request,
        &fielded("docs/ranked.md", "hash-1", value.clone(), &declared),
    );

    let again = request
        .pin_vault_schema(b"version: 1\n", "schema-1")
        .expect("pinning the same schema");
    assert!(!again.repinned);
    assert_eq!(again.invalidated.typed_values_discarded, 0);
    assert_eq!(
        stored_fields(&mut request, "docs/ranked.md"),
        FieldRows::derive(Some(&value), &declared),
        "a pin of the schema already pinned cleared a typed value"
    );

    let moved = request
        .pin_vault_schema(b"version: 1\nfields: {}\n", "schema-2")
        .expect("re-pinning a schema");
    assert!(moved.repinned);
    assert_eq!(moved.invalidated.typed_values_discarded, 2);
    assert_eq!(request.counters().get("typed_values_discarded"), Some(2));
    assert_eq!(
        stored_fields(&mut request, "docs/ranked.md"),
        FieldRows::derive(Some(&value), &DeclaredFields::none()),
        "the pin left a typed value standing, or took more than the typed half"
    );
    request.finish();
    store
        .verify_integrity()
        .expect("a store whose typed values a pin cleared");
}

/// **A document's field rows are the rows its frontmatter derives.** The
/// value and its rows are set together and only together, so setting a
/// document's frontmatter again replaces its rows with the new value's, and
/// taking the value away takes the rows with it.
#[test]
fn a_documents_rows_are_the_rows_its_frontmatter_derives() {
    let typed =
        DeclaredFields::under("schema-1").declare_field("rank", number(), Some(integer_order()));
    let one = map(vec![("title", string("one")), ("rank", string("4"))]);
    let another = map(vec![("title", string("another"))]);

    let facts = fielded("docs/a.md", "hash-1", one.clone(), &typed);
    assert_eq!(facts.frontmatter(), Some(&one));
    assert_eq!(facts.fields(), &FieldRows::derive(Some(&one), &typed));

    let facts = facts.with_frontmatter(Some(another.clone()), &typed);
    assert_eq!(facts.frontmatter(), Some(&another));
    assert_eq!(facts.fields(), &FieldRows::derive(Some(&another), &typed));

    let facts = facts.with_frontmatter(None, &typed);
    assert_eq!(facts.frontmatter(), None);
    assert!(facts.fields().is_empty());
}
