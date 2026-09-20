//! The vault schema's content model.
//!
//! Every case reads bytes a vault author could have written and asserts the
//! model those bytes mean. The parse is pure, so a case needs no directory and
//! no clock.

use std::cmp::Ordering;

use norn_config::schema::typed::{Comparison, ComparisonSignal, Offset};
use norn_config::schema::{
    FieldType, Pattern, TypedValue, UndeclaredTags, VaultSchema, VaultSchemaError,
};

/// A schema declaring one of everything the grammar has.
const WHOLE: &[u8] = b"version: 1
fields:
  title:
    type: text
    required: true
  created:
    type: date
  rating:
    type: number
  draft:
    type: boolean
  status:
    type: text
    one_of: [draft, live, retired]
tags:
  declared: [area, project]
  patterns: [\"person/**\"]
  undeclared: report
folders:
  - path: journal
    description: One document per day
  - path: archive
paths:
  ambiguity_ignore: [\"archive/**\", \"attachments/*\"]
";

#[test]
fn a_whole_schema_reads_every_section_it_declares() {
    let schema = VaultSchema::parse(WHOLE).expect("a whole schema");

    let fields: Vec<(&str, FieldType, bool)> = schema
        .fields()
        .map(|(key, field)| (key, field.kind(), field.required()))
        .collect();
    assert_eq!(
        fields,
        vec![
            ("created", FieldType::Date, false),
            ("draft", FieldType::Boolean, false),
            ("rating", FieldType::Number, false),
            ("status", FieldType::Text, false),
            ("title", FieldType::Text, true),
        ]
    );
    let status: Vec<&str> = schema
        .field("status")
        .expect("the status field")
        .one_of()
        .expect("a closed set")
        .collect();
    assert_eq!(status, vec!["draft", "live", "retired"]);
    assert!(
        schema
            .field("title")
            .expect("the title field")
            .one_of()
            .is_none()
    );

    let facet = schema.tags();
    assert_eq!(
        facet.declared().collect::<Vec<_>>(),
        vec!["area", "project"]
    );
    assert_eq!(
        facet
            .patterns()
            .iter()
            .map(Pattern::as_str)
            .collect::<Vec<_>>(),
        vec!["person/**"]
    );
    assert_eq!(facet.undeclared(), UndeclaredTags::Report);

    let folders: Vec<(&str, Option<&str>)> = schema
        .folders()
        .iter()
        .map(|folder| (folder.path(), folder.description()))
        .collect();
    assert_eq!(
        folders,
        vec![("journal", Some("One document per day")), ("archive", None)]
    );

    assert_eq!(
        schema
            .ambiguity_ignore()
            .iter()
            .map(Pattern::as_str)
            .collect::<Vec<_>>(),
        vec!["archive/**", "attachments/*"]
    );
}

#[test]
fn a_schema_that_declares_only_its_version_judges_no_document() {
    let schema = VaultSchema::parse(b"version: 1\n").expect("a bare schema");

    assert_eq!(schema.fields().count(), 0);
    assert_eq!(schema.tags().declared().count(), 0);
    assert!(schema.folders().is_empty());
    assert!(schema.ambiguity_ignore().is_empty());
    assert!(!schema.judges_documents());
}

/// A vault that lists its tags without saying what an unlisted one means has
/// not asked for findings, so listing them costs nothing.
#[test]
fn a_declared_vocabulary_alone_judges_no_document() {
    let schema =
        VaultSchema::parse(b"version: 1\ntags:\n  declared: [area]\n").expect("a listed facet");

    assert!(schema.tags().admits("area"));
    assert!(!schema.tags().admits("project"));
    assert!(!schema.tags().reports_undeclared());
    assert!(!schema.judges_documents());
}

/// The other direction of the same rule: `report` over an empty vocabulary
/// would make every tag in the vault a finding, which is a position no author
/// takes by writing one word.
#[test]
fn reporting_over_an_empty_vocabulary_judges_no_document() {
    let schema =
        VaultSchema::parse(b"version: 1\ntags:\n  undeclared: report\n").expect("an empty facet");

    assert!(!schema.tags().reports_undeclared());
    assert!(!schema.judges_documents());
}

#[test]
fn a_reporting_facet_admits_its_names_and_its_patterns() {
    let schema = VaultSchema::parse(WHOLE).expect("a whole schema");
    let facet = schema.tags();

    assert!(facet.admits("area"));
    assert!(facet.admits("project"));
    assert!(facet.admits("person/ada"));
    assert!(facet.admits("person/ada/notes"));
    // `**` covers the run of no segments, so the pattern admits its own root.
    assert!(facet.admits("person"));
    assert!(!facet.admits("ephemeral"));
    // Case is compared as written.
    assert!(!facet.admits("Area"));
    assert!(facet.reports_undeclared());
    assert!(schema.judges_documents());
}

#[test]
fn the_model_is_a_pure_function_of_the_bytes() {
    let once = VaultSchema::parse(WHOLE).expect("a whole schema");
    let twice = VaultSchema::parse(WHOLE).expect("the same bytes again");

    assert_eq!(once, twice);
}

// ---- refusals ----

#[test]
fn bytes_that_are_not_utf8_are_refused_rather_than_read() {
    let error = VaultSchema::parse(&[0xff, 0xfe]).expect_err("bytes that are not text");

    assert!(matches!(error, VaultSchemaError::NotUtf8 { .. }), "{error}");
}

#[test]
fn text_that_is_not_yaml_is_refused_rather_than_read() {
    let error = VaultSchema::parse(b"\tversion: 1\n").expect_err("text that is not YAML");

    assert!(matches!(error, VaultSchemaError::NotYaml { .. }), "{error}");
}

#[test]
fn a_root_that_is_not_a_mapping_is_refused() {
    let error = VaultSchema::parse(b"- version\n").expect_err("a sequence at the root");

    assert_eq!(
        error,
        VaultSchemaError::NotAMapping {
            found: "a sequence"
        }
    );
}

#[test]
fn an_absent_version_is_refused_because_the_grammar_is_unknown() {
    let error = VaultSchema::parse(b"fields: {}\n").expect_err("a schema with no version");

    assert!(matches!(error, VaultSchemaError::Version { .. }), "{error}");
}

#[test]
fn a_version_this_build_does_not_read_is_refused() {
    let error = VaultSchema::parse(b"version: 9\n").expect_err("a later grammar");

    assert!(
        matches!(&error, VaultSchemaError::Version { detail } if detail.contains("version 9")),
        "{error}"
    );
}

#[test]
fn a_section_of_the_wrong_shape_names_itself() {
    let error = VaultSchema::parse(b"version: 1\nfields:\n  created:\n    type: instant\n")
        .expect_err("a type nothing declares");

    assert_eq!(
        error,
        VaultSchemaError::Section {
            at: "fields.created.type".to_string(),
            wanted: "a declared type",
            found: "a string".to_string(),
        }
    );
}

#[test]
fn an_empty_ignore_pattern_is_refused_as_a_pattern() {
    let error = VaultSchema::parse(b"version: 1\npaths:\n  ambiguity_ignore: [\"\"]\n")
        .expect_err("a pattern naming no set");

    assert!(
        matches!(&error, VaultSchemaError::Section { at, .. } if at == "paths.ambiguity_ignore"),
        "{error}"
    );
}

/// Every refusal above is a value. Nothing in the grammar panics on input, and
/// a caller that meets one still holds the process it met it on.
#[test]
fn no_malformed_schema_panics() {
    for bytes in [
        &b""[..],
        b"version: 1\nfields: []\n",
        b"version: 1\ntags: 4\n",
        b"version: 1\ntags:\n  undeclared: maybe\n",
        b"version: 1\nfolders: {}\n",
        b"version: 1\nfolders:\n  - description: no path\n",
        b"version: 1\npaths: []\n",
        b"version: true\n",
        b"version: 1\nfields:\n  title: text\n",
    ] {
        // An empty file is a valid schema with no declarations; the rest refuse.
        let _ = VaultSchema::parse(bytes);
    }
}

// ---- the typing function ----

#[test]
fn an_undeclared_field_is_typed_as_the_text_it_is_written_as() {
    let schema = VaultSchema::parse(b"version: 1\n").expect("a bare schema");

    assert_eq!(schema.declared_type("anything"), FieldType::Text);
    assert_eq!(
        schema.typed("anything", "10").expect("text"),
        TypedValue::Text("10".to_string())
    );
}

#[test]
fn a_number_field_orders_numerically_rather_than_as_text() {
    let schema = VaultSchema::parse(WHOLE).expect("a whole schema");

    let two = schema.typed("rating", "2").expect("a number");
    let ten = schema.typed("rating", "10").expect("a number");
    assert_eq!(two.cmp(&ten), Ordering::Less);
    assert!(two.sort_key() < ten.sort_key());
    // The text reading of the same pair is the order the declaration replaces.
    assert_eq!("10".cmp("2"), Ordering::Less);
}

#[test]
fn a_number_field_orders_across_zero_and_the_sort_key_agrees() {
    let schema = VaultSchema::parse(WHOLE).expect("a whole schema");
    let written = ["-12.5", "-0.5", "0", "0.5", "12.5", "1000"];

    let values: Vec<TypedValue> = written
        .iter()
        .map(|raw| schema.typed("rating", raw).expect("a number"))
        .collect();
    for pair in values.windows(2) {
        assert_eq!(pair[0].cmp(&pair[1]), Ordering::Less, "{pair:?}");
        assert!(pair[0].sort_key() < pair[1].sort_key(), "{pair:?}");
    }
}

#[test]
fn a_date_field_orders_chronologically_rather_than_as_text() {
    let schema = VaultSchema::parse(WHOLE).expect("a whole schema");
    let written = [
        "1969-07-20",
        "2026-03-04",
        "2026-03-04T09:00:00",
        "2026-03-04T09:00:00Z",
        "2026-03-04T10:00:00+01:00",
        "2026-03-05",
    ];

    let values: Vec<TypedValue> = written
        .iter()
        .map(|raw| schema.typed("created", raw).expect("a date"))
        .collect();
    // `2026-03-04T09:00:00Z` and `2026-03-04T10:00:00+01:00` are one instant
    // written two ways, so the list is non-decreasing rather than strictly
    // increasing.
    for pair in values.windows(2) {
        assert!(pair[0].cmp(&pair[1]) != Ordering::Greater, "{pair:?}");
        assert!(pair[0].sort_key() <= pair[1].sort_key(), "{pair:?}");
    }
    assert_eq!(values[3].cmp(&values[4]), Ordering::Equal);
    // A date before the epoch sorts below one after it, which a text order of
    // the same strings also happens to give — so the case that separates the
    // two orders is the one below.
    assert_eq!(values[0].cmp(&values[1]), Ordering::Less);
}

#[test]
fn a_date_orders_chronologically_where_its_text_does_not() {
    let schema = VaultSchema::parse(WHOLE).expect("a whole schema");

    let noon = schema
        .typed("created", "2026-03-04T12:00:00+05:00")
        .expect("a date");
    let evening = schema
        .typed("created", "2026-03-04T09:00:00Z")
        .expect("a date");
    // The first string sorts after the second as text and before it in time.
    assert_eq!(
        "2026-03-04T12:00:00+05:00".cmp("2026-03-04T09:00:00Z"),
        Ordering::Greater
    );
    assert_eq!(noon.cmp(&evening), Ordering::Less);
}

#[test]
fn a_comparison_between_a_stated_and_an_unstated_offset_is_signalled() {
    let schema = VaultSchema::parse(WHOLE).expect("a whole schema");
    let stated = schema
        .typed("created", "2026-03-04T09:00:00Z")
        .expect("a date");
    let unstated = schema
        .typed("created", "2026-03-04T09:00:00")
        .expect("a date");

    assert_eq!(
        stated.compare(&unstated),
        Comparison {
            ordering: Ordering::Equal,
            signal: Some(ComparisonSignal::MixedOffset),
        }
    );
    assert_eq!(
        unstated.compare(&stated).signal,
        Some(ComparisonSignal::MixedOffset)
    );
}

/// Two stated offsets are two instants, and two unstated readings are two
/// readings of one clock. Neither pair assumes a zone, so neither is signalled.
#[test]
fn a_comparison_between_two_alike_offsets_is_not_signalled() {
    let schema = VaultSchema::parse(WHOLE).expect("a whole schema");
    let read = |raw: &str| schema.typed("created", raw).expect("a date");

    assert_eq!(
        read("2026-03-04T09:00:00Z")
            .compare(&read("2026-03-04T10:00:00+02:00"))
            .signal,
        None
    );
    assert_eq!(
        read("2026-03-04")
            .compare(&read("2026-03-05T09:00:00"))
            .signal,
        None
    );
}

#[test]
fn a_date_records_whether_its_written_form_stated_an_offset() {
    let schema = VaultSchema::parse(WHOLE).expect("a whole schema");
    let offset = |raw: &str| match schema.typed("created", raw).expect("a date") {
        TypedValue::Date(date) => date.offset(),
        other => panic!("a date field read as {other:?}"),
    };

    assert_eq!(offset("2026-03-04"), Offset::Unstated);
    assert_eq!(offset("2026-03-04T09:00:00"), Offset::Unstated);
    assert_eq!(offset("2026-03-04T09:00:00Z"), Offset::Stated(0));
    assert_eq!(offset("2026-03-04T09:00:00+02:30"), Offset::Stated(150));
    assert_eq!(offset("2026-03-04T09:00:00-08:00"), Offset::Stated(-480));
}

#[test]
fn a_value_that_is_not_its_declared_type_is_a_refusal_a_caller_decides_about() {
    let schema = VaultSchema::parse(WHOLE).expect("a whole schema");

    assert_eq!(
        schema
            .typed("rating", "high")
            .expect_err("not a number")
            .declared,
        FieldType::Number
    );
    assert_eq!(
        schema
            .typed("created", "soon")
            .expect_err("not a date")
            .declared,
        FieldType::Date
    );
    assert_eq!(
        schema
            .typed("draft", "yes")
            .expect_err("not a boolean")
            .declared,
        FieldType::Boolean
    );
    // A number that is not finite is not a number this order can hold.
    assert!(schema.typed("rating", "NaN").is_err());
    assert!(schema.typed("rating", "inf").is_err());
}

#[test]
fn a_boolean_field_orders_false_before_true() {
    let schema = VaultSchema::parse(WHOLE).expect("a whole schema");
    let no = schema.typed("draft", "false").expect("a boolean");
    let yes = schema.typed("draft", "true").expect("a boolean");

    assert_eq!(no.cmp(&yes), Ordering::Less);
    assert!(no.sort_key() < yes.sort_key());
}

// ---- the pattern language ----

#[test]
fn a_pattern_matches_the_set_its_grammar_names() {
    let cases: &[(&str, &str, bool)] = &[
        ("archive", "archive", true),
        ("archive", "archive/notes.md", false),
        ("archive/**", "archive/notes.md", true),
        ("archive/**", "archive/deep/notes.md", true),
        ("archive/**", "archive/", true),
        ("archive/**", "archive", true),
        ("archive/*", "archive/notes.md", true),
        ("archive/*", "archive/deep/notes.md", false),
        ("**/notes.md", "notes.md", true),
        ("**/notes.md", "a/b/notes.md", true),
        ("note?.md", "note1.md", true),
        ("note?.md", "note.md", false),
        ("*.md", "notes.md", true),
        ("*.md", "a/notes.md", false),
        ("person/**", "person/ada", true),
        ("person/**", "person", true),
    ];

    for (pattern, subject, expected) in cases {
        let pattern = Pattern::parse(pattern).expect("a pattern");
        assert_eq!(
            pattern.matches(subject),
            *expected,
            "{pattern} vs {subject}"
        );
    }
}

#[test]
fn a_pattern_folds_no_case() {
    let pattern = Pattern::parse("Archive/**").expect("a pattern");

    assert!(pattern.matches("Archive/notes.md"));
    assert!(!pattern.matches("archive/notes.md"));
}
