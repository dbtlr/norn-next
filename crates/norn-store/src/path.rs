//! The segment-aware path representation and indexed suffix probes.
//!
//! A suffix names every document whose vault path ends in its segments.
//! `glossary` names `**/glossary.md`, and `norn/glossary` names
//! `**/norn/glossary.md`. The complete named path is normalized before its
//! Markdown extension is removed. The key reverses the remaining segments
//! and terminates each with `/`:
//! `docs/norn/glossary.md` becomes `glossary/norn/docs/`.
//!
//! Paths and targets use [`comparison_key`] on every root. Stored paths keep
//! their original spelling for filesystem identity and answer ordering; the
//! suffix and path comparison columns are derived beside them. `BINARY`
//! collation is load-bearing: a separator-terminated prefix and its bytewise
//! successor bound exactly its indexed range, with no table scan.
//!
//! Only `.md` is optional, in any ASCII case. A target `v1.2` opens `v1.2/`
//! alone, while `note.md` opens both `note/` and `note.md/`, naming
//! `note.md` and `note.md.md`. Neither spelling takes precedence. These two
//! ranges are disjoint: `.` sorts before `/`, so neither prefixes the other.
//! A finding records both class keys so either class changing reaches it.
//!
//! A document's class is its normalized stem followed by `/`; a path key
//! is its normalized complete vault path. The two spaces drive the class and
//! path maintenance that keeps link health current inside each changeset.

use std::collections::BTreeSet;
use std::path::Path;

use unicode_normalization::UnicodeNormalization;

use norn_wire::{PathProblem, is_refused_character, is_refused_segment};

use crate::error::StoreError;

/// The separator between segments, in a path and in a reversed key alike.
const SEPARATOR: char = '/';

/// What [`DocumentPath::rendered`] puts in place of anything the grammar
/// refuses.
///
/// The Unicode replacement character, which is already what lossy UTF-8 decoding
/// produces — so one marker covers undecodable bytes and refused characters
/// alike, and a reader sees one symbol for "the vault's spelling is not
/// printable here".
pub const RENDERED_MARKER: char = '\u{FFFD}';

/// A vault-root-relative document path, with the derived forms the store
/// indexes it by.
///
/// Constructed once, from one input, so that the columns it feeds and the forms
/// a reader recomputes cannot disagree with each other.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct DocumentPath {
    path: String,
    suffix_key: String,
    path_key: String,
    stem: String,
    depth: usize,
}

impl From<&norn_wire::DocumentPath> for DocumentPath {
    /// A wire document path as the store names it. Infallible: the wire reads
    /// a document path through the grammar [`DocumentPath::new`] refuses by,
    /// so a path that crossed the seam is one the store holds.
    fn from(path: &norn_wire::DocumentPath) -> Self {
        DocumentPath::new(path.as_str())
            .expect("the wire and the store read one document-path grammar")
    }
}

impl DocumentPath {
    /// Read `path` as a vault-root-relative document path.
    ///
    /// The refusals are the wire's document-path grammar
    /// ([`PathProblem::of_document`]), the one every reader of a document
    /// path keeps. Each of them would produce a suffix key that addresses the
    /// wrong documents — or, for a control character, a key whose bytes no
    /// reader can print back — so refusing is what keeps the index honest
    /// about what it holds.
    pub fn new(path: &str) -> Result<Self, StoreError> {
        if let Some(problem) = PathProblem::of_document(path) {
            return Err(StoreError::Path {
                path: path.to_string(),
                problem: problem.message(),
            });
        }
        let path_key = comparison_key(path);
        let leaf = path.rsplit(SEPARATOR).next().expect("a path has a leaf");
        let suffix_key = if is_vault_file_leaf(leaf) {
            suffix_in_document_frame(path)
        } else {
            suffix_of_path(&path_key)
        };
        Ok(DocumentPath {
            path: path.to_string(),
            path_key,
            suffix_key,
            stem: markdown_stem(leaf).to_string(),
            depth: path.split(SEPARATOR).count(),
        })
    }

    /// A document path naming the place a spelling occupies, whether or not the
    /// grammar admits that spelling.
    ///
    /// **Total**, which is what makes it usable where something has to be said
    /// *about* a path no document can be stored under: a caller that cannot be
    /// handed a path has nowhere to file what it knows, and silence is the
    /// outcome that has to be impossible. It is built by rendering rather than
    /// by parsing — bytes that are not UTF-8 and every character
    /// [`DocumentPath::new`] refuses become [`RENDERED_MARKER`], a segment the
    /// grammar refuses whole carries the marker ahead of it, and a leaf whose
    /// stem the grammar refuses carries it ahead of the leaf. The renderings sit
    /// beside the refusals they answer, so the two move together; a caller
    /// spelling the refusal set itself would drift the moment the grammar did.
    ///
    /// **Totality rests on the two candidates below answering every refusal.**
    /// Per-segment rendering answers all of them but the leaf's stem, and
    /// marking ahead of the leaf answers that one — so a refusal added to the
    /// grammar without a rendering leaves neither candidate standing and this
    /// panics rather than collapsing every such path onto one subject. The
    /// corpus in `tests/paths.rs` is what holds the two sets together.
    ///
    /// **A rendering names a place, never an identity.** Nothing derives a
    /// document under one. A real document whose own name carries the marker
    /// occupies the same key as a rendering of some other path, and that
    /// collision is visible — one key, one row — rather than silent.
    pub fn rendered(path: &Path) -> Self {
        if let Some(spelling) = path.to_str()
            && let Ok(document) = DocumentPath::new(spelling)
        {
            return document;
        }
        let separator = SEPARATOR.to_string();
        let rendered = path
            .to_string_lossy()
            .split(SEPARATOR)
            .map(rendered_segment)
            .collect::<Vec<String>>()
            .join(&separator);
        // The leaf is the one segment whose *stem* the grammar reads, so a leaf
        // that survives its own rendering can still reduce to a name that opens
        // no ambiguity class. Marking ahead of the leaf is the shortest
        // rendering that leaves the rest of the name readable.
        let (ancestors, leaf) = rendered
            .rsplit_once(SEPARATOR)
            .unwrap_or(("", rendered.as_str()));
        let marked = if ancestors.is_empty() {
            format!("{RENDERED_MARKER}{leaf}")
        } else {
            format!("{ancestors}{separator}{RENDERED_MARKER}{leaf}")
        };
        [rendered, marked]
            .into_iter()
            .find_map(|candidate| DocumentPath::new(&candidate).ok())
            .expect("every refusal the grammar states has a rendering that answers it")
    }

    /// The path as written, which is the document's name and unique key.
    pub fn as_str(&self) -> &str {
        &self.path
    }

    /// Whether the path names a vault file: its leaf carries a non-Markdown
    /// extension and no segment leads with a dot. Obsidian ignores every
    /// dot-led file and folder, so a dotfile, or any file under `.git`,
    /// `.obsidian` or `.trash`, is no link target; an extensionless leaf names
    /// none either.
    pub fn is_vault_file(&self) -> bool {
        let mut segments = self.path.rsplit('/');
        let leaf = segments.next().expect("a path has a leaf");
        is_vault_file_leaf(leaf) && !segments.any(|folder| folder.starts_with('.'))
    }

    /// Whether this path carries [`RENDERED_MARKER`] anywhere in it.
    ///
    /// This is what a caller asks to tell a place a rendering can name from one
    /// only a spelling the grammar admits can reach: [`DocumentPath::rendered`]
    /// answers every refusal by putting the marker in the segment it answers, so
    /// a place with no marker is nothing's rendering. A real document whose own
    /// name carries the marker answers `true` as well — that is the collision
    /// `rendered` documents, one key holding one row, and the key alone does not
    /// say which side spelled it.
    pub fn carries_marker(&self) -> bool {
        self.path.contains(RENDERED_MARKER)
    }

    /// The longest ancestor of this path whose segments are spelled the way the
    /// vault spells them.
    ///
    /// Rendering is per segment, so every segment before the first one carrying
    /// [`RENDERED_MARKER`] is a name the vault holds as written — which is what
    /// makes this the narrowest root a search for the spellings rendering to
    /// this place can start from. A path with no marker at all is its own
    /// spelling throughout, so the whole path is the answer; a path whose first
    /// segment carries the marker has no such ancestor but the vault root, which
    /// is the empty spelling.
    ///
    /// The answer is always a segment-aligned prefix of the path, and it stays
    /// one only because a rendering marks inside the segment it answers. The
    /// corpus in `tests/paths.rs` is what holds the two together.
    pub fn unrendered_ancestor(&self) -> &str {
        match self
            .path
            .split(SEPARATOR)
            .position(|segment| segment.contains(RENDERED_MARKER))
        {
            None => &self.path,
            Some(0) => "",
            // The separator before the marked segment is what the length of the
            // ancestor stops one short of.
            Some(marked) => {
                let spanned: usize = self
                    .path
                    .split(SEPARATOR)
                    .take(marked)
                    .map(|segment| segment.len() + SEPARATOR.len_utf8())
                    .sum();
                &self.path[..spanned - SEPARATOR.len_utf8()]
            }
        }
    }

    /// The segment-reversed key a suffix probe ranges over.
    pub fn suffix_key(&self) -> &str {
        &self.suffix_key
    }

    /// The comparison key of the complete vault path.
    pub fn path_key(&self) -> &str {
        &self.path_key
    }

    /// The leaf segment with only its Markdown extension removed.
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

    /// The suffix addresses that could name this document, shortest first.
    /// For each trailing segment count, the leaf's stem precedes its complete
    /// spelling. Only `.md` is removed. The first spelling whose class contains
    /// the candidate alone is its minimal disambiguating suffix.
    pub fn suffix_spellings(&self) -> impl Iterator<Item = String> + '_ {
        let segments: Vec<&str> = self.path.split(SEPARATOR).collect();
        let leaf = *segments.last().expect("a path has a leaf");
        let with_extension = (leaf != self.stem).then_some(leaf);
        (1..=segments.len()).flat_map(move |count| {
            let ancestors = segments[segments.len() - count..segments.len() - 1].join("/");
            let spelled = move |leaf: &str| {
                if ancestors.is_empty() {
                    leaf.to_string()
                } else {
                    format!("{ancestors}{SEPARATOR}{leaf}")
                }
            };
            std::iter::once(spelled(&self.stem)).chain(with_extension.map(spelled))
        })
    }

    /// The indexed bounds containing this path's segment-aligned descendants.
    ///
    /// The path itself is not inside these bounds; a subtree query includes it
    /// separately. The separator is load-bearing: `a/ <= path < a0` contains
    /// `a/child.md`, but not `ab/child.md`.
    pub(crate) fn descendant_bounds(&self) -> (String, String) {
        descendant_bounds(&self.path)
    }

    /// The normalized stem class this document belongs to.
    /// A changeset names this key to reach findings about the same class.
    pub(crate) fn class_key_in(&self, _key: SuffixKey) -> ClassKey {
        let end = self
            .suffix_key
            .find(SEPARATOR)
            .expect("a suffix has a leaf");
        ClassKey::of_prefix(&self.suffix_key[..=end])
    }

    /// The normalized complete path a path-addressed link is filed under.
    /// Document and link keys use [`spell_path_key`] so changeset maintenance
    /// and link resolution agree.
    pub(crate) fn path_key_in(&self, key: SuffixKey) -> PathKey {
        // Normalization preserves the document-path grammar.
        PathKey(spell_path_key(&self.path, key))
    }
}

/// The comparison key of a complete vault path.
/// Documents and links call this same function for path-keyed maintenance.
pub(crate) fn spell_path_key(path: &str, _key: SuffixKey) -> String {
    comparison_key(path)
}

/// A vault-root-relative directory, as the range of stored paths beneath it.
///
/// **A directory that names no document still holds documents.** `..md` reduces
/// to a `.` stem and is refused as a document path, while `..md/note.md` is an
/// ordinary path the store keeps a row for — so the rows under such a directory
/// need an address its own spelling cannot give them. This is that address: the
/// same segment-aligned prefix range `DocumentPath::descendant_bounds` opens,
/// taken from a spelling that is a directory and nothing more.
///
/// The refusals are the wire's segment rules ([`PathProblem::of_segments`])
/// and stop there, because a stem is a document's property and a directory
/// has none. A spelling those refusals
/// reject poisons every path beneath it — no descendant of a segment carrying a
/// backslash or a control byte is storable either — so refusing here is
/// refusing an address that would range over nothing.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct DirectoryPrefix {
    path: String,
}

impl DirectoryPrefix {
    /// Read `path` as a vault-root-relative directory.
    pub fn new(path: &str) -> Result<Self, StoreError> {
        match PathProblem::of_segments(path) {
            Some(problem) => Err(StoreError::Path {
                path: path.to_string(),
                problem: problem.message(),
            }),
            None => Ok(DirectoryPrefix {
                path: path.to_string(),
            }),
        }
    }

    /// The directory as written.
    pub fn as_str(&self) -> &str {
        &self.path
    }

    /// The indexed bounds containing this directory's segment-aligned
    /// descendants. The directory itself is never a row inside them.
    pub(crate) fn descendant_bounds(&self) -> (String, String) {
        descendant_bounds(&self.path)
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
/// It is produced by `DocumentPath::class_key_in` (crate-private; a store
/// mints one through [`crate::Request::class_key_of`]) and
/// [`SuffixProbe::class_keys`], which is where the two sides of resolution mint
/// the same form.
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

    /// The suffix address this class key is the reversal of: its segments in
    /// the opposite order, joined by the separator. `glossary/norn/` is the
    /// key `norn/glossary` opens first, so that is the address it reads back
    /// as.
    ///
    /// The address's own probe opens this class, and where its leaf carries a
    /// `.md`, the class of the extensionless stem as well: `note.md/`
    /// reads back as `note.md`, which opens `note.md/` and `note/`.
    pub fn address(&self) -> String {
        let segments = self.0.strip_suffix(SEPARATOR).unwrap_or(&self.0);
        let reversed: Vec<&str> = segments.split(SEPARATOR).rev().collect();
        reversed.join(&SEPARATOR.to_string())
    }

    /// The bounds of exactly this class: this key as the inclusive lower
    /// bound, and the key just past everything it opens as the exclusive
    /// upper one.
    ///
    /// A class key **is** a probe's lower bound, so this is the round trip back.
    /// It is what findings maintenance ranges over once a changed path has named
    /// the class it affects, and building the range here rather than from the
    /// stem again is what keeps the key that is reported and the key that is
    /// discarded the same bytes.
    pub(crate) fn bounds(&self) -> (String, String) {
        let Range { lower, upper } = bounded(self.0.clone());
        (lower, upper)
    }

    /// The class this key's first segment spells: the key of the class every
    /// document whose stem that segment is belongs to, which is the one class
    /// a change to such a document names. `glossary/norn/` is in `glossary/`,
    /// and a one-segment key is its own class.
    ///
    /// A changed path's class is always one segment, so a change reaches a
    /// key exactly when it names this class.
    pub(crate) fn leading(&self) -> ClassKey {
        match self.0.find(SEPARATOR) {
            Some(at) => ClassKey(self.0[..=at].to_string()),
            None => self.clone(),
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

/// The key a finding about a path-addressed link is filed under: one exact
/// vault path the link spells.
///
/// **A path key is the other key space from a [`ClassKey`], and the two never
/// meet.** A class key ends in the separator and ranges over every key it
/// prefixes; a path key is a document path, which never ends in one, and it is
/// matched by equality alone. A path key is therefore refused where it ends in
/// the separator, and a class key handed over as a path key is refused by that
/// same rule. Findings file the two in separate tables, so no class range
/// reaches a path key it prefixes: the class `glossary/` holds nothing from the
/// path key `glossary/x.md`.
///
/// The link index and changed paths use the same normalized complete path.
/// A key outside that comparison space is refused where it is filed or
/// explained ([`crate::IncrementOutcome::affected_paths`]).
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct PathKey(String);

impl PathKey {
    /// Read `text` as a path key, refusing a class key and anything that is no
    /// document path.
    pub fn new(text: &str) -> Result<Self, StoreError> {
        // `DocumentPath::new` already refuses a separator-terminated spelling
        // — it reduces to an empty final segment — so this check exists only
        // to give that refusal its clearer, path-key-specific message.
        if text.ends_with(SEPARATOR) {
            return Err(StoreError::Path {
                path: text.to_string(),
                problem: "it is separator-terminated, which is a class key's spelling and never \
                          a path's",
            });
        }
        DocumentPath::new(text)?;
        Ok(PathKey(text.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The comparison key of segment-reversed document stems and ancestors.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum SuffixKey {
    /// `documents.suffix_key`, compared bytewise after normalization.
    Comparison,
}

impl SuffixKey {
    /// Whether a class key is normalized for resolution.
    pub(crate) fn holds(self, class: &ClassKey) -> bool {
        comparison_key(class.as_str()) == class.as_str()
    }

    /// Whether a path key is normalized for resolution.
    pub(crate) fn holds_path(self, path: &PathKey) -> bool {
        comparison_key(path.as_str()) == path.as_str()
    }

    /// The indexed column holding the comparison key.
    pub(crate) fn column(self) -> &'static str {
        "suffix_key"
    }
}

/// The bounds of the prefix ranges over a segment-reversed key that one probe
/// opens in the comparison key.
///
/// Ranges rather than patterns, because a range over an index is what SQLite
/// answers without a scan. A probe carries one range or two: a target whose leaf
/// carries `.md` has two readings and opens both.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SuffixProbe {
    key: SuffixKey,
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
    /// Which stored suffix key this probe's ranges bound.
    pub fn key(&self) -> SuffixKey {
        self.key
    }

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

    /// How many ranges the probe opens: one, or two where the target ends
    /// in the optional `.md` extension.
    pub fn range_count(&self) -> usize {
        self.ranges.len()
    }

    /// Every ambiguity class this probe reads, one per reading of the leaf.
    /// A finding records every key so a change in either disjoint class reaches it.
    pub fn class_keys(&self) -> BTreeSet<ClassKey> {
        self.ranges
            .iter()
            .map(|range| ClassKey::of_prefix(&range.lower))
            .collect()
    }
}

/// The indexed ranges a suffix target opens under [`comparison_key`].
/// Only a trailing `.md` is optional; a target carrying it opens both its
/// written stem and its extensionless stem. Relative and rooted forms,
/// empty segments, control characters and `.` or `..` stems are refused.
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
    let leaf = segments.last().expect("a target has a leaf");
    let stem = markdown_stem(leaf);
    if stem == "." || stem == ".." {
        return refuse("its leaf reduces to a `.` or `..` stem, which names no class");
    }
    // Lowercase is contextual. Normalize each complete document name before
    // removing `.md`, so full-path comparison twins also share a suffix key.
    let mut ranges = Vec::with_capacity(2);
    if stem != *leaf {
        ranges.push(bounded(suffix_of_path(&comparison_key(target))));
    }
    ranges.push(bounded(suffix_in_document_frame(target)));
    Ok(SuffixProbe {
        key: SuffixKey::Comparison,
        ranges,
    })
}

/// The ambiguity class of the complete document name `stem.md`.
///
/// The complete name is normalized before `.md` is removed. One range
/// holds every document with that comparison stem, regardless of directory,
/// and every finding whose class key opens with it.
///
/// The refusals mirror [`ClassKey::new`] and [`suffix_probe`]: an empty stem,
/// one carrying the separator (a stem is one segment, never a path), one that
/// is a `.` or `..` name, and one carrying a control byte. Formatting an
/// unrefused stem into a lower bound is what the class key's own debug
/// assertion trusts, so a caller-supplied stem is checked here rather than
/// left to trip that assertion later.
pub(crate) fn class_probe(stem: &str) -> Result<SuffixProbe, StoreError> {
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
        key: SuffixKey::Comparison,
        ranges: vec![bounded(suffix_in_document_frame(stem))],
    })
}

/// Normalize the complete `name.md` before stripping `.md`, so a vault file
/// and the document-oriented probe derive their class from the same string.
/// Where contextual lowercasing depends on what follows a name's last character,
/// as with final capital sigma, a suffix-addressed link spelling the folded
/// character itself may not reach a vault file that the file's own spelling reaches.
fn suffix_in_document_frame(name: &str) -> String {
    suffix_of_path(&comparison_key(&format!("{name}.md")))
}

fn is_vault_file_leaf(leaf: &str) -> bool {
    !leaf.starts_with('.')
        && leaf
            .rfind('.')
            .is_some_and(|at| at > 0 && !leaf[at + 1..].eq_ignore_ascii_case("md"))
}

/// Reverse a normalized complete path, dropping only its Markdown extension.
fn suffix_of_path(path_key: &str) -> String {
    let mut segments = path_key.rsplit(SEPARATOR);
    let leaf = segments.next().expect("a path has a leaf");
    std::iter::once(markdown_stem(leaf))
        .chain(segments)
        .map(|segment| format!("{segment}{SEPARATOR}"))
        .collect()
}

/// The prefix, and the key just past everything it opens.
///
/// `lower` ends with the separator — that is what makes a prefix match
/// segment-aligned — so its [`prefix_successor`] is that separator stepped to
/// the next character, and always exists.
fn bounded(lower: String) -> Range {
    debug_assert!(
        lower.ends_with(SEPARATOR),
        "a probe prefix is separator-terminated: {lower}"
    );
    let upper = prefix_successor(&lower).expect("a separator steps to the character after it");
    Range { lower, upper }
}

/// The bounds containing everything segment-aligned beneath `path`.
///
/// The separator is load-bearing: `a/ <= key < a0` holds `a/child.md` and not
/// `ab/child.md`.
fn descendant_bounds(path: &str) -> (String, String) {
    let Range { lower, upper } = bounded(format!("{path}{SEPARATOR}"));
    (lower, upper)
}

/// The least text that sorts after every text starting with `prefix`, or
/// nothing where no text does.
///
/// The last character that has a next one is stepped to it, and every
/// character after it is dropped. In UTF-8 the next character is also the next
/// text bytewise, so under `BINARY` the result is an exclusive upper bound on
/// every text starting with `prefix` and on nothing that does not. A prefix
/// that is empty, or made only of the last character there is, has none.
///
/// The one prefix step in the store: a directory's descendant range, a suffix
/// probe's ranges and a path glob's literal-prefix range are each bounded by it.
pub(crate) fn prefix_successor(prefix: &str) -> Option<String> {
    let mut characters: Vec<char> = prefix.chars().collect();
    while let Some(last) = characters.pop() {
        let stepped = (u32::from(last) + 1..=u32::from(char::MAX)).find_map(char::from_u32);
        if let Some(stepped) = stepped {
            characters.push(stepped);
            return Some(characters.into_iter().collect());
        }
    }
    None
}

/// The segment a folder named `name` occupies in every place
/// [`DocumentPath::rendered`] names beneath it.
///
/// Rendering is per segment and only the leaf's stem is read as a stem, so
/// this is the whole of what a folder contributes to a rendering. A caller
/// looking for the spellings that render onto a place lists a folder and keeps
/// the names whose rendering is the place's segment there; the corpus in
/// `tests/paths.rs` holds this to `rendered` for every refusal it states.
pub fn rendered_folder_name(name: &std::ffi::OsStr) -> String {
    rendered_segment(&name.to_string_lossy())
}

/// One segment as a document-path segment: every character the grammar refuses
/// replaced, and a segment the grammar refuses whole marked ahead of.
///
/// This is the rendering half of [`DocumentPath::new`]'s refusals, and it
/// answers them one for one — the backslash and control-byte refusals by
/// replacement, and the empty, `.` and `..` segment refusals by the marker. The
/// pair is what makes [`DocumentPath::rendered`] total.
fn rendered_segment(segment: &str) -> String {
    let mut rendered: String = segment
        .chars()
        .map(|character| {
            if is_refused_character(character) {
                RENDERED_MARKER
            } else {
                character
            }
        })
        .collect();
    if is_refused_segment(&rendered) {
        rendered.insert(0, RENDERED_MARKER);
    }
    rendered
}

/// A leaf with only its Markdown extension removed, in any ASCII case.
pub(crate) fn markdown_stem(leaf: &str) -> &str {
    match leaf.rfind('.').filter(|&dot| dot > 0) {
        Some(dot) if leaf[dot + 1..].eq_ignore_ascii_case("md") => &leaf[..dot],
        _ => leaf,
    }
}

/// Paths and link targets compare by NFC under Unicode default lowercasing.
/// The final composition keeps lowercase expansions in the same canonical form.
/// Suffix derivation normalizes complete named paths before removing `.md`,
/// because Unicode default lowercase can depend on the following extension.
pub fn comparison_key(text: &str) -> String {
    text.nfc()
        .collect::<String>()
        .to_lowercase()
        .nfc()
        .collect()
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

/// ASCII lowercase for filesystem path ordering and query path globs.
pub(crate) fn fold_ascii_case(text: &str) -> String {
    text.to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A class key's bounds open exactly the class it names.
    #[test]
    fn a_class_key_bounds_the_class_it_names() {
        let class = ClassKey::new("foo/").expect("a class key");
        assert_eq!(class.bounds(), ("foo/".to_string(), "foo0".to_string()));
    }

    /// Class keys are admitted only in NFC under Unicode default lowercasing.
    /// Uppercase and decomposed spellings are outside the comparison form.
    #[test]
    fn class_keys_are_admitted_only_in_normalized_comparison_form() {
        for (key, folded) in [
            ("foo/", true),
            ("Foo/", false),
            ("été/", true),
            ("Été/", false),
            ("e\u{301}te\u{301}/", false),
        ] {
            let class = ClassKey::new(key).expect("a class key");
            assert_eq!(SuffixKey::Comparison.holds(&class), folded, "`{key}`");
        }
    }

    /// Path keys are admitted only in NFC under Unicode default lowercasing.
    /// A document records the same comparison key for path maintenance.
    #[test]
    fn path_keys_are_admitted_only_in_normalized_comparison_form() {
        for (key, folded) in [
            ("dir/t.md", true),
            ("Dir/t.md", false),
            ("été.md", true),
            ("Été.md", false),
            ("e\u{301}te\u{301}.md", false),
        ] {
            let path = PathKey::new(key).expect("a path key");
            assert_eq!(SuffixKey::Comparison.holds_path(&path), folded, "`{key}`");
        }
        let document = DocumentPath::new("Dir/T.md").expect("a document path");
        assert_eq!(
            document.path_key_in(SuffixKey::Comparison).as_str(),
            "dir/t.md"
        );
    }

    /// **`class_probe` validates like every other constructor here.** A stem
    /// handed over unvalidated would format into a lower bound
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
        let class = class_probe("glossary").expect("a class stem");
        assert_eq!(class.key(), SuffixKey::Comparison);
        assert_eq!(
            class.ranges().collect::<Vec<_>>(),
            [("glossary/", "glossary0")]
        );
    }

    /// A separator steps to the character after it, which is what bounds a
    /// segment-aligned prefix: `a/` opens up to `a0`.
    #[test]
    fn a_separator_terminated_prefix_steps_its_separator() {
        assert_eq!(prefix_successor("a/"), Some("a0".to_string()));
        assert_eq!(
            descendant_bounds("notes"),
            ("notes/".to_string(), "notes0".to_string())
        );
    }

    #[test]
    fn a_prefix_ending_in_the_last_character_steps_the_one_before_it() {
        assert_eq!(prefix_successor("a\u{10FFFF}"), Some("b".to_string()));
        assert_eq!(prefix_successor("\u{10FFFF}"), None);
        assert_eq!(prefix_successor(""), None);
        assert_eq!(prefix_successor("\u{D7FF}"), Some("\u{E000}".to_string()));
    }
}
