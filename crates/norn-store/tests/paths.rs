//! The segment-aware path encoding, and the probes that read it.
//!
//! Resolution is a *prefix* relation between two segment-reversed keys, and
//! these cases pin the reduction on both sides of it: what a document path
//! becomes, what a target becomes, and which pairs are therefore in range. The
//! range itself is exercised against real rows in `store/pillars.rs`; here the
//! question is whether the keys say what they are supposed to say.

use std::path::Path;

use norn_store::{ClassKey, DocumentPath, StoreError, class_probe, suffix_probe};

/// The one range a single-reduction probe opens.
fn only_range(target: &str) -> (String, String) {
    let probe = suffix_probe(target).expect("a suffix target");
    assert_eq!(
        probe.range_count(),
        1,
        "`{target}` opens more than one range"
    );
    let (lower, upper) = probe.ranges().next().expect("a range");
    (lower.to_string(), upper.to_string())
}

/// Every prefix a probe opens, in the order it carries them.
fn prefixes(target: &str) -> Vec<String> {
    suffix_probe(target)
        .expect("a suffix target")
        .ranges()
        .map(|(lower, _)| lower.to_string())
        .collect()
}

/// Every ambiguity class a probe names, in the set's own order.
fn class_keys(target: &str) -> Vec<String> {
    suffix_probe(target)
        .expect("a suffix target")
        .class_keys()
        .iter()
        .map(|key| key.as_str().to_string())
        .collect()
}

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
        ("notes/v1.2.md", "v1.2/notes/", "v1.2", 2),
        ("notes.tar.gz", "notes.tar/", "notes.tar", 1),
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

/// Every spelling the document-path grammar refuses, beside the word its refusal
/// has to name.
///
/// One corpus, read twice: once to check that each refusal says what it is, and
/// once to check that [`DocumentPath::rendered`] answers every one of them. A
/// refusal added to the grammar without a rendering fails the second read.
const UNNORMALIZED: &[(&str, &str)] = &[
    ("", "empty"),
    ("/docs/glossary.md", "absolute"),
    ("docs\\glossary.md", "backslash"),
    ("docs//glossary.md", "empty segment"),
    ("docs/glossary.md/", "empty segment"),
    ("docs/./glossary.md", "`.` or `..`"),
    ("../glossary.md", "`.` or `..`"),
    // A leaf whose extension-stripped stem is `.` or `..` would mint a class
    // key out of a segment the same reader refuses as a segment.
    ("..alpha", "`.` or `..` stem"),
    ("docs/..alpha", "`.` or `..` stem"),
    ("...md", "`.` or `..` stem"),
    ("docs/glossary\0.md", "NUL"),
    ("docs/gloss\u{7}ary.md", "control"),
];

/// The spellings a normalized path does not have. Each of them would produce a
/// key that addresses the wrong documents, or bytes no reader can print back.
#[test]
fn an_unnormalized_path_is_refused_with_the_reason() {
    for (path, needle) in UNNORMALIZED.iter().copied() {
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

/// Rendering answers every refusal the grammar states, which is what makes it
/// total: a caller with something to say about a path always has a path to say
/// it about.
#[test]
fn every_refused_spelling_still_renders_to_a_place() {
    for (path, _) in UNNORMALIZED.iter().copied() {
        let rendered = DocumentPath::rendered(Path::new(path));
        assert!(
            DocumentPath::new(rendered.as_str()).is_ok(),
            "`{path}` rendered to `{}`, which the grammar refuses",
            rendered.as_str()
        );
    }
}

/// A spelling the grammar admits renders to itself. The rendering is a fallback,
/// never a normalization, so nothing a caller could have parsed is rewritten.
#[test]
fn a_spelling_the_grammar_admits_renders_to_itself() {
    for path in ["glossary.md", "docs/norn/glossary.md", ".gitignore"] {
        assert_eq!(DocumentPath::rendered(Path::new(path)).as_str(), path);
    }
}

/// What each refused class renders to. The marker stands in for the characters
/// the grammar refuses, and ahead of a segment it refuses whole.
#[test]
fn a_refused_spelling_renders_the_bytes_the_grammar_refuses() {
    for (path, expected) in [
        ("docs\\glossary.md", "docs\u{fffd}glossary.md"),
        ("docs/gloss\u{7}ary.md", "docs/gloss\u{fffd}ary.md"),
        ("docs/glossary\0.md", "docs/glossary\u{fffd}.md"),
        ("docs//glossary.md", "docs/\u{fffd}/glossary.md"),
        ("/docs/glossary.md", "\u{fffd}/docs/glossary.md"),
        ("docs/./glossary.md", "docs/\u{fffd}./glossary.md"),
        ("../glossary.md", "\u{fffd}../glossary.md"),
        ("..alpha", "\u{fffd}..alpha"),
        ("docs/..alpha", "docs/\u{fffd}..alpha"),
        ("", "\u{fffd}"),
    ] {
        assert_eq!(DocumentPath::rendered(Path::new(path)).as_str(), expected);
    }
}

/// Path bytes that are not UTF-8 render through the same marker as a character
/// the grammar refuses, because a reader has one question about both.
#[cfg(unix)]
#[test]
fn path_bytes_that_are_not_utf8_render_through_the_marker() {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;

    let path = Path::new(OsStr::from_bytes(b"docs/bad-\xff.md"));
    assert_eq!(
        DocumentPath::rendered(path).as_str(),
        "docs/bad-\u{fffd}.md"
    );
}

/// The probe a target with no dot in its leaf reduces to, and the pairs that are
/// therefore in range.
///
/// The upper bound is the prefix with its final separator stepped by one, so a
/// key is in range exactly when it opens with the prefix — which is what makes
/// the predicate a range over an index rather than a pattern match. The step is
/// a **byte** step, and the comparison is `BINARY`; both are what make it exact.
#[test]
fn a_suffix_target_reduces_to_the_prefix_its_candidates_share() {
    for (target, lower) in [
        ("glossary", "glossary/"),
        ("norn/glossary", "glossary/norn/"),
        ("docs/norn/glossary", "glossary/norn/docs/"),
    ] {
        let (read_lower, read_upper) = only_range(target);
        assert_eq!(read_lower, lower, "the probe for `{target}`");
        assert_eq!(read_upper, format!("{}0", &lower[..lower.len() - 1]));
    }
}

/// **A dot in a written target is ambiguous, so the target probes both
/// reductions.** Nothing in the string says whether the dot opens an extension,
/// and choosing one reading answers confidently and sometimes wrongly.
#[test]
fn a_target_whose_leaf_carries_a_dot_opens_both_reductions() {
    // `v1.2` is a version, and reducing always would resolve it to `v1` — a
    // single, confident, wrong candidate.
    assert_eq!(prefixes("v1.2"), vec!["v1/", "v1.2/"]);
    // `notes.tar` is a name, and reducing never would leave `notes.tar.gz`
    // unreachable by anything shorter than its whole leaf.
    assert_eq!(prefixes("notes.tar"), vec!["notes/", "notes.tar/"]);
    // A target that names its extension reaches the document that dropped it,
    // which is the case the reduction existed for in the first place.
    assert_eq!(prefixes("glossary.md"), vec!["glossary/", "glossary.md/"]);
    assert_eq!(
        prefixes("norn/glossary.md"),
        vec!["glossary/norn/", "glossary.md/norn/"]
    );

    // A dotfile has one reduction, because the leading dot is part of the name.
    assert_eq!(prefixes(".env"), vec![".env/"]);
    // And a dotfile with an extension has two, the wider of which is the class
    // the store puts `.env` and `.env.local` in together.
    assert_eq!(prefixes(".env.local"), vec![".env/", ".env.local/"]);

    // Every range is bounded the same way, whichever reduction produced it.
    for (lower, upper) in suffix_probe("v1.2").expect("a suffix target").ranges() {
        assert_eq!(upper, format!("{}0", &lower[..lower.len() - 1]));
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
        probe
            .ranges()
            .any(|(lower, upper)| key.as_str() >= lower && key.as_str() < upper)
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
/// one as a suffix would address documents nobody named. So is a root-anchored
/// one: widening `/norn/glossary` to every `**/norn/glossary.md` would answer a
/// question about one path with an answer about a whole class.
#[test]
fn a_target_that_is_not_a_suffix_address_is_refused() {
    for (target, needle) in [
        ("./note.md", "relative"),
        ("../note.md", "relative"),
        ("docs/./note.md", "relative"),
        ("/norn/glossary", "absolute"),
        ("", "empty"),
        ("docs//note", "empty segment"),
        ("..alpha", "`.` or `..` stem"),
        ("note\0.md", "NUL"),
    ] {
        let error = suffix_probe(target).expect_err("a target that is not a suffix address");
        let StoreError::Path { problem, .. } = &error else {
            panic!("`{target}` was refused as {error:?} rather than as a path");
        };
        assert!(
            problem.contains(needle),
            "`{target}` was refused for `{problem}`, which does not name {needle}"
        );
    }
}

/// An ambiguity class is a probe of length one, and a document names its own.
#[test]
fn a_class_key_is_the_stem_with_the_separator() {
    let document = DocumentPath::new("docs/norn/glossary.md").expect("a document path");
    assert_eq!(document.class_key().as_str(), "glossary/");

    let class = class_probe("glossary").expect("a class stem");
    assert_eq!(class.range_count(), 1);
    let (lower, upper) = class.ranges().next().expect("a range");
    assert_eq!(lower, "glossary/");
    assert_eq!(upper, "glossary0");

    // Every suffix target that can address the document opens with the class
    // key, which is why one range answers "which findings does a change to this
    // path reach".
    for target in ["glossary", "norn/glossary", "docs/norn/glossary"] {
        let probe = suffix_probe(target).expect("a suffix target");
        for (lower, _) in probe.ranges() {
            assert!(
                lower.starts_with(document.class_key().as_str()),
                "`{target}` is outside the class of `{}`",
                document.as_str()
            );
        }
    }

    // A probe names one class per reduction, and a two-reduction target names
    // both: the reductions are disjoint rather than nested, because `.` (0x2e)
    // sorts before `/` (0x2f), so neither range holds the other's keys and one key
    // could not stand for both.
    assert_eq!(class_keys("glossary"), vec!["glossary/"]);
    assert_eq!(class_keys("glossary.md"), vec!["glossary.md/", "glossary/"]);
    assert_eq!(class_keys("v1.2"), vec!["v1.2/", "v1/"]);
    assert_eq!(class_keys("notes.tar"), vec!["notes.tar/", "notes/"]);
    assert!("notes.tar/" < "notes/");
    assert!(!"notes.tar/".starts_with("notes/"));

    // The document each of those targets can resolve to is in one of the classes
    // the target named, which is what makes a change to it reach a finding about
    // the target.
    for (target, at) in [
        ("glossary.md", "docs/glossary.md"),
        ("v1.2", "notes/v1.2.md"),
        ("notes.tar", "archive/notes.tar.gz"),
    ] {
        let document = DocumentPath::new(at).expect("a document path");
        assert!(
            suffix_probe(target)
                .expect("a suffix target")
                .class_keys()
                .contains(&document.class_key()),
            "`{target}` does not name the class `{at}` is in"
        );
    }
}

/// **A class key is validated, because an unterminated one names a range no probe
/// opens.** A finding stored under one is at rest and permanently invisible to
/// the maintenance that owns its lifecycle.
#[test]
fn a_class_key_that_no_probe_opens_is_refused() {
    for text in ["glossary/", "glossary/norn/", ".env/"] {
        assert_eq!(
            ClassKey::new(text).expect("a class key").as_str(),
            text,
            "`{text}` is a class key"
        );
    }
    for (text, needle) in [
        ("", "empty"),
        ("glossary", "separator-terminated"),
        ("glossary/norn", "separator-terminated"),
        ("glossary//", "empty segment"),
        ("glossary/./", "`.` or `..`"),
        ("../", "`.` or `..`"),
        ("glossary\0/", "NUL"),
    ] {
        let error = ClassKey::new(text).expect_err("not a class key");
        let StoreError::Path { problem, .. } = &error else {
            panic!("`{text}` was refused as {error:?} rather than as a path");
        };
        assert!(
            problem.contains(needle),
            "`{text}` was refused for `{problem}`, which does not name {needle}"
        );
    }
}

/// **`class_probe` validates like every other public constructor here.** A
/// stem handed over unvalidated would format into a lower bound
/// [`ClassKey::of_prefix`] trusts, tripping its debug assertion downstream
/// instead of being refused at the boundary that took it.
#[test]
fn a_class_probe_refuses_what_no_class_opens() {
    for (stem, needle) in [
        ("", "empty"),
        ("glossary/norn", "separator"),
        (".", "`.` or `..`"),
        ("..", "`.` or `..`"),
        ("gloss\0ary", "NUL"),
        ("gloss\u{7}ary", "control"),
    ] {
        let error = class_probe(stem).expect_err("not a class stem");
        let StoreError::Path { problem, .. } = &error else {
            panic!("`{stem}` was refused as {error:?} rather than as a path");
        };
        assert!(
            problem.contains(needle),
            "`{stem}` was refused for `{problem}`, which does not name {needle}"
        );
    }
}
