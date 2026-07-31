//! The vault-name grammar, the token-label grammar, and the two syntactic
//! demands every recorded path is held to.

use std::ffi::OsString;
use std::os::unix::ffi::OsStringExt;
use std::path::PathBuf;

use norn_config::machine::tokens::TokenLabel;
use norn_config::registry::{SchemaSource, VaultRoot};
use norn_config::{ConfigDirs, ConfigError, VaultName};

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

/// A name is a directory component, and a directory component has a length
/// every filesystem norn runs on agrees about.
#[test]
fn a_name_is_bounded_at_the_length_a_directory_component_is() {
    let longest = format!("a{}", "x".repeat(VaultName::MAXIMUM_BYTES - 1));
    assert_eq!(longest.len(), VaultName::MAXIMUM_BYTES);
    assert_eq!(
        VaultName::new(&longest).expect("the longest name").as_str(),
        longest
    );

    let error = VaultName::new(format!("{longest}x")).expect_err("a name past the bound");
    let ConfigError::IllegalName { problem, .. } = error else {
        panic!("a long name was refused as something other than an illegal name");
    };
    assert!(problem.contains("255 bytes"), "{problem}");
}

#[test]
fn an_absolute_root_is_kept_exactly_as_given() {
    let root = VaultRoot::new("/home/person/notes").expect("an absolute root");
    assert_eq!(root.as_path().to_str(), Some("/home/person/notes"));
}

/// Every path this crate records or builds under is absolute and is UTF-8. A
/// relative one resolves against whatever directory the reading process
/// happens to be in, and machine-local state is read by more than one; one
/// that is not text cannot survive the round trip through the file it is
/// written into, and rendering it lossily would report a success that recorded
/// a different directory.
#[test]
fn a_path_that_is_not_absolute_utf8_is_refused_with_its_subject() {
    let mangled = PathBuf::from(OsString::from_vec(vec![b'/', 0xff, 0xfe]));
    for (subject, relative, not_text) in [
        (
            "vault root",
            VaultRoot::new("notes").err(),
            VaultRoot::new(mangled.clone()).err(),
        ),
        (
            "schema source",
            SchemaSource::new("schemas/notes.yaml").err(),
            SchemaSource::new(mangled.clone()).err(),
        ),
        (
            "config base",
            ConfigDirs::new("config", "/data").err(),
            ConfigDirs::new(mangled.clone(), "/data").err(),
        ),
        (
            "data base",
            ConfigDirs::new("/config", "data").err(),
            ConfigDirs::new("/config", mangled.clone()).err(),
        ),
    ] {
        for (error, expected) in [(relative, "absolute"), (not_text, "UTF-8")] {
            let error = error.unwrap_or_else(|| panic!("a {subject} that is not one was accepted"));
            let ConfigError::IllegalPath {
                subject: refused,
                problem,
                ..
            } = error
            else {
                panic!("a {subject} was refused as something other than an illegal path");
            };
            assert_eq!(refused, subject);
            assert!(
                problem.contains(expected),
                "a {subject} was refused with `{problem}`, which does not mention `{expected}`"
            );
        }
    }
}

/// The empty path is relative, and refusing it is what keeps a root from
/// meaning "wherever this process happens to be".
#[test]
fn an_empty_root_is_refused() {
    for text in ["", "./notes", "../notes"] {
        let error = VaultRoot::new(text).expect_err(&format!("`{text}` is not a vault root"));
        assert!(
            matches!(error, ConfigError::IllegalPath { .. }),
            "`{text}` was refused as something other than an illegal path: {error}"
        );
    }
}

/// A label is looser than a vault name — it names a machine or a service
/// rather than a directory and a URL component — and it is bounded, because it
/// is a table key and a thing a person types.
#[test]
fn the_labels_the_grammar_accepts_and_refuses() {
    for text in ["a", "0", "laptop", "laptop-2", "ci_runner.3"] {
        let label = TokenLabel::new(text).unwrap_or_else(|e| panic!("`{text}` was refused: {e}"));
        assert_eq!(label.as_str(), text);
        assert_eq!(label.to_string(), text);
    }
    for (text, needle) in [
        ("", "at least one character"),
        ("-laptop", "opens with"),
        ("_laptop", "opens with"),
        (".laptop", "opens with"),
        ("Laptop", "opens with"),
        ("my laptop", "holds only"),
        ("laptop/2", "holds only"),
        ("laptøp", "holds only"),
    ] {
        let error = TokenLabel::new(text).expect_err(&format!("`{text}` is not a label"));
        let ConfigError::IllegalLabel { label, problem } = error else {
            panic!("`{text}` was refused as something other than an illegal label");
        };
        assert_eq!(label, text);
        assert!(
            problem.contains(needle),
            "`{text}` was refused with `{problem}`, which does not mention `{needle}`"
        );
    }

    assert!(TokenLabel::new("x".repeat(TokenLabel::MAXIMUM_BYTES)).is_ok());
    let error = TokenLabel::new("x".repeat(TokenLabel::MAXIMUM_BYTES + 1))
        .expect_err("a label past the bound");
    let ConfigError::IllegalLabel { problem, .. } = error else {
        panic!("a long label was refused as something other than an illegal label");
    };
    assert!(problem.contains("64 bytes"), "{problem}");
}
