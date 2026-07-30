//! The quoting ladder: every emitted scalar reads back as itself, in the
//! context it was emitted into.
//!
//! NRN-118, NRN-141, NRN-142. The shape every case here forbids is an emission
//! judged by a list of hazardous shapes: the list was wrong in four confirmed
//! ways, and each way silently changed what a document said. Correctness is
//! decided by re-parsing instead — and where no style re-parses, the emission
//! refuses.

use norn_text::{
    Document, EditError, LineEnding, Mapping, RenderError, ScalarContext, Value,
    frontmatter_reads_back, render_document,
};

/// Write `text` as a block value and read the whole document back.
fn round_trip_block(text: &str) -> Value {
    let edited = Document::parse("---\nfield: seed\nafter: kept\n---\nbody\n")
        .set_field("field", &Value::String(text.to_string()))
        .unwrap_or_else(|error| panic!("emitting {text:?} as a block value: {error}"));
    let reread = Document::parse(&edited);
    // Nothing else in the block may be disturbed by a hostile value.
    let map = reread
        .frontmatter()
        .and_then(Value::as_map)
        .unwrap_or_else(|| panic!("emitting {text:?} left the block unparseable: {edited:?}"))
        .clone();
    assert_eq!(
        map.get("after"),
        Some(&Value::String("kept".into())),
        "emitting {text:?} disturbed a neighbouring field"
    );
    map.get("field").cloned().expect("the edited field")
}

/// Write `text` as the sole item of a flow sequence and read it back.
fn round_trip_flow(text: &str) -> Value {
    let edited = Document::parse("---\nitems: [seed]\nafter: kept\n---\n")
        .set_field(
            "items",
            &Value::Sequence(vec![Value::String(text.to_string())]),
        )
        .unwrap_or_else(|error| panic!("emitting {text:?} as a flow item: {error}"));
    let reread = Document::parse(&edited);
    let map = reread
        .frontmatter()
        .and_then(Value::as_map)
        .unwrap_or_else(|| panic!("emitting {text:?} left the block unparseable: {edited:?}"))
        .clone();
    assert_eq!(
        map.get("after"),
        Some(&Value::String("kept".into())),
        "emitting {text:?} disturbed a neighbouring field"
    );
    match map.get("items") {
        Some(Value::Sequence(items)) if items.len() == 1 => items[0].clone(),
        other => panic!("emitting {text:?} as a flow item produced {other:?}"),
    }
}

/// Write `key` as a field name and read the whole document back.
fn round_trip_key(key: &str) -> Mapping {
    let fields: Mapping = [(key.to_string(), Value::String("value".into()))]
        .into_iter()
        .collect();
    let rendered = render_document(&fields, "body\n", LineEnding::Lf)
        .unwrap_or_else(|error| panic!("emitting {key:?} as a key: {error}"));
    Document::parse(&rendered)
        .frontmatter()
        .and_then(Value::as_map)
        .unwrap_or_else(|| panic!("emitting {key:?} as a key left the block unparseable"))
        .clone()
}

/// The whole document a block-context emission produces.
fn block_document(source: &str, field: &str, value: &Value) -> String {
    Document::parse(source)
        .set_field(field, value)
        .unwrap_or_else(|error| panic!("emitting {value:?} into {source:?}: {error}"))
}

/// The whole document a flow-item emission produces.
fn flow_document(value: &Value) -> String {
    Document::parse("---\nitems: [seed]\n---\nbody\n")
        .set_field("items", &Value::Sequence(vec![value.clone()]))
        .unwrap_or_else(|error| panic!("emitting {value:?} as a flow item: {error}"))
}

/// The whole document a key emission produces.
fn key_document(key: &str) -> String {
    let fields: Mapping = [(key.to_string(), Value::String("value".into()))]
        .into_iter()
        .collect();
    render_document(&fields, "body\n", LineEnding::Lf)
        .unwrap_or_else(|error| panic!("emitting {key:?} as a key: {error}"))
}

// ── Block context (NRN-118) ──────────────────────────────────────────────

/// Every one of these was emitted plain by the denylist and read back as
/// something else.
#[test]
fn hostile_block_values_read_back_as_themselves() {
    for text in [
        // A trailing bare colon: `k: note:` is a nested mapping, not a string.
        "note:",
        "a: b",
        // Newlines and carriage returns fold to a space inside single quotes.
        "one\ntwo",
        "one\r\ntwo",
        "\r",
        // Numeric- and boolean-looking strings stop being strings.
        "123",
        "1.5",
        "true",
        "false",
        "yes",
        "no",
        "on",
        "off",
        "null",
        "~",
        "0x1f",
        "1e3",
        "12:30",
        "2024-01-01",
        // Leading indicators.
        "#comment",
        "- item",
        "? key",
        "*alias",
        "&anchor",
        "!tag",
        "|literal",
        ">folded",
        "%directive",
        "@reserved",
        "`backtick",
        "[bracket",
        "{brace",
        ",comma",
        // Quotes of both kinds, and both at once.
        "it's",
        "say \"hi\"",
        "it's a \"quote\"",
        "''",
        "\"\"",
        // Whitespace at the edges, and nothing at all.
        " leading",
        "trailing ",
        "\ttab",
        "",
        "   ",
        // Comment-looking interiors.
        "value # not a comment",
        "value#nothash",
        // Non-ASCII, including the separators YAML folds if left bare.
        "héllo wörld",
        "日本語",
        "emoji \u{1f600}",
        "next\u{85}line",
        "line\u{2028}separator",
        "paragraph\u{2029}separator",
        // Control characters, which YAML forbids literally inside quotes.
        "bell\u{07}",
        "null byte\u{0}",
        "escape\u{1b}[0m",
        "vertical\u{0b}tab",
        "form\u{0c}feed",
        "backspace\u{08}",
        "delete\u{7f}",
        // Backslashes, which the double-quoted terminal has to escape.
        "back\\slash",
        "\\n literal",
        "trailing backslash\\",
    ] {
        assert_eq!(
            round_trip_block(text),
            Value::String(text.to_string()),
            "block value {text:?}"
        );
    }
}

/// NRN-141's flow trap. A flow item verified only in block context mis-splits
/// on a comma, and a bracket makes the whole document unparseable — which
/// takes every field in the block with it, not just the edited one.
#[test]
fn hostile_flow_items_read_back_as_themselves() {
    for text in [
        "a,b",
        "a]b",
        "a[b",
        "a}b",
        "a{b",
        "a: b",
        "note:",
        "123",
        "true",
        "",
        " padded ",
        "it's",
        "say \"hi\"",
        "one\ntwo",
        "#comment",
        "value # not a comment",
        "héllo",
        "control\u{07}",
    ] {
        assert_eq!(
            round_trip_flow(text),
            Value::String(text.to_string()),
            "flow item {text:?}"
        );
    }
}

/// NRN-142's key trap. A bare `#foo` key turns its line into a comment, a bare
/// `a: b` key splits the line into nested mappings, and `123`, `true` and
/// `null` stop being strings.
#[test]
fn hostile_keys_read_back_as_themselves() {
    for key in [
        "#foo",
        "a: b",
        "key:",
        "123",
        "1.5",
        "true",
        "false",
        "null",
        "~",
        "yes",
        "off",
        "- dash",
        "? question",
        "&anchor",
        "*alias",
        "!tag",
        "[bracket",
        "{brace",
        ",comma",
        "|pipe",
        ">gt",
        "%percent",
        "@at",
        "`tick",
        "'single'",
        "\"double\"",
        " leading",
        "trailing ",
        "with space",
        "with\ttab",
        "with\nnewline",
        "héllo",
        "日本語",
        "control\u{07}",
        "back\\slash",
        "---",
        "...",
    ] {
        let map = round_trip_key(key);
        assert_eq!(map.keys().collect::<Vec<_>>(), [key], "key {key:?}");
        assert_eq!(map.get(key), Some(&Value::String("value".into())));
    }
}

/// A key so long no parser will read it back defeats every rank, so the
/// emission refuses. Returning the double-quoted render unverified is what
/// this forbids: it produces a document nothing can parse.
#[test]
fn a_key_no_parser_will_read_back_refuses_rather_than_emitting_unproven_bytes() {
    let key = "k".repeat(2000);
    let fields: Mapping = [(key.clone(), Value::Int(1))].into_iter().collect();
    assert_eq!(
        render_document(&fields, "", LineEnding::Lf),
        Err(RenderError::NotRoundTrippable {
            text: key.clone(),
            context: ScalarContext::Key,
        })
    );
    // The same refusal reaches an edit that would have to write the key.
    assert!(matches!(
        Document::parse("---\ntitle: t\n---\n").set_field(&key, &Value::Int(1)),
        Err(EditError::Render(RenderError::NotRoundTrippable {
            context: ScalarContext::Key,
            ..
        }))
    ));
}

// ── Minimal, and never a downgrade ───────────────────────────────────────

#[test]
fn a_value_that_needs_no_quoting_gets_none() {
    let plain = |value: &str| block_document("---\ntitle: seed\n---\n", "title", &value.into());
    assert_eq!(plain("hello world"), "---\ntitle: hello world\n---\n");
    // A single quote inside a value is not a hazard in block context, so the
    // value stays plain rather than escalating on the strength of a character.
    assert_eq!(plain("it's fine"), "---\ntitle: it's fine\n---\n");
}

#[test]
fn an_explicit_quote_style_is_a_floor() {
    assert_eq!(
        block_document(
            "---\ntitle: 'seed'\n---\n",
            "title",
            &"plain would do".into()
        ),
        "---\ntitle: 'plain would do'\n---\n"
    );
    assert_eq!(
        block_document(
            "---\ntitle: \"seed\"\n---\n",
            "title",
            &"plain would do".into()
        ),
        "---\ntitle: \"plain would do\"\n---\n"
    );
    // Single-quoted stays single where it can, and climbs when it cannot: a
    // value carrying a newline cannot be single-quoted, because the newline
    // folds to a space.
    assert_eq!(
        block_document("---\ntitle: 'seed'\n---\n", "title", &"one\ntwo".into()),
        "---\ntitle: \"one\\ntwo\"\n---\n"
    );
}

#[test]
fn escalation_climbs_no_further_than_it_must() {
    let plain = |value: &str| block_document("---\ntitle: seed\n---\n", "title", &value.into());
    // Plain fails on the trailing colon; single quotes are enough.
    assert_eq!(plain("note:"), "---\ntitle: 'note:'\n---\n");
    // Single quotes cannot hold a newline, so this one reaches the terminal.
    assert_eq!(plain("one\ntwo"), "---\ntitle: \"one\\ntwo\"\n---\n");
    // A plain identifier key needs no quoting at all.
    let fields: Mapping = [("status", "draft")].into_iter().collect();
    assert_eq!(
        render_document(&fields, "", LineEnding::Lf),
        Ok("---\nstatus: draft\n---\n".to_string())
    );
}

// ── The golden emission table (NORN-25) ──────────────────────────────────

/// Every row is a value, and the exact document emitting it produces in each
/// of the three contexts.
///
/// The rest of this file asks whether a value reads back through this crate,
/// which is a question this crate answers about itself: swap the parser behind
/// the seam and every emitted byte could change while every assertion stayed
/// green. These rows are the bytes, written down. They are also the YAML-1.1
/// trap list — the spellings whose meaning moved between dialects — so a
/// resolution change shows up here as a diff rather than as silence.
#[test]
fn the_dialect_traps_emit_exactly_these_bytes() {
    // (value, block document, flow document, key document)
    let rows: &[(&str, &str, &str, &str)] = &[
        // YAML 1.1 read these as booleans. Under the 1.2 core schema they are
        // strings, and a string that reads back as itself stays plain.
        (
            "yes",
            "---\nfield: yes\n---\nbody\n",
            "---\nitems: [yes]\n---\nbody\n",
            "---\nyes: value\n---\nbody\n",
        ),
        (
            "no",
            "---\nfield: no\n---\nbody\n",
            "---\nitems: [no]\n---\nbody\n",
            "---\nno: value\n---\nbody\n",
        ),
        (
            "on",
            "---\nfield: on\n---\nbody\n",
            "---\nitems: [on]\n---\nbody\n",
            "---\non: value\n---\nbody\n",
        ),
        (
            "off",
            "---\nfield: off\n---\nbody\n",
            "---\nitems: [off]\n---\nbody\n",
            "---\noff: value\n---\nbody\n",
        ),
        (
            "y",
            "---\nfield: y\n---\nbody\n",
            "---\nitems: [y]\n---\nbody\n",
            "---\ny: value\n---\nbody\n",
        ),
        (
            "n",
            "---\nfield: n\n---\nbody\n",
            "---\nitems: [n]\n---\nbody\n",
            "---\nn: value\n---\nbody\n",
        ),
        (
            "Y",
            "---\nfield: Y\n---\nbody\n",
            "---\nitems: [Y]\n---\nbody\n",
            "---\nY: value\n---\nbody\n",
        ),
        // `true` in any casing the core schema does not name is still a
        // string, and one it does name escalates.
        (
            "True",
            "---\nfield: 'True'\n---\nbody\n",
            "---\nitems: ['True']\n---\nbody\n",
            "---\n'True': value\n---\nbody\n",
        ),
        // Sexagesimal, octal-by-leading-zero and digit separators: all three
        // were numbers in 1.1 and are strings now.
        (
            "12:34:56",
            "---\nfield: 12:34:56\n---\nbody\n",
            "---\nitems: [12:34:56]\n---\nbody\n",
            "---\n12:34:56: value\n---\nbody\n",
        ),
        (
            "0777",
            "---\nfield: 0777\n---\nbody\n",
            "---\nitems: [0777]\n---\nbody\n",
            "---\n0777: value\n---\nbody\n",
        ),
        (
            "1_000",
            "---\nfield: 1_000\n---\nbody\n",
            "---\nitems: [1_000]\n---\nbody\n",
            "---\n1_000: value\n---\nbody\n",
        ),
        (
            "2026-07-29",
            "---\nfield: 2026-07-29\n---\nbody\n",
            "---\nitems: [2026-07-29]\n---\nbody\n",
            "---\n2026-07-29: value\n---\nbody\n",
        ),
        // Exponent-shaped and number-shaped strings do read back as numbers,
        // so they escalate to a quote.
        (
            "1e3",
            "---\nfield: '1e3'\n---\nbody\n",
            "---\nitems: ['1e3']\n---\nbody\n",
            "---\n'1e3': value\n---\nbody\n",
        ),
        (
            "-1.2e-3",
            "---\nfield: '-1.2e-3'\n---\nbody\n",
            "---\nitems: ['-1.2e-3']\n---\nbody\n",
            "---\n'-1.2e-3': value\n---\nbody\n",
        ),
        (
            "123",
            "---\nfield: '123'\n---\nbody\n",
            "---\nitems: ['123']\n---\nbody\n",
            "---\n'123': value\n---\nbody\n",
        ),
        (
            ".inf",
            "---\nfield: '.inf'\n---\nbody\n",
            "---\nitems: ['.inf']\n---\nbody\n",
            "---\n'.inf': value\n---\nbody\n",
        ),
        (
            "null",
            "---\nfield: 'null'\n---\nbody\n",
            "---\nitems: ['null']\n---\nbody\n",
            "---\n'null': value\n---\nbody\n",
        ),
        // A trailing colon is a nested mapping in block and key position; in
        // key position the emission spells the colon twice rather than quoting.
        (
            "note:",
            "---\nfield: 'note:'\n---\nbody\n",
            "---\nitems: ['note:']\n---\nbody\n",
            "---\nnote:: value\n---\nbody\n",
        ),
        (
            "a: b",
            "---\nfield: 'a: b'\n---\nbody\n",
            "---\nitems: ['a: b']\n---\nbody\n",
            "---\n'a: b': value\n---\nbody\n",
        ),
        // A leading `#` turns the rest of the line into a comment.
        (
            "#lead",
            "---\nfield: '#lead'\n---\nbody\n",
            "---\nitems: ['#lead']\n---\nbody\n",
            "---\n'#lead': value\n---\nbody\n",
        ),
        // Flow punctuation is inert in a block value and structural in a flow
        // item, which is the whole reason context is carried.
        (
            "a,b",
            "---\nfield: a,b\n---\nbody\n",
            "---\nitems: ['a,b']\n---\nbody\n",
            "---\na,b: value\n---\nbody\n",
        ),
        (
            "a]b",
            "---\nfield: a]b\n---\nbody\n",
            "---\nitems: ['a]b']\n---\nbody\n",
            "---\na]b: value\n---\nbody\n",
        ),
        (
            "[a]",
            "---\nfield: '[a]'\n---\nbody\n",
            "---\nitems: ['[a]']\n---\nbody\n",
            "---\n'[a]': value\n---\nbody\n",
        ),
        (
            "{a}",
            "---\nfield: '{a}'\n---\nbody\n",
            "---\nitems: ['{a}']\n---\nbody\n",
            "---\n'{a}': value\n---\nbody\n",
        ),
        // The Unicode separators. NEL folds to a space wherever it is left
        // bare, so it reaches the double-quoted escape in every context; the
        // line and paragraph separators survive inside single quotes and in a
        // flow item, and only key position needs the escape.
        (
            "next\u{85}line",
            "---\nfield: \"next\\Nline\"\n---\nbody\n",
            "---\nitems: [\"next\\Nline\"]\n---\nbody\n",
            "---\n\"next\\Nline\": value\n---\nbody\n",
        ),
        (
            "line\u{2028}sep",
            "---\nfield: 'line\u{2028}sep'\n---\nbody\n",
            "---\nitems: [line\u{2028}sep]\n---\nbody\n",
            "---\n\"line\\Lsep\": value\n---\nbody\n",
        ),
        (
            "para\u{2029}sep",
            "---\nfield: 'para\u{2029}sep'\n---\nbody\n",
            "---\nitems: [para\u{2029}sep]\n---\nbody\n",
            "---\n\"para\\Psep\": value\n---\nbody\n",
        ),
        // Nothing, and nothing but whitespace.
        (
            "",
            "---\nfield: ''\n---\nbody\n",
            "---\nitems: ['']\n---\nbody\n",
            "---\n'': value\n---\nbody\n",
        ),
        (
            "  ",
            "---\nfield: '  '\n---\nbody\n",
            "---\nitems: ['  ']\n---\nbody\n",
            "---\n'  ': value\n---\nbody\n",
        ),
    ];

    for (text, block, flow, key) in rows {
        let value = Value::String((*text).to_string());
        assert_eq!(
            block_document("---\nfield: seed\n---\nbody\n", "field", &value),
            *block,
            "block value {text:?}"
        );
        assert_eq!(flow_document(&value), *flow, "flow item {text:?}");
        assert_eq!(key_document(text), *key, "key {text:?}");
    }
}

/// The same table for the non-string scalars, which have one spelling each and
/// no quoting ladder to climb.
#[test]
fn the_non_string_scalars_emit_exactly_these_bytes() {
    let rows: &[(Value, &str, &str)] = &[
        (
            Value::Null,
            "---\nfield: ~\n---\nbody\n",
            "---\nitems: [~]\n---\nbody\n",
        ),
        (
            Value::Bool(true),
            "---\nfield: true\n---\nbody\n",
            "---\nitems: [true]\n---\nbody\n",
        ),
        (
            Value::Int(-7),
            "---\nfield: -7\n---\nbody\n",
            "---\nitems: [-7]\n---\nbody\n",
        ),
        // An integral float keeps its point, or it would read back as an int.
        (
            Value::Float(1.0),
            "---\nfield: 1.0\n---\nbody\n",
            "---\nitems: [1.0]\n---\nbody\n",
        ),
        (
            Value::Float(1e3),
            "---\nfield: 1000.0\n---\nbody\n",
            "---\nitems: [1000.0]\n---\nbody\n",
        ),
        (
            Value::Float(-0.0),
            "---\nfield: -0.0\n---\nbody\n",
            "---\nitems: [-0.0]\n---\nbody\n",
        ),
        (
            Value::Float(f64::NAN),
            "---\nfield: .nan\n---\nbody\n",
            "---\nitems: [.nan]\n---\nbody\n",
        ),
        (
            Value::Float(f64::INFINITY),
            "---\nfield: .inf\n---\nbody\n",
            "---\nitems: [.inf]\n---\nbody\n",
        ),
        (
            Value::Float(f64::NEG_INFINITY),
            "---\nfield: -.inf\n---\nbody\n",
            "---\nitems: [-.inf]\n---\nbody\n",
        ),
    ];

    for (value, block, flow) in rows {
        assert_eq!(
            block_document("---\nfield: seed\n---\nbody\n", "field", value),
            *block,
            "block value {value:?}"
        );
        assert_eq!(flow_document(value), *flow, "flow item {value:?}");
    }
}

// ── The non-string scalars ───────────────────────────────────────────────

#[test]
fn the_non_string_scalars_read_back_as_their_own_shapes() {
    for value in [
        Value::Null,
        Value::Bool(true),
        Value::Bool(false),
        Value::Int(0),
        Value::Int(-1),
        Value::Int(i64::MAX),
        Value::Int(i64::MIN),
        Value::Float(1.5),
        // A float whose value is integral must not read back as an integer.
        Value::Float(1.0),
        Value::Float(-0.0),
        Value::Float(f64::INFINITY),
        Value::Float(f64::NEG_INFINITY),
    ] {
        let edited = Document::parse("---\nfield: seed\n---\n")
            .set_field("field", &value)
            .unwrap_or_else(|error| panic!("emitting {value:?}: {error}"));
        assert_eq!(
            Document::parse(&edited)
                .frontmatter()
                .and_then(Value::as_map)
                .and_then(|map| map.get("field"))
                .cloned(),
            Some(value.clone()),
            "scalar {value:?}"
        );
    }
}

/// `.nan` is a value like any other. Equality over the value model is total,
/// so a not-a-number float equals itself, an edit that writes one can be
/// proven, and a document already holding one is editable at all — under IEEE
/// equality it was not, because the post-image check compared a value against
/// itself and lost.
#[test]
fn a_not_a_number_float_round_trips_like_every_other_scalar() {
    let edited = Document::parse("---\nfield: seed\n---\n")
        .set_field("field", &Value::Float(f64::NAN))
        .expect("writing .nan");
    assert_eq!(edited, "---\nfield: .nan\n---\n");
    assert!(frontmatter_reads_back(
        &edited,
        &Value::Map([("field", Value::Float(f64::NAN))].into_iter().collect())
    ));

    // A document that already holds one edits like any other document, and the
    // untouched `.nan` beside the edit stays `.nan`.
    let holding = "---\nratio: .nan\ntitle: t\n---\nbody\n";
    assert_eq!(
        Document::parse(holding).set_field("title", &Value::String("new".into())),
        Ok("---\nratio: .nan\ntitle: new\n---\nbody\n".to_string())
    );
    // And it re-emits from scratch: a whole document written out of what was
    // read back reproduces the block.
    let read = Document::parse(holding);
    let map = read.frontmatter().and_then(Value::as_map).expect("a map");
    assert_eq!(
        render_document(map, "body\n", LineEnding::Lf),
        Ok("---\nratio: .nan\ntitle: t\n---\nbody\n".to_string())
    );
}

/// Signed zero is two values, not one. Total-ordering equality is what makes
/// an emission of `-0.0` that read back as `0.0` a refusal rather than a
/// silent rewrite.
#[test]
fn negative_zero_is_not_zero() {
    assert_ne!(Value::Float(-0.0), Value::Float(0.0));
    let edited = Document::parse("---\nfield: seed\n---\n")
        .set_field("field", &Value::Float(-0.0))
        .expect("writing -0.0");
    assert_eq!(edited, "---\nfield: -0.0\n---\n");
    assert!(frontmatter_reads_back(
        &edited,
        &Value::Map([("field", Value::Float(-0.0))].into_iter().collect())
    ));
    assert!(!frontmatter_reads_back(
        &edited,
        &Value::Map([("field", Value::Float(0.0))].into_iter().collect())
    ));
}

// ── Whole documents written from scratch ─────────────────────────────────

#[test]
fn a_rendered_document_reads_back_as_the_fields_it_was_given() {
    let mut fields = Mapping::new();
    fields.insert("title", "A note: with a colon");
    fields.insert("count", Value::Int(3));
    fields.insert("tags", Value::Sequence(vec!["a,b".into(), "c".into()]));
    fields.insert("empty", Value::Sequence(vec![]));
    fields.insert("nothing", Value::Null);
    let rendered = render_document(&fields, "# body\n", LineEnding::Lf).expect("a document");
    let document = Document::parse(&rendered);
    assert_eq!(document.frontmatter(), Some(&Value::Map(fields)));
    assert_eq!(document.body(), "# body\n");
}

/// Whether an unset field belongs in a document is a question about the vault,
/// not about its syntax, so a null is written rather than dropped.
#[test]
fn a_null_field_is_written_rather_than_silently_dropped() {
    let fields: Mapping = [("status", Value::Null)].into_iter().collect();
    assert_eq!(
        render_document(&fields, "", LineEnding::Lf),
        Ok("---\nstatus: ~\n---\n".to_string())
    );
}

#[test]
fn a_rendered_body_gains_the_terminator_it_is_missing() {
    let fields: Mapping = [("title", "t")].into_iter().collect();
    assert_eq!(
        render_document(&fields, "no trailing break", LineEnding::Lf),
        Ok("---\ntitle: t\n---\nno trailing break\n".to_string())
    );
    assert_eq!(
        render_document(&Mapping::new(), "", LineEnding::Lf),
        Ok("---\n---\n".to_string())
    );
}

/// A block holding no fields is `---\n---\n`, which reads as null. That is not
/// a lost round trip: the crate promotes that null to a mapping the moment a
/// field is written into it, so null and the empty mapping are the same block
/// written twice, and the reads-back predicate says so.
#[test]
fn a_block_written_from_no_fields_reads_back_as_the_empty_mapping() {
    let rendered =
        render_document(&Mapping::new(), "body\n", LineEnding::Lf).expect("an empty block");
    assert_eq!(rendered, "---\n---\nbody\n");
    assert!(frontmatter_reads_back(
        &rendered,
        &Value::Map(Mapping::new())
    ));
    // Read as a value it is null, and it takes a first field like any other
    // empty block.
    assert_eq!(Document::parse(&rendered).frontmatter(), Some(&Value::Null));
    assert!(frontmatter_reads_back(&rendered, &Value::Null));
    assert_eq!(
        Document::parse(&rendered).set_field("title", &Value::String("t".into())),
        Ok("---\ntitle: t\n---\nbody\n".to_string())
    );
    // The equivalence is only for an empty mapping; a block holding fields is
    // not null and does not read back as one.
    let held: Mapping = [("title", "t")].into_iter().collect();
    assert!(!frontmatter_reads_back(
        "---\ntitle: t\n---\n",
        &Value::Map(Mapping::new())
    ));
    assert!(frontmatter_reads_back(
        "---\ntitle: t\n---\n",
        &Value::Map(held)
    ));
}

#[test]
fn a_rendered_document_refuses_a_nested_mapping() {
    let nested: Mapping = [("inner", "x")].into_iter().collect();
    let fields: Mapping = [("outer", Value::Map(nested))].into_iter().collect();
    assert_eq!(
        render_document(&fields, "", LineEnding::Lf),
        Err(RenderError::NonScalarValue { kind: "map" })
    );
}

#[test]
fn a_rendered_document_refuses_a_sequence_inside_a_sequence() {
    let fields: Mapping = [(
        "outer",
        Value::Sequence(vec![Value::Sequence(vec!["inner".into()])]),
    )]
    .into_iter()
    .collect();
    assert_eq!(
        render_document(&fields, "", LineEnding::Lf),
        Err(RenderError::NonScalarValue { kind: "sequence" })
    );
}

/// The refusal vocabulary is exactly three reasons, and each one is reachable.
/// A fourth that no path constructs is a variant nobody can act on — and a
/// dead variant is what this crate deleted rather than inherited.
#[test]
fn every_render_refusal_is_reachable() {
    let long_key: Mapping = [("k".repeat(2000), Value::Int(1))].into_iter().collect();
    let nested: Mapping = [("outer", Value::Map(Mapping::new()))]
        .into_iter()
        .collect();
    let seen = [
        render_document(&long_key, "", LineEnding::Lf),
        render_document(&nested, "", LineEnding::Lf),
        Document::parse("---\nk: scalar\n---\n")
            .set_field("k", &Value::Sequence(vec!["a".into()]))
            .map_err(|error| match error {
                EditError::Render(render) => render,
                other => panic!("unexpected {other:?}"),
            }),
    ];
    let reasons: Vec<&'static str> = seen
        .iter()
        .map(|outcome| match outcome {
            Err(RenderError::NotRoundTrippable { .. }) => "not-round-trippable",
            Err(RenderError::NonScalarValue { .. }) => "non-scalar",
            Err(RenderError::SequenceIntoScalar) => "sequence-into-scalar",
            Ok(_) => panic!("expected a refusal"),
        })
        .collect();
    assert_eq!(
        reasons,
        ["not-round-trippable", "non-scalar", "sequence-into-scalar"]
    );
}

/// A value no span can name is refused by the field layer, before any bytes
/// are rendered: the render layer is never handed a style it could not write.
#[test]
fn a_structural_value_is_refused_before_anything_is_rendered() {
    assert_eq!(
        Document::parse("---\nk: |\n  literal\n---\n").set_field("k", &Value::String("x".into())),
        Err(EditError::FieldNotEditable {
            field: "k".to_string()
        })
    );
}
