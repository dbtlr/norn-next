//! The vault-name grammar, and the root's one syntactic demand.

use norn_config::registry::VaultRoot;
use norn_config::{ConfigError, VaultName};

/// The grammar is RFC 3986's scheme production: a lowercase letter, then
/// lowercase letters, digits, `+`, `.` and `-`.
#[test]
fn the_names_the_grammar_accepts() {
    for text in [
        "a",
        "notes",
        "notes2",
        "norn-vault",
        "a.b",
        "a+b",
        "a-b.c+d",
        "x1234567890",
    ] {
        let name = VaultName::new(text).unwrap_or_else(|e| panic!("`{text}` was refused: {e}"));
        assert_eq!(name.as_str(), text);
        assert_eq!(name.to_string(), text);
    }
}

/// Each refusal with the reason it carries. The underscore case is the one
/// that matters most: `norn-text`'s protocol-identifier grammar admits `_`,
/// this one does not, and the two are deliberately different — a protocol
/// identifier is read out of a document, a vault name becomes a directory and
/// a URL component.
#[test]
fn the_names_the_grammar_refuses() {
    for (text, needle) in [
        ("", "at least one character"),
        ("Notes", "lowercase ASCII letter"),
        ("1notes", "lowercase ASCII letter"),
        ("-notes", "lowercase ASCII letter"),
        (".notes", "lowercase ASCII letter"),
        ("+notes", "lowercase ASCII letter"),
        ("_notes", "lowercase ASCII letter"),
        ("notes_1", "lowercase ASCII letters, digits"),
        ("notesX", "lowercase ASCII letters, digits"),
        ("note s", "lowercase ASCII letters, digits"),
        ("notes/deep", "lowercase ASCII letters, digits"),
        ("notés", "lowercase ASCII letters, digits"),
        ("notes:1", "lowercase ASCII letters, digits"),
    ] {
        let error = VaultName::new(text).expect_err(&format!("`{text}` is not a vault name"));
        let ConfigError::IllegalName { name, problem } = error else {
            panic!("`{text}` was refused as something other than an illegal name");
        };
        assert_eq!(name, text);
        assert!(
            problem.contains(needle),
            "`{text}` was refused with `{problem}`, which does not mention `{needle}`"
        );
    }
}

/// The grammar's free consequence: the dangerous directory components are
/// unspellable, because a name opens with a lowercase letter and holds no
/// separator.
#[test]
fn the_grammar_makes_a_traversal_component_unspellable() {
    for text in [".", "..", "../..", "/", "a/../b"] {
        assert!(
            VaultName::new(text).is_err(),
            "`{text}` was accepted as a vault name"
        );
    }
}

#[test]
fn an_absolute_root_is_kept_exactly_as_given() {
    let root = VaultRoot::new("/home/person/notes").expect("an absolute root");
    assert_eq!(root.as_path().to_str(), Some("/home/person/notes"));
}

/// A relative root resolves against whatever directory the reading process
/// happens to be in, and machine-local state is read by more than one process.
#[test]
fn a_relative_root_is_refused() {
    for text in ["notes", "./notes", "../notes", ""] {
        let error = VaultRoot::new(text).expect_err(&format!("`{text}` is not a vault root"));
        assert!(
            matches!(error, ConfigError::RelativeRoot { .. }),
            "`{text}` was refused as something other than a relative root: {error}"
        );
    }
}
