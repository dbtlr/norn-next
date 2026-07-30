//! The segment-aware path encoding, and the probes that read it.
//!
//! Resolution is a *prefix* relation between two segment-reversed keys, and
//! these cases pin the reduction on both sides of it: what a document path
//! becomes, what a target becomes, and which pairs are therefore in range. The
//! range itself is exercised against real rows in `pillars.rs`; here the
//! question is whether the keys say what they are supposed to say.

use norn_store::{DocumentPath, StoreError, class_probe, suffix_probe};

/// The table from the encoding's own documentation, as cases.
#[test]
fn a_path_reverses_its_segments_and_drops_the_leaf_extension() {
    for (path, suffix_key, stem, depth) in [
        ("glossary.md", "glossary/", "glossary", 1),
        (
            "docs/norn/glossary.md",
            "glossary/norn/docs/",
            "glossary",
            3,
        ),
        ("docs/norn/index.md", "index/norn/docs/", "index", 3),
        ("a.b.md", "a.b/", "a.b", 1),
        ("notes/README", "README/notes/", "README", 2),
    ] {
        let read = DocumentPath::new(path).expect("a document path");
        assert_eq!(read.as_str(), path);
        assert_eq!(read.suffix_key(), suffix_key, "the suffix key of `{path}`");
        assert_eq!(read.stem(), stem, "the stem of `{path}`");
        assert_eq!(read.depth(), depth, "the depth of `{path}`");
    }
}

/// A dotfile is a name, not an empty stem: reducing `.gitignore` to nothing
/// would put every dotfile in one ambiguity class.
#[test]
fn a_leading_dot_is_part_of_the_name() {
    let read = DocumentPath::new("docs/.gitignore").expect("a document path");
    assert_eq!(read.stem(), ".gitignore");
    assert_eq!(read.suffix_key(), ".gitignore/docs/");
}

/// The spellings a normalized path does not have. Each of them would produce a
/// key that addresses the wrong documents.
#[test]
fn an_unnormalized_path_is_refused_with_the_reason() {
    for (path, needle) in [
        ("", "empty"),
        ("/docs/glossary.md", "absolute"),
        ("docs\\glossary.md", "backslash"),
        ("docs//glossary.md", "empty segment"),
        ("docs/glossary.md/", "empty segment"),
        ("docs/./glossary.md", "`.` or `..`"),
        ("../glossary.md", "`.` or `..`"),
    ] {
        let error = DocumentPath::new(path).expect_err("an unnormalized path");
        let StoreError::Path { problem, .. } = &error else {
            panic!("`{path}` was refused as {error:?} rather than as a path");
        };
        assert!(
            problem.contains(needle),
            "`{path}` was refused for `{problem}`, which does not name {needle}"
        );
    }
}

/// The probe a target reduces to, and the pairs that are therefore in range.
///
/// The upper bound is the prefix with its final separator stepped by one, so a
/// key is in range exactly when it opens with the prefix — which is what makes
/// the predicate a range over an index rather than a pattern match.
#[test]
fn a_suffix_target_reduces_to_the_prefix_its_candidates_share() {
    for (target, lower) in [
        ("glossary", "glossary/"),
        ("glossary.md", "glossary/"),
        ("norn/glossary", "glossary/norn/"),
        ("norn/glossary.md", "glossary/norn/"),
        ("/norn/glossary", "glossary/norn/"),
    ] {
        let probe = suffix_probe(target).expect("a suffix target");
        assert_eq!(probe.lower(), lower, "the probe for `{target}`");
        assert_eq!(probe.upper(), format!("{}0", &lower[..lower.len() - 1]));
    }
}

/// Segment alignment is structural: the prefix ends with the separator, so a
/// key cannot match across a segment boundary however similar the bytes are.
#[test]
fn a_probe_matches_whole_segments_only() {
    let probe = suffix_probe("norn/glossary").expect("a suffix target");
    let in_range = |path: &str| {
        let key = DocumentPath::new(path).expect("a document path");
        let key = key.suffix_key().to_string();
        key.as_str() >= probe.lower() && key.as_str() < probe.upper()
    };

    assert!(in_range("docs/norn/glossary.md"));
    assert!(in_range("archive/docs/norn/glossary.md"));
    assert!(in_range("norn/glossary.md"));

    assert!(!in_range("docs/norntest/glossary.md"));
    assert!(!in_range("docs/norn/glossary-old.md"));
    assert!(!in_range("docs/other/glossary.md"));
    assert!(!in_range("norn/docs/glossary.md"));
}

/// A target relative to a document is a different resolution mode, and reading
/// one as a suffix would address documents nobody named.
#[test]
fn a_document_relative_target_is_not_a_suffix_address() {
    for target in ["./note.md", "../note.md", "docs/./note.md"] {
        let error = suffix_probe(target).expect_err("a relative target");
        let StoreError::Path { problem, .. } = &error else {
            panic!("`{target}` was refused as {error:?} rather than as a path");
        };
        assert!(problem.contains("relative"), "`{target}`: {problem}");
    }
    assert!(suffix_probe("").is_err());
    assert!(suffix_probe("docs//note").is_err());
}

/// An ambiguity class is a probe of length one, and a document names its own.
#[test]
fn a_class_key_is_the_stem_with_the_separator() {
    let document = DocumentPath::new("docs/norn/glossary.md").expect("a document path");
    assert_eq!(document.class_key(), "glossary/");

    let class = class_probe("glossary");
    assert_eq!(class.lower(), "glossary/");
    assert_eq!(class.upper(), "glossary0");

    // Every suffix target that can address the document opens with the class
    // key, which is why one range answers "which findings does a change to this
    // path reach".
    for target in ["glossary", "norn/glossary", "docs/norn/glossary"] {
        let probe = suffix_probe(target).expect("a suffix target");
        assert!(
            probe.lower().starts_with(document.class_key().as_str()),
            "`{target}` is outside the class of `{}`",
            document.as_str()
        );
    }
}
