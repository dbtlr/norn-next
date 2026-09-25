//! The store's own fact vocabulary — what a caller hands over, and what it
//! reads back.
//!
//! **These types are the store's, not the wire's.** The store↔host seam is
//! inside one process: a wire type crosses the client/host seam, and putting one
//! here would make the vocabulary that surfaces render answerable to what a
//! database column happens to need. The host maps the text layer's facts onto
//! these, which is the same mapping it already performs to compose them.
//!
//! Fields are plain and public. The crate is pre-1.0, so adding one is a cheap
//! breaking change a caller should see, and hiding them behind constructors
//! would buy nothing: there is no invariant between two fields of a fact that
//! the store does not check where it writes them.
//!
//! # Spans are body-relative, and a fact carries no ordinal
//!
//! A span is the position the text layer reported, which is relative to the
//! document body; `documents.body_offset` is the frame that turns one into a
//! document offset.
//!
//! Order is the position in the slice. A fact carries no ordinal of its own
//! because the store assigns one from the slice index, which is exactly the
//! emission order the text layer contracts — and because two spellings of a
//! row's position could disagree. Field rows are the one exception: a field
//! row's ordinal is its place under its key rather than in a slice, and
//! [`crate::FieldRows::derive`] — the only thing that makes one — assigns it.
//!
//! # In and out are the same types, except where the projection is one-way
//!
//! The fact rows read back are the fact rows written, so a caller can compare
//! them and a re-derivation can be checked for having changed anything. The
//! document-level types differ in one place only: frontmatter goes in as a value
//! tree and comes back as the canonical JSON text it projected to, because that
//! projection does not run backwards. See [`crate::json`].

use std::collections::BTreeSet;

use norn_wire::{FindingKind, Severity};

use crate::fields::{ContentModel, FieldRows};
use crate::json::FrontmatterValue;
use crate::path::{ClassKey, DocumentPath};

/// A position in a document body: 1-based line and column, 0-based byte offset.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Span {
    pub line: u64,
    pub column: u64,
    pub byte_offset: u64,
}

/// The written form a link was recognized from.
///
/// Plain rather than extensible: a third family arriving should break every
/// match that dispatches on this one, because a family whose resolution nobody
/// chose is the defect the closed set prevents.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LinkFamily {
    Wikilink,
    Markdown,
}

impl LinkFamily {
    /// The whole vocabulary, which is what `links.family` is checked against.
    pub(crate) const ALL: &'static [LinkFamily] = &[LinkFamily::Wikilink, LinkFamily::Markdown];

    /// The family as `links.family` holds it.
    pub const fn as_str(self) -> &'static str {
        match self {
            LinkFamily::Wikilink => "wikilink",
            LinkFamily::Markdown => "markdown",
        }
    }

    pub(crate) fn from_str(stored: &str) -> Option<Self> {
        match stored {
            "wikilink" => Some(LinkFamily::Wikilink),
            "markdown" => Some(LinkFamily::Markdown),
            _ => None,
        }
    }
}

/// Which home a tag was read from.
///
/// The two are read by different grammars — a body marker opens a run, a
/// frontmatter entry has to be a tag whole — so which one an author used is a
/// fact about the document rather than an implementation detail of the scan.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TagSource {
    Body,
    Frontmatter,
}

impl TagSource {
    /// The whole vocabulary, which is what `document_tags.source` is checked
    /// against.
    pub(crate) const ALL: &'static [TagSource] = &[TagSource::Body, TagSource::Frontmatter];

    /// The source as `document_tags.source` holds it.
    pub const fn as_str(self) -> &'static str {
        match self {
            TagSource::Body => "body",
            TagSource::Frontmatter => "frontmatter",
        }
    }

    pub(crate) fn from_str(stored: &str) -> Option<Self> {
        match stored {
            "body" => Some(TagSource::Body),
            "frontmatter" => Some(TagSource::Frontmatter),
            _ => None,
        }
    }
}

/// One link token, as syntax.
///
/// There is no resolution field and no addressing mode: how this target
/// resolves derives from `protocol` and `family`, protocol first, in the one
/// place that derivation lives.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LinkFact {
    pub family: LinkFamily,
    /// A wikilink embed (`![[…]]`). Never true for an inline Markdown link.
    pub embed: bool,
    /// The `protocol://` prefix, sentinel excluded. `None` is
    /// written-without-a-protocol, which is a distinct fact from any protocol.
    pub protocol: Option<String>,
    /// The target stem as written: no normalization, no percent-decoding, no
    /// default extension.
    pub target: String,
    pub title: Option<String>,
    /// A heading anchor after `#`. Mutually exclusive with `block_ref`.
    pub anchor: Option<String>,
    /// A block reference after `#^`.
    pub block_ref: Option<String>,
    pub span: Span,
}

/// One heading.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HeadingFact {
    pub level: u8,
    /// The heading's text with inline markup flattened. A wikilink `#anchor`
    /// addresses this.
    pub text: String,
    /// The anchor form, document-order dedupe suffix included. An inline
    /// Markdown `#fragment` addresses this.
    pub slug: String,
    pub span: Span,
    /// Where the heading construct ends and the section body begins.
    pub body_offset: u64,
    /// Whether the heading sits inside a blockquote or a list item, which is
    /// what makes a section replace addressed at it unsafe.
    pub inside_container: bool,
}

/// One block-id definition — the target side of a `#^` reference.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BlockFact {
    pub block_id: String,
    /// `None` where the caller cannot name the definition's position.
    pub span: Option<Span>,
}

/// One `#tag`, recorded as written.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TagFact {
    /// The name without the marker, nesting included, case as written.
    pub name: String,
    pub source: TagSource,
    /// `None` for a frontmatter tag whose entry's bytes cannot be named as one
    /// span.
    pub span: Option<Span>,
}

/// Everything one document says, as the store takes it.
///
/// A document's frontmatter and field rows are read, never assigned: a
/// caller that could assign one could leave it disagreeing with the other.
///
/// ```compile_fail,E0616
/// use norn_store::{DocumentFacts, DocumentPath};
///
/// let mut facts = DocumentFacts::new(
///     DocumentPath::new("a.md").expect("a path"),
///     "hash",
///     "body",
///     4,
/// );
/// facts.frontmatter = None;
/// ```
///
/// ```compile_fail,E0616
/// use norn_store::{DocumentFacts, DocumentPath, FieldRows};
///
/// let mut facts = DocumentFacts::new(
///     DocumentPath::new("a.md").expect("a path"),
///     "hash",
///     "body",
///     4,
/// );
/// facts.fields = FieldRows::default();
/// ```
#[derive(Clone, Debug, PartialEq)]
pub struct DocumentFacts {
    pub path: DocumentPath,
    /// The hash the filesystem seam computed over the bytes it read. Only a
    /// content hash concludes "unchanged".
    pub content_hash: String,
    /// The size of the whole document those bytes came from — frontmatter block
    /// included.
    ///
    /// A document is its frontmatter block and then its body, and the body runs
    /// to the end of it, so this **is** `body_offset` plus the body's length.
    /// The three are checked against each other where a document is written, and
    /// a set of them that does not add up is refused: it describes no file.
    pub byte_length: u64,
    /// The document's body, frontmatter block excluded.
    pub body: String,
    /// Where `body` begins in the document, which is the frame every span here
    /// is relative to.
    pub body_offset: u64,
    /// The frontmatter value tree, or `None` where there is no projection to
    /// make — no block, or a block that did not parse. Private, and set with
    /// `fields` through [`DocumentFacts::with_frontmatter`] alone.
    frontmatter: Option<FrontmatterValue>,
    /// How many **frontmatter-scoped** diagnostics the parse raised, which is
    /// what discriminates the two things `frontmatter: None` can mean: zero is
    /// "there was no block", and nonzero is "there was a block and it did not
    /// read". A count over every diagnostic a parse raised could not tell those
    /// apart once one diagnostic came from anywhere else, so the caller counts
    /// the frontmatter ones — a text-layer diagnostic's code says whether the
    /// note it files names the block.
    pub frontmatter_diagnostic_count: u32,
    /// Links in the text layer's contracted emission order.
    pub links: Vec<LinkFact>,
    /// Headings in document order.
    pub headings: Vec<HeadingFact>,
    /// Block-id definitions in document order.
    pub blocks: Vec<BlockFact>,
    /// Tags in the order they were read, both homes.
    pub tags: Vec<TagFact>,
    /// The field rows `frontmatter` derives. Private, and set with
    /// `frontmatter` through [`DocumentFacts::with_frontmatter`] alone, so the
    /// rows a document carries are the rows its own frontmatter derives: no
    /// caller can hand over one without the other.
    fields: FieldRows,
    /// The fingerprint of the schema `fields` were derived under, or `None`
    /// where they were derived under no schema. Set with `fields`, and read by
    /// the increment, which refuses typed values derived under a schema the
    /// store does not pin.
    fields_schema: Option<String>,
}

impl DocumentFacts {
    /// A document with no derived facts at all: the row, its body, its size, and
    /// nothing else. The fact lists are then assigned by the caller, which keeps
    /// a document with none of them from having to name four empty vectors, and
    /// the frontmatter is set through [`DocumentFacts::with_frontmatter`].
    ///
    /// `byte_length` is taken rather than defaulted from `body`. The two are the
    /// same number only for a document with no frontmatter block, and a default
    /// that was right until the caller set `body_offset` is a default that
    /// reports the wrong document size for every document that has one.
    pub fn new(
        path: DocumentPath,
        content_hash: impl Into<String>,
        body: impl Into<String>,
        byte_length: u64,
    ) -> Self {
        DocumentFacts {
            path,
            content_hash: content_hash.into(),
            byte_length,
            body: body.into(),
            body_offset: 0,
            frontmatter: None,
            frontmatter_diagnostic_count: 0,
            links: Vec::new(),
            headings: Vec::new(),
            blocks: Vec::new(),
            tags: Vec::new(),
            fields: FieldRows::default(),
            fields_schema: None,
        }
    }

    /// The same document with `frontmatter` as its value and the field rows it
    /// derives under `declared`.
    ///
    /// The one way the pair is set, so the rows are always the ones the value
    /// derives; the declaration decides only their typed half, and the facts
    /// keep the fingerprint of the schema it was read from beside them.
    pub fn with_frontmatter(
        mut self,
        frontmatter: Option<FrontmatterValue>,
        declared: &ContentModel,
    ) -> Self {
        self.fields = FieldRows::derive(&self.path, frontmatter.as_ref(), declared);
        self.fields_schema = declared.schema().map(str::to_string);
        self.frontmatter = frontmatter;
        self
    }

    /// The frontmatter value tree, or `None` where there is no projection to
    /// make.
    pub fn frontmatter(&self) -> Option<&FrontmatterValue> {
        self.frontmatter.as_ref()
    }

    /// The field rows the frontmatter derives, typed half included.
    pub fn fields(&self) -> &FieldRows {
        &self.fields
    }

    /// The fingerprint of the schema the field rows were derived under, or
    /// `None` where they were derived under no schema.
    pub fn fields_schema(&self) -> Option<&str> {
        self.fields_schema.as_deref()
    }
}

/// A document's row, as it stands.
///
/// The derived path forms are in `path`, which is the same type that produced
/// them, so a reader gets the suffix key and the stem without recomputing
/// either.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoredDocument {
    pub path: DocumentPath,
    pub content_hash: String,
    pub byte_length: u64,
    pub body_offset: u64,
    /// The canonical JSON projection, or `None` where no projection exists.
    pub frontmatter: Option<String>,
    /// How many frontmatter-scoped diagnostics the parse raised. Zero beside a
    /// `None` projection is "no block"; nonzero is "a block that did not read".
    pub frontmatter_diagnostic_count: u32,
    /// The write generation this row was last derived at. Generations order;
    /// `derived_at` does not.
    pub generation: i64,
    /// Unix seconds, for a person reading a report.
    pub derived_at: i64,
}

/// The suffix keys one document row holds, beside the path that has to
/// produce them: what [`crate::Request::suffix_keys_after`] pages.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoredSuffixKeys {
    pub path: DocumentPath,
    /// `documents.suffix_key`, which [`DocumentPath::suffix_key`] recomputes.
    pub raw: String,
    /// `documents.folded_suffix_key`, which
    /// [`DocumentPath::folded_suffix_key`] recomputes.
    pub folded: String,
}

/// The case behaviour a vault root was **proven** to have at the filesystem
/// seam, as the store carries it.
///
/// It selects four things, and rewrites no path in any of them — a stored path
/// keeps the spelling the tree carries:
///
/// - **The store's recorded rebuild input.** A store records the order its rows
///   were derived under, and an open under another one rebuilds from zero
///   ([`crate::Store::path_order`]).
/// - **The suffix key a class probes**: the raw key where the root tells
///   spellings apart and the ASCII-folded key where it folds them
///   ([`crate::SuffixKey::under`]), which is also the key space a finding's
///   classes are filed in.
/// - **The ambiguity-ignore globs' fold**: bytewise where the root tells
///   spellings apart and with ASCII case folded where it folds them
///   ([`crate::AmbiguityIgnore::admits`]).
/// - **The collation a heal pages stored documents under**: bytewise, or
///   `NOCASE` with a bytewise tie-break, as the walk it merges against orders
///   paths. The heal hands the page its walk's proven order.
///
/// This crate depends on nothing in the filesystem seam, so each fold here —
/// the `NOCASE` collation, the folded suffix key, and the ignore globs'
/// [`norn_wire::CaseFold::Ascii`] — is **another implementation of the seam's
/// rule, not a derivation of it**. The contract all of them are held to is
/// written once — ASCII lowercase, then bytes, with the byte comparison
/// breaking a fold's ties — and each carries a test against it over the same
/// sample, so widening one implementation and not the others fails.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StoredPathOrder {
    /// Preserve bytewise UTF-8 path order.
    Sensitive,
    /// Fold ASCII case — `A`–`Z` onto `a`–`z`, every other byte as itself —
    /// which is exactly SQLite's `NOCASE` collation and exactly the seam's
    /// fold. Order under it is made total by a bytewise tie-break, so two paths
    /// that fold together still page in one fixed order.
    AsciiCaseInsensitive,
}

impl StoredPathOrder {
    /// The spelling a store records the order its rows were derived under by.
    pub const fn as_str(self) -> &'static str {
        match self {
            StoredPathOrder::Sensitive => "case-sensitive",
            StoredPathOrder::AsciiCaseInsensitive => "ascii-case-insensitive",
        }
    }

    /// The order a recorded spelling names, or `None` for a spelling no build
    /// records.
    pub(crate) fn from_recorded(recorded: &str) -> Option<Self> {
        [
            StoredPathOrder::Sensitive,
            StoredPathOrder::AsciiCaseInsensitive,
        ]
        .into_iter()
        .find(|order| order.as_str() == recorded)
    }
}

/// The derivation a store's rows were written by, as the deriver names it.
///
/// **It is a rebuild input beside the DDL fingerprint and the path order.** A
/// store records the version it was created under, and an open under another
/// one rebuilds from zero ([`crate::Store::derivation_version`]): rows another
/// derivation wrote for the same input are not rows this one would write, and
/// no increment converges them, because an increment derives a file again only
/// when its bytes move.
///
/// The number is the deriver's and it is opaque here: this crate records it
/// and compares it, and knows nothing of what changed between two of them.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct DerivationVersion(u32);

impl DerivationVersion {
    pub const fn new(version: u32) -> Self {
        DerivationVersion(version)
    }

    pub const fn get(self) -> u32 {
        self.0
    }

    /// The spelling a store records the version by: its decimal digits, with no
    /// leading zero.
    pub(crate) fn recorded(self) -> String {
        self.0.to_string()
    }
}

impl std::fmt::Display for DerivationVersion {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}", self.0)
    }
}

/// One key the link index holds a stored link under, as it reads back.
///
/// Derived at the write from the link and the path of the document holding
/// it, so it is not a fact a caller hands over: the `link_keys` table in
/// [`crate::ddl`] states what each key is.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoredLinkKey {
    /// The position of the link this is a key of among the document's links.
    pub link: u64,
    /// The key as the target spells it: a suffix address's segment-reversed
    /// prefix, or a vault path.
    pub key: String,
    /// The key with ASCII case folded.
    pub folded_key: String,
    /// How many segments a suffix address spells, and `None` beside a path.
    pub segments: Option<u64>,
}

/// A document's row and every fact row derived from it, in ordinal order.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoredFacts {
    pub document: StoredDocument,
    pub body: String,
    pub links: Vec<LinkFact>,
    /// The keys the link index holds the links under, in link order and then
    /// in key order.
    pub link_keys: Vec<StoredLinkKey>,
    pub headings: Vec<HeadingFact>,
    pub blocks: Vec<BlockFact>,
    pub tags: Vec<TagFact>,
    /// The field rows, typed half included, in key order and then ordinal
    /// order.
    pub fields: FieldRows,
}

/// How a document's death was learned.
///
/// Provenance is on the tombstone because a vault losing documents to the wrong
/// provenance is otherwise invisible: a prune that should have been a refusal
/// looks exactly like a deletion somebody asked for.
///
/// **One death's provenance, not a changeset's.** The changeset carries its own
/// mark — [`crate::IncrementProvenance`] — saying where its post-state came
/// from, and a changeset carrying deaths carries both.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Provenance {
    /// A full tree heal found the path absent.
    HealPrune,
    /// The watcher reported the path removed.
    WatcherRemoval,
    /// An applied plan deleted it.
    PlanDelete,
    /// The path is on disk and yields no facts the store can represent, so the
    /// row went and a finding says why.
    ///
    /// Distinct from every other provenance because the file is **present**.
    /// Recording such a death as a prune or a removal would say the path is
    /// gone from the vault, which is the wrong-provenance loss this column
    /// exists to make visible.
    Quarantine,
}

impl Provenance {
    /// The whole vocabulary, which is what `tombstones.provenance` is checked
    /// against.
    pub(crate) const ALL: &'static [Provenance] = &[
        Provenance::HealPrune,
        Provenance::WatcherRemoval,
        Provenance::PlanDelete,
        Provenance::Quarantine,
    ];

    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Provenance::HealPrune => "heal-prune",
            Provenance::WatcherRemoval => "watcher-removal",
            Provenance::PlanDelete => "plan-delete",
            Provenance::Quarantine => "quarantine",
        }
    }

    pub(crate) fn from_str(stored: &str) -> Option<Self> {
        match stored {
            "heal-prune" => Some(Provenance::HealPrune),
            "watcher-removal" => Some(Provenance::WatcherRemoval),
            "plan-delete" => Some(Provenance::PlanDelete),
            "quarantine" => Some(Provenance::Quarantine),
            _ => None,
        }
    }
}

/// A recorded death.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoredTombstone {
    pub path: DocumentPath,
    /// The hash the document was last derived at, or `None` where the death was
    /// learned from a path nothing had derived.
    pub last_content_hash: Option<String>,
    pub provenance: Provenance,
    pub generation: i64,
    pub recorded_at: i64,
}

/// One live document row, as the change feed projects it.
///
/// **Fingerprints and a position, and nothing else.** The feed exists so a
/// lane-2 consumer can decide what to fetch without fetching anything: it reads
/// the hash of the part it derives from, compares it against the one it recorded
/// last time, and asks for the document only where the two differ. A projection
/// carrying the body would make every page cost what the fetch it exists to
/// avoid costs.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FeedDocument {
    pub path: DocumentPath,
    /// The write generation this row was last derived at, which is what orders
    /// it in the feed.
    pub generation: i64,
    /// The hash of the whole document. **Only a content hash concludes
    /// "unchanged"** about the document as a document.
    pub content_hash: String,
    /// The hash of the stored body, for a consumer that derives from bodies.
    pub body_hash: String,
    /// The hash of the stored frontmatter projection, and `None` exactly where
    /// there is no projection — no block, or a block that did not parse.
    pub frontmatter_projection_hash: Option<String>,
}

/// One recorded death, as the change feed projects it.
///
/// The other half of what a consumer has to see: a consumer reading the living
/// alone keeps deriving from a path that is gone. `last_content_hash` is what a
/// consumer matches its own recorded state against, and it is absent for the
/// same reason it is absent on the row — a death learned from a path already
/// absent has nothing left to hash.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FeedTombstone {
    pub path: DocumentPath,
    /// The write generation the death was recorded at, which is what orders it
    /// in the feed.
    pub generation: i64,
    pub last_content_hash: Option<String>,
}

/// One resolution candidate in a finding's bounded head.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CandidateFact {
    pub path: DocumentPath,
    /// The minimal disambiguating suffix this candidate is named by.
    pub suffix: String,
}

/// A structured statement that vault state violates a rule or cannot be
/// resolved unambiguously.
///
/// The store records the wire vocabulary rather than defining it. Typed inputs
/// keep an ordinary write from minting a kind or severity that no surface can
/// advertise; the at-rest projection remains text and integrity verification
/// detects values outside either registry.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FindingFacts {
    pub kind: FindingKind,
    pub severity: Severity,
    /// The path the finding is about, whether or not a document is stored there.
    pub path: DocumentPath,
    /// Every ambiguity class this finding is maintained by, which is
    /// [`crate::SuffixProbe::class_keys`] for the probe it was read from — one
    /// class per reduction of the target, and empty for a finding that is not
    /// about resolution at all.
    ///
    /// A set rather than one key, because a target's two reductions are two
    /// disjoint classes and a finding filed under only one of them is invisible to
    /// maintenance that names the other. Validated keys, because a key no probe's
    /// range opens is the same invisibility by another route.
    pub class_keys: BTreeSet<ClassKey>,
    /// What the finding is about **inside** its subject, as written, and
    /// `None` where the subject is the whole of what it is about.
    ///
    /// **Not always a path.** A resolution finding carries the resolution
    /// target it could not resolve, which is a document reference; a finding
    /// about one named thing on a document that derived whole carries that
    /// thing's own name — a tag, a field key. The column is text a reader
    /// filters a class by, and nothing reads it as a path: the class-scoped
    /// maintenance a resolution finding needs is keyed by
    /// [`FindingFacts::class_keys`], which its producer computes from the probe
    /// it read, and the candidate head is [`CandidateFact`] rows carrying their
    /// own validated paths.
    pub target: Option<String>,
    pub span: Option<Span>,
    /// The candidates, in deterministic resolution-ladder order, bounded at
    /// [`CANDIDATE_HEAD`].
    pub candidates: Vec<CandidateFact>,
    /// How many candidates there were, which is what makes the head a head. It
    /// cannot be smaller than the head it heads, and a value that is is refused
    /// beside the bound itself.
    pub candidates_total: u64,
    pub message: String,
    /// Anything further, as text the caller has already projected. It travels in
    /// only: nothing reads it back as a typed shape.
    pub detail: Option<String>,
}

/// A finding's candidate list is a **bounded head**: the first five in
/// deterministic order, and a total.
///
/// The bound holds at rest exactly as it holds on the wire, and it is the
/// wire's number rather than a second one written here: a payload bounded
/// only at the rendering step is a payload the second consumer emits
/// unbounded, and two spellings of the bound could disagree.
pub const CANDIDATE_HEAD: usize = norn_wire::CANDIDATE_HEAD;

/// A finding as it stands, with the head of its candidates.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoredFinding {
    pub kind: String,
    pub severity: String,
    pub path: DocumentPath,
    /// Every ambiguity class the finding is in, and empty for a finding that is
    /// not about resolution. Any one of them reaches it.
    pub class_keys: BTreeSet<ClassKey>,
    pub target: Option<String>,
    pub span: Option<Span>,
    pub candidates: Vec<CandidateFact>,
    pub candidates_total: u64,
    pub message: String,
    pub detail: Option<String>,
    /// The vault-schema fingerprint this finding was derived under, which is
    /// what a schema edit invalidates it by.
    pub vault_schema_fingerprint: String,
    /// The generation a repair plan cites when it was planned against this
    /// finding.
    pub generation: i64,
}

/// One term the full-text index holds, as the index itself reports it.
///
/// The counts are the index's own: how many documents hold the term, and how
/// many times it occurs across them. Neither is a row identifier, so two
/// databases that indexed the same bodies report the same terms whatever order
/// their documents were written in.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IndexedTerm {
    pub term: String,
    pub documents: u64,
    pub occurrences: u64,
}

/// The pinned vault-schema projection.
///
/// Derived state: the file the bytes came from is the sole authority, and this
/// answers which schema derived state was derived under rather than what the
/// schema says.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VaultSchemaPin {
    pub bytes: Vec<u8>,
    pub fingerprint: String,
    pub generation: i64,
}

/// What a maintenance act discarded.
///
/// Three acts report one: a schema pin, a class discard a caller asked for, and
/// the class-scoped discard an increment folds into its own transaction.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Invalidation {
    /// Findings the act removed — derived under a different vault schema, or
    /// belonging to a class being re-derived. Parse-fact rows carry no schema key
    /// and no class, so none of them is ever counted here.
    pub findings_discarded: u64,
    /// Field values whose typed sort key the act cleared, because it was derived
    /// under a different vault schema. Only a schema pin clears one; the row
    /// and its raw text stay.
    pub typed_values_discarded: u64,
}

/// What pinning a vault schema did.
///
/// The pin and the discard the new schema key implies are **one act**. Two
/// transactions would leave a generation in which the pinned key says a set of
/// findings is dead and the findings are still readable, and no caller can close
/// that window from outside.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SchemaPin {
    /// The generation the pin happened at, or the generation the standing pin was
    /// taken at where the schema had not moved and nothing was written.
    pub generation: i64,
    /// Whether this call wrote a pin at all. A schema whose bytes and
    /// fingerprint already match what is pinned is not a schema change, so it
    /// takes no generation and derives nothing.
    pub repinned: bool,
    /// The derived state the new schema key invalidated.
    pub invalidated: Invalidation,
}

/// How much each pillar is holding.
///
/// A structural reading for a maintenance report, not a query surface: it
/// answers *how much is at rest* and never *which rows*.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PillarReport {
    pub documents: u64,
    pub tombstones: u64,
    pub findings: u64,
    pub finding_candidates: u64,
    /// Rows in the migration ledger. Zero through the pre-release build, where
    /// a store schema change is a rebuild rather than a migration.
    pub migrations_applied: u64,
}
