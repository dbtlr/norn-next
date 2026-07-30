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
//! | path | `suffix_key` | stem |
//! |---|---|---|
//! | `glossary.md` | `glossary/` | `glossary` |
//! | `docs/norn/glossary.md` | `glossary/norn/docs/` | `glossary` |
//! | `docs/norn/index.md` | `index/norn/docs/` | `index` |
//!
//! A suffix target becomes the same form, and resolving it is the **prefix**
//! relation between the two: `norn/glossary` becomes `glossary/norn/`, which is
//! a prefix of `glossary/norn/docs/` and of nothing else in the table. Since
//! byte-order ranges over an index are how SQLite answers a prefix predicate
//! without a scan, [`SuffixProbe`] carries ranges rather than a pattern:
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
//!   lets one index answer both.
//! - **Bytes are compared as bytes.** Case, dot-prefix and separator
//!   normalization belong to the filesystem seam, which is the workspace's one
//!   path-spelling normalization point. The store therefore requires a
//!   normalized path and refuses one that is obviously not — but it folds
//!   nothing itself, so nothing here can disagree with the seam about what two
//!   paths are. **`BINARY` collation is load-bearing**: it is what makes the
//!   exclusive upper bound below exact, and a case-insensitive collation on
//!   `suffix_key` would silently change which keys a range holds.
//!
//! # A target has two reductions, and it probes both
//!
//! Dropping a final extension is unambiguous on the stored side — the store
//! holds the whole path and knows which segment is the leaf — and ambiguous on
//! the target side, because a dot in a written target may be an extension or may
//! be part of the name. `v1.2` is a version, `notes.md` is a file, and nothing
//! in the two strings tells them apart.
//!
//! So a target reduces **both ways** and the probe is the union: `v1.2` opens
//! `v1.2/` and `v1/`, `notes.tar` opens `notes.tar/` and `notes/`. Choosing one
//! reduction would answer confidently and wrongly — reducing always would make
//! `v1.2` resolve to `notes/v1.md` as a single candidate, and reducing never
//! would leave `notes.tar.gz` unreachable by any target that names less than the
//! whole leaf. Two ranges cost two index seeks and report the ambiguity that is
//! really there.
//!
//! A target with no dot in its leaf has one reduction and one range.
//!
//! # An ambiguity class is a probe of length one
//!
//! The class a document belongs to is named by its stem, and its bounds are
//! [`class_probe`] over that stem. Both directions of findings maintenance are
//! the same range read: which documents are in a class, and which findings are
//! about a class. See [`crate::ddl::findings`].
//!
//! **The two reductions are two classes, not one.** They are not nested: `.` is
//! `0x2e` and `/` is `0x2f`, so `notes.tar/` sorts *before* `notes/` rather than
//! inside it, and neither reduction's range holds the other's keys. A probe
//! therefore names a **set** of class keys — [`SuffixProbe::class_keys`] — and a
//! finding recorded from one belongs to every class in that set.

use std::collections::BTreeSet;

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

const _: () = assert!(
    SEPARATOR.len_utf8() == 1,
    "the upper bound replaces the final separator with one character, so the separator has to be \
     one byte for the arithmetic to be a byte step"
);

/// A vault-root-relative document path, with the derived forms the store
/// indexes it by.
///
/// Constructed once, from one input, so that the columns it feeds and the forms
/// a reader recomputes cannot disagree with each other.
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
    /// doubled or trailing separator), a `.` or `..` segment, a leaf whose
    /// extension-stripped stem is `.` or `..`, and a NUL or control byte. Each
    /// of them would produce a suffix key that addresses the wrong documents —
    /// or, for a control byte, a key whose bytes no reader can print back — so
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
        if let Some(problem) = control_byte_problem(path) {
            return refuse(problem);
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
        let stem = leaf_stem(leaf);
        if stem == "." || stem == ".." {
            return refuse("its leaf reduces to a `.` or `..` stem, which names no class");
        }
        let stem = stem.to_string();

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
    ///
    /// Emitting a candidate as its *minimal disambiguating suffix* needs to know
    /// how much of a path to take, and this is that number, derived here from the
    /// same input the keys are.
    pub fn depth(&self) -> usize {
        self.depth
    }

    /// The key of the ambiguity class this document belongs to: its stem, with
    /// the separator that makes the match segment-aligned.
    ///
    /// One class, not a set: the store holds the whole path, so which segment is
    /// the leaf and which bytes are its extension are settled facts here. The
    /// ambiguity is the written target's, and it is [`SuffixProbe::class_keys`]
    /// that carries it.
    pub fn class_key(&self) -> ClassKey {
        // A document's own stem already passed every refusal `class_probe`
        // applies — it came from a leaf segment `DocumentPath::new` already
        // checked for a `.`/`..` reduction and a control byte, and a leaf
        // segment carries no separator by construction.
        let probe = class_probe(&self.stem).expect("a document's own stem is a valid class probe");
        ClassKey::of_prefix(&probe.ranges[0].lower)
    }
}

/// The key of one ambiguity class, as a finding stores it.
///
/// A class key is a **separator-terminated segment-reversed prefix** —
/// `glossary/`, `glossary/norn/` — which is exactly the lower bound of the range
/// that opens the class. The type exists because that is not a property text
/// happens to have: a class key missing its terminator names a prefix no probe's
/// range covers, so a finding stored under one is at rest and permanently
/// invisible to the maintenance that owns its lifecycle.
///
/// It is produced by [`DocumentPath::class_key`] and [`SuffixProbe::class_keys`],
/// which is where the two sides of resolution mint the same form.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ClassKey(String);

impl ClassKey {
    /// Read `text` as a class key, refusing what no probe would open.
    pub fn new(text: &str) -> Result<Self, StoreError> {
        let refuse = |problem| {
            Err(StoreError::Path {
                path: text.to_string(),
                problem,
            })
        };
        if text.is_empty() {
            return refuse("it is empty");
        }
        if let Some(problem) = control_byte_problem(text) {
            return refuse(problem);
        }
        if !text.ends_with(SEPARATOR) {
            return refuse(
                "it is not separator-terminated, so it names a prefix no ambiguity class opens",
            );
        }
        let segments = text.strip_suffix(SEPARATOR).unwrap_or(text);
        for segment in segments.split(SEPARATOR) {
            match segment {
                "" => return refuse("it carries an empty segment"),
                "." | ".." => return refuse("it carries a `.` or `..` segment"),
                _ => {}
            }
        }
        Ok(ClassKey(text.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The probe over exactly this class: one range, this key as its lower
    /// bound.
    ///
    /// A class key **is** a probe's lower bound, so this is the round trip back.
    /// It is what findings maintenance ranges over once a changed path has named
    /// the class it affects, and building the range here rather than from the
    /// stem again is what keeps the key that is reported and the key that is
    /// discarded the same bytes.
    pub(crate) fn probe(&self) -> SuffixProbe {
        SuffixProbe {
            ranges: vec![bounded(self.0.clone())],
        }
    }

    /// A probe prefix as the class key it is.
    ///
    /// The prefix comes from [`bounded`], which holds the terminator, and its
    /// segments are the target's or the stem's — so what [`ClassKey::new`] refuses
    /// is what the probe's own construction already refused. The check is kept as
    /// a debug assertion rather than dropped, because this is the one place a key
    /// enters the type without being read as text.
    fn of_prefix(prefix: &str) -> Self {
        debug_assert!(
            ClassKey::new(prefix).is_ok(),
            "a probe prefix is a class key: {prefix}"
        );
        ClassKey(prefix.to_string())
    }
}

/// The bounds of the prefix ranges over a segment-reversed key that one probe
/// opens.
///
/// Ranges rather than patterns, because a range over an index is what SQLite
/// answers without a scan. A probe carries one range or two: a target whose leaf
/// carries a dot has two reductions and opens both.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SuffixProbe {
    ranges: Vec<Range>,
}

/// One prefix range. `lower` is also the prefix itself, which is the form a
/// finding stores as its class key.
#[derive(Clone, Debug, Eq, PartialEq)]
struct Range {
    lower: String,
    upper: String,
}

impl SuffixProbe {
    /// Every range this probe opens, as `(lower, upper)` pairs: `lower`
    /// inclusive, `upper` exclusive.
    ///
    /// `upper` is the prefix with its final separator stepped on, so a range
    /// holds exactly the keys its prefix opens. There is always at least one.
    pub fn ranges(&self) -> impl ExactSizeIterator<Item = (&str, &str)> {
        self.ranges
            .iter()
            .map(|range| (range.lower.as_str(), range.upper.as_str()))
    }

    /// How many ranges the probe opens: one, or two for a target with two
    /// reductions.
    pub fn range_count(&self) -> usize {
        self.ranges.len()
    }

    /// Every ambiguity class this probe reads: one per reduction, so one key or
    /// two.
    ///
    /// A finding recorded from a probe belongs to **all** of them, because the
    /// reductions are disjoint rather than nested. `notes.tar` opens `notes.tar/`
    /// and `notes/`, and under `BINARY` collation `notes.tar/` sorts before
    /// `notes/` — so neither range holds the other's keys, and a finding filed
    /// under one alone is invisible to maintenance that named the other. Which is
    /// a finding nothing ever revisits: the hazard class-scoped maintenance
    /// exists to close.
    pub fn class_keys(&self) -> BTreeSet<ClassKey> {
        self.ranges
            .iter()
            .map(|range| ClassKey::of_prefix(&range.lower))
            .collect()
    }
}

/// The probe a suffix target resolves through.
///
/// The target is read as segments and its segments are reversed, with the leaf
/// taken **both** as written and with its final extension dropped — see the
/// module documentation for why one reduction cannot be chosen. The refusals are
/// the spellings that are not a suffix address at all: an empty target, an
/// absolute one, one carrying `.` or `..` (which is a document-relative path, a
/// different resolution mode), one carrying an empty segment, one whose leaf
/// reduces to a `.` or `..` stem, and one carrying a control byte.
pub fn suffix_probe(target: &str) -> Result<SuffixProbe, StoreError> {
    let refuse = |problem| {
        Err(StoreError::Path {
            path: target.to_string(),
            problem,
        })
    };
    if target.is_empty() {
        return refuse("it is empty");
    }
    if target.starts_with(SEPARATOR) {
        return refuse("it is absolute, and a suffix address is a suffix of a relative path");
    }
    if let Some(problem) = control_byte_problem(target) {
        return refuse(problem);
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
    let stem = leaf_stem(leaf);
    if stem == "." || stem == ".." {
        return refuse("its leaf reduces to a `.` or `..` stem, which names no class");
    }

    let reversed = |head: &str| {
        let mut lower = String::with_capacity(target.len() + 1);
        lower.push_str(head);
        lower.push(SEPARATOR);
        for segment in ancestors.iter().rev() {
            lower.push_str(segment);
            lower.push(SEPARATOR);
        }
        lower
    };

    let mut ranges = vec![bounded(reversed(stem))];
    if stem != *leaf {
        ranges.push(bounded(reversed(leaf)));
    }
    Ok(SuffixProbe { ranges })
}

/// The probe over the ambiguity class a stem names.
///
/// Every document whose leaf reduces to `stem` is in the range, whatever
/// directory it sits in, and so is every finding whose class key opens with it.
/// One reduction and therefore one range: a stem is already reduced.
///
/// The refusals mirror [`ClassKey::new`] and [`suffix_probe`]: an empty stem,
/// one carrying the separator (a stem is one segment, never a path), one that
/// is a `.` or `..` name, and one carrying a control byte. Formatting an
/// unrefused stem into a lower bound is what the class key's own debug
/// assertion trusts, so a caller-supplied stem is checked here rather than
/// left to trip that assertion later.
pub fn class_probe(stem: &str) -> Result<SuffixProbe, StoreError> {
    let refuse = |problem| {
        Err(StoreError::Path {
            path: stem.to_string(),
            problem,
        })
    };
    if stem.is_empty() {
        return refuse("it is empty");
    }
    if stem.contains(SEPARATOR) {
        return refuse("it carries a separator, and a stem is one segment");
    }
    if stem == "." || stem == ".." {
        return refuse("it is a `.` or `..` name, which names no class");
    }
    if let Some(problem) = control_byte_problem(stem) {
        return refuse(problem);
    }
    Ok(SuffixProbe {
        ranges: vec![bounded(format!("{stem}{SEPARATOR}"))],
    })
}

/// The prefix, and the key just past everything it opens.
///
/// `lower` ends with the separator — that is what makes a prefix match
/// segment-aligned — so the exclusive bound is that separator stepped by one,
/// and the arithmetic is exact rather than a byte-level search for a successor.
fn bounded(lower: String) -> Range {
    debug_assert!(
        lower.ends_with(SEPARATOR),
        "a probe prefix is separator-terminated: {lower}"
    );
    let mut upper = lower.clone();
    upper.pop();
    upper.push(PAST_SEPARATOR);
    Range { lower, upper }
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

/// The refusal for a NUL or other control byte, which no printable key holds and
/// which a C string reader would cut short.
fn control_byte_problem(text: &str) -> Option<&'static str> {
    if text.contains('\0') {
        return Some("it carries a NUL byte");
    }
    text.chars()
        .any(char::is_control)
        .then_some("it carries a control character")
}
