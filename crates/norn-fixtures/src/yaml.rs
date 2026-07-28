//! One YAML scalar emitter. Every *interpolated* scalar written into generated
//! frontmatter — titles drawn from the word pools, enumerated values, aliases —
//! goes through [`scalar`], so a pool or naming change cannot silently emit a
//! value that YAML reparses into something else.
//!
//! Bare, unambiguous values (alphanumerics, interior spaces, ISO datetimes)
//! pass through byte for byte. Values carrying YAML-significant characters are
//! double-quoted and escaped.

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
    value.contains(": ")
        || value.contains(" #")
        || value.ends_with(':')
        || value.ends_with(char::is_whitespace)
        || value.contains(['"', '#', '\n', '\r'])
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
