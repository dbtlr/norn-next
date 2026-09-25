//! Lane 1's per-document derivation act: one document's observation in, the
//! changeset entry and the finding it implies out.
//!
//! An observation is a path, the bytes standing at it, the content hash of
//! those bytes, and the stored row that path replaces. A [`Plan`] is what comes
//! back. The module is pure — no IO, no store handle — so it decides what a job
//! writes and the job decides when to write it. Equal observations plan equal
//! writes, and that determinism is what lets incremental maintenance land the
//! same derived state a from-zero rebuild over the same tree lands: the two
//! runs' observations differ in the stored rows they replace, so their plans
//! differ in the deaths those rows imply, while the state they land converges.
//! Accumulating plans, bounding a changeset, reading bytes, walking a vault and
//! timing any of it are orchestration's.
//!
//! This module also owns the closed cause vocabulary and the two discard sides
//! read off it — which finding kinds a re-derivation by spelling or by bytes
//! takes.
//!
//! **A plan is a function of the observation and the vault's declaration.**
//! [`plan_document`] takes the pinned vault schema's [`Declared`] beside the
//! document, because a finding keyed by the schema fingerprint — and a typed
//! value a pin of another fingerprint clears — is derived under the
//! declaration that fingerprint names. The declaration is read off
//! the store's own pin, so the model a plan derives under and the fingerprint
//! its findings are stamped with come from one set of bytes.
//!
//! **Findings are minted here and nowhere else.** [`plan_document`] and
//! [`plan_quarantine`] are [`PlannedFinding`]'s two constructors, and they are
//! what holds a finding's subject and its cause coherent: the subject is the
//! place the act read, and the cause is what the act concluded there. Code
//! outside this module consumes plans; it does not build them.
//!
//! **The death vocabulary spans the seam by design.** A death planned here
//! always answers a verdict on a document that is still on disk, which is
//! [`Provenance::Quarantine`]: the file stands and the row can no longer
//! account for it. A death answering an absence instead — a path a walk no
//! longer finds ([`Provenance::HealPrune`]), a removal the watcher reports
//! ([`Provenance::WatcherRemoval`]) — is concluded where the tree is read,
//! which is orchestration.

use std::collections::BTreeSet;
use std::path::Path;

use norn_config::schema::{FieldType, UndeclaredTags, VaultSchema};
use norn_store::{
    BlockFact, Change, ContentModel, DerivationVersion, DiscardScope, DocumentFacts, DocumentPath,
    FieldDeclaration, FrontmatterValue, HeadingFact, LinkFact, LinkFamily, Provenance, Span,
    TagFact, TagSource, TypedOrder,
};
use norn_text::{BlockRefusal, Document, SourceSpan, Value};
use norn_wire::{FindingKind, FindingScope, Severity, TagStance};

/// The derivation this build writes a store's rows by, recorded in every store
/// it creates and judged at every open: a store another version wrote is
/// rebuilt from zero.
///
/// **It names the whole derivation**, not this module alone: every crate's
/// contribution to the rows a store holds, as [ADR 0026] states the scope. It
/// moves whenever any of that writes different rows for the same vault bytes,
/// and only then; a refactor that writes the same rows leaves it where it is.
///
/// [ADR 0026]: https://github.com/dbtlr/norn/blob/main/docs/decisions/0026-a-derived-store-records-the-derivation-that-wrote-it.md
///
/// **The digest in `tests/derivation.rs` forces it.** That suite derives a
/// pinned corpus from zero and digests every derived row, pinned beside the
/// version it was taken under, and it fails when the digest moves while this
/// does not.
pub const DERIVATION_VERSION: DerivationVersion = DerivationVersion::new(3);

/// Why a path the vault holds produces no document facts.
///
/// One variant per finding kind, which is how a reader tells a name the store
/// cannot hold from bytes the parser cannot read. Every one of them leaves the
/// deriving act with nothing to store: no identity to hold a row under, or no
/// text to read facts out of.
// Crate-visible because [`Cause::Undecodable`] carries it in a field reachable
// at that visibility, which anything narrower puts under `private_interfaces`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Undecodable {
    /// The path bytes are not UTF-8.
    PathBytes,
    /// The path is UTF-8 and is not a document path.
    PathSpelling,
    /// The document's bytes are not UTF-8.
    BodyBytes,
}

impl Undecodable {
    /// The finding kind, which is the cause class a reader dispatches on.
    ///
    /// The vocabulary is the wire's, so a kind recorded in the findings table
    /// is the same string every surface advertises and filters by.
    const fn kind(self) -> FindingKind {
        match self {
            Undecodable::PathBytes => FindingKind::PathBytesNotUtf8,
            Undecodable::PathSpelling => FindingKind::PathNamesNoDocument,
            Undecodable::BodyBytes => FindingKind::BodyBytesNotUtf8,
        }
    }

    /// The cause as the finding's message states it.
    const fn statement(self) -> &'static str {
        match self {
            Undecodable::PathBytes => "its path bytes are not UTF-8",
            Undecodable::PathSpelling => "its path names no document",
            Undecodable::BodyBytes => "its bytes are not UTF-8",
        }
    }

    /// What an act has to read to conclude this cause.
    ///
    /// The match is exhaustive because the answer is what a finding of this
    /// cause discards at its subject: a cause added without a side here has no
    /// scope to file under, so the next variant states its side or nothing
    /// compiles.
    const fn decided(self) -> Decided {
        match self {
            // [`document_path`] reads the name and opens nothing, so these two
            // are concluded wherever a path is in hand.
            Undecodable::PathBytes | Undecodable::PathSpelling => Decided::BySpelling,
            // This is read out of the file the place names, so concluding it
            // means having opened it.
            Undecodable::BodyBytes => Decided::ByBytes,
        }
    }
}

/// Why a document that derives carries no frontmatter value.
///
/// The block was read by nothing, so the document's fields are unknown: it is
/// the vault's own defect and not a shape of a document. The row still holds
/// every fact the act could derive — identity, body, headings, links, body
/// tags — and this cause is what a finding beside that row states, because a
/// row alone would answer *this document has no tags, no title, no aliases*
/// about fields nothing ever read.
///
/// One variant per way [`norn_text::BlockRefusal`] leaves a block unread, each
/// fixed by a different edit to the document.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum UnreadBlock {
    /// The block opens and never closes.
    Unclosed,
    /// Nothing read the block: it is not well-formed, or it is well-formed and
    /// says something no value can be made of — a key written twice, a merge
    /// directive naming no mapping.
    Unreadable,
    /// The block is past [`norn_text::FRONTMATTER_MAX_BYTES`], so the text
    /// layer refuses it unparsed rather than paying a read that grows with the
    /// block's own length.
    TooLarge,
}

impl UnreadBlock {
    /// The cause behind the state the text layer reports.
    ///
    /// The match carries no wildcard, so a new way to leave a block unread
    /// arrives here as a cause rather than as silence on a derived row.
    const fn of(refusal: &BlockRefusal) -> Self {
        match refusal {
            BlockRefusal::Unclosed => UnreadBlock::Unclosed,
            BlockRefusal::Unreadable { .. } => UnreadBlock::Unreadable,
            BlockRefusal::TooLarge { .. } => UnreadBlock::TooLarge,
        }
    }

    /// The finding kind, which is the cause class a reader dispatches on.
    pub(crate) const fn kind(self) -> FindingKind {
        match self {
            UnreadBlock::Unclosed => FindingKind::FrontmatterUnclosed,
            UnreadBlock::Unreadable => FindingKind::FrontmatterUnreadable,
            UnreadBlock::TooLarge => FindingKind::FrontmatterTooLarge,
        }
    }

    /// The cause as the finding's message states it.
    const fn statement(self) -> &'static str {
        match self {
            UnreadBlock::Unclosed => "its frontmatter block never closes",
            UnreadBlock::Unreadable => "its frontmatter block is not well-formed",
            UnreadBlock::TooLarge => "its frontmatter block is past the bound that is read",
        }
    }
}

/// How a document disagrees with the vault's declared tag facet.
///
/// The document derives whole: every tag it carries is on its row, and the
/// finding is the judgment beside them. One variant per way a facet can be
/// broken, which today is the only one the facet declares.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TagBreach {
    /// The document carries a tag the declared vocabulary does not admit.
    Undeclared,
}

impl TagBreach {
    /// The finding kind, which is the cause class a reader dispatches on.
    const fn kind(self) -> FindingKind {
        match self {
            TagBreach::Undeclared => FindingKind::UndeclaredTag,
        }
    }

    /// The cause as the finding's message states it.
    const fn statement(self) -> &'static str {
        match self {
            TagBreach::Undeclared => "the vault does not declare it",
        }
    }
}

/// Why a finding this crate records stands where it stands.
///
/// The two families differ in what the deriving act left behind, which is what
/// [`FindingKind::scope`] says about the kind each records under: an
/// undecodable path leaves no row, so its finding is about the place; an unread
/// block leaves the row it could derive, so its finding is about the document
/// standing at that place.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Cause {
    /// Nothing about the path is derivable.
    Undecodable(Undecodable),
    /// The document derives and its frontmatter block was read by nothing.
    UnreadBlock(UnreadBlock),
    /// The document derives whole and disagrees with the vault's declared tag
    /// facet.
    TagBreach(TagBreach),
}

impl Cause {
    /// The finding kind this cause is recorded under.
    pub(crate) const fn kind(self) -> FindingKind {
        match self {
            Cause::Undecodable(cause) => cause.kind(),
            Cause::UnreadBlock(cause) => cause.kind(),
            Cause::TagBreach(cause) => cause.kind(),
        }
    }

    /// How urgently a finding of this cause is reported.
    ///
    /// The two derivation defects are errors: derived state is missing
    /// something the vault holds, and a reader of that state gets a wrong
    /// answer until it is fixed. A facet breach is a warning: the document
    /// derived whole, every fact it holds is on its row, and what stands is a
    /// disagreement between the vault's own declaration and its contents.
    pub(crate) const fn severity(self) -> Severity {
        match self {
            Cause::Undecodable(_) | Cause::UnreadBlock(_) => Severity::Error,
            Cause::TagBreach(_) => Severity::Warning,
        }
    }

    /// What an act has to read to conclude this cause.
    pub(crate) const fn decided(self) -> Decided {
        match self {
            Cause::Undecodable(cause) => cause.decided(),
            // A block is read out of the document's own bytes, so concluding
            // that nothing read it means having opened them. So is a tag: the
            // facts a facet judges are read from the document itself.
            Cause::UnreadBlock(_) | Cause::TagBreach(_) => Decided::ByBytes,
        }
    }

    /// The finding's message: the subject, what happened to it, and the cause.
    pub(crate) fn message(self, subject: &DocumentPath) -> String {
        match self {
            Cause::Undecodable(cause) => format!(
                "`{}` is quarantined: {}",
                subject.as_str(),
                cause.statement()
            ),
            Cause::UnreadBlock(cause) => format!(
                "`{}` derives without its frontmatter: {}",
                subject.as_str(),
                cause.statement()
            ),
            // The tag itself is the finding's target rather than part of its
            // message, so a reader filters the class by the name without
            // parsing prose.
            Cause::TagBreach(cause) => format!(
                "`{}` carries a tag the vault's schema does not admit: {}",
                subject.as_str(),
                cause.statement()
            ),
        }
    }
}

/// What an act read to conclude a cause, which is what a finding of that cause
/// replaces at the place it is filed at.
///
/// One place holds findings from both sides at once, because a rendering names
/// a place rather than an identity: the content findings there are about the
/// document the place names, and the spelling findings there are about the
/// refused spellings that render onto it. An act concludes one side of that
/// place and says nothing about the other, so its discard takes one side and
/// leaves the other standing — a finding a job deleted without re-filing it
/// would be a true statement gone until an unrelated vault heal.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) enum Decided {
    /// The spelling alone decides it: the act read paths and opened no bytes,
    /// so what it concludes is what the grammar says about the names it read.
    BySpelling,
    /// The document's own bytes decide it: the act opened the file the place
    /// names, so it concludes what those bytes say and nothing about the other
    /// spellings rendering there.
    ByBytes,
}

impl Decided {
    /// The kinds an act of this side re-derives at the place it files at, which
    /// is exactly what recording its finding discards there.
    ///
    /// Every quarantine files through this one mapping — the merge walk's
    /// refused spellings and refused documents, the sweep of a poisoned root,
    /// the reading of a vacated one, and the dirty-path loop — so a job that
    /// read both sides of a place re-derives both and takes neither, and the
    /// two scopes tell apart only where an act reaches one side alone.
    pub(crate) const fn rederives(self) -> DiscardScope<'static> {
        match self {
            Decided::BySpelling => DiscardScope::Kinds(&SPELLING_KINDS),
            Decided::ByBytes => DiscardScope::Kinds(&CONTENT_KINDS),
        }
    }

    /// Whether two sides are the same one, which is the comparison a `const`
    /// context has instead of `PartialEq`.
    const fn same(self, other: Decided) -> bool {
        matches!(
            (self, other),
            (Decided::BySpelling, Decided::BySpelling) | (Decided::ByBytes, Decided::ByBytes)
        )
    }
}

/// Every cause a finding this crate records states, which is what the two
/// sides below are read off.
///
/// A cause absent from this list has its kind in neither side, so a finding of
/// it discards nothing it re-derives and stands beside its own previous copy at
/// every heal. Three things hold the list to the enums: [`Undecodable::decided`]
/// is exhaustive, so the next variant states its side or nothing compiles; the
/// classification below holds every kind the registry advertises to exactly one
/// of this list and [`KINDS_NO_CAUSE_CARRIES`]; and the scope agreement beside
/// it holds each cause to a kind that stands where that cause leaves a row or
/// leaves none. A cause minted under a kind an older cause already carries is
/// reached by neither side, and the ADR that closes both cause sets is what
/// stands in front of one.
const CAUSES: [Cause; 7] = [
    Cause::Undecodable(Undecodable::PathBytes),
    Cause::Undecodable(Undecodable::PathSpelling),
    Cause::Undecodable(Undecodable::BodyBytes),
    Cause::UnreadBlock(UnreadBlock::Unclosed),
    Cause::UnreadBlock(UnreadBlock::Unreadable),
    Cause::UnreadBlock(UnreadBlock::TooLarge),
    Cause::TagBreach(TagBreach::Undeclared),
];

/// The finding kinds no cause above carries.
///
/// Quarantine, the unread block and the tag facet are the producers recording
/// findings today, so the list is empty. A kind minted for another producer —
/// an ambiguity a resolution reads, a field a schema refuses — is named here, which
/// is the one line that keeps the classification below a reading of the registry
/// rather than a claim that every kind the registry holds is this crate's.
const KINDS_NO_CAUSE_CARRIES: [FindingKind; 0] = [];

// Every kind [`FindingKind::ALL`] advertises is carried by one cause or is
// named as no cause's, and no two causes carry one kind. The registry is a
// general one, so a kind minted for another producer is a growth this crate
// answers by classifying it rather than by widening a side no act re-derives.
const _: () = {
    let mut index = 0;
    while index < FindingKind::ALL.len() {
        let kind = FindingKind::ALL[index];
        assert!(
            causes_carrying(kind) + times_named_uncarried(kind) == 1,
            "a finding kind is carried by no cause and named as no producer's, \
             or is claimed twice"
        );
        index += 1;
    }
};

// Every cause records under a kind whose scope matches what the act deriving it
// leaves at the subject. A cause that left no row filed under a document-scoped
// kind would stand beside a row that is not there; one that left a row filed
// under a place-scoped kind would be withheld by the very row it is about, so
// nothing would ever report it.
const _: () = {
    let mut index = 0;
    while index < CAUSES.len() {
        assert!(
            scope_agrees(CAUSES[index]),
            "a cause records under a kind whose scope disagrees with what its \
             deriving act leaves at the subject"
        );
        index += 1;
    }
};

/// Whether a cause's kind stands where that cause leaves the subject.
const fn scope_agrees(cause: Cause) -> bool {
    matches!(
        (cause, cause.kind().scope()),
        (Cause::Undecodable(_), FindingScope::Place)
            | (Cause::UnreadBlock(_), FindingScope::Document)
            | (Cause::TagBreach(_), FindingScope::Document)
    )
}

/// Whether two kinds are the same one, which is the comparison a `const`
/// context has instead of `PartialEq`.
const fn same_kind(left: FindingKind, right: FindingKind) -> bool {
    let (left, right) = (left.as_str().as_bytes(), right.as_str().as_bytes());
    if left.len() != right.len() {
        return false;
    }
    let mut index = 0;
    while index < left.len() {
        if left[index] != right[index] {
            return false;
        }
        index += 1;
    }
    true
}

/// How many causes in [`CAUSES`] record findings under this kind.
const fn causes_carrying(kind: FindingKind) -> usize {
    let mut count = 0;
    let mut index = 0;
    while index < CAUSES.len() {
        if same_kind(CAUSES[index].kind(), kind) {
            count += 1;
        }
        index += 1;
    }
    count
}

/// How many times [`KINDS_NO_CAUSE_CARRIES`] names this kind.
const fn times_named_uncarried(kind: FindingKind) -> usize {
    let mut count = 0;
    let mut index = 0;
    while index < KINDS_NO_CAUSE_CARRIES.len() {
        if same_kind(KINDS_NO_CAUSE_CARRIES[index], kind) {
            count += 1;
        }
        index += 1;
    }
    count
}

/// How many causes [`Cause::decided`] puts on one side.
const fn decided_count(decided: Decided) -> usize {
    let mut count = 0;
    let mut index = 0;
    while index < CAUSES.len() {
        if CAUSES[index].decided().same(decided) {
            count += 1;
        }
        index += 1;
    }
    count
}

/// The kinds one side re-derives: every cause on that side, as the kind it is
/// recorded under.
const fn decided_kinds<const N: usize>(decided: Decided) -> [FindingKind; N] {
    let mut kinds = [FindingKind::PathBytesNotUtf8; N];
    let mut filled = 0;
    let mut index = 0;
    while index < CAUSES.len() {
        if CAUSES[index].decided().same(decided) {
            kinds[filled] = CAUSES[index].kind();
            filled += 1;
        }
        index += 1;
    }
    assert!(
        filled == N,
        "the side holds a different count of causes than it filled"
    );
    kinds
}

/// The kinds a spelling alone decides, which is what an act that opens no bytes
/// replaces at the place it files at.
const SPELLING_KINDS: [FindingKind; decided_count(Decided::BySpelling)] =
    decided_kinds(Decided::BySpelling);

/// The kinds a document's own bytes decide, which is what an act that opened
/// them replaces at the place those bytes are read at.
const CONTENT_KINDS: [FindingKind; decided_count(Decided::ByBytes)] =
    decided_kinds(Decided::ByBytes);

/// The kinds an unread frontmatter block is stated under, which is what a row
/// asserting that defect implies stands beside it.
///
/// **This is what makes the pair check kind-precise.** A row carrying an absent
/// frontmatter projection beside a nonzero frontmatter-diagnostic count owes a
/// finding of one of these kinds and of no other: a document-scoped finding of
/// some other kind — a facet breach, say — can stand at the same path about
/// something else entirely, and reading its presence as the pair being whole
/// would leave the block's own finding lost.
pub(crate) const UNREAD_BLOCK_KINDS: [FindingKind; unread_block_count()] = unread_block_kinds();

/// How many causes in [`CAUSES`] are an unread frontmatter block.
const fn unread_block_count() -> usize {
    let mut count = 0;
    let mut index = 0;
    while index < CAUSES.len() {
        if matches!(CAUSES[index], Cause::UnreadBlock(_)) {
            count += 1;
        }
        index += 1;
    }
    count
}

/// [`CAUSES`]' unread-block members, read as the kinds they record under.
const fn unread_block_kinds() -> [FindingKind; unread_block_count()] {
    let mut kinds = [FindingKind::FrontmatterUnreadable; unread_block_count()];
    let mut filled = 0;
    let mut index = 0;
    while index < CAUSES.len() {
        if matches!(CAUSES[index], Cause::UnreadBlock(_)) {
            kinds[filled] = CAUSES[index].kind();
            filled += 1;
        }
        index += 1;
    }
    kinds
}

/// Every side a place is read on, which is what a prune asks its account for one
/// at a time.
///
/// A job that read a spelling and found no document concluded one side of the
/// place and nothing about the other, so the two are taken separately: what a
/// prune takes on a side is what an act of that side would have re-derived
/// there.
pub(crate) const SIDES: [Decided; 2] = [Decided::BySpelling, Decided::ByBytes];

// Every cause reads its place on a side the prune asks about. A cause on a side
// absent here is one no prune ever concludes the absence of, which is a finding
// standing at a place nothing accounts for.
const _: () = {
    let mut index = 0;
    while index < CAUSES.len() {
        assert!(
            sides_reading(CAUSES[index].decided()) == 1,
            "a cause reads its place on a side the prune does not take"
        );
        index += 1;
    }
};

/// How many of [`SIDES`] are this one.
const fn sides_reading(decided: Decided) -> usize {
    let mut count = 0;
    let mut index = 0;
    while index < SIDES.len() {
        if SIDES[index].same(decided) {
            count += 1;
        }
        index += 1;
    }
    count
}

/// Every kind a walk of a place can conclude, which is what the page a prune
/// reads its scope through selects on.
///
/// The sides are what a prune *takes*, one at a time; this is what makes a
/// subject worth reading at all. It is [`CAUSES`] whole — the two sides together
/// — and a kind minted for another producer is outside it, because a producer
/// that never walks a place is one whose findings no walk can conclude.
pub(crate) const WALKED_KINDS: [FindingKind; CAUSES.len()] = walked_kinds();

/// [`CAUSES`] read as the kinds they record under.
const fn walked_kinds() -> [FindingKind; CAUSES.len()] {
    let mut kinds = [FindingKind::PathBytesNotUtf8; CAUSES.len()];
    let mut index = 0;
    while index < CAUSES.len() {
        kinds[index] = CAUSES[index].kind();
        index += 1;
    }
    kinds
}

/// One document held out of derived state, and why.
#[derive(Clone, Debug)]
pub(crate) struct Quarantine {
    cause: Undecodable,
    /// The decoder's own account of the refusal, which the finding carries in
    /// its detail beside the spelling it was read from.
    problem: String,
}

/// One document derived without its frontmatter, and why.
///
/// The document keeps its row: this is what stands beside it, so that the
/// fields nothing read are absent from derived state *and* stated rather than
/// silently absent.
#[derive(Clone, Debug)]
pub(crate) struct UnreadFrontmatter {
    pub(crate) cause: UnreadBlock,
    /// The reader's own account of the refusal, where the cause is not the
    /// whole of it. A block that never closes has nothing to add.
    pub(crate) problem: Option<String>,
}

/// The document path a vault-relative spelling names, or why it names none.
///
/// This is the one place a walked or watched path becomes a document identity,
/// so the two ways a spelling fails to be one are told apart here rather than
/// at each caller.
pub(crate) fn document_path(path: &Path) -> Result<DocumentPath, Quarantine> {
    let Some(spelling) = path.to_str() else {
        return Err(Quarantine {
            cause: Undecodable::PathBytes,
            problem: "the path bytes are not valid UTF-8".to_string(),
        });
    };
    DocumentPath::new(spelling).map_err(|problem| Quarantine {
        cause: Undecodable::PathSpelling,
        problem: problem.to_string(),
    })
}

/// One document as derived state holds it: the facts, and the defect standing
/// beside them where the document derives with something unread.
pub(crate) struct Derived {
    pub(crate) facts: DocumentFacts,
    pub(crate) unread_frontmatter: Option<UnreadFrontmatter>,
}

/// Derive one document's facts from its bytes, the field rows' typed half
/// under `model`.
pub(crate) fn map_document(
    path: &str,
    bytes: &[u8],
    hash: String,
    model: &ContentModel,
) -> Result<Derived, Quarantine> {
    // Identity before content: a path that names no document has nothing to
    // say about its own bytes.
    let document_path = document_path(Path::new(path))?;
    let source = std::str::from_utf8(bytes).map_err(|problem| Quarantine {
        cause: Undecodable::BodyBytes,
        problem: problem.to_string(),
    })?;
    let document = Document::parse(source);
    // The text layer reads a block only up to its own bound and says so rather
    // than parsing past it, so this costs the bound at worst however the block
    // is shaped. A block nothing read — unclosed, not well-formed, or past the
    // bound — leaves the fields unknown, which the document reports as state:
    // the facts below are derived without them, and the finding beside them is
    // where the absence is stated.
    let unread_frontmatter = document
        .frontmatter_refusal()
        .map(|refusal| UnreadFrontmatter {
            cause: UnreadBlock::of(refusal),
            problem: refusal.problem(),
        });
    let scan = document.scan_body();
    let mut facts = DocumentFacts::new(document_path, hash, document.body(), bytes.len() as u64)
        .with_frontmatter(document.frontmatter().map(map_value), model);
    facts.body_offset = document.body_start() as u64;
    facts.frontmatter_diagnostic_count = document
        .diagnostics()
        .iter()
        .filter(|d| d.code.frontmatter_scoped())
        .count() as u32;
    facts.links = document
        .frontmatter_wikilinks()
        .into_iter()
        .chain(scan.links())
        .map(map_link)
        .collect();
    facts.headings = scan
        .headings()
        .iter()
        .map(|h| HeadingFact {
            level: h.level,
            text: h.text.clone(),
            slug: h.slug.clone(),
            span: span(h.span),
            body_offset: h.body_offset as u64,
            inside_container: h.inside_container,
        })
        .collect();
    facts.blocks = scan
        .block_ids()
        .into_iter()
        .map(|b| BlockFact {
            block_id: b.id,
            span: Some(span(b.span)),
        })
        .collect();
    facts.tags = scan
        .tags()
        .into_iter()
        .map(|t| TagFact {
            name: t.name,
            source: TagSource::Body,
            span: t.span.map(span),
        })
        .chain(document.frontmatter_tags().into_iter().map(|t| TagFact {
            name: t.name,
            source: TagSource::Frontmatter,
            span: t.span.map(span),
        }))
        .collect();
    Ok(Derived {
        facts,
        unread_frontmatter,
    })
}

/// One finding a plan asks a job to file: the subject it stands at, the cause
/// it states, and the formatted detail — the spelling this finding was read
/// from, and the reader's own account of the refusal where there is one.
///
/// The cause rides with it because it is what decides how much of the subject
/// recording the finding replaces, and whether a document row at the subject
/// withholds it.
///
/// The subject and the cause agree because [`plan_document`] and
/// [`plan_quarantine`] are the only two acts that mint one: each states the
/// cause it concluded at the place it read. A caller receives findings and
/// records them; it does not assemble them.
#[derive(Debug, PartialEq)]
pub(crate) struct PlannedFinding {
    pub(crate) subject: DocumentPath,
    pub(crate) cause: Cause,
    pub(crate) detail: String,
    /// What the finding is about inside its subject, where the cause is about
    /// one named thing on the document rather than the document itself. A tag
    /// breach carries the tag; a derivation defect carries nothing, because the
    /// subject is the whole of what it is about.
    pub(crate) target: Option<String>,
}

/// One document's planned outcome: a change and a finding, each present when
/// the observation implies one.
///
/// The ordering that lands the change before the finding it stands beside is
/// enforced by the flush path, not by this type.
#[derive(Debug, PartialEq)]
pub(crate) struct Plan {
    pub(crate) change: Option<Change>,
    /// Every finding the observation implies, in the order a reader meets them:
    /// the derivation defect the act concluded, then the facet breaches the
    /// declaration judges. A document can break a facet once per tag, so this
    /// is a list rather than the single answer a derivation defect is.
    pub(crate) findings: Vec<PlannedFinding>,
}

/// Plan what one document's bytes write, taking with them the row they can no
/// longer account for.
///
/// `stored` is the row standing at this path, which every caller already knows:
/// the merge reads it off the page it is walking and the scoped paths read it by
/// key. The store holds only what it can represent, so a document that stops
/// decoding leaves nothing behind but the finding — and the row's death is a
/// **quarantine**, because the file is still there. That is the one death
/// vocabulary this act reaches: a death answering an absence is concluded where
/// the tree is read, which is orchestration.
///
/// A document that decodes and whose frontmatter block was read by nothing
/// **keeps its row**: the facts the act could derive are derived, and the
/// finding planned beside them is what says the fields are unknown rather than
/// absent.
///
/// `path` is the spelling as the vault holds it, which is what a quarantine's
/// subject is rendered from where the grammar admits no document path.
///
/// `declared` is the pinned vault schema's declaration. It decides the facet
/// findings and the typed half of the field rows: a document that does not
/// decode is judged against nothing, because a vault declaration says what a
/// document's facts must be and there are no facts.
pub(crate) fn plan_document(
    path: &Path,
    spelling: &str,
    bytes: &[u8],
    hash: String,
    stored: Option<&DocumentPath>,
    declared: &Declared,
) -> Plan {
    match map_document(spelling, bytes, hash, declared.content_model()) {
        Ok(derived) => {
            let subject = derived.facts.path.clone();
            let mut findings = Vec::new();
            if let Some(unread) = derived.unread_frontmatter {
                let detail = match unread.problem {
                    Some(problem) => format!("{path:?}: {problem}"),
                    None => format!("{path:?}"),
                };
                findings.push(PlannedFinding {
                    subject: subject.clone(),
                    cause: Cause::UnreadBlock(unread.cause),
                    detail,
                    target: None,
                });
            }
            findings.extend(plan_tag_facet(&subject, &derived.facts, declared.schema()));
            Plan {
                change: Some(Change::Upsert(derived.facts)),
                findings,
            }
        }
        Err(quarantine) => Plan {
            change: stored.map(|row| Change::Death {
                path: row.clone(),
                provenance: Provenance::Quarantine,
            }),
            findings: vec![plan_quarantine(path, quarantine)],
        },
    }
}

/// The declaration a plan derives under: the pinned schema's content model,
/// and the typed orders its declared fields hand the store, named by the
/// fingerprint the schema is pinned under.
///
/// Built once per schema rather than per document, and only through
/// [`Declared::pinned`] and [`Declared::unpinned`], so the typed orders a plan
/// fills the field pillar with are always the ones the schema beside them
/// declares, and carry the fingerprint the store compares with its own pin.
pub(crate) struct Declared {
    schema: VaultSchema,
    content_model: ContentModel,
}

impl Declared {
    /// The declaration `schema` makes, pinned under `fingerprint`.
    pub(crate) fn pinned(schema: VaultSchema, fingerprint: impl Into<String>) -> Self {
        let content_model = content_model(&schema, fingerprint.into());
        Declared {
            schema,
            content_model,
        }
    }

    /// The declaration of a vault with no schema pinned, which declares
    /// nothing.
    pub(crate) fn unpinned() -> Self {
        Declared {
            schema: VaultSchema::default(),
            content_model: ContentModel::none(),
        }
    }

    /// The schema, as `norn-config` reads it.
    pub(crate) fn schema(&self) -> &VaultSchema {
        &self.schema
    }

    /// The content model as the store reads it.
    pub(crate) fn content_model(&self) -> &ContentModel {
        &self.content_model
    }
}

/// What `schema` declares, pinned under `fingerprint`, as the store reads it:
/// every declared field with its type, whether it is required and its closed
/// set, and for each whose type does not order as text, the typed order that
/// type reads a raw value into; the declared tags, the tag patterns and the
/// stance on an undeclared tag; the declared folders; and the ambiguity-ignore
/// patterns, the places the schema keeps out of ambiguity classes, which
/// the resolver applies and `describe` reports as path rules.
///
/// A raw value that does not read as its declared type has no sort key, which
/// is the store's `NULL`: the document still carries the value, and a typed
/// order has nothing to place it by.
fn content_model(schema: &VaultSchema, fingerprint: String) -> ContentModel {
    let declared = schema.fields().fold(
        ContentModel::under(fingerprint),
        |declared, (key, field)| {
            let mut declaration = field_declaration(field.kind());
            if field.required() {
                declaration = declaration.required();
            }
            if let Some(values) = field.one_of() {
                declaration = declaration.one_of(values);
            }
            declared.declare_field(key, declaration)
        },
    );
    let tags = schema.tags();
    let declared = tags.declared().fold(declared, ContentModel::declare_tag);
    let declared = tags
        .patterns()
        .iter()
        .fold(declared, |declared, pattern| {
            declared.declare_tag_pattern(pattern.as_str())
        })
        .declare_undeclared_tags(match tags.undeclared() {
            UndeclaredTags::Allow => TagStance::Allow,
            UndeclaredTags::Report => TagStance::Report,
        });
    let declared = schema.folders().iter().fold(declared, |declared, folder| {
        declared.declare_folder(folder.path(), folder.description().map(str::to_string))
    });
    schema
        .ambiguity_ignore()
        .iter()
        .fold(declared, |declared, pattern| {
            declared.declare_ambiguity_ignore(pattern.clone())
        })
}

/// A field declared as `kind`, as the store reads it: under the wire type a
/// `describe` facet reports, which is spelled as `kind` is — the two enums are
/// one vocabulary, held equal by spelling in `norn-config`'s suite — and, for a
/// type that does not order as text, with the typed order `kind` reads a raw
/// value into.
fn field_declaration(kind: FieldType) -> FieldDeclaration {
    let order = || TypedOrder::new(move |raw| kind.read(raw).ok().map(|value| value.sort_key()));
    match kind {
        FieldType::Text => FieldDeclaration::text(),
        FieldType::Number => FieldDeclaration::number(order()),
        FieldType::Boolean => FieldDeclaration::boolean(order()),
        FieldType::Date => FieldDeclaration::date(order()),
        FieldType::Tags => FieldDeclaration::tags(),
    }
}

/// Judge a document's tags against the vault's declared tag facet.
///
/// The tags are the ones already on the facts: the facet is a judgment over
/// what the document says rather than a second reading of it, which is what
/// makes the tag rows schema-independent parse facts and these findings the
/// schema-keyed answer about them.
///
/// **One finding per distinct undeclared name, not one per token.** A document
/// that writes `#draft` in its frontmatter and three more times in its body has
/// one thing wrong with it, and a reader paging the class wants the names.
/// Order is the order the names are first written, so equal documents plan
/// equal writes.
///
/// A facet that reports nothing yields nothing here, which includes every vault
/// that has not declared a tag vocabulary at all.
fn plan_tag_facet(
    subject: &DocumentPath,
    facts: &DocumentFacts,
    declared: &VaultSchema,
) -> Vec<PlannedFinding> {
    let facet = declared.tags();
    if !facet.reports_undeclared() {
        return Vec::new();
    }
    let mut seen = BTreeSet::new();
    facts
        .tags
        .iter()
        .filter(|tag| !facet.admits(&tag.name))
        .filter(|tag| seen.insert(tag.name.clone()))
        .map(|tag| PlannedFinding {
            subject: subject.clone(),
            cause: Cause::TagBreach(TagBreach::Undeclared),
            detail: format!("`#{}`, written in the {}", tag.name, source(tag.source)),
            target: Some(tag.name.clone()),
        })
        .collect()
}

/// Where a tag was written, as the finding's detail names it.
const fn source(source: TagSource) -> &'static str {
    match source {
        TagSource::Body => "body",
        TagSource::Frontmatter => "frontmatter",
    }
}

/// Plan the finding that says why a path contributes no facts.
///
/// The subject is the place the path occupies — its own spelling where the
/// grammar admits one, and a rendering of it where the grammar does not.
pub(crate) fn plan_quarantine(path: &Path, quarantine: Quarantine) -> PlannedFinding {
    PlannedFinding {
        subject: DocumentPath::rendered(path),
        cause: Cause::Undecodable(quarantine.cause),
        detail: format!("{path:?}: {}", quarantine.problem),
        target: None,
    }
}

fn map_link(link: norn_text::Link) -> LinkFact {
    LinkFact {
        family: match link.family {
            norn_text::LinkFamily::Wikilink => LinkFamily::Wikilink,
            norn_text::LinkFamily::Markdown => LinkFamily::Markdown,
        },
        embed: link.embed,
        protocol: link.protocol,
        target: link.target,
        title: link.title,
        anchor: link.anchor,
        block_ref: link.block_ref,
        span: span(link.span),
    }
}
fn span(value: SourceSpan) -> Span {
    Span {
        line: value.line as u64,
        column: value.column as u64,
        byte_offset: value.byte_offset as u64,
    }
}
fn map_value(value: &Value) -> FrontmatterValue {
    match value {
        Value::Null => FrontmatterValue::Null,
        Value::Bool(v) => FrontmatterValue::Bool(*v),
        Value::Int(v) => FrontmatterValue::Int(*v),
        Value::Float(v) => FrontmatterValue::Float(*v),
        Value::String(v) => FrontmatterValue::String(v.clone()),
        Value::Sequence(v) => FrontmatterValue::Sequence(v.iter().map(map_value).collect()),
        Value::Map(v) => FrontmatterValue::Map(
            v.iter()
                .map(|(k, v)| (k.to_owned(), map_value(v)))
                .collect(),
        ),
    }
}

// The direct [`map_document`] and [`plan_document`] cases live here; the two in
// `production.rs` sit with the oversized-block fixture they read their sources
// from.
#[cfg(test)]
mod tests {
    use super::*;

    /// A vault that has declared nothing, which is what most cases here plan
    /// under: the observation alone decides the plan.
    fn undeclaring() -> Declared {
        Declared::unpinned()
    }

    /// A vault whose declared tag vocabulary is `front` and everything under
    /// `area/`, and which reports anything else.
    fn reporting() -> Declared {
        Declared::pinned(
            VaultSchema::parse(
                b"version: 1\ntags:\n  declared: [front]\n  patterns: [\"area/**\"]\n  undeclared: report\n",
            )
            .expect("a schema declaring a tag facet"),
            "reporting",
        )
    }

    /// **The text layer's reading of a link and the wire's address agree.**
    /// `norn-text` states the syntax-only rule — protocol first, family
    /// second — and the wire's selector refines it: the reserved `vault`
    /// protocol is read from the vault root, a wikilink's as a rooted name and
    /// a Markdown link's as a path, every other protocol addresses
    /// no document, a wikilink written without one is a suffix address, and a
    /// Markdown target written without one is a path, or addresses no
    /// document where it opens with a URI scheme. An empty target names the
    /// holding document under either family. The links are the ones the text
    /// layer recognized in a body, as derivation stores them.
    #[test]
    fn the_text_layers_reading_of_a_link_agrees_with_the_wires_address() {
        use norn_text::Resolution;
        use norn_wire::{LinkAddress, VAULT_PROTOCOL};
        let body = "[[notes/x]] [[vault://notes/x.md]] [[https://example.com|web]] \
                    [[#Heading]] [[mailto:x]] [t](../y.md) [t](/z.md?raw=1) \
                    [t](vault://notes/y.md) [t](https://example.com) \
                    [t](mailto:someone@example.com) [t](#frag)\n";
        let expected = [
            LinkAddress::Suffix("notes/x"),
            LinkAddress::RootedName("notes/x.md"),
            LinkAddress::Elsewhere,
            LinkAddress::HoldingDocument,
            LinkAddress::Suffix("mailto:x"),
            LinkAddress::Relative("../y.md"),
            LinkAddress::Rooted("z.md"),
            LinkAddress::Rooted("notes/y.md"),
            LinkAddress::Elsewhere,
            LinkAddress::Elsewhere,
            LinkAddress::HoldingDocument,
        ];
        let links = norn_text::BodyScan::new(body).links();
        assert_eq!(links.len(), expected.len(), "{links:?}");
        for (link, expected) in links.iter().zip(expected) {
            let stored = map_link(link.clone());
            let family = match stored.family {
                LinkFamily::Wikilink => norn_wire::LinkFamily::Wikilink,
                LinkFamily::Markdown => norn_wire::LinkFamily::Markdown,
            };
            let address = LinkAddress::of(family, stored.protocol.as_deref(), &stored.target);
            assert_eq!(address, expected, "{}", link.raw);
            let agrees = match (link.resolution(), address) {
                (
                    Resolution::Protocol(VAULT_PROTOCOL),
                    LinkAddress::Rooted(_) | LinkAddress::RootedName(_),
                ) => true,
                (Resolution::Protocol(scheme), LinkAddress::Elsewhere) => scheme != VAULT_PROTOCOL,
                (Resolution::Suffix, LinkAddress::Suffix(_) | LinkAddress::HoldingDocument) => true,
                (
                    Resolution::RelativePath,
                    LinkAddress::Relative(_)
                    | LinkAddress::Rooted(_)
                    | LinkAddress::HoldingDocument
                    | LinkAddress::Elsewhere,
                ) => true,
                _ => false,
            };
            assert!(
                agrees,
                "`{}`: the text layer reads {:?}, the wire {address:?}",
                link.raw,
                link.resolution()
            );
        }
    }

    /// **The declaration a find is compiled under carries the schema's
    /// ambiguity-ignore set**, so a resolution reads the set of the schema the
    /// snapshot pins and no other.
    #[test]
    fn the_declaration_carries_the_schemas_ambiguity_ignore_set() {
        let declared = Declared::pinned(
            VaultSchema::parse(b"version: 1\npaths:\n  ambiguity_ignore: [\"archive/**\"]\n")
                .expect("a schema declaring an ambiguity-ignore set"),
            "ignoring",
        );
        let globs: Vec<&str> = declared
            .content_model()
            .ambiguity_ignore()
            .patterns()
            .iter()
            .map(|glob| glob.as_str())
            .collect();
        assert_eq!(globs, ["archive/**"]);
        assert!(
            Declared::unpinned()
                .content_model()
                .ambiguity_ignore()
                .patterns()
                .is_empty()
        );
    }

    /// **A pinned schema's declaration reports every declaration the schema
    /// makes**, each as the facet `describe` answers with, in the order of the
    /// text that keys it: each field with its type, whether it is required and
    /// its closed set, the tags, the patterns, the stance, the folders and the
    /// ambiguity-ignore patterns. A vault with no schema pinned declares
    /// nothing, and a schema silent on tags states the default stance.
    #[test]
    fn a_pinned_declaration_reports_every_declaration_its_schema_makes() {
        use norn_wire::{Facet, FacetKind, FieldType as Wire, PathRuleKind};

        let declared = Declared::pinned(
            VaultSchema::parse(
                b"version: 1
fields:
  title: {type: text, required: true}
  due: {type: date}
  status: {type: text, one_of: [live, draft]}
tags:
  declared: [project, area]
  patterns: [\"person/**\", \"area/**\"]
  undeclared: report
folders:
  - path: journal
    description: One document per day
  - path: archive
paths:
  ambiguity_ignore: [\"archive/**\"]
",
            )
            .expect("a schema declaring every shape"),
            "every-shape",
        );
        let facets = |kind| {
            declared
                .content_model()
                .facets_of(kind, None)
                .collect::<Vec<Facet>>()
        };
        assert_eq!(
            facets(FacetKind::DeclaredField),
            vec![
                Facet::declared_field("due", Wire::Date, false, None),
                Facet::declared_field(
                    "status",
                    Wire::Text,
                    false,
                    Some(vec!["draft".to_string(), "live".to_string()])
                ),
                Facet::declared_field("title", Wire::Text, true, None),
            ]
        );
        assert!(declared.content_model().typed_order("due").is_some());
        assert!(declared.content_model().typed_order("title").is_none());
        assert_eq!(
            facets(FacetKind::DeclaredTag),
            vec![Facet::declared_tag("area"), Facet::declared_tag("project")]
        );
        assert_eq!(
            facets(FacetKind::TagPattern),
            vec![
                Facet::tag_pattern("area/**"),
                Facet::tag_pattern("person/**")
            ]
        );
        assert_eq!(
            facets(FacetKind::UndeclaredTags),
            vec![Facet::undeclared_tags(TagStance::Report)]
        );
        assert_eq!(
            facets(FacetKind::Folder),
            vec![
                Facet::folder("archive", None),
                Facet::folder("journal", Some("One document per day".to_string())),
            ]
        );
        assert_eq!(
            facets(FacetKind::PathRule),
            vec![Facet::path_rule(
                PathRuleKind::AmbiguityIgnore,
                "archive/**"
            )]
        );
        assert_eq!(facets(FacetKind::ObservedField), Vec::new());

        let silent = Declared::pinned(
            VaultSchema::parse(b"version: 1\n").expect("a schema declaring nothing"),
            "silent",
        );
        assert_eq!(
            silent
                .content_model()
                .facets_of(FacetKind::UndeclaredTags, None)
                .collect::<Vec<Facet>>(),
            vec![Facet::undeclared_tags(TagStance::Allow)]
        );
        let unpinned = undeclaring();
        for kind in FacetKind::ALL {
            assert_eq!(
                unpinned.content_model().facets_of(kind, None).count(),
                0,
                "{kind:?}"
            );
        }
    }

    /// **Every field type is declared as the wire type spelled as it is, and
    /// carries a typed order exactly where it does not order as text.** One
    /// field of each of the five types, each reported with its own type:
    /// `number`, `boolean` and `date` read a raw value into a typed sort key,
    /// and `text` and `tags` are ordered by their raw text.
    #[test]
    fn every_field_type_is_declared_as_its_own_wire_type() {
        use norn_wire::{Facet, FacetKind};

        let mut schema = String::from("version: 1\nfields:\n");
        for kind in FieldType::ALL {
            schema.push_str(&format!("  {0}: {{type: {0}}}\n", kind.as_str()));
        }
        let declared = Declared::pinned(
            VaultSchema::parse(schema.as_bytes()).expect("a schema declaring every type"),
            "every-type",
        );
        let reported: Vec<(String, &str)> = declared
            .content_model()
            .facets_of(FacetKind::DeclaredField, None)
            .map(|facet| match facet {
                Facet::DeclaredField {
                    key, field_type, ..
                } => (key, field_type.as_str()),
                other => panic!("a declared field's facet: {other:?}"),
            })
            .collect();
        let mut expected: Vec<(String, &str)> = FieldType::ALL
            .into_iter()
            .map(|kind| (kind.as_str().to_string(), kind.as_str()))
            .collect();
        expected.sort();
        assert_eq!(reported, expected);
        for kind in FieldType::ALL {
            assert_eq!(
                declared
                    .content_model()
                    .typed_order(kind.as_str())
                    .is_some(),
                !kind.orders_as_text(),
                "{kind:?}"
            );
        }
    }

    /// **The two discard sides partition the causes.** The sides are read off
    /// [`CAUSES`] through [`Cause::decided`], so a cause whose kind falls out of
    /// both is a cause no act re-derives — which is a copy of that finding per
    /// heal — and a kind in both is one side taking the other's work.
    ///
    /// The kinds this crate records are a subset of the registry rather than
    /// the whole of it: what holds the two apart is the classification beside
    /// [`CAUSES`], which a kind minted for another producer is named in.
    #[test]
    fn the_two_discard_sides_partition_the_causes() {
        let mut carried: Vec<&str> = CAUSES.iter().map(|cause| cause.kind().as_str()).collect();
        carried.sort_unstable();

        let mut scoped: Vec<&str> = SPELLING_KINDS
            .iter()
            .chain(CONTENT_KINDS.iter())
            .map(FindingKind::as_str)
            .collect();
        scoped.sort_unstable();
        assert_eq!(scoped, carried, "a cause the two scopes do not partition");

        // A prune reads neither side: an unaccounted place holds nothing a walk
        // files there, so what it takes is every cause a walk can conclude — the
        // two sides together and nothing else.
        let mut walked: Vec<&str> = WALKED_KINDS.iter().map(FindingKind::as_str).collect();
        walked.sort_unstable();
        assert_eq!(
            walked, carried,
            "the walked-scope prune takes a kind no walk concludes, or leaves one it does"
        );

        let registry: Vec<&str> = FindingKind::ALL.iter().map(FindingKind::as_str).collect();
        for kind in &carried {
            assert!(
                registry.contains(kind),
                "`{kind}` is a cause's kind the registry does not advertise"
            );
        }

        // Which side a cause discards on and where its findings may stand are
        // different questions, and every cause that leaves a row answers the
        // second one the same way: an act that opened a document's bytes and
        // derived a row from them files beside that row.
        for cause in CAUSES {
            let expected = match cause {
                Cause::Undecodable(_) => FindingScope::Place,
                Cause::UnreadBlock(_) | Cause::TagBreach(_) => FindingScope::Document,
            };
            assert_eq!(
                cause.kind().scope(),
                expected,
                "`{}` stands somewhere its deriving act does not leave it",
                cause.kind()
            );
        }
    }

    /// **A quarantine files at the place it read and takes only a row that
    /// stands there.** The subject is the rendered place rather than an
    /// identity, because a spelling the grammar refuses names none. The change
    /// is the row the observation replaces: an observation that replaces no row
    /// plans no death, and a death it does plan is a
    /// [`Provenance::Quarantine`], because the file the row cannot account for
    /// is still on disk.
    #[test]
    fn a_quarantine_files_at_the_rendered_place_and_takes_only_a_row_that_stands() {
        for (spelling, bytes, cause) in [
            (
                "note.md",
                b"# heading\n\xff".as_slice(),
                Undecodable::BodyBytes,
            ),
            (
                "notes/bad\\name.md",
                b"# heading\n".as_slice(),
                Undecodable::PathSpelling,
            ),
        ] {
            let path = Path::new(spelling);
            let hash = || norn_fs::ContentHash::of(bytes).to_string();

            let unheld = plan_document(path, spelling, bytes, hash(), None, &undeclaring());
            let [finding] = &unheld.findings[..] else {
                panic!("a refused document states why it contributes no facts");
            };
            assert_eq!(finding.cause, Cause::Undecodable(cause));
            assert_eq!(
                finding.subject,
                DocumentPath::rendered(path),
                "the finding stands somewhere other than the place the act read"
            );
            assert_eq!(
                unheld.change, None,
                "a quarantine took a row the store does not hold"
            );

            let stored = DocumentPath::new("note.md").expect("a document path");
            let held = plan_document(path, spelling, bytes, hash(), Some(&stored), &undeclaring());
            assert_eq!(
                held.change,
                Some(Change::Death {
                    path: stored,
                    provenance: Provenance::Quarantine,
                }),
                "the row a refused document leaves behind died some other way"
            );
        }

        // The third verdict is concluded before there is a spelling to plan
        // from — `plan_document` takes one — so it reaches a plan through
        // [`document_path`], and the finding says the same two things.
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt;

            let path = Path::new(std::ffi::OsStr::from_bytes(b"bad-\xff.md"));
            let quarantine =
                document_path(path).expect_err("non-UTF-8 path bytes name no document");
            let finding = plan_quarantine(path, quarantine);
            assert_eq!(
                finding.cause,
                Cause::Undecodable(Undecodable::PathBytes),
                "path bytes that are not UTF-8 filed under another cause"
            );
            assert_eq!(finding.subject, DocumentPath::rendered(path));
        }
    }

    /// **A block nothing read keeps the document's row and states the
    /// absence.** Every way a block goes unread plans the upsert of the facts
    /// the act could derive and a finding of that cause's own kind. The detail
    /// carries the spelling the block was read from — escaped, because a
    /// rendering is not injective — and the reader's account of the refusal
    /// where the cause is not the whole of it. A block that never closes has
    /// nothing to add, so its detail is the spelling alone.
    #[test]
    fn an_unread_block_plans_an_upsert_and_a_finding_naming_the_spelling_it_read() {
        let too_large = format!(
            "---\nk: {}\n---\n# heading\n",
            "a".repeat(norn_text::FRONTMATTER_MAX_BYTES)
        );
        for (source, cause, states_a_problem) in [
            (
                "---\ntitle: note\n# heading\n".to_string(),
                UnreadBlock::Unclosed,
                false,
            ),
            (
                "---\ntitle: : :\n---\n# heading\n".to_string(),
                UnreadBlock::Unreadable,
                true,
            ),
            (too_large, UnreadBlock::TooLarge, true),
        ] {
            let bytes = source.as_bytes();
            let hash = || norn_fs::ContentHash::of(bytes).to_string();
            let problem = map_document("note.md", bytes, hash(), &ContentModel::none())
                .expect("a document whose block went unread still derives")
                .unread_frontmatter
                .expect("the block was read by nothing")
                .problem;
            assert_eq!(
                problem.is_some(),
                states_a_problem,
                "{cause:?} accounts for its refusal another way"
            );

            let plan = plan_document(
                Path::new("note.md"),
                "note.md",
                bytes,
                hash(),
                None,
                &undeclaring(),
            );
            assert!(
                matches!(plan.change, Some(Change::Upsert(_))),
                "{cause:?} cost the document the row it derives"
            );
            let [finding] = &plan.findings[..] else {
                panic!("the unknown fields are stated");
            };
            assert_eq!(finding.cause, Cause::UnreadBlock(cause));
            let expected = match &problem {
                Some(problem) => format!("\"note.md\": {problem}"),
                None => "\"note.md\"".to_string(),
            };
            assert_eq!(
                finding.detail, expected,
                "{cause:?} details its refusal in another shape"
            );
        }
    }

    /// **A document that derives is planned from its own bytes alone.** The
    /// upsert carries exactly the facts the act derived, and the finding beside
    /// it — where the block went unread — stands at those facts' own identity.
    /// The row the observation replaces decides nothing in this arm: an
    /// identity that still derives takes no death.
    #[test]
    fn a_document_that_derives_upserts_its_facts_and_plans_no_death() {
        let stored = DocumentPath::new("note.md").expect("a document path");

        let whole = b"---\ntags: [front]\n---\n# Heading\n[[target]] #body\n".as_slice();
        let hash = norn_fs::ContentHash::of(whole).to_string();
        let derived = map_document("note.md", whole, hash.clone(), &ContentModel::none())
            .expect("a document derives");
        let plan = plan_document(
            Path::new("note.md"),
            "note.md",
            whole,
            hash.clone(),
            Some(&stored),
            &undeclaring(),
        );
        assert_eq!(
            plan.change,
            Some(Change::Upsert(derived.facts)),
            "the upsert carries something other than the facts the act derived"
        );
        assert!(
            plan.findings.is_empty(),
            "a document that derives whole under a vault that declares nothing states a cause"
        );
        assert_eq!(
            plan,
            plan_document(
                Path::new("note.md"),
                "note.md",
                whole,
                hash,
                None,
                &undeclaring()
            ),
            "the row the observation replaces changed a plan that still derives"
        );

        let unread = b"---\ntitle: note\n# Heading\n".as_slice();
        let hash = norn_fs::ContentHash::of(unread).to_string();
        let derived = map_document("note.md", unread, hash.clone(), &ContentModel::none())
            .expect("a document derives");
        let plan = plan_document(
            Path::new("note.md"),
            "note.md",
            unread,
            hash.clone(),
            Some(&stored),
            &undeclaring(),
        );
        assert!(
            matches!(plan.change, Some(Change::Upsert(_))),
            "a block nothing read cost the document its row"
        );
        let [finding] = &plan.findings[..] else {
            panic!("the unknown fields are stated");
        };
        assert_eq!(
            finding.subject, derived.facts.path,
            "a derived document's finding stands at another identity"
        );
        assert_eq!(
            plan,
            plan_document(
                Path::new("note.md"),
                "note.md",
                unread,
                hash,
                None,
                &undeclaring()
            ),
            "the row the observation replaces changed a plan that still derives"
        );
    }

    /// **What a side re-derives is the kinds of the causes that side decides.**
    /// An act that opened nothing concludes what the grammar says about the
    /// names it read, so filing one of those verdicts replaces the path kinds
    /// and nothing else. An act that opened a document's bytes concludes what
    /// those bytes say — the body verdict and every unread block — and says
    /// nothing about the other spellings rendering onto the same place.
    #[test]
    fn a_side_rederives_the_kinds_of_the_causes_it_decides() {
        let rederived = |side: Decided| {
            let DiscardScope::Kinds(kinds) = side.rederives() else {
                panic!("{side:?} replaces every kind standing at the place");
            };
            let mut kinds: Vec<&str> = kinds.iter().map(FindingKind::as_str).collect();
            kinds.sort_unstable();
            kinds
        };
        assert_eq!(
            rederived(Decided::BySpelling),
            [
                "document/path-bytes-not-utf8",
                "document/path-names-no-document"
            ]
        );
        assert_eq!(
            rederived(Decided::ByBytes),
            [
                "document/body-bytes-not-utf8",
                "document/frontmatter-too-large",
                "document/frontmatter-unclosed",
                "document/frontmatter-unreadable",
                "document/undeclared-tag"
            ]
        );
    }

    /// **A second reading of one observation plans the same writes.** The act
    /// reads its arguments and nothing else — no clock, no store, no tree — so
    /// two readings of one document agree on the change and on the finding.
    /// That is what lets incremental maintenance land the derived state a
    /// from-zero rebuild over the same tree lands.
    #[test]
    fn a_second_reading_of_one_observation_plans_the_same_writes() {
        let stored = DocumentPath::new("note.md").expect("a document path");
        for source in [
            b"---\ntags: [front]\n---\n# Heading\n[[target]] #body\n".as_slice(),
            b"---\ntitle: note\n# Heading\n".as_slice(),
            b"# Heading\n\xff".as_slice(),
            b"---\ntitle: note\nkind: doc\nstatus: draft\n---\n# Heading\n".as_slice(),
        ] {
            let plan = || {
                plan_document(
                    Path::new("note.md"),
                    "note.md",
                    source,
                    norn_fs::ContentHash::of(source).to_string(),
                    Some(&stored),
                    &reporting(),
                )
            };
            assert_eq!(
                plan(),
                plan(),
                "two readings of one observation planned different writes"
            );
        }
    }

    /// **A key written twice reaches the same degradation whichever spelling
    /// wrote it.** The text layer refuses the block either way, so the row, the
    /// body facts, the note count and the cause a finding is filed under agree
    /// between a document whose duplicate the parser sees and one whose keys
    /// only collapse into a single name. Both accounts name the repeated key;
    /// how precisely each places it is the text layer's contract, not this
    /// layer's.
    #[test]
    fn a_repeated_key_degrades_alike_whether_or_not_a_tag_spelled_it() {
        let derive = |source: &str| {
            let bytes = source.as_bytes();
            map_document(
                "note.md",
                bytes,
                norn_fs::ContentHash::of(bytes).to_string(),
                &ContentModel::none(),
            )
            .expect("a document whose block went unread still derives")
        };
        let plain = derive("---\nk: 1\nk: 2\n---\n# heading\n");
        let tagged = derive("---\n!x k: 1\nk: 2\n---\n# heading\n");
        for (spelling, derived) in [("plain", &plain), ("tagged", &tagged)] {
            assert!(
                derived.facts.frontmatter().is_none(),
                "the {spelling} duplicate produced a projection"
            );
            assert_eq!(
                derived.facts.frontmatter_diagnostic_count, 1,
                "the {spelling} duplicate counted another number of block-scoped notes"
            );
            assert_eq!(
                derived.facts.headings.len(),
                1,
                "the {spelling} duplicate lost the body facts the act could derive"
            );
            let unread = derived
                .unread_frontmatter
                .as_ref()
                .expect("the block was read by nothing");
            assert_eq!(unread.cause, UnreadBlock::Unreadable);
            assert_eq!(
                unread.cause.kind().as_str(),
                "document/frontmatter-unreadable"
            );
        }
        for (spelling, derived) in [("plain", plain), ("tagged", tagged)] {
            let problem = derived
                .unread_frontmatter
                .expect("refused")
                .problem
                .expect("a refusal the reader accounted for");
            assert!(
                problem.contains("duplicate entry with key \"k\""),
                "the {spelling} duplicate's finding does not name the repeated key: {problem:?}"
            );
        }
    }

    /// **The store's projection bound is not a fourth outcome a document can
    /// reach.** The store refuses a frontmatter projection nesting past
    /// `MAX_FRONTMATTER_DEPTH`, and that refusal would withdraw the whole
    /// increment rather than one document — so the bound has to stand above what
    /// any readable block can carry. The text layer refuses the deeper block
    /// first, and a block it refuses is the degradation above: a row, and a
    /// finding naming the cause. The ceiling is searched rather than assumed, so
    /// either bound moving toward the other fails here.
    #[test]
    fn no_readable_block_nests_deeper_than_the_store_projects() {
        let block_nesting = |depth: usize| {
            let mut source = String::from("---\nk: ");
            source.push_str(&"[".repeat(depth));
            source.push_str(&"]".repeat(depth));
            source.push_str("\n---\n# body\n");
            source
        };
        let derive = |source: &str| {
            let bytes = source.as_bytes();
            map_document(
                "note.md",
                bytes,
                norn_fs::ContentHash::of(bytes).to_string(),
                &ContentModel::none(),
            )
            .expect("a document whose block went unread still derives")
        };

        let refused = (1..=norn_store::MAX_FRONTMATTER_DEPTH)
            .find(|depth| derive(&block_nesting(*depth)).unread_frontmatter.is_some())
            .expect("the text layer reads every block the store's bound admits");
        let deepest = derive(&block_nesting(refused - 1));
        let projection = deepest
            .facts
            .frontmatter()
            .expect("the deepest block the text layer reads produced no projection");
        norn_store::canonical_json(projection)
            .expect("the deepest block the text layer reads is past the store's bound");
        assert_eq!(
            derive(&block_nesting(refused))
                .unread_frontmatter
                .expect("the block past the ceiling was read")
                .cause,
            UnreadBlock::Unreadable,
            "a block the text layer will not nest through took another outcome"
        );
    }

    /// **Where the body starts differs by cause.** A closed block bounds its own
    /// bytes, so an unreadable one is skipped whole and nothing inside it is
    /// read. A block that never closes bounds nothing, so the document is body
    /// from its first byte and the links and tags written in the lines that
    /// opened like a block are the document's own body facts. The finding says
    /// the block was read by nothing; it does not say the text is unread.
    #[test]
    fn an_unclosed_block_bounds_nothing_so_its_text_reads_as_body() {
        let derive = |source: &str| {
            let bytes = source.as_bytes();
            map_document(
                "note.md",
                bytes,
                norn_fs::ContentHash::of(bytes).to_string(),
                &ContentModel::none(),
            )
            .expect("a document whose block went unread still derives")
            .facts
        };

        let unclosed = derive("---\ntags: [alpha]\nlink: [[Some Target]]\nnote: #hashtag\n");
        assert_eq!(unclosed.body_offset, 0, "an unclosed block bounded a body");
        assert_eq!(
            unclosed
                .tags
                .iter()
                .map(|t| t.name.as_str())
                .collect::<Vec<_>>(),
            ["hashtag"]
        );
        assert!(
            unclosed.tags.iter().all(|t| t.source == TagSource::Body),
            "a tag was attributed to a block nothing read"
        );
        assert_eq!(
            unclosed
                .links
                .iter()
                .map(|l| l.target.as_str())
                .collect::<Vec<_>>(),
            ["Some Target"]
        );

        // The same text inside a block that closes: the block is skipped whole,
        // so none of it is read either as frontmatter or as body.
        let closed = derive("---\ntags: [alpha]\nlink: [[Some Target]]\nnote: : :\n---\n# body\n");
        assert!(closed.body_offset > 0, "a closed block bounded no body");
        assert!(closed.tags.is_empty(), "a skipped block yielded a tag");
        assert!(closed.links.is_empty(), "a skipped block yielded a link");
    }

    #[test]
    fn mapper_is_the_complete_text_to_store_boundary() {
        let source = b"---\ntags: [front]\nkind: note\n---\n# Heading\n[[target#Part|Title]] #body\nblock ^id\n";
        let derived = map_document(
            "note.md",
            source,
            norn_fs::ContentHash::of(source).to_string(),
            &ContentModel::none(),
        )
        .unwrap();
        let facts = derived.facts;
        assert!(derived.unread_frontmatter.is_none());
        assert!(facts.frontmatter().is_some());
        assert_eq!(facts.headings.len(), 1);
        assert_eq!(facts.links.len(), 1);
        assert_eq!(facts.blocks.len(), 1);
        assert_eq!(facts.tags.len(), 2);
    }

    /// The count beside an absent frontmatter projection is what tells a
    /// document with no block apart from one whose block did not read, so what
    /// the mapper counts is the notes the text layer scoped to the block.
    ///
    /// Every code that layer raises is scoped to the block today, so no
    /// document here is one the filter and a count of every note disagree
    /// over. What the filter holds is the seam: a note the text layer raises
    /// about something other than the block leaves this count through it, and
    /// the scope of a code is that layer's own answer rather than a spelling
    /// read here.
    #[test]
    fn the_frontmatter_note_count_separates_no_block_from_a_block_that_did_not_read() {
        for (source, projection, notes) in [
            (b"# Heading\nbody\n".to_vec(), false, 0),
            (b"---\ntitle: note\n---\nbody\n".to_vec(), true, 0),
            (b"---\ntitle: note\nbody\n".to_vec(), false, 1),
        ] {
            let facts = map_document(
                "note.md",
                &source,
                norn_fs::ContentHash::of(&source).to_string(),
                &ContentModel::none(),
            )
            .unwrap()
            .facts;
            let read = String::from_utf8(source).unwrap();
            assert_eq!(
                facts.frontmatter().is_some(),
                projection,
                "the projection of `{read}` is not what the block is"
            );
            assert_eq!(
                facts.frontmatter_diagnostic_count, notes,
                "`{read}` raised another count of block-scoped notes"
            );
        }
    }

    /// **The facet judges the tags the document already put on its row.** A
    /// document carrying names the declared vocabulary does not admit plans one
    /// finding per distinct name, each targeted at the name so a reader filters
    /// the class without reading prose, and the admitted names plan nothing.
    #[test]
    fn a_reporting_facet_plans_one_finding_per_undeclared_name() {
        let source = b"---\ntags: [front, ephemeral]\n---\n# Heading\n#area/work #draft #draft\n";
        let hash = norn_fs::ContentHash::of(source).to_string();

        let plan = plan_document(
            Path::new("note.md"),
            "note.md",
            source,
            hash,
            None,
            &reporting(),
        );

        assert!(
            matches!(plan.change, Some(Change::Upsert(_))),
            "a facet breach cost the document the row it derives"
        );
        let targets: Vec<Option<&str>> = plan
            .findings
            .iter()
            .map(|finding| finding.target.as_deref())
            .collect();
        // Body tags come before frontmatter tags on the row, and a repeated
        // name is one finding.
        assert_eq!(targets, vec![Some("draft"), Some("ephemeral")]);
        for finding in &plan.findings {
            assert_eq!(finding.cause, Cause::TagBreach(TagBreach::Undeclared));
            assert_eq!(finding.cause.severity(), Severity::Warning);
            assert_eq!(
                finding.subject,
                DocumentPath::new("note.md").expect("a document path")
            );
        }
        assert_eq!(plan.findings[0].detail, "`#draft`, written in the body");
        assert_eq!(
            plan.findings[1].detail,
            "`#ephemeral`, written in the frontmatter"
        );
    }

    /// The control on the case above: the same bytes under a vault that has
    /// declared no tag vocabulary plan the upsert and nothing else. A facet
    /// finding is the schema's judgment, so a vault that judges nothing has
    /// none.
    #[test]
    fn the_same_document_plans_no_facet_finding_where_nothing_is_declared() {
        let source = b"---\ntags: [front, ephemeral]\n---\n# Heading\n#area/work #draft #draft\n";
        let hash = norn_fs::ContentHash::of(source).to_string();

        let plan = plan_document(
            Path::new("note.md"),
            "note.md",
            source,
            hash,
            None,
            &undeclaring(),
        );

        assert!(matches!(plan.change, Some(Change::Upsert(_))));
        assert!(plan.findings.is_empty());
    }

    /// **A document with no facts is judged against nothing.** A declaration
    /// says what a document's facts must be, and a path that does not decode
    /// has none — so the plan is the quarantine alone, whatever the facet
    /// declares.
    #[test]
    fn a_document_that_does_not_decode_is_judged_against_no_facet() {
        let source = b"# heading\n\xff".as_slice();
        let hash = norn_fs::ContentHash::of(source).to_string();

        let plan = plan_document(
            Path::new("note.md"),
            "note.md",
            source,
            hash,
            None,
            &reporting(),
        );

        let [finding] = &plan.findings[..] else {
            panic!("a refused document plans its quarantine and nothing else");
        };
        assert_eq!(
            finding.cause,
            Cause::Undecodable(Undecodable::BodyBytes),
            "a document with no facts was judged against the vault's declaration"
        );
    }

    /// A facet breach leaves the document whole, so it is reported as a warning
    /// rather than as the error a derivation defect is.
    #[test]
    fn the_two_finding_families_carry_the_severity_their_effect_on_derived_state_has() {
        for cause in CAUSES {
            let expected = match cause {
                Cause::Undecodable(_) | Cause::UnreadBlock(_) => Severity::Error,
                Cause::TagBreach(_) => Severity::Warning,
            };
            assert_eq!(cause.severity(), expected, "`{}`", cause.kind());
        }
    }

    /// A store holding `documents`, each derived through the host's own
    /// declaration of `schema` and [`plan_document`] and written in one
    /// changeset, and the find a request over it answers: the found paths, in
    /// page order.
    struct DerivedVault {
        _scratch: norn_testkit::scratch::Scratch,
        _store: norn_store::Store,
        reader: std::sync::Arc<norn_store::SnapshotReader>,
        declared: Declared,
    }

    impl DerivedVault {
        fn new(label: &str, schema: &[u8], documents: &[(&str, String)]) -> Self {
            const FINGERPRINT: &str = "compared";
            let declared = Declared::pinned(
                VaultSchema::parse(schema).expect("a schema declaring typed fields"),
                FINGERPRINT,
            );
            let scratch = norn_testkit::scratch::Scratch::new(label);
            let mut store = norn_store::Store::open(
                scratch.join("store.sqlite3"),
                norn_store::StoredPathOrder::Sensitive,
                crate::DERIVATION_VERSION,
            )
            .expect("a store");
            store
                .begin_request()
                .pin_vault_schema(schema, FINGERPRINT)
                .expect("pinning the compared schema");
            let changes: Vec<Change> = documents
                .iter()
                .map(|(path, bytes)| {
                    let hash = norn_fs::ContentHash::of(bytes.as_bytes()).to_string();
                    plan_document(
                        Path::new(path),
                        path,
                        bytes.as_bytes(),
                        hash,
                        None,
                        &declared,
                    )
                    .change
                    .expect("a document that derives")
                })
                .collect();
            store
                .begin_request()
                .apply_increment(norn_store::IncrementProvenance::Derived, changes, &[])
                .expect("writing the derived documents");
            let reader = std::sync::Arc::new(store.open_reader().reader.expect("a reader"));
            DerivedVault {
                _scratch: scratch,
                _store: store,
                reader,
                declared,
            }
        }

        fn request() -> norn_wire::FindParams {
            norn_wire::FindParams::new(norn_wire::VaultAddress::name(
                norn_wire::VaultName::new("compared").expect("a vault name"),
            ))
        }

        fn find(&self, params: norn_wire::FindParams) -> Vec<String> {
            self.reader
                .try_take()
                .expect("an idle reader")
                .establish()
                .snapshot
                .expect("a snapshot")
                .find(&params, self.declared.content_model())
                .expect("a find")
                .rows
                .into_iter()
                .map(|row| row.path.as_str().to_string())
                .collect()
        }
    }

    /// One compared document: its path, then the raw text it writes under
    /// `when`, `weight`, `code` and `flag`.
    type Compared = (
        &'static str,
        &'static str,
        &'static str,
        &'static str,
        &'static str,
    );

    /// The raw text one compared document writes under one key.
    type Written = fn(&Compared) -> &'static str;

    /// The documents the comparison case derives, and what each writes under
    /// `when` (a date), `weight` (a number), `code` (text) and `flag` (a
    /// boolean). The raw text is what the in-process rule reads; the store
    /// reads the same bytes through [`plan_document`].
    ///
    /// The dates part the raw order from the typed one, and mix a stated
    /// offset with an unstated one: `a.md` names 09:00 UTC and `b.md` 10:00 at
    /// no stated offset. The numbers part them too — `10` sorts before `9` as
    /// text — and `b.md`, `f.md` and `g.md` write nine as an integer, as a
    /// string and as a float, three raw texts the declared number reads as one
    /// value. `code` writes one as a number and once as a string. `flag`
    /// writes booleans bare and as strings padded with a space, which the
    /// declared boolean reads as the same values and whose raw text sorts
    /// apart from them: `" true"` before `false` and `"false "` after it.
    const COMPARED: [Compared; 7] = [
        ("a.md", "2026-03-04T09:00:00Z", "10", "1", "true"),
        ("b.md", "2026-03-04T10:00:00", "9", "\"1\"", "false"),
        ("c.md", "2026-03-04T09:30:00+01:00", "9.5", "2", "\" true\""),
        ("d.md", "2026-03-04", "-1", "10", "false"),
        ("e.md", "2026-03-03T23:00:00-05:00", "100", "x", "true"),
        ("f.md", "2026-03-05", "\"9\"", "y", "\"false \""),
        ("g.md", "2026-03-06", "9.0", "z", "true"),
    ];

    /// **One comparison rule governs the store's typed order and the
    /// in-process comparison.** Documents are derived through the host's own
    /// declaration and plan, written through a changeset, and found through
    /// the builder: a typed sort, both ways, and a typed bound, equality,
    /// inequality and membership part answer exactly what
    /// [`TypedValue::compare`] answers over the same raw text — across a
    /// mixed-offset pair, which the rule signals and orders by reading the
    /// unstated side at offset zero, across a number whose text order is not
    /// its numeric one, across `9` and `9.0`, two texts one number, and across
    /// booleans whose raw text is not their boolean order. On a
    /// key declared as text equality is over the raw text, so `1` written as a
    /// number and `"1"` written as a string are one value to an equality part,
    /// as they are to the rule.
    #[test]
    fn the_stores_typed_order_and_the_in_process_comparison_are_one_rule() {
        use std::cmp::Ordering;

        use norn_config::schema::{ComparisonSignal, FieldType, TypedValue};
        use norn_wire::{Direction, Predicate, Sort, SortKey};

        let documents: Vec<(&str, String)> = COMPARED
            .iter()
            .map(|(path, when, weight, code, flag)| {
                (
                    *path,
                    format!(
                        "---\nwhen: {when}\nweight: {weight}\ncode: {code}\nflag: {flag}\n---\nbody\n"
                    ),
                )
            })
            .collect();
        let vault = DerivedVault::new(
            "norn-host-comparison",
            b"version: 1\nfields:\n  when:\n    type: date\n  weight:\n    type: number\n  code:\n    type: text\n  flag:\n    type: boolean\n",
            &documents,
        );
        let find = |params| vault.find(params);
        let request = DerivedVault::request;
        let unquoted = |raw: &str| raw.trim_matches('"').to_string();
        let typed = |kind: FieldType, raw: &str| {
            kind.read(&unquoted(raw))
                .unwrap_or_else(|_| panic!("`{raw}` reads as {kind}"))
        };

        // Each typed key, with the raw text a compared row writes under it.
        let typed_keys: [(&str, FieldType, Written); 3] = [
            ("when", FieldType::Date, |row| row.1),
            ("weight", FieldType::Number, |row| row.2),
            ("flag", FieldType::Boolean, |row| row.4),
        ];
        for (key, kind, written) in typed_keys {
            let value = |path: &str| {
                let row = COMPARED
                    .iter()
                    .find(|row| row.0 == path)
                    .expect("a compared document");
                typed(kind, written(row))
            };
            // The rule's order: the typed comparison, the path breaking a tie.
            let mut expected: Vec<String> = COMPARED.iter().map(|row| row.0.to_string()).collect();
            expected.sort_by(|left, right| {
                value(left)
                    .compare(&value(right))
                    .ordering
                    .then_with(|| left.cmp(right))
            });
            let ascending =
                find(request().with_sort(Sort::new(SortKey::field(key), Direction::Ascending)));
            assert_eq!(
                ascending, expected,
                "`{key}` ascending is not the rule's order"
            );
            let descending =
                find(request().with_sort(Sort::new(SortKey::field(key), Direction::Descending)));
            expected.reverse();
            assert_eq!(
                descending, expected,
                "`{key}` descending is not the rule's order"
            );

            // A bound, an equality and an inequality compare under the same
            // rule, at every value as the one named: `after` is what the rule
            // calls greater, `before` less, `eq` equal and `not_eq` anything
            // else.
            let answered_by = |wanted: &dyn Fn(&TypedValue) -> bool| {
                let mut matching: Vec<String> = COMPARED
                    .iter()
                    .map(|row| row.0.to_string())
                    .filter(|path| wanted(&value(path)))
                    .collect();
                matching.sort_by_key(|path| path.to_ascii_lowercase());
                matching
            };
            for row in &COMPARED {
                let raw = unquoted(written(row));
                let bound = typed(kind, &raw);
                let ordering = |found: &TypedValue| found.compare(&bound).ordering;
                for (predicate, matching) in [
                    (
                        Predicate::after(key, raw.clone()),
                        answered_by(&|found| ordering(found) == Ordering::Greater),
                    ),
                    (
                        Predicate::before(key, raw.clone()),
                        answered_by(&|found| ordering(found) == Ordering::Less),
                    ),
                    (
                        Predicate::equal_to(key, raw.clone()),
                        answered_by(&|found| ordering(found) == Ordering::Equal),
                    ),
                    (
                        Predicate::not_equal_to(key, raw.clone()),
                        answered_by(&|found| ordering(found) != Ordering::Equal),
                    ),
                ] {
                    assert_eq!(
                        find(request().with_predicates([predicate.clone()])),
                        matching,
                        "{predicate:?} does not answer what the rule does"
                    );
                }
            }
        }

        // The mixed-offset pair: the rule signals it, orders the stated 09:00Z
        // before the unstated 10:00, and the store's order agrees.
        let comparison = typed(FieldType::Date, "2026-03-04T09:00:00Z")
            .compare(&typed(FieldType::Date, "2026-03-04T10:00:00"));
        assert_eq!(comparison.signal, Some(ComparisonSignal::MixedOffset));
        assert_eq!(comparison.ordering, Ordering::Less);
        let by_when =
            find(request().with_sort(Sort::new(SortKey::field("when"), Direction::Ascending)));
        let at = |path: &str| by_when.iter().position(|found| found == path);
        assert!(at("a.md") < at("b.md"), "{by_when:?}");
        // `10` before `9` as text; after it as a number.
        let by_weight =
            find(request().with_sort(Sort::new(SortKey::field("weight"), Direction::Ascending)));
        assert!(
            by_weight.iter().position(|found| found == "b.md")
                < by_weight.iter().position(|found| found == "a.md"),
            "{by_weight:?}"
        );

        // Numeric-versus-text equality: one written as a number and one as a
        // string are the one raw text, to the builder's equality and to the
        // rule alike.
        assert_eq!(
            find(request().with_predicates([Predicate::equal_to("code", "1")])),
            ["a.md", "b.md"]
        );
        assert_eq!(
            TypedValue::Text("1".to_string())
                .compare(&typed(FieldType::Text, "\"1\""))
                .ordering,
            Ordering::Equal
        );
        // On a number, equality is the number's: `9`, `"9"` and `9.0` are
        // one value to the rule, and to an equality or membership part named
        // with any of the three spellings.
        for nine in ["9", "9.0", "\"9\""] {
            assert_eq!(
                typed(FieldType::Number, "9")
                    .compare(&typed(FieldType::Number, nine))
                    .ordering,
                Ordering::Equal
            );
        }
        for nine in ["9", "9.0", "09"] {
            assert_eq!(
                find(request().with_predicates([Predicate::equal_to("weight", nine)])),
                ["b.md", "f.md", "g.md"],
                "`weight` equal to {nine}"
            );
        }
        assert_eq!(
            find(request().with_predicates([Predicate::in_any(
                "weight",
                ["9.0".to_string(), "1e2".to_string()]
            )])),
            ["b.md", "e.md", "f.md", "g.md"]
        );
    }

    /// **Matching is symmetric in the shape of the stored value.** A value
    /// written bare and the same value written inside a sequence are one value
    /// to an equality, an inequality and a membership part, under a declared
    /// number and under text alike: every scalar a key holds is a value of the
    /// key, so `[9]` holds nine as `9` does, and `[3, 9.0]` holds it too.
    #[test]
    fn a_value_written_bare_and_inside_a_sequence_meet_one_part_alike() {
        use norn_wire::Predicate;

        let documents: Vec<(&str, String)> = [
            ("bare.md", "9", "a"),
            ("bracketed.md", "[9]", "[a]"),
            ("among.md", "[3, 9.0]", "[b, a]"),
            ("other.md", "3", "b"),
        ]
        .into_iter()
        .map(|(path, weight, code)| {
            (
                path,
                format!("---\nweight: {weight}\ncode: {code}\n---\nbody\n"),
            )
        })
        .collect();
        let vault = DerivedVault::new(
            "norn-host-stored-shape",
            b"version: 1\nfields:\n  weight:\n    type: number\n  code:\n    type: text\n",
            &documents,
        );
        let find = |part: Predicate| vault.find(DerivedVault::request().with_predicates([part]));
        for (key, value) in [("weight", "9"), ("code", "a")] {
            assert_eq!(
                find(Predicate::equal_to(key, value)),
                ["among.md", "bare.md", "bracketed.md"],
                "`{key}` equal to {value}"
            );
            assert_eq!(
                find(Predicate::in_any(key, [value.to_string()])),
                ["among.md", "bare.md", "bracketed.md"],
                "`{key}` in [{value}]"
            );
            assert_eq!(
                find(Predicate::not_equal_to(key, value)),
                ["other.md"],
                "`{key}` not equal to {value}"
            );
        }
    }
}
