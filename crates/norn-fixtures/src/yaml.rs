//! One YAML scalar emitter. Every *interpolated* scalar written into generated
//! frontmatter — titles drawn from the word pools, enumerated values, aliases —
//! goes through [`scalar`], so a pool or naming change cannot silently emit a
//! value that YAML reparses into something else.
//!
//! Two classes are quoted and escaped: values carrying YAML-significant
//! *characters*, and values whose characters are innocent but whose
//! plain-scalar *resolution* is not a string — `null`, `no`, `0x1f`, `1e3`.
//! The second class is the one that reads back as a different type rather than
//! as a syntax error, so it is the one worth catching before a word pool grows
//! an entry like `off`. Everything else passes through byte for byte.
//!
//! **Date and time shapes are a known, accepted exception.** A YAML 1.1
//! parser resolves `2024-01-01` to a date and `12:30` to a sexagesimal
//! integer; neither is quoted here, so a value of that shape reaching
//! [`scalar`] would emit bare and could read back as a non-string. That is
//! accepted rather than overlooked: no value any word pool holds has that
//! shape, and the timestamps generated frontmatter carries are written
//! directly rather than through this emitter, so nothing routed here is
//! exposed today. Quoting them instead would put quotes around every emitted
//! date the moment one *is* routed here, which is a worse default for a
//! fixture meant to look like a hand-written document. If a pool ever grows a
//! date-shaped value, this is the paragraph that has to change with it.

/// Render `value` as a YAML scalar, quoting and escaping only when it carries
/// YAML-significant characters.
pub fn scalar(value: &str) -> String {
    if needs_quoting(value) {
        // Line breaks are escaped rather than emitted physically: YAML folds a
        // physical break inside a quoted scalar, so parsing would not
        // reproduce the input value.
        let escaped = value
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('\n', "\\n")
            .replace('\r', "\\r");
        format!("\"{escaped}\"")
    } else {
        value.to_string()
    }
}

/// Whether `value` cannot be emitted as a bare (unquoted) YAML scalar.
fn needs_quoting(value: &str) -> bool {
    let Some(first) = value.chars().next() else {
        // The empty string must be quoted to be a scalar rather than null.
        return true;
    };
    // A leading indicator character changes how YAML parses the node.
    const LEADING_SPECIALS: &str = "!&*?|>@`\"'%#-{}[],:";
    if first.is_whitespace() || LEADING_SPECIALS.contains(first) {
        return true;
    }
    // Interior sequences that break a bare scalar or a flow context, plus any
    // comment introducer or embedded quote.
    if value.contains(": ")
        || value.contains(" #")
        || value.ends_with(':')
        || value.ends_with(char::is_whitespace)
        || value.contains(['"', '#', '\n', '\r'])
    {
        return true;
    }
    // A bare scalar that YAML *resolves* to something other than a string is
    // the trap this emitter exists to close: `title: null` is not the title
    // "null", and `status: no` is the boolean false. The characters are all
    // innocent, so only the resolution rules catch it.
    resolves_to_a_non_string(value)
}

/// Whether YAML's plain-scalar resolution would read `value` as a null, a
/// boolean or a number rather than as a string.
///
/// Deliberately wider than any single YAML version: the 1.1 boolean words
/// (`yes`, `no`, `on`, `off`, `y`, `n`) are not booleans in 1.2 Core, but
/// parsers in the wild resolve them, and a fixture that reads differently
/// depending on the reader is worse than a redundantly quoted one. Quoting a
/// value that did not need it costs two bytes; not quoting one that did costs
/// a wrong type.
fn resolves_to_a_non_string(value: &str) -> bool {
    const NON_STRINGS: &[&str] = &[
        "null", "nil", "~", "true", "false", "yes", "no", "on", "off", "y", "n",
    ];
    let folded = value.to_ascii_lowercase();
    if NON_STRINGS.contains(&folded.as_str()) {
        return true;
    }
    looks_numeric(&folded)
}

/// Whether `value` (already lowercased) is one of YAML's numeric forms.
fn looks_numeric(value: &str) -> bool {
    let body = value.strip_prefix(['+', '-']).unwrap_or(value);
    if body.is_empty() {
        return false;
    }
    // The infinities and not-a-number, which carry a leading `.`.
    if matches!(body, ".inf" | ".nan") {
        return true;
    }
    // Hexadecimal, octal and binary integers.
    for (prefix, radix) in [("0x", 16u32), ("0o", 8), ("0b", 2)] {
        if let Some(digits) = body.strip_prefix(prefix) {
            return !digits.is_empty() && digits.chars().all(|c| c.is_digit(radix) || c == '_');
        }
    }
    // Decimal integers and floats, with or without an exponent. Underscores
    // are digit separators in YAML 1.1 and are accepted here for the same
    // reason the 1.1 booleans are.
    let (mantissa, exponent) = match body.split_once('e') {
        Some((mantissa, exponent)) => (mantissa, Some(exponent)),
        None => (body, None),
    };
    if let Some(exponent) = exponent {
        let digits = exponent.strip_prefix(['+', '-']).unwrap_or(exponent);
        if digits.is_empty() || !digits.chars().all(|c| c.is_ascii_digit()) {
            return false;
        }
    }
    let (whole, fraction) = match mantissa.split_once('.') {
        Some((whole, fraction)) => (whole, fraction),
        None => (mantissa, ""),
    };
    // `1.` and `.5` are both numbers, so the digit may sit on either side of
    // the point; what neither side may carry is anything but digits.
    let digity = |part: &str| part.chars().all(|c| c.is_ascii_digit() || c == '_');
    let has_digit = whole
        .chars()
        .chain(fraction.chars())
        .any(|c| c.is_ascii_digit());
    has_digit && digity(whole) && digity(fraction)
}

#[cfg(test)]
mod tests {
    use super::scalar;

    #[test]
    fn bare_values_pass_through_unchanged() {
        for v in [
            "backlog",
            "Sprout 0",
            "Zephyr 29",
            "2024-05-03T14:23:00Z",
            "Über Notiz",
            "not-a-date",
        ] {
            assert_eq!(scalar(v), v, "expected {v} to pass through unquoted");
        }
    }

    #[test]
    fn values_yaml_would_resolve_to_a_non_string_are_quoted() {
        for v in [
            "null", "Null", "NULL", "~", "nil", "true", "False", "TRUE", "yes", "No", "on", "OFF",
            "y", "N",
        ] {
            assert_eq!(scalar(v), format!("\"{v}\""), "expected {v} to be quoted");
        }
    }

    #[test]
    fn numeric_forms_are_quoted() {
        for v in [
            "0", "42", "-7", "+3", "1_000", "3.14", "-0.5", ".5", "1.", "1e3", "1E-3", "2.5e+4",
            "0x1f", "0o17", "0b1011", ".inf", "-.INF", ".nan",
        ] {
            assert_eq!(scalar(v), format!("\"{v}\""), "expected {v} to be quoted");
        }
    }

    #[test]
    fn values_that_merely_look_numeric_stay_bare() {
        // Each of these emits bare. The date and datetime shapes are the known
        // exception documented on this module: a YAML 1.1 parser resolves them
        // to a timestamp rather than to a string, and they are left bare
        // deliberately because nothing routed through this emitter has that
        // shape.
        for v in [
            "2024-01-01",
            "2024-05-03T14:23:00Z",
            "1e",
            "1e3x",
            "0x",
            "12abc",
            "v2",
            "3 4",
            "Sprout 0012",
            "nope",
            "onwards",
            "nullify",
        ] {
            assert_eq!(scalar(v), v, "expected {v} to pass through unquoted");
        }
    }

    #[test]
    fn significant_values_are_quoted() {
        assert_eq!(scalar(""), "\"\"");
        assert_eq!(scalar("a: b"), "\"a: b\"");
        assert_eq!(scalar("- leading dash"), "\"- leading dash\"");
        assert_eq!(scalar("has \"quote"), "\"has \\\"quote\"");
        assert_eq!(scalar("trailing "), "\"trailing \"");
        assert_eq!(scalar("line\nbreak"), "\"line\\nbreak\"");
        assert_eq!(scalar("carriage\rreturn"), "\"carriage\\rreturn\"");
    }
}
