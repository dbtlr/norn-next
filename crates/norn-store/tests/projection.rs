//! The frontmatter projection: canonical bytes, and the shapes JSON cannot hold.
//!
//! The projection's contract is that the same value always writes the same
//! bytes, whatever order it was read in — because a column whose contents depend
//! on something the column cannot be queried by is a column two equal documents
//! disagree about.
//!
//! That the bytes are JSON the JSON1 reader accepts is checked against a real
//! database in `pillars.rs`, through the store's own verification.

use norn_store::{FrontmatterValue as Value, MAX_FRONTMATTER_DEPTH, StoreError, canonical_json};

/// The projection of a value within the depth bound, which is every value below.
fn projected(value: &Value) -> String {
    canonical_json(value).expect("a value within the nesting bound")
}

fn map(entries: &[(&str, Value)]) -> Value {
    Value::Map(
        entries
            .iter()
            .map(|(key, value)| ((*key).to_string(), value.clone()))
            .collect(),
    )
}

fn text(value: &str) -> Value {
    Value::String(value.to_string())
}

/// Every shape of the value model, and the bytes it writes.
#[test]
fn each_shape_writes_the_bytes_it_is_contracted_to() {
    for (value, written) in [
        (Value::Null, "null"),
        (Value::Bool(true), "true"),
        (Value::Bool(false), "false"),
        (Value::Int(0), "0"),
        (Value::Int(-42), "-42"),
        (Value::Int(i64::MAX), "9223372036854775807"),
        (Value::Float(1.5), "1.5"),
        (text("norn"), "\"norn\""),
        (Value::Sequence(vec![]), "[]"),
        (
            Value::Sequence(vec![Value::Int(1), text("two"), Value::Null]),
            "[1,\"two\",null]",
        ),
        (map(&[]), "{}"),
        (
            map(&[("a", Value::Int(1)), ("b", Value::Sequence(vec![]))]),
            "{\"a\":1,\"b\":[]}",
        ),
    ] {
        assert_eq!(projected(&value), written, "projecting {value:?}");
    }
}

/// Keys are sorted, so document order — which the file carries — does not reach
/// the column.
#[test]
fn a_value_projects_the_same_bytes_whatever_order_its_keys_arrived_in() {
    let one = map(&[
        ("title", text("Norn")),
        ("draft", Value::Bool(false)),
        ("aliases", Value::Sequence(vec![text("n")])),
    ]);
    let other = map(&[
        ("aliases", Value::Sequence(vec![text("n")])),
        ("title", text("Norn")),
        ("draft", Value::Bool(false)),
    ]);
    assert_eq!(projected(&one), projected(&other));
    assert_eq!(
        projected(&one),
        "{\"aliases\":[\"n\"],\"draft\":false,\"title\":\"Norn\"}"
    );
}

/// Nesting is canonicalized all the way down, not only at the top level.
#[test]
fn a_nested_map_is_canonicalized_too() {
    let one = map(&[("meta", map(&[("z", Value::Int(1)), ("a", Value::Int(2))]))]);
    let other = map(&[("meta", map(&[("a", Value::Int(2)), ("z", Value::Int(1))]))]);
    assert_eq!(projected(&one), projected(&other));
    assert_eq!(projected(&one), "{\"meta\":{\"a\":2,\"z\":1}}");
}

/// The canonicalization is total: a composed value whose entries repeat a key
/// is written by the run's last entry, which is the value a second write
/// through a map that replaces in place would leave. No document produces one —
/// a block writing a key twice is refused where it is read.
#[test]
fn a_repeated_key_keeps_its_last_value() {
    let repeated = map(&[
        ("a", Value::Int(1)),
        ("b", Value::Int(2)),
        ("a", Value::Int(3)),
    ]);
    assert_eq!(projected(&repeated), "{\"a\":3,\"b\":2}");
}

/// An integral float keeps a fractional marker, or `json_type` would call it an
/// integer and the projection would report a shape the document does not have.
#[test]
fn an_integral_float_keeps_its_fractional_marker() {
    assert_eq!(projected(&Value::Float(1.0)), "1.0");
    assert_eq!(projected(&Value::Float(-0.0)), "-0.0");
    assert_eq!(projected(&Value::Int(1)), "1");
}

/// Whatever the magnitude, the digits read back to the same double. The form
/// carries no exponent, so an extreme value is long rather than compact — one
/// spelling of a number is what makes two equal values project equal bytes.
#[test]
fn a_float_reads_back_to_the_double_it_was_written_from() {
    for value in [0.1_f64, 1.0, -0.0, 1e21, 1e-9, 1e300, f64::MAX, f64::MIN] {
        let written = projected(&Value::Float(value));
        assert!(
            !written.contains(['e', 'E']),
            "`{written}` carries an exponent"
        );
        let read: f64 = written
            .parse()
            .unwrap_or_else(|error| panic!("`{written}` does not read back as a number: {error}"));
        assert_eq!(read.to_bits(), value.to_bits(), "`{written}`");
    }
}

/// JSON has no NaN and no infinity, so a non-finite float projects to `null`
/// rather than to a spelling no JSON reader agrees about.
#[test]
fn a_non_finite_float_projects_to_null() {
    for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        assert_eq!(projected(&Value::Float(value)), "null");
    }
}

/// Exactly what JSON requires is escaped, and nothing else: the quote, the
/// backslash, and the C0 controls.
#[test]
fn a_string_escapes_what_json_requires_and_leaves_the_rest_as_utf8() {
    assert_eq!(
        projected(&text("a \"quoted\" \\ path")),
        "\"a \\\"quoted\\\" \\\\ path\""
    );
    assert_eq!(
        projected(&text("line\nreturn\rtab\t")),
        "\"line\\nreturn\\rtab\\t\""
    );
    assert_eq!(projected(&text("\u{8}\u{c}\u{1}")), "\"\\b\\f\\u0001\"");
    assert_eq!(projected(&text("日本語 café")), "\"日本語 café\"");
}

/// A key is a string and is escaped as one.
#[test]
fn a_key_is_escaped_like_any_other_string() {
    assert_eq!(
        projected(&map(&[("a \"b\"", Value::Null)])),
        "{\"a \\\"b\\\"\":null}"
    );
}

/// `depth` sequences around a scalar, which nests `depth` deep.
fn nested(depth: usize) -> Value {
    let mut value = Value::Null;
    for _ in 1..depth {
        value = Value::Sequence(vec![value]);
    }
    value
}

/// **The projection is bounded, and both sides of the bound are pinned.** A value
/// at the bound projects; one past it is refused rather than written, because the
/// alternative is bytes SQLite's JSON reader rejects — which the store's own
/// verification reports as damage, permanently, through the rung that discards
/// the database.
#[test]
fn a_value_at_the_nesting_bound_projects_and_one_past_it_is_refused() {
    let at_the_bound = projected(&nested(MAX_FRONTMATTER_DEPTH));
    assert!(at_the_bound.starts_with('['), "{at_the_bound:.20}");
    assert_eq!(
        at_the_bound.matches('[').count(),
        MAX_FRONTMATTER_DEPTH - 1,
        "the value at the bound did not project every level"
    );

    let error = canonical_json(&nested(MAX_FRONTMATTER_DEPTH + 1))
        .expect_err("a value past the nesting bound");
    let StoreError::Bound { limit, given, .. } = error else {
        panic!("nesting was refused as {error:?} rather than as a bound");
    };
    assert_eq!(limit, MAX_FRONTMATTER_DEPTH);
    assert!(given > limit, "{given} is not past {limit}");
}

/// **A value far past the bound is refused and then freed, neither step recursing
/// once per level.** The measurement is a walk with an explicit stack and the
/// value's own drop is another, so a caller that composed a deep tree reads the
/// refusal and then goes on running — a recursive drop would abort the process at
/// the end of the scope that took the error. The same value carried through the
/// store's own write is `store/facts.rs`'s case.
#[test]
fn a_value_far_past_the_bound_is_refused_and_dropped_without_overflowing() {
    let pathological = nested(100_000);
    let error = canonical_json(&pathological).expect_err("a pathologically nested value");
    assert!(matches!(error, StoreError::Bound { .. }), "{error:?}");
    drop(pathological);
}

/// A map nests as deeply as a sequence and is freed the same way, so the iterative
/// drop is about nesting rather than about which container does it. Built by moving
/// rather than through `map`, which clones what it is handed.
#[test]
fn a_deep_map_is_dropped_without_overflowing() {
    let mut value = Value::Null;
    for _ in 0..100_000 {
        value = Value::Map(vec![("k".to_string(), value)]);
    }
    drop(value);
}

/// A map nests as a sequence does, so the bound is about nesting rather than
/// about which container does it.
#[test]
fn a_map_nests_against_the_same_bound() {
    let mut value = Value::Null;
    for _ in 1..=MAX_FRONTMATTER_DEPTH {
        value = map(&[("k", value)]);
    }
    assert!(matches!(
        canonical_json(&value),
        Err(StoreError::Bound { .. })
    ));
}
