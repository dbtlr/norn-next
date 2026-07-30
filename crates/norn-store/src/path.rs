//! The segment-aware path representation, and the probes that read it.
//!
//! Targets resolve by **right-to-left, segment-aligned path suffix**:
//! `glossary` addresses any `**/glossary.md`, `norn/glossary` only
//! `**/norn/glossary.md`, and stem resolution is the one-segment case. That is
//! one grammar across every surface, and this module is how the store makes it
//! an indexed range scan instead of a walk over every path in the vault.
//!
//! # The encoding: segments reversed, leaf stem first, separator-terminated
//!
//! A document's `suffix_key` is its segments in reverse order, with the leaf's
//! final extension removed, each followed by `/`:
//!
//! | path | `suffix_key` | `stem` |
//! |---|---|---|
//! | `glossary.md` | `glossary/` | `glossary` |
//! | `docs/norn/glossary.md` | `glossary/norn/docs/` | `glossary` |
//! | `docs/norn/index.md` | `index/norn/docs/` | `index` |
//!
//! A suffix target becomes the same form, and resolving it is the **prefix**
//! relation between the two: `norn/glossary` becomes `glossary/norn/`, which is
//! a prefix of `glossary/norn/docs/` and of nothing else in the table. Since
//! byte-order ranges over an index are how SQLite answers a prefix predicate
//! without a scan, [`SuffixProbe`] carries the range rather than a pattern:
//! `suffix_key >= lower AND suffix_key < upper`.
//!
//! Three properties make the encoding right rather than merely clever:
//!
//! - **Segment alignment is structural.** Every probe ends with the separator,
//!   so `glossary/norn/` cannot match `glossary/norntest/docs/`. A substring or
//!   `LIKE '%'` formulation has to re-check alignment after the fact; this one
//!   cannot match across a segment boundary at all.
//! - **The extension is dropped from the leaf and only the leaf.** `glossary`
//!   addressing `glossary.md` is the whole reason stem resolution reads as a
//!   one-segment suffix, and dropping the extension in the stored key is what
//!   lets one index answer both. A target that names its extension is reduced
//!   the same way, so `norn/glossary.md` and `norn/glossary` are one probe.
//! - **Bytes are compared as bytes.** Case, dot-prefix and separator
//!   normalization belong to the filesystem seam, which is the workspace's one
//!   path-spelling normalization point. The store therefore requires a
//!   normalized path and refuses one that is obviously not — but it folds
//!   nothing itself, so nothing here can disagree with the seam about what two
//!   paths are.
//!
//! # An ambiguity class is a probe of length one
//!
//! The class a document belongs to is named by its stem, and its bounds are
//! [`class_probe`] over that stem. Both directions of findings maintenance are
//! the same range read: which documents are in a class, and which findings are
//! about a class. See [`crate::ddl::findings`].

use crate::error::StoreError;

/// The separator between segments, in a path and in a reversed key alike.
const SEPARATOR: char = '/';

/// The code point immediately after [`SEPARATOR`], which is what an exclusive
/// upper bound over a separator-terminated prefix ends with.
const PAST_SEPARATOR: char = '0';

const _: () = assert!(
    SEPARATOR as u32 + 1 == PAST_SEPARATOR as u32,
    "an exclusive upper bound is the prefix with its final separator stepped by one, so the two \
     characters have to be adjacent"
);

/// A vault-root-relative document path, with the derived forms the store
/// indexes it by.
///
/// Constructed once, from one input, so that the three columns it feeds cannot
/// disagree with each other.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct DocumentPath {
    path: String,
    suffix_key: String,
    stem: String,
    depth: usize,
}

impl DocumentPath {
    /// Read `path` as a vault-root-relative document path.
    ///
    /// The refusals are the spellings a normalized path does not have: an empty
    /// path, an absolute one, a Windows separator, an empty segment (which is a
    /// doubled or trailing separator), and a `.` or `..` segment. Each of them
    /// would produce a suffix key that addresses the wrong documents, so
    /// refusing is what keeps the index honest about what it holds.
    pub fn new(path: &str) -> Result<Self, StoreError> {
        let refuse = |problem| {
            Err(StoreError::Path {
                path: path.to_string(),
                problem,
            })
        };
        if path.is_empty() {
            return refuse("it is empty");
        }
        if path.starts_with(SEPARATOR) {
            return refuse("it is absolute, and a document path is vault-root-relative");
        }
        if path.contains('\\') {
            return refuse("it carries a backslash; segments are separated by `/`");
        }
        let segments: Vec<&str> = path.split(SEPARATOR).collect();
        for segment in &segments {
            match *segment {
                "" => return refuse("it carries an empty segment"),
                "." | ".." => return refuse("it carries a `.` or `..` segment"),
                _ => {}
            }
        }

        let (leaf, ancestors) = segments
            .split_last()
            .expect("a non-empty path splits into at least one segment");
        let stem = leaf_stem(leaf).to_string();

        let mut suffix_key = String::with_capacity(path.len() + 1);
        suffix_key.push_str(&stem);
        suffix_key.push(SEPARATOR);
        for segment in ancestors.iter().rev() {
            suffix_key.push_str(segment);
            suffix_key.push(SEPARATOR);
        }

        Ok(DocumentPath {
            path: path.to_string(),
            suffix_key,
            stem,
            depth: segments.len(),
        })
    }

    /// The path as written, which is the document's name and unique key.
    pub fn as_str(&self) -> &str {
        &self.path
    }

    /// The segment-reversed key a suffix probe ranges over.
    pub fn suffix_key(&self) -> &str {
        &self.suffix_key
    }

    /// The leaf segment with its final extension removed.
    pub fn stem(&self) -> &str {
        &self.stem
    }

    /// How many segments the path has.
    pub fn depth(&self) -> usize {
        self.depth
    }

    /// The key of the ambiguity class this document belongs to: its stem, with
    /// the separator that makes the match segment-aligned.
    pub fn class_key(&self) -> String {
        class_probe(&self.stem).lower
    }
}

/// The bounds of one prefix range over a segment-reversed key.
///
/// A range rather than a pattern, because a range over an index is what SQLite
/// answers without a scan. `lower` is also the prefix itself, which is the form
/// a finding stores as its class key.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SuffixProbe {
    lower: String,
    upper: String,
}

impl SuffixProbe {
    /// The inclusive lower bound, which is the prefix every key in the range
    /// begins with.
    pub fn lower(&self) -> &str {
        &self.lower
    }

    /// The exclusive upper bound: the prefix with its last byte stepped on, so
    /// that the range holds exactly the keys the prefix opens.
    pub fn upper(&self) -> &str {
        &self.upper
    }
}

/// The probe a suffix target resolves through.
///
/// The target is read as segments, its leaf's final extension is dropped, and
/// the segments are reversed — the same reduction a document path goes through,
/// so that the two meet. The refusals are the spellings that are not a suffix
/// address at all: an empty target, one carrying `.` or `..` (which is a
/// document-relative path, a different resolution mode), and one carrying an
/// empty segment.
pub fn suffix_probe(target: &str) -> Result<SuffixProbe, StoreError> {
    let refuse = |problem| {
        Err(StoreError::Path {
            path: target.to_string(),
            problem,
        })
    };
    let target = target.strip_prefix(SEPARATOR).unwrap_or(target);
    if target.is_empty() {
        return refuse("it is empty");
    }
    let segments: Vec<&str> = target.split(SEPARATOR).collect();
    for segment in &segments {
        match *segment {
            "" => return refuse("it carries an empty segment"),
            "." | ".." => {
                return refuse("it is relative to a document, which is not a suffix address");
            }
            _ => {}
        }
    }
    let (leaf, ancestors) = segments
        .split_last()
        .expect("a non-empty target splits into at least one segment");

    let mut lower = String::with_capacity(target.len() + 1);
    lower.push_str(leaf_stem(leaf));
    lower.push(SEPARATOR);
    for segment in ancestors.iter().rev() {
        lower.push_str(segment);
        lower.push(SEPARATOR);
    }
    Ok(bounded(lower))
}

/// The probe over the ambiguity class a stem names.
///
/// Every document whose leaf reduces to `stem` is in the range, whatever
/// directory it sits in, and so is every finding whose class key opens with it.
pub fn class_probe(stem: &str) -> SuffixProbe {
    bounded(format!("{stem}{SEPARATOR}"))
}

/// The prefix, and the key just past everything it opens.
///
/// `lower` ends with the separator — that is what makes a prefix match
/// segment-aligned — so the exclusive bound is that separator stepped by one,
/// and the arithmetic is exact rather than a byte-level search for a successor.
fn bounded(lower: String) -> SuffixProbe {
    debug_assert!(
        lower.ends_with(SEPARATOR),
        "a probe prefix is separator-terminated: {lower}"
    );
    let mut upper = lower.clone();
    upper.pop();
    upper.push(PAST_SEPARATOR);
    SuffixProbe { lower, upper }
}

/// A leaf segment with its final extension removed.
///
/// The dot has to be inside the name for the extension to be one: `.gitignore`
/// is a name, not an empty stem with an extension, and reducing it to nothing
/// would put every dotfile in the same ambiguity class.
fn leaf_stem(leaf: &str) -> &str {
    match leaf.rfind('.') {
        Some(dot) if dot > 0 => &leaf[..dot],
        _ => leaf,
    }
}
