//! The frontmatter projection: canonical bytes, and the shapes JSON cannot hold.
//!
//! The projection's contract is that the same value always writes the same
//! bytes, whatever order it was read in — because a column whose contents depend
//! on something the column cannot be queried by is a column two equal documents
//! disagree about.
//!
//! That the bytes are JSON the JSON1 reader accepts is checked against a real
//! database in `pillars.rs`, through the store's own verification.

use norn_store::{FrontmatterValue as Value, canonical_json};

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
        assert_eq!(canonical_json(&value), written, "projecting {value:?}");
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
    assert_eq!(canonical_json(&one), canonical_json(&other));
    assert_eq!(
        canonical_json(&one),
        "{\"aliases\":[\"n\"],\"draft\":false,\"title\":\"Norn\"}"
    );
}

/// Nesting is canonicalized all the way down, not only at the top level.
#[test]
fn a_nested_map_is_canonicalized_too() {
    let one = map(&[("meta", map(&[("z", Value::Int(1)), ("a", Value::Int(2))]))]);
    let other = map(&[("meta", map(&[("a", Value::Int(2)), ("z", Value::Int(1))]))]);
    assert_eq!(canonical_json(&one), canonical_json(&other));
    assert_eq!(canonical_json(&one), "{\"meta\":{\"a\":2,\"z\":1}}");
}

/// The canonicalization is total: a repeated key keeps its last value, which is
/// how the value model itself reads a second write to one.
#[test]
fn a_repeated_key_keeps_its_last_value() {
    let repeated = map(&[
        ("a", Value::Int(1)),
        ("b", Value::Int(2)),
        ("a", Value::Int(3)),
    ]);
    assert_eq!(canonical_json(&repeated), "{\"a\":3,\"b\":2}");
}

/// An integral float keeps a fractional marker, or `json_type` would call it an
/// integer and the projection would report a shape the document does not have.
#[test]
fn an_integral_float_keeps_its_fractional_marker() {
    assert_eq!(canonical_json(&Value::Float(1.0)), "1.0");
    assert_eq!(canonical_json(&Value::Float(-0.0)), "-0.0");
    assert_eq!(canonical_json(&Value::Int(1)), "1");
}

/// Whatever the magnitude, the digits read back to the same double. The form
/// carries no exponent, so an extreme value is long rather than compact — one
/// spelling of a number is what makes two equal values project equal bytes.
#[test]
fn a_float_reads_back_to_the_double_it_was_written_from() {
    for value in [0.1_f64, 1.0, -0.0, 1e21, 1e-9, 1e300, f64::MAX, f64::MIN] {
        let written = canonical_json(&Value::Float(value));
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
        assert_eq!(canonical_json(&Value::Float(value)), "null");
    }
}

/// Exactly what JSON requires is escaped, and nothing else: the quote, the
/// backslash, and the C0 controls.
#[test]
fn a_string_escapes_what_json_requires_and_leaves_the_rest_as_utf8() {
    assert_eq!(
        canonical_json(&text("a \"quoted\" \\ path")),
        "\"a \\\"quoted\\\" \\\\ path\""
    );
    assert_eq!(
        canonical_json(&text("line\nreturn\rtab\t")),
        "\"line\\nreturn\\rtab\\t\""
    );
    assert_eq!(
        canonical_json(&text("\u{8}\u{c}\u{1}")),
        "\"\\b\\f\\u0001\""
    );
    assert_eq!(canonical_json(&text("日本語 café")), "\"日本語 café\"");
}

/// A key is a string and is escaped as one.
#[test]
fn a_key_is_escaped_like_any_other_string() {
    assert_eq!(
        canonical_json(&map(&[("a \"b\"", Value::Null)])),
        "{\"a \\\"b\\\"\":null}"
    );
}
