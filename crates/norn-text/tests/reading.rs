//! Reading a document: where the frontmatter block is, what it is worth, and
//! what reading it had to work around.
//!
//! Reading is forgiving by contract — a block that cannot be parsed yields a
//! diagnostic and a usable body, never an error return — so most of what this
//! file states is which diagnostic, and what survives beside it.

use norn_text::{Document, LineEnding, Value, ValueStyle};

fn codes<'a>(document: &'a Document<'a>) -> Vec<&'a str> {
    document
        .diagnostics()
        .iter()
        .map(|diagnostic| diagnostic.code.as_str())
        .collect()
}

fn map_of(source: &str) -> norn_text::Mapping {
    match Document::parse(source).frontmatter() {
        Some(Value::Map(map)) => map.clone(),
        other => panic!("expected a mapping, got {other:?}"),
    }
}

// ── The block, the body, and the boundary between them ───────────────────

#[test]
fn a_document_without_frontmatter_is_all_body() {
    let document = Document::parse("# heading\n");
    assert_eq!(document.frontmatter(), None);
    assert_eq!(document.frontmatter_range(), None);
    assert_eq!(document.body(), "# heading\n");
    assert_eq!(document.body_start(), 0);
    assert!(document.diagnostics().is_empty());
}

#[test]
fn the_block_range_covers_the_yaml_and_the_body_starts_past_the_closing_fence() {
    let source = "---\ntitle: hello\n---\n# heading\n";
    let document = Document::parse(source);
    let range = document.frontmatter_range().expect("a block");
    assert_eq!(&source[range], "title: hello\n");
    assert_eq!(document.body(), "# heading\n");
    assert_eq!(&source[document.body_start()..], "# heading\n");
    assert!(document.diagnostics().is_empty());
}

#[test]
fn a_crlf_document_reads_as_crlf() {
    let source = "---\r\ntitle: hello\r\n---\r\n# heading\r\n";
    let document = Document::parse(source);
    assert_eq!(document.line_ending(), LineEnding::Crlf);
    assert_eq!(
        document.frontmatter(),
        Some(&Value::Map([("title", "hello")].into_iter().collect()))
    );
    assert_eq!(document.body(), "# heading\r\n");
}

#[test]
fn a_document_with_no_line_break_at_all_reads_as_lf() {
    assert_eq!(
        Document::parse("no breaks here").line_ending(),
        LineEnding::Lf
    );
}

/// A fence carrying trailing spaces or tabs is a fence, at both ends. An
/// editor that trims neither writes both, and a fence this reader does not
/// recognize is not a smaller contract but a corrupting one: the fields go
/// into a second block synthesized above the real one.
#[test]
fn a_fence_with_trailing_whitespace_is_still_a_fence() {
    for (source, label) in [
        ("---   \ntitle: hello\n---\nbody\n", "loose opener"),
        ("---\ttitle\n", "not a fence at all"),
        ("---\ntitle: hello\n---   \nbody\n", "loose closer"),
        (
            "---\t\ntitle: hello\n---\t\nbody\n",
            "both loose, with tabs",
        ),
        (
            "---\r\ntitle: hello\r\n--- \r\nbody\r\n",
            "loose CRLF closer",
        ),
    ] {
        let document = Document::parse(source);
        if label == "not a fence at all" {
            assert_eq!(document.frontmatter(), None, "{label}");
            continue;
        }
        assert_eq!(
            document.frontmatter(),
            Some(&Value::Map([("title", "hello")].into_iter().collect())),
            "{label}"
        );
        assert!(document.diagnostics().is_empty(), "{label}");
        assert!(document.body().starts_with("body"), "{label}");
    }
    // Four dashes is not a fence, and neither is a fence with anything but
    // whitespace after it.
    for source in [
        "----\ntitle: hello\n---\n",
        "--- x\ntitle: hello\n---\n",
        "---\ntitle: hello\n--- x\n",
    ] {
        assert!(
            !matches!(Document::parse(source).frontmatter(), Some(Value::Map(_))),
            "{source:?} is not a block"
        );
    }
}

#[test]
fn an_unclosed_block_is_diagnosed_and_the_whole_document_stays_body() {
    let source = "---\ntitle: hello\n# heading (no close)\n";
    let document = Document::parse(source);
    assert_eq!(codes(&document), ["frontmatter-unclosed"]);
    assert_eq!(document.frontmatter(), None);
    assert_eq!(document.frontmatter_range(), None);
    assert_eq!(document.body(), source);
    assert_eq!(document.body_start(), 0);
}

#[test]
fn malformed_yaml_is_diagnosed_and_still_reports_where_the_block_was() {
    let document = Document::parse("---\ntitle: : :\n---\n# body\n");
    assert_eq!(codes(&document), ["frontmatter-parse-failed"]);
    assert_eq!(document.frontmatter(), None);
    assert_eq!(document.frontmatter_range(), Some(4..15));
    assert_eq!(document.body(), "# body\n");
    // A parse failure is a state of its own, not an absence: the diagnostic
    // carries the parser's own account of what went wrong.
    assert!(document.diagnostics()[0].detail.is_some());
}

/// NRN-371, NRN-114: a document norn itself creates has an empty block, and it
/// must stay mutable. An empty block is null with a range, not an absent one.
#[test]
fn an_empty_block_reads_as_null_with_a_range() {
    let source = "---\n---\n# body\n";
    let document = Document::parse(source);
    assert_eq!(document.frontmatter(), Some(&Value::Null));
    assert_eq!(document.frontmatter_range(), Some(4..4));
    assert_eq!(document.body(), "# body\n");
    assert_eq!(document.body_start(), 8);
    assert!(document.diagnostics().is_empty());
}

#[test]
fn a_block_holding_a_sequence_reads_as_a_sequence_and_has_no_fields() {
    let document = Document::parse("---\n- one\n- two\n---\n# body\n");
    assert_eq!(
        document.frontmatter(),
        Some(&Value::Sequence(vec!["one".into(), "two".into()]))
    );
    assert!(document.fields().is_empty());
    assert!(document.diagnostics().is_empty());
}

// ── The byte-order mark (NRN-349, NRN-385) ───────────────────────────────

#[test]
fn a_byte_order_mark_does_not_hide_the_block() {
    let source = "\u{feff}---\ntitle: hello\n---\n# body\n";
    let document = Document::parse(source);
    assert!(document.has_byte_order_mark());
    assert_eq!(
        document.frontmatter(),
        Some(&Value::Map([("title", "hello")].into_iter().collect()))
    );
    // Offsets are content-absolute, mark included, so a splice computed from
    // them lands where it was measured.
    let range = document.frontmatter_range().expect("a block");
    assert_eq!(range, 7..20);
    assert_eq!(&source[range], "title: hello\n");
    assert_eq!(&source[document.body_start()..], "# body\n");
    assert!(document.diagnostics().is_empty());
}

#[test]
fn a_byte_order_mark_alone_does_not_invent_a_block() {
    let source = "\u{feff}# just a heading\n";
    let document = Document::parse(source);
    assert!(document.has_byte_order_mark());
    assert_eq!(document.frontmatter(), None);
    assert_eq!(document.frontmatter_range(), None);
    assert_eq!(document.body(), source);
    assert_eq!(document.body_start(), 0);
}

// ── The value model, and what it strips at the boundary ──────────────────

#[test]
fn the_seven_shapes_read_as_themselves() {
    let map = map_of(
        "---\nnothing:\nflag: true\ncount: 3\nratio: 1.5\nname: text\nlist:\n  - a\nnested:\n  \
         inner: x\n---\n",
    );
    assert_eq!(map.get("nothing"), Some(&Value::Null));
    assert_eq!(map.get("flag"), Some(&Value::Bool(true)));
    assert_eq!(map.get("count"), Some(&Value::Int(3)));
    assert_eq!(map.get("ratio"), Some(&Value::Float(1.5)));
    assert_eq!(map.get("name"), Some(&Value::String("text".into())));
    assert_eq!(map.get("list"), Some(&Value::Sequence(vec!["a".into()])));
    assert_eq!(
        map.get("nested"),
        Some(&Value::Map([("inner", "x")].into_iter().collect()))
    );
}

#[test]
fn a_mapping_keeps_the_order_the_document_wrote() {
    let map = map_of("---\nzebra: 1\nalpha: 2\nmiddle: 3\n---\n");
    assert_eq!(map.keys().collect::<Vec<_>>(), ["zebra", "alpha", "middle"]);
}

/// A non-string key has no addressable form, so its entry is dropped rather
/// than coerced into a field the document does not contain. The block's spans
/// go with it: the scanner still sees the line the value model no longer
/// names, and absorbing that line into a neighbour is how an unrelated remove
/// deletes it.
#[test]
fn a_non_string_key_is_dropped_and_makes_the_block_uneditable() {
    let document = Document::parse("---\n1: x\ntrue: y\nname: z\n---\n");
    assert_eq!(
        document.frontmatter(),
        Some(&Value::Map([("name", "z")].into_iter().collect()))
    );
    assert_eq!(
        codes(&document),
        [
            "frontmatter-non-string-key",
            "frontmatter-non-string-key",
            "frontmatter-not-editable"
        ]
    );
    assert!(document.fields().is_empty());
}

#[test]
fn a_sequence_key_and_a_null_key_are_dropped_without_losing_the_rest() {
    let document = Document::parse("---\n[a, b]: x\n~: v\nname: z\n---\n");
    assert_eq!(
        document.frontmatter(),
        Some(&Value::Map([("name", "z")].into_iter().collect()))
    );
    assert_eq!(
        document
            .diagnostics()
            .iter()
            .filter(|d| d.code == "frontmatter-non-string-key")
            .count(),
        2
    );
}

/// An explicit tag is dropped and its value kept. The tag's bytes are never
/// rewritten either: a value opening with `!` carries no value span, so no
/// edit can splice over the marker.
#[test]
fn an_explicit_tag_is_stripped_and_its_value_kept() {
    let document = Document::parse("---\na: !foo bar\n---\n");
    assert_eq!(
        document.frontmatter(),
        Some(&Value::Map([("a", "bar")].into_iter().collect()))
    );
    assert_eq!(codes(&document), ["frontmatter-tag-stripped"]);
    let field = document.field("a").expect("the key stays visible");
    assert_eq!(field.value_range, None);
}

/// A tag the YAML parser itself resolves — `!!str` and the other core schema
/// tags — never reaches the value model, so nothing is reported for it.
#[test]
fn a_core_schema_tag_is_resolved_by_the_parser_and_reported_by_nobody() {
    let document = Document::parse("---\na: !!str 5\n---\n");
    assert_eq!(
        document.frontmatter(),
        Some(&Value::Map([("a", "5")].into_iter().collect()))
    );
    assert!(document.diagnostics().is_empty());
}

#[test]
fn an_integer_past_i64_is_carried_as_a_float_and_said_so() {
    let document = Document::parse("---\na: 18446744073709551615\n---\n");
    assert_eq!(codes(&document), ["frontmatter-integer-out-of-range"]);
    let map = map_of("---\na: 18446744073709551615\n---\n");
    assert_eq!(map.get("a"), Some(&Value::Float(18446744073709551615.0)));
}

#[test]
fn the_non_finite_floats_survive_as_floats() {
    let map = map_of("---\na: .nan\nb: .inf\nc: -.inf\n---\n");
    assert!(matches!(map.get("a"), Some(Value::Float(f)) if f.is_nan()));
    assert_eq!(map.get("b"), Some(&Value::Float(f64::INFINITY)));
    assert_eq!(map.get("c"), Some(&Value::Float(f64::NEG_INFINITY)));
}

/// Duplicate keys are not a value-model question. The block is not
/// well-formed YAML, so it does not parse and there is no value to strip
/// anything from.
#[test]
fn duplicate_keys_are_a_parse_failure_not_a_silent_last_wins() {
    let document = Document::parse("---\na: 1\na: 2\n---\n");
    assert_eq!(codes(&document), ["frontmatter-parse-failed"]);
    assert_eq!(document.frontmatter(), None);
}

/// An anchor and its alias are expanded by the parser before a value exists,
/// so the model holds the expansion and there is nothing to strip. The marker
/// bytes are still never rewritten: neither value carries a span.
#[test]
fn an_anchor_and_its_alias_read_as_the_expanded_value() {
    let source = "---\na: &x 1\nb: *x\n---\n";
    let document = Document::parse(source);
    assert_eq!(
        document.frontmatter(),
        Some(&Value::Map(
            [("a", Value::Int(1)), ("b", Value::Int(1))]
                .into_iter()
                .collect()
        ))
    );
    assert!(document.diagnostics().is_empty());
    assert_eq!(document.field("a").expect("a").value_range, None);
    assert_eq!(document.field("b").expect("b").value_range, None);
}

/// A merge key is a directive, not a field. It is expanded before a value
/// exists, so the model holds the merged mapping and `<<` is never a key in
/// it — carrying one would be a field naming a document construct rather than
/// a document's data.
#[test]
fn a_merge_key_is_expanded_and_never_becomes_a_field() {
    let source = "---\n<<: {a: 1}\nb: 2\n---\n";
    let document = Document::parse(source);
    let map = map_of(source);
    assert!(!map.contains_key("<<"));
    assert_eq!(map.get("a"), Some(&Value::Int(1)));
    assert_eq!(map.get("b"), Some(&Value::Int(2)));
    assert!(
        !document.fields().iter().any(|field| field.name == "<<"),
        "the merge key is not a field"
    );
    // Expansion itself is silent: the only thing said about the block is that
    // its per-field split cannot be trusted, which is why every field edit in
    // it refuses.
    assert_eq!(codes(&document), ["frontmatter-not-editable"]);
}

/// An alias merge expands the same way, at any depth.
#[test]
fn a_merge_key_expands_through_an_alias_and_inside_a_nested_mapping() {
    let map = map_of("---\nbase: &b\n  a: 1\nouter:\n  <<: *b\n  c: 3\n---\n");
    assert_eq!(
        map.get("outer"),
        Some(&Value::Map(
            [("c", Value::Int(3)), ("a", Value::Int(1))]
                .into_iter()
                .collect()
        ))
    );
}

/// A `<<` naming something that is not a mapping is not a merge, and there is
/// no honest reading of it: expanding is impossible and carrying it as a field
/// is the phantom the expansion exists to prevent. The block is refused.
#[test]
fn a_merge_key_that_names_no_mapping_refuses_the_block() {
    let document = Document::parse("---\n<<: 1\ntitle: t\n---\n");
    assert_eq!(codes(&document), ["frontmatter-parse-failed"]);
    assert_eq!(document.frontmatter(), None);
}

/// A merge line belongs to no field, so no field's range may absorb it. It
/// bounds the entry above it instead, and the block it sits in refuses every
/// field edit — reading is unaffected and the bytes round-trip.
#[test]
fn a_merge_line_is_never_absorbed_into_a_neighbouring_field() {
    // The merge contributes nothing new here, so every parsed key is locatable
    // and the entry above the merge line stops at it rather than swallowing it.
    let source = "---\nbase: &b {title: x}\n<<: *b\ntitle: t\n---\n";
    let document = Document::parse(source);
    let base = document.field("base").expect("base is a field");
    assert_eq!(&source[base.line_range.clone()], "base: &b {title: x}\n");
    assert!(!document.fields().iter().any(|field| field.name == "<<"));
}

// ── Field spans ──────────────────────────────────────────────────────────

fn styles(source: &str) -> Vec<(String, ValueStyle, Option<String>)> {
    Document::parse(source)
        .fields()
        .iter()
        .map(|field| {
            (
                field.name.clone(),
                field.style,
                field
                    .value_range
                    .clone()
                    .map(|range| source[range].to_string()),
            )
        })
        .collect()
}

#[test]
fn a_scalar_value_span_holds_exactly_the_value_bytes() {
    let source = "---\nplain: hello world\nsingle: 'it''s'\ndouble: \"a\\tb\"\n---\n";
    assert_eq!(
        styles(source),
        [
            (
                "plain".to_string(),
                ValueStyle::Plain,
                Some("hello world".to_string())
            ),
            (
                "single".to_string(),
                ValueStyle::SingleQuoted,
                Some("'it''s'".to_string())
            ),
            (
                "double".to_string(),
                ValueStyle::DoubleQuoted,
                Some("\"a\\tb\"".to_string())
            ),
        ]
    );
}

#[test]
fn a_plain_value_span_stops_before_a_trailing_comment_and_its_padding() {
    let source = "---\ntitle: hello   # a note\n---\n";
    assert_eq!(
        styles(source),
        [(
            "title".to_string(),
            ValueStyle::Plain,
            Some("hello".to_string())
        )]
    );
}

#[test]
fn a_hash_that_is_not_preceded_by_space_is_part_of_the_value() {
    let source = "---\ntitle: a#b\n---\n";
    assert_eq!(
        styles(source),
        [(
            "title".to_string(),
            ValueStyle::Plain,
            Some("a#b".to_string())
        )]
    );
}

#[test]
fn a_value_holding_a_colon_is_spanned_precisely() {
    let source = "---\ntime: 12:30\n---\n";
    assert_eq!(
        styles(source),
        [(
            "time".to_string(),
            ValueStyle::Plain,
            Some("12:30".to_string())
        )]
    );
}

#[test]
fn a_multibyte_value_slices_to_the_whole_string() {
    let source = "---\ntitle: héllo wörld\n---\n";
    assert_eq!(
        styles(source),
        [(
            "title".to_string(),
            ValueStyle::Plain,
            Some("héllo wörld".to_string())
        )]
    );
}

/// Every structural style that cannot be named as one span on the key line
/// declines a value span and keeps its key visible, so a whole-entry remove
/// still works.
#[test]
fn structural_values_decline_a_value_span() {
    for (source, expected) in [
        ("---\nk: |\n  literal\n---\n", ValueStyle::BlockLiteral),
        ("---\nk: |+\n  literal\n---\n", ValueStyle::BlockLiteral),
        ("---\nk: >\n  folded\n---\n", ValueStyle::BlockFolded),
        ("---\nk: >-\n  folded\n---\n", ValueStyle::BlockFolded),
        ("---\nk: [a, b]\n---\n", ValueStyle::FlowSequence),
        ("---\nk: {a: 1}\n---\n", ValueStyle::FlowMapping),
        ("---\nk:\n  - a\n---\n", ValueStyle::BlockSequence),
        ("---\nk:\n  a: 1\n---\n", ValueStyle::BlockMapping),
    ] {
        let document = Document::parse(source);
        let field = document.field("k").expect("the key stays visible");
        assert_eq!(field.style, expected, "for {source:?}");
        assert_eq!(field.value_range, None, "for {source:?}");
        assert_eq!(
            &source[field.line_range.clone()],
            &source[4..source.len() - 4]
        );
    }
}

/// A literal null token is editable in place; a `# comment` re-parses to null
/// and must not be mistaken for one.
#[test]
fn a_null_token_is_editable_and_a_comment_that_reads_as_null_is_not() {
    let source = "---\nexplicit: ~\ncommented: # nothing here\n---\n";
    let document = Document::parse(source);
    let explicit = document.field("explicit").expect("explicit");
    assert_eq!(
        explicit.value_range.clone().map(|range| &source[range]),
        Some("~")
    );
    // A key followed only by a comment is a stub, not a field whose value is
    // the comment. The insertion point sits between the colon and the comment,
    // so writing the field leaves the comment standing.
    let commented = document.field("commented").expect("commented");
    assert_eq!(commented.style, ValueStyle::EmptyValue);
    let range = commented.value_range.clone().expect("an insertion point");
    assert_eq!(&source[range.clone()], "");
    assert_eq!(&source[..range.start], "---\nexplicit: ~\ncommented:");
    assert_eq!(
        Document::parse(source).set_field("commented", &Value::String("draft".into())),
        Ok("---\nexplicit: ~\ncommented: draft # nothing here\n---\n".to_string())
    );
}

#[test]
fn a_quoted_or_numeric_key_is_visible_under_the_name_the_parser_uses() {
    for (source, name) in [
        ("---\n\"key with spaces\": 1\n---\n", "key with spaces"),
        ("---\n'it''s': 1\n---\n", "it's"),
        ("---\n123: 1\n---\n", "123"),
    ] {
        let document = Document::parse(source);
        // A numeric key is dropped by the value model, so only the quoted keys
        // survive as fields; both spellings must at least agree with the
        // parser about the name.
        if let Some(Value::Map(map)) = document.frontmatter()
            && map.contains_key(name)
        {
            assert!(document.field(name).is_some(), "for {source:?}");
        }
    }
}

#[test]
fn a_column_zero_comment_is_not_a_key() {
    let document = Document::parse("---\n# not: a key\ntitle: hello\n---\n");
    assert_eq!(
        document
            .fields()
            .iter()
            .map(|field| field.name.as_str())
            .collect::<Vec<_>>(),
        ["title"]
    );
}

#[test]
fn an_indented_line_is_not_a_top_level_key() {
    let document = Document::parse("---\nouter:\n  inner: 1\n---\n");
    assert_eq!(
        document
            .fields()
            .iter()
            .map(|field| field.name.as_str())
            .collect::<Vec<_>>(),
        ["outer"]
    );
}
