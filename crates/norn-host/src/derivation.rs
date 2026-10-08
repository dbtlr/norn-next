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
use std::sync::Arc;

use norn_config::schema::{
    Breach, FieldType, FindingIdentity, ForbiddenFix, Offset, Rule, RuleFinding, RuleWork, Shape,
    TypedValue, UndeclaredTags, VaultSchema,
};
use norn_store::{
    AnchorReadings, BlockFact, Change, ContentModel, DerivationVersion, DiscardScope,
    DocumentFacts, DocumentPath, FieldDeclaration, FrontmatterValue, HeadingFact, HeldBlock,
    LinkAnchor, LinkFact, LinkFamily, LinkKey, OffsetSpelling, Provenance, Span, TagFact,
    TagSource, TypedOrder,
};
use norn_text::{BlockRefusal, Document, SourceSpan, Value};
use norn_wire::{
    AuthoredValue, CaseFold, FieldShape, FindingKind, FindingScope, RuleAllowedPaths,
    RuleClosedSet, RuleExclude, RuleForbiddenFix, RuleMatch, SchemaRule, Severity, TagStance,
    ValueMap, fold_tag,
};

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
pub const DERIVATION_VERSION: DerivationVersion = DerivationVersion::new(10);

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
    /// The document derives whole and disagrees with a field declaration or
    /// with the combined constraint of the schema rules selecting it
    /// ([`VaultSchema::judge`]).
    RuleBreach(Breach),
}

impl Cause {
    /// The finding kind this cause is recorded under.
    pub(crate) const fn kind(self) -> FindingKind {
        match self {
            Cause::Undecodable(cause) => cause.kind(),
            Cause::UnreadBlock(cause) => cause.kind(),
            Cause::TagBreach(cause) => cause.kind(),
            Cause::RuleBreach(breach) => breach.kind(),
        }
    }

    /// How urgently a finding of this cause is reported, where the cause
    /// alone decides it.
    ///
    /// The two derivation defects are errors: derived state is missing
    /// something the vault holds, and a reader of that state gets a wrong
    /// answer until it is fixed. A facet breach is a warning: the document
    /// derived whole, every fact it holds is on its row, and what stands is a
    /// disagreement between the vault's own declaration and its contents. A
    /// rule breach is the same disagreement, reported at the highest
    /// severity of the rules it cites, which its judgment states and its
    /// planned finding carries; this is the floor a rule stating none is
    /// reported at.
    pub(crate) const fn severity(self) -> Severity {
        match self {
            Cause::Undecodable(_) | Cause::UnreadBlock(_) => Severity::Error,
            Cause::TagBreach(_) | Cause::RuleBreach(_) => Severity::Warning,
        }
    }

    /// What an act has to read to conclude this cause.
    pub(crate) const fn decided(self) -> Decided {
        match self {
            Cause::Undecodable(cause) => cause.decided(),
            // A block is read out of the document's own bytes, so concluding
            // that nothing read it means having opened them. So is a tag: the
            // facts a facet judges are read from the document itself. So is a
            // rule breach: it judges the frontmatter those bytes hold, at the
            // path they stand at, and the act that re-reads them re-judges it.
            Cause::UnreadBlock(_) | Cause::TagBreach(_) | Cause::RuleBreach(_) => Decided::ByBytes,
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
            // The field, the value and the rules cited are the finding's
            // target, value and rule set rather than part of its message, so
            // a reader filters by them without parsing prose and the message's
            // bytes grow with none of them.
            Cause::RuleBreach(breach) => format!(
                "`{}` disagrees with the vault's schema: {}",
                subject.as_str(),
                breach.statement()
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
const CAUSES: [Cause; 16] = [
    Cause::Undecodable(Undecodable::PathBytes),
    Cause::Undecodable(Undecodable::PathSpelling),
    Cause::Undecodable(Undecodable::BodyBytes),
    Cause::UnreadBlock(UnreadBlock::Unclosed),
    Cause::UnreadBlock(UnreadBlock::Unreadable),
    Cause::UnreadBlock(UnreadBlock::TooLarge),
    Cause::TagBreach(TagBreach::Undeclared),
    Cause::RuleBreach(Breach::RequiredMissing),
    Cause::RuleBreach(Breach::Forbidden),
    Cause::RuleBreach(Breach::NotOneOf),
    Cause::RuleBreach(Breach::TooLong),
    Cause::RuleBreach(Breach::TypeMismatch),
    Cause::RuleBreach(Breach::ShapeMismatch),
    Cause::RuleBreach(Breach::Misplaced),
    Cause::RuleBreach(Breach::FieldRulesConflict),
    Cause::RuleBreach(Breach::DocumentRulesConflict),
];

/// The finding kinds no cause above carries.
///
/// Quarantine, the unread block, the tag facet and rule judgment are the
/// producers recording findings today. Link health's three kinds — broken,
/// ambiguous, missing anchor — are named here because they have no cause in
/// this crate: ADR 0027 rules the store to judge link health in SQL and file
/// those findings itself, inside the changeset, over per-document facts this
/// crate's derivation already writes, so no per-document act here ever
/// concludes one. A kind minted for another producer is named here too, which
/// is the one line that keeps the classification below a reading of the
/// registry rather than a claim that every kind the registry holds is this
/// crate's.
const KINDS_NO_CAUSE_CARRIES: [FindingKind; 3] = [
    FindingKind::Broken,
    FindingKind::Ambiguous,
    FindingKind::MissingAnchor,
];

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
            | (Cause::RuleBreach(_), FindingScope::Document)
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
    let source = document_source(bytes).map_err(|problem| Quarantine {
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
    facts.links = links_of(&document, &scan);
    facts.headings = scan
        .headings()
        .iter()
        .map(|h| HeadingFact {
            level: h.level,
            text: h.text.clone(),
            reading: norn_text::heading_reading(&h.text),
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
    // The file's order: the frontmatter stands before the body, so its tags
    // come first, and a tag's ordinal is where the file writes it.
    facts.tags = document
        .frontmatter_tags()
        .into_iter()
        .map(|t| TagFact {
            name: t.name,
            source: TagSource::Frontmatter,
            span: t.span.map(span),
        })
        .chain(scan.tags().into_iter().map(|t| TagFact {
            name: t.name,
            source: TagSource::Body,
            span: t.span.map(span),
        }))
        .collect();
    Ok(Derived {
        facts,
        unread_frontmatter,
    })
}

/// One finding a plan asks a job to file: the subject it stands at, the cause
/// it states, the severity it is reported at, and the formatted detail — the
/// spelling this finding was read from, and the reader's own account of the
/// refusal where there is one — with the rules it cites and the value it
/// judged where a rule breach names them.
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
    /// subject is the whole of what it is about. A rule breach about a field
    /// carries the field.
    pub(crate) target: Option<String>,
    /// How urgently it is reported: the cause's own severity, or, for a rule
    /// breach, the highest of the rules it cites.
    pub(crate) severity: Severity,
    /// The rules a rule breach cites, by name; empty for every other cause and
    /// for a type or shape mismatch, which no rule states.
    pub(crate) rules: BTreeSet<String>,
    /// The offending value a rule breach names, spelled as the store keeps a
    /// field value: a scalar as its field row's text, a list or a map as its
    /// canonical JSON. `None` for every other cause and for a breach naming
    /// no value.
    pub(crate) value: Option<String>,
    /// What tells a rule breach from another as the write gate compares a
    /// plan's result with what stood before it: its kind, field, offending
    /// value and combined constraint, the last by value
    /// ([`norn_config::schema::FindingIdentity`]). `None` for every other
    /// cause, which the gate tells apart by its kind and target. Never
    /// filed: the store keys a finding by its kind, field and value.
    pub(crate) identity: Option<FindingIdentity>,
}

impl PlannedFinding {
    /// A finding about `subject` whose cause alone decides it: no rule cited,
    /// no value named, at the cause's own severity.
    fn of(subject: DocumentPath, cause: Cause, detail: String, target: Option<String>) -> Self {
        PlannedFinding {
            subject,
            cause,
            detail,
            target,
            severity: cause.severity(),
            rules: BTreeSet::new(),
            value: None,
            identity: None,
        }
    }
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
    /// What judging the document against the schema's field declarations and
    /// rules cost, which no statement counter sees; zero where nothing was
    /// judged.
    pub(crate) rule_work: RuleWork,
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
/// findings, the rule findings and the typed half of the field rows: a
/// document that does not decode is judged against nothing, because a vault
/// declaration says what a document's facts must be and there are no facts.
///
/// `case` is how a rule's path globs compare their literal letters with the
/// document's path: the store's recorded path order names it
/// (`norn_store::StoredPathOrder::glob_case`), as for every glob over a vault
/// path.
pub(crate) fn plan_document(
    path: &Path,
    spelling: &str,
    bytes: &[u8],
    hash: String,
    stored: Option<&DocumentPath>,
    declared: &Declared,
    case: CaseFold,
) -> Plan {
    match map_document(spelling, bytes, hash, declared.content_model()) {
        Ok(derived) => {
            let subject = derived.facts.path.clone();
            let mut findings = Vec::new();
            let mut rule_work = RuleWork::default();
            match derived.unread_frontmatter {
                Some(unread) => {
                    let detail = match unread.problem {
                        Some(problem) => format!("{path:?}: {problem}"),
                        None => format!("{path:?}"),
                    };
                    findings.push(PlannedFinding::of(
                        subject.clone(),
                        Cause::UnreadBlock(unread.cause),
                        detail,
                        None,
                    ));
                }
                // A block nothing read leaves the fields unknown rather than
                // absent, so no rule is judged against them: a required field
                // would be reported missing from a block that may hold it.
                // The unread block's own finding is what stands instead.
                None => {
                    let (judged, work) = plan_rule_judgment(
                        path,
                        &subject,
                        derived.facts.frontmatter(),
                        declared.schema(),
                        case,
                    );
                    findings.extend(judged);
                    rule_work = work;
                }
            }
            findings.extend(plan_tag_facet(&subject, &derived.facts, declared.schema()));
            Plan {
                change: Some(Change::Upsert(derived.facts)),
                findings,
                rule_work,
            }
        }
        Err(quarantine) => Plan {
            change: stored.map(|row| Change::Death {
                path: row.clone(),
                provenance: Provenance::Quarantine,
            }),
            findings: vec![plan_quarantine(path, quarantine)],
            rule_work: RuleWork::default(),
        },
    }
}

/// Judge a document's frontmatter and path against the vault schema's field
/// declarations and rules ([`VaultSchema::judge`]): one finding per field,
/// constraint kind and offending value, and the work the judgment paid.
///
/// **The judgment reads this document alone.** Its inputs are the path, the
/// frontmatter the facts were derived from and the schema, so its findings
/// are filed in the document's own changeset under the fingerprint the
/// schema is pinned by, as the tag facet's are, and a re-derivation of the
/// document re-judges them. A frontmatter whose top level is no map holds no
/// field, as it derives no field row.
///
/// A schema declaring no field and stating no rule judges nothing, and pays
/// nothing to say so.
fn plan_rule_judgment(
    path: &Path,
    subject: &DocumentPath,
    frontmatter: Option<&FrontmatterValue>,
    schema: &VaultSchema,
    case: CaseFold,
) -> (Vec<PlannedFinding>, RuleWork) {
    if schema.fields().next().is_none() && schema.rules().next().is_none() {
        return (Vec::new(), RuleWork::default());
    }
    let judgment = schema.judge(subject.as_str(), &authored_fields(frontmatter), case);
    let work = judgment.work();
    let findings = judgment
        .into_findings()
        .into_iter()
        .map(|finding| rule_finding(path, subject, finding))
        .collect();
    (findings, work)
}

/// Judge the document at `subject` whose frontmatter block came to `block`
/// against the vault schema's field declarations and rules, as
/// [`plan_document`] judges one from its bytes: the rule findings it files,
/// and the work that cost. A block nothing read is judged against nothing,
/// as there.
///
/// **What the write gate judges a carried document by.** A document a move
/// carries byte for byte is judged again at its destination, since a rule's
/// path selectors and allowed paths read where it stands, and the gate reads
/// its block from the store's projection where the index vouches for it
/// ([`norn_store::Snapshot::held_frontmatter`]) and from its bytes
/// ([`frontmatter_block`]) where it does not, so this is the one judgment
/// either reading meets. Every other finding a document's bytes conclude is
/// a function of its bytes alone, the same wherever it stands.
pub(crate) fn judge_block(
    subject: &DocumentPath,
    block: &HeldBlock,
    declared: &Declared,
    case: CaseFold,
) -> (Vec<PlannedFinding>, RuleWork) {
    let frontmatter = match block {
        HeldBlock::Read(value) => Some(value),
        HeldBlock::None => None,
        HeldBlock::Unread => return (Vec::new(), RuleWork::default()),
    };
    plan_rule_judgment(
        Path::new(subject.as_str()),
        subject,
        frontmatter,
        declared.schema(),
        case,
    )
}

/// What the frontmatter block of the document `bytes` spell came to, read as
/// a derivation reads it ([`map_document`]); `None` where the bytes decode as
/// no document.
pub(crate) fn frontmatter_block(bytes: &[u8]) -> Option<HeldBlock> {
    let source = document_source(bytes).ok()?;
    let document = parsed(source);
    Some(if document.frontmatter_refusal().is_some() {
        HeldBlock::Unread
    } else {
        match document.frontmatter() {
            Some(value) => HeldBlock::Read(map_value(value)),
            None => HeldBlock::None,
        }
    })
}

/// The fields the frontmatter of the document `bytes` spell writes, as a
/// written value holds them and rule judgment reads them: none where it
/// carries no block, and `None` where its fields cannot be read — the bytes
/// decode as no document, its block is one nothing read, or the block's top
/// level is no map. What `new` at a bare path takes as the caller's values
/// beneath which the rule defaults fill.
pub(crate) fn written_fields(bytes: &[u8]) -> Option<ValueMap> {
    match frontmatter_block(bytes)? {
        // An empty block reads as null: it holds no field.
        HeldBlock::None | HeldBlock::Read(FrontmatterValue::Null) => Some(ValueMap::default()),
        HeldBlock::Read(value @ FrontmatterValue::Map(_)) => Some(authored_fields(Some(&value))),
        HeldBlock::Read(_) | HeldBlock::Unread => None,
    }
}

/// The planned finding a rule judgment's `finding` is filed as.
fn rule_finding(path: &Path, subject: &DocumentPath, finding: RuleFinding) -> PlannedFinding {
    PlannedFinding {
        subject: subject.clone(),
        cause: Cause::RuleBreach(finding.breach()),
        detail: format!("{path:?}"),
        target: finding.field().map(str::to_string),
        severity: finding.severity(),
        rules: finding.rules().iter().cloned().collect(),
        value: finding.value().and_then(stored_spelling),
        identity: Some(finding.identity().clone()),
    }
}

/// A frontmatter's fields as a written value holds them, the last write of a
/// repeated key standing as the canonical projection keeps it, and none where
/// the frontmatter is absent or its top level is no map.
fn authored_fields(frontmatter: Option<&FrontmatterValue>) -> ValueMap {
    let Some(FrontmatterValue::Map(entries)) = frontmatter else {
        return ValueMap::default();
    };
    let mut fields: Vec<(String, AuthoredValue)> = Vec::with_capacity(entries.len());
    for (key, value) in entries {
        fields.retain(|(held, _)| held != key);
        fields.push((key.clone(), authored(value)));
    }
    ValueMap::new(fields).expect("each key is held once, its last write standing")
}

/// One frontmatter value as a written value holds it. A float no written value
/// can hold — `NaN` or an infinity — is null, as the canonical projection and
/// the field rows read it.
fn authored(value: &FrontmatterValue) -> AuthoredValue {
    match value {
        FrontmatterValue::Null => AuthoredValue::Null,
        FrontmatterValue::Bool(flag) => AuthoredValue::Bool(*flag),
        FrontmatterValue::Int(number) => AuthoredValue::Integer(*number),
        FrontmatterValue::Float(number) => {
            AuthoredValue::float(*number).unwrap_or(AuthoredValue::Null)
        }
        FrontmatterValue::String(text) => AuthoredValue::String(text.clone()),
        FrontmatterValue::Sequence(items) => {
            AuthoredValue::List(items.iter().map(authored).collect())
        }
        FrontmatterValue::Map(_) => AuthoredValue::Map(authored_fields(Some(value))),
    }
}

/// An offending value spelled as the store keeps a field value: a scalar as
/// its field row's raw text, a list or a map as its canonical JSON. A null has
/// no spelling, and names no value.
fn stored_spelling(value: &AuthoredValue) -> Option<String> {
    match value {
        AuthoredValue::List(_) | AuthoredValue::Map(_) => {
            norn_store::canonical_json(&projected(value)).ok()
        }
        scalar => scalar.scalar_text(),
    }
}

/// A written value as the store's projection takes it.
fn projected(value: &AuthoredValue) -> FrontmatterValue {
    match value {
        AuthoredValue::Null => FrontmatterValue::Null,
        AuthoredValue::Bool(flag) => FrontmatterValue::Bool(*flag),
        AuthoredValue::Integer(number) => FrontmatterValue::Int(*number),
        AuthoredValue::Float(number) => FrontmatterValue::Float(number.get()),
        AuthoredValue::String(text) => FrontmatterValue::String(text.clone()),
        AuthoredValue::List(items) => {
            FrontmatterValue::Sequence(items.iter().map(projected).collect())
        }
        AuthoredValue::Map(map) => FrontmatterValue::Map(
            map.entries()
                .iter()
                .map(|(key, value)| (key.clone(), projected(value)))
                .collect(),
        ),
    }
}

/// The declaration a plan derives and a read compiles under: the pinned
/// schema's content model, and the typed orders its declared fields hand the
/// store, named by the fingerprint the schema is pinned under.
///
/// Built once per schema rather than per document, and only through
/// [`Declared::pinned`] and [`Declared::unpinned`], so the typed orders a plan
/// fills the field pillar with are always the ones the schema beside them
/// declares, and carry the fingerprint the store compares with its own pin.
/// The model is shared, because a read carries the one its attachment's store
/// pins for the read's whole length.
pub(crate) struct Declared {
    schema: VaultSchema,
    content_model: Arc<ContentModel>,
}

impl Declared {
    /// The declaration `schema` makes, pinned under `fingerprint`.
    pub(crate) fn pinned(schema: VaultSchema, fingerprint: impl Into<String>) -> Self {
        let content_model = Arc::new(content_model(&schema, fingerprint.into()));
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
            content_model: Arc::new(ContentModel::none()),
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

    /// The content model, shared with whatever outlives this declaration.
    pub(crate) fn shared_content_model(&self) -> &Arc<ContentModel> {
        &self.content_model
    }
}

/// What `schema` declares, pinned under `fingerprint`, as the store reads it:
/// every declared field with its type and the shape it declares, and for each
/// whose type does not order as text, the typed order that type reads a raw
/// value into; the declared tags, the tag patterns and the stance on an
/// undeclared tag; the ambiguity-ignore patterns, the places the schema keeps
/// out of ambiguity classes, which the resolver applies and `describe` reports
/// as path rules; each creation rule and the inbox, every template as its
/// source text, which `describe` alone reports; and each schema rule as the
/// schema writes it, which `describe` reports and a `validate` naming a rule
/// is checked against.
///
/// A raw value that does not read as its declared type has no sort key, which
/// is the store's `NULL`: the document still carries the value, and a typed
/// order has nothing to place it by.
fn content_model(schema: &VaultSchema, fingerprint: String) -> ContentModel {
    let declared = schema.fields().fold(
        ContentModel::under(fingerprint),
        |declared, (key, field)| {
            declared.declare_field(
                key,
                field_declaration(field.kind()).with_shape(field.shape().map(field_shape)),
            )
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
    let declared = schema
        .ambiguity_ignore()
        .iter()
        .fold(declared, |declared, pattern| {
            declared.declare_ambiguity_ignore(pattern.clone())
        });
    let declared = schema.creation_rules().fold(declared, |declared, rule| {
        declared.declare_creation_rule(
            rule.name(),
            rule.target().as_str(),
            rule.variables().to_vec(),
            rule.frontmatter_defaults(),
            rule.body().map(|body| body.as_str().to_string()),
        )
    });
    let declared = match schema.inbox() {
        Some(inbox) => declared.declare_inbox(inbox.target().as_str()),
        None => declared,
    };
    schema.rules().fold(declared, |declared, rule| {
        declared.declare_rule(schema_rule(rule))
    })
}

/// A declared shape as the wire spells it: the same two members, held equal
/// by spelling in `norn-config`'s suite.
const fn field_shape(shape: Shape) -> FieldShape {
    match shape {
        Shape::Single => FieldShape::Single,
        Shape::List => FieldShape::List,
    }
}

/// `rule` as `describe` reports it: every part as the schema writes it — a
/// selector's every value in the order written and in the spelling it is
/// compared by, each glob and route as its source text, a default as its
/// source — and every part the rule does not declare left out.
fn schema_rule(rule: &Rule) -> SchemaRule {
    let selector = rule.selector();
    let declared = SchemaRule::new(rule.name(), rule.severity())
        .with_match(RuleMatch::new(
            selector
                .frontmatter()
                .map(|(key, values)| (key.to_string(), values.to_vec())),
            selector.path().map(|glob| glob.as_str().to_string()),
        ))
        .with_exclude(RuleExclude::new(
            selector
                .exclude()
                .iter()
                .map(|glob| glob.as_str().to_string()),
        ));
    let declared = match rule.description() {
        Some(description) => declared.with_description(description),
        None => declared,
    };
    let declared = rule
        .required()
        .fold(declared, |declared, (field, default)| {
            declared.with_required(field, default.map(|default| default.source()))
        });
    let declared = rule.forbidden().fold(declared, |declared, (field, fix)| {
        declared.with_forbidden(
            field,
            match fix {
                ForbiddenFix::Unfixed => None,
                ForbiddenFix::Remove => Some(RuleForbiddenFix::Remove),
                ForbiddenFix::RenameTo(target) => Some(RuleForbiddenFix::RenameTo(target.clone())),
            },
        )
    });
    let declared = rule.one_of().fold(declared, |declared, (field, set)| {
        declared.with_one_of(
            field,
            RuleClosedSet::new(
                set.values().iter().cloned(),
                set.synonyms()
                    .map(|(written, member)| (written.to_string(), member.to_string())),
            ),
        )
    });
    let declared = rule
        .max_length()
        .fold(declared, |declared, (field, limit)| {
            declared.with_max_length(field, limit)
        });
    match rule.allowed_paths() {
        Some(allowed) => declared.with_allowed_paths(RuleAllowedPaths::new(
            allowed.paths().iter().map(|glob| glob.as_str().to_string()),
            allowed.route().map(|route| route.as_str().to_string()),
        )),
        None => declared,
    }
}

/// A field declared as `kind`, as the store reads it: under the wire type a
/// `describe` facet reports, which is spelled as `kind` is — the two enums are
/// one vocabulary, held equal by spelling in `norn-config`'s suite — and, for a
/// type that does not order as text, with the typed order `kind` reads a raw
/// value into. A date's order is dated: beside each sort key it says whether
/// the date stated an offset, which the store records beside the typed key.
/// A link orders as text and carries the key the schema compares it by.
fn field_declaration(kind: FieldType) -> FieldDeclaration {
    let order = || TypedOrder::new(move |raw| kind.read(raw).ok().map(|value| value.sort_key()));
    match kind {
        FieldType::Text => FieldDeclaration::text(),
        FieldType::Number => FieldDeclaration::number(order()),
        FieldType::Boolean => FieldDeclaration::boolean(order()),
        FieldType::Date => FieldDeclaration::date(TypedOrder::dated(|raw| {
            match FieldType::Date.read(raw).ok()? {
                value @ TypedValue::Date(date) => Some((
                    value.sort_key(),
                    match date.offset() {
                        Offset::Stated(_) => OffsetSpelling::Stated,
                        Offset::Unstated => OffsetSpelling::Unstated,
                    },
                )),
                _ => None,
            }
        })),
        FieldType::Tags => FieldDeclaration::tags(),
        // The store compares a link by the key the schema reads it into and
        // reads no link syntax itself, so the reading crosses as a closure,
        // as a typed order does.
        FieldType::Link => FieldDeclaration::link(LinkKey::new(|raw| {
            match FieldType::Link.read(raw).ok()? {
                TypedValue::Link(key) => Some(key),
                _ => None,
            }
        })),
    }
}

/// Judge a document's tags against the vault's declared tag facet.
///
/// The tags are the ones already on the facts: the facet is a judgment over
/// what the document says rather than a second reading of it, which is what
/// makes the tag rows schema-independent parse facts and these findings the
/// schema-keyed answer about them.
///
/// **One finding per distinct undeclared tag, not one per token.** A document
/// that writes `#draft` in its frontmatter and three more times in its body has
/// one thing wrong with it, and a reader paging the class wants the names.
/// Tags are distinct under the tag fold, so `#Draft` and `#draft` are one
/// finding, which names the spelling the document writes first. Order is the
/// order the tags are first written, so equal documents plan equal writes.
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
        .filter(|tag| seen.insert(fold_tag(&tag.name)))
        .map(|tag| {
            PlannedFinding::of(
                subject.clone(),
                Cause::TagBreach(TagBreach::Undeclared),
                format!("`#{}`, written in the {}", tag.name, source(tag.source)),
                Some(tag.name.clone()),
            )
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
    PlannedFinding::of(
        DocumentPath::rendered(path),
        Cause::Undecodable(quarantine.cause),
        format!("{path:?}: {}", quarantine.problem),
        None,
    )
}

/// The text a file's `bytes` spell as a vault document, or why they spell
/// none: **the one rule by which bytes decode as a document.** Bytes that do
/// not are quarantined — no row is derived for them, so no link resolves to
/// them — wherever the path names a document. A file streamed without its
/// bytes is held to this rule through [`streamed_decodes`].
pub(crate) fn document_source(bytes: &[u8]) -> Result<&str, std::str::Utf8Error> {
    std::str::from_utf8(bytes)
}

/// Whether `bytes` decode as a vault document ([`document_source`]): what a
/// resolved plan records of each file state it carries, so that its
/// resolution change set reads a file as a link's candidate only where the
/// store would derive a document from it.
pub(crate) fn decodes(bytes: &[u8]) -> bool {
    document_source(bytes).is_ok()
}

/// Whether a file read as a stream, its bytes not kept, decodes as a vault
/// document: [`decodes`] for a caller holding only what
/// [`norn_fs::stream_optional_and_hash`] answers of the file.
///
/// **The one rule, in its streamed form.** [`document_source`] is the rule;
/// this is the single place a streamed reading is held to it, so a caller
/// that never holds a file's bytes reads its decodability here rather than
/// from the stream's own UTF-8 verdict. The rule is
/// [`std::str::from_utf8`]'s verdict over the whole buffer, which is the
/// verdict the stream carries, so the form is thin; the test beside the rule
/// holds the two to one answer over a corpus of encodings, and a rule that
/// grew past UTF-8 would fail it until this grew with it.
///
/// **Where it is asked.** A move whose document the plan carries byte for
/// byte records the moved document's decodability from here, planning and
/// the applier alike, without holding its body
/// (`crate::planner::view::Body::Streamed`).
pub(crate) fn streamed_decodes(streamed: &norn_fs::StreamedHash) -> bool {
    streamed.is_utf8()
}

/// Every link the document `bytes` spell holds, in the order its facts
/// list them: its frontmatter's wikilinks, then its body's links, each read
/// as a derivation reads it. Bytes that do not decode hold none, as a
/// document they quarantine derives none.
///
/// **One reading of a document's links.** The resolution change set reads
/// the links a plan's composed documents hold here, so a link it records is
/// the link the store derives once the plan lands.
pub(crate) fn document_links(bytes: &[u8]) -> Vec<LinkFact> {
    let Ok(source) = document_source(bytes) else {
        return Vec::new();
    };
    parsed_links(&parsed(source))
}

/// The links the parsed `document` holds, as [`document_links`] reads them
/// from its bytes: for a caller that parsed the document for another reason
/// and reads its links off that one parse.
pub(crate) fn parsed_links(document: &Document<'_>) -> Vec<LinkFact> {
    links_of(document, &document.scan_body())
}

/// The document `source` spells, as the text layer parses it: the one parse
/// a plan's reading of a document's links, and its respelling of them, go
/// through, counted on a test's thread so a case can hold planning to one
/// parse of a body ([`PARSES`]). The body scans the text layer runs on that
/// document, and the re-scan of the rewritten bytes a respelling verifies
/// itself by, are its own and not counted here.
pub(crate) fn parsed(source: &str) -> Document<'_> {
    #[cfg(test)]
    PARSES.with(|parses| parses.set(parses.get() + 1));
    Document::parse(source)
}

#[cfg(test)]
thread_local! {
    /// How many documents [`parsed`] has parsed on this thread.
    pub(crate) static PARSES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// The links `document`, whose body `scan` read, holds: its frontmatter's
/// wikilinks, then its body's.
fn links_of(document: &Document<'_>, scan: &norn_text::BodyScan<'_>) -> Vec<LinkFact> {
    document
        .frontmatter_wikilinks()
        .into_iter()
        .chain(scan.links())
        .map(map_link)
        .collect()
}

/// The store's fact for one link the text layer parsed. An empty anchor —
/// `note#`, `note#^` — names no place, so the fact carries none; a heading
/// anchor carries the readings the text layer's section resolver matches it
/// by.
fn map_link(link: norn_text::Link) -> LinkFact {
    let anchor = match (link.anchor, link.block_ref) {
        (Some(written), _) => {
            norn_text::anchor_readings(&written).map(|readings| LinkAnchor::Heading {
                written,
                readings: AnchorReadings {
                    text: readings.text,
                    marked: readings.marked,
                },
            })
        }
        (None, Some(id)) => (!id.is_empty()).then_some(LinkAnchor::Block { id }),
        (None, None) => None,
    };
    LinkFact {
        family: match link.family {
            norn_text::LinkFamily::Wikilink => LinkFamily::Wikilink,
            norn_text::LinkFamily::Markdown => LinkFamily::Markdown,
        },
        embed: link.embed,
        protocol: link.protocol,
        target: link.target,
        title: link.title,
        anchor,
        span: link.span.map(span),
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

    /// **The bar on one decode rule for held and streamed bytes.** For every
    /// file in a corpus of encodings — valid characters of every width, an
    /// overlong encoding, an encoded surrogate, a code point past U+10FFFF,
    /// each width of character cut short at the end of the file, characters
    /// valid and invalid split across the stream's 64 KiB chunk boundary, a
    /// byte-order mark and a NUL — the streamed reading's verdict through
    /// [`streamed_decodes`] is what [`decodes`] answers of the whole bytes.
    ///
    /// The forbidden shape is two decode rules: a moved document planned from
    /// a stream as decoding where its derivation would quarantine it, or the
    /// reverse, would record a file state the store never derives.
    #[test]
    #[allow(clippy::disallowed_methods)] // Harness scaffolding: the files streamed.
    fn a_streamed_reading_decodes_exactly_where_the_whole_bytes_do() {
        // The stream's chunk, which norn-fs keeps private.
        const CHUNK: usize = 64 * 1024;
        let boundary = |bytes: &[u8]| {
            let mut file = vec![b'a'; CHUNK - 1];
            file.extend_from_slice(bytes);
            file.extend_from_slice(b" tail\n");
            file
        };
        let mut corpus: Vec<(String, Vec<u8>)> = vec![
            ("empty".into(), Vec::new()),
            ("ascii".into(), b"# Note\n\nplain\n".to_vec()),
            (
                "every width".into(),
                "a \u{e9} \u{20ac} \u{1f600}\n".as_bytes().to_vec(),
            ),
            ("overlong two".into(), b"a\xc0\x80b".to_vec()),
            ("overlong three".into(), b"a\xe0\x80\x80b".to_vec()),
            ("overlong four".into(), b"a\xf0\x80\x80\x80b".to_vec()),
            ("surrogate".into(), b"a\xed\xa0\x80b".to_vec()),
            ("past U+10FFFF".into(), b"a\xf4\x90\x80\x80b".to_vec()),
            ("lead past F4".into(), b"a\xf5\x80\x80\x80b".to_vec()),
            ("stray continuation".into(), b"a\x80b".to_vec()),
            ("bom".into(), b"\xef\xbb\xbf# Note\n".to_vec()),
            ("nul".into(), b"a\0b\n".to_vec()),
        ];
        for character in ["\u{e9}", "\u{20ac}", "\u{1f600}"] {
            let bytes = character.as_bytes();
            for cut in 1..bytes.len() {
                corpus.push((
                    format!("{character:?} cut to {cut} at EOF"),
                    [b"text ".as_slice(), &bytes[..cut]].concat(),
                ));
            }
            corpus.push((
                format!("{character:?} across the chunk boundary"),
                boundary(bytes),
            ));
        }
        for (name, bytes) in [
            ("surrogate across the chunk boundary", &b"\xed\xa0\x80"[..]),
            ("overlong across the chunk boundary", b"\xe0\x80\x80"),
            ("cut character at the chunk boundary", b"\xe2\x82 "),
        ] {
            corpus.push((name.into(), boundary(bytes)));
        }

        let scratch = norn_testkit::scratch::Scratch::new("streamed-decodes");
        let root = scratch.join("vault");
        std::fs::create_dir_all(&root).expect("a vault root");
        let mut verdicts = BTreeSet::new();
        for (at, (name, bytes)) in corpus.iter().enumerate() {
            let relative = format!("{at:02}.md");
            std::fs::write(root.join(&relative), bytes).expect("a file");
            let streamed = norn_fs::stream_optional_and_hash(&root, Path::new(&relative))
                .expect("a streamed read")
                .expect("the file is there");
            assert_eq!(streamed.len(), bytes.len() as u64, "{name}");
            assert_eq!(streamed_decodes(&streamed), decodes(bytes), "{name}");
            verdicts.insert(decodes(bytes));
        }
        assert_eq!(
            verdicts,
            BTreeSet::from([false, true]),
            "the corpus holds both verdicts"
        );
    }

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

    /// **A tag declared twice under the tag fold is one declared-tag facet**,
    /// at the spelling the schema writes first.
    #[test]
    fn a_tag_declared_in_two_spellings_is_one_facet_at_its_first() {
        use norn_wire::{Facet, FacetKind};

        let declared = Declared::pinned(
            VaultSchema::parse(b"version: 1\ntags:\n  declared: [Work, alpha, work, WORK]\n")
                .expect("a schema repeating a tag"),
            "repeating",
        );
        assert_eq!(
            declared
                .content_model()
                .facets_of(FacetKind::DeclaredTag, None)
                .collect::<Vec<Facet>>(),
            vec![Facet::declared_tag("Work"), Facet::declared_tag("alpha")]
        );
    }

    /// **A tag pattern written twice under the tag fold is one tag-pattern
    /// facet**, at the spelling the schema writes first.
    #[test]
    fn a_tag_pattern_written_in_two_spellings_is_one_facet_at_its_first() {
        use norn_wire::{Facet, FacetKind};

        let declared = Declared::pinned(
            VaultSchema::parse(b"version: 1\ntags:\n  patterns: [\"Area/**\", \"area/**\"]\n")
                .expect("a schema repeating a pattern"),
            "repeating",
        );
        assert_eq!(
            declared
                .content_model()
                .facets_of(FacetKind::TagPattern, None)
                .collect::<Vec<Facet>>(),
            vec![Facet::tag_pattern("Area/**")]
        );
    }

    /// **A pinned schema's declaration reports every declaration the schema
    /// makes**, each as the facet `describe` answers with, in the order of the
    /// text that keys it: each field with its type, the tags, the patterns,
    /// the stance and the ambiguity-ignore patterns, and each creation rule and
    /// the inbox with every template as its source text. A vault with no
    /// schema pinned declares nothing, and a schema silent on tags states the
    /// default stance.
    #[test]
    fn a_pinned_declaration_reports_every_declaration_its_schema_makes() {
        use norn_wire::{
            AuthoredValue, Facet, FacetKind, FieldType as Wire, PathRuleKind, ValueMap,
        };

        let declared = Declared::pinned(
            VaultSchema::parse(
                b"version: 1
fields:
  title: {type: text}
  due: {type: date}
  status: {type: text}
tags:
  declared: [project, area]
  patterns: [\"person/**\", \"area/**\"]
  undeclared: report
paths:
  ambiguity_ignore: [\"archive/**\"]
creatable:
  task:
    target: \"tasks/{{var.project}}-{{seq}}.md\"
    variables: [project, title]
    frontmatter_defaults:
      status: todo
      tags: [\"{{var.project|slug}}\", 2]
    body: \"# {{var.title}}\\n\"
  note:
    target: \"notes/{{date}}.md\"
inbox:
  target: \"inbox/{{date}}-{{seq}}.md\"
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
                Facet::declared_field("due", Wire::Date, None),
                Facet::declared_field("status", Wire::Text, None),
                Facet::declared_field("title", Wire::Text, None),
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
            facets(FacetKind::PathRule),
            vec![Facet::path_rule(
                PathRuleKind::AmbiguityIgnore,
                "archive/**"
            )]
        );
        assert_eq!(
            facets(FacetKind::CreationRule),
            vec![
                Facet::creation_rule(
                    "note",
                    "notes/{{date}}.md",
                    Vec::new(),
                    ValueMap::default(),
                    None
                ),
                Facet::creation_rule(
                    "task",
                    "tasks/{{var.project}}-{{seq}}.md",
                    vec!["project".to_string(), "title".to_string()],
                    ValueMap::new([
                        ("status".to_string(), AuthoredValue::string("todo")),
                        (
                            "tags".to_string(),
                            AuthoredValue::list([
                                AuthoredValue::string("{{var.project|slug}}"),
                                AuthoredValue::Integer(2),
                            ])
                        ),
                    ])
                    .expect("each key once"),
                    Some("# {{var.title}}\n".to_string())
                ),
            ]
        );
        assert_eq!(
            facets(FacetKind::Inbox),
            vec![Facet::inbox("inbox/{{date}}-{{seq}}.md")]
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

    /// **A pinned schema's rules and field shapes reach the declaration as
    /// the schema writes them.** Each rule is one facet, in name order, every
    /// part it declares spelled as written — a selector's values as a list
    /// however many were written, a default and a route as their source text,
    /// a forbidden field's fix — and every part it does not left out, its
    /// severity always stated. Each field carries the shape it declares, and
    /// none where it declares none.
    #[test]
    fn a_pinned_declaration_reports_its_rules_and_shapes_as_written() {
        use norn_wire::{
            AuthoredValue, Facet, FacetKind, FieldShape, FieldType as Wire, RuleAllowedPaths,
            RuleClosedSet, RuleExclude, RuleForbiddenFix, RuleMatch, SchemaRule,
        };

        let declared = Declared::pinned(
            VaultSchema::parse(
                b"version: 1
fields:
  status: {type: text, shape: single}
  aliases: {type: text, shape: list}
  due: {type: date}
rules:
  tasks:
    description: what a task holds
    severity: error
    match:
      frontmatter: {type: task}
      path: \"projects/<project>/**\"
    exclude:
      path: [\"projects/archive/**\"]
    required:
      status: {default: todo}
      project: {default: \"{{path.project}}\"}
      due:
    forbidden:
      assignee: {rename_to: owner}
      legacy: remove
      draft:
    one_of:
      status: {values: [todo, done], synonyms: {complete: done}}
    max_length:
      title: 80
    allowed_paths:
      paths: [\"projects/**\"]
      route: \"projects/{{path.project}}/\"
  areas:
    match: {path: \"areas/**\"}
",
            )
            .expect("a schema declaring rules and shapes"),
            "rules-and-shapes",
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
                Facet::declared_field("aliases", Wire::Text, Some(FieldShape::List)),
                Facet::declared_field("due", Wire::Date, None),
                Facet::declared_field("status", Wire::Text, Some(FieldShape::Single)),
            ]
        );
        assert_eq!(
            facets(FacetKind::Rule),
            vec![
                Facet::rule(
                    SchemaRule::new("areas", Severity::Warning)
                        .with_match(RuleMatch::new([], Some("areas/**".to_string())))
                ),
                Facet::rule(
                    SchemaRule::new("tasks", Severity::Error)
                        .with_description("what a task holds")
                        .with_match(RuleMatch::new(
                            [("type".to_string(), vec!["task".to_string()])],
                            Some("projects/<project>/**".to_string()),
                        ))
                        .with_exclude(RuleExclude::new(["projects/archive/**".to_string()]))
                        .with_required("status", Some(AuthoredValue::string("todo")))
                        .with_required("project", Some(AuthoredValue::string("{{path.project}}")))
                        .with_required("due", None)
                        .with_forbidden(
                            "assignee",
                            Some(RuleForbiddenFix::RenameTo("owner".to_string()))
                        )
                        .with_forbidden("legacy", Some(RuleForbiddenFix::Remove))
                        .with_forbidden("draft", None)
                        .with_one_of(
                            "status",
                            RuleClosedSet::new(
                                ["todo".to_string(), "done".to_string()],
                                [("complete".to_string(), "done".to_string())],
                            )
                        )
                        .with_max_length("title", 80)
                        .with_allowed_paths(RuleAllowedPaths::new(
                            ["projects/**".to_string()],
                            Some("projects/{{path.project}}/".to_string()),
                        ))
                ),
            ]
        );
        assert!(declared.content_model().declares_rule("tasks"));
        assert!(!declared.content_model().declares_rule("task"));
    }

    /// **Every field type is declared as the wire type spelled as it is, and
    /// carries a typed order exactly where it does not order as text.** One
    /// field of each type, each reported with its own type: `number`,
    /// `boolean` and `date` read a raw value into a typed sort key, and
    /// `text`, `tags` and `link` are ordered by their raw text.
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

    /// **A field declared `link` hands the store the schema's own key for a
    /// link**: the alias dropped and nothing else, and none for a text that is
    /// no link. The store reads no link syntax, so the closure the declaration
    /// carries is the one reading, and a tag key beside it still folds as a tag.
    #[test]
    fn a_link_field_is_declared_with_the_schemas_key_for_a_link() {
        let declared = Declared::pinned(
            VaultSchema::parse(
                b"version: 1\nfields:\n  project: { type: link }\n  labels: { type: tags }\n",
            )
            .expect("a schema declaring a link"),
            "link-declared",
        );
        let model = declared.content_model();
        assert_eq!(
            model.fold("project", "[[alpha#Plan|The plan]]"),
            Some("[[alpha#Plan]]".to_string())
        );
        assert_eq!(model.fold("project", "alpha"), None);
        assert_eq!(model.fold("labels", "#Work"), Some("work".to_string()));
        assert_eq!(model.fold("undeclared", "[[alpha]]"), None);
        assert!(
            model.typed_order("project").is_none(),
            "a link orders as text"
        );
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
                Cause::UnreadBlock(_) | Cause::TagBreach(_) | Cause::RuleBreach(_) => {
                    FindingScope::Document
                }
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

            let unheld = plan_document(
                path,
                spelling,
                bytes,
                hash(),
                None,
                &undeclaring(),
                CaseFold::Exact,
            );
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
            let held = plan_document(
                path,
                spelling,
                bytes,
                hash(),
                Some(&stored),
                &undeclaring(),
                CaseFold::Exact,
            );
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
                CaseFold::Exact,
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

    /// **A block nothing read is judged against no rule.** Its fields are
    /// unknown rather than absent, so a rule requiring a field it may hold
    /// would report it missing from a block that holds it: the unread
    /// block's own finding is the one the plan carries. The same rule
    /// judges a block that reads.
    #[test]
    fn an_unread_block_is_judged_against_no_rule() {
        let requiring = Declared::pinned(
            VaultSchema::parse(b"version: 1\nrules:\n  owned: { required: { owner: } }\n")
                .expect("a schema stating a selectorless rule"),
            "requiring",
        );
        let plan = |source: &str| {
            let bytes = source.as_bytes();
            plan_document(
                Path::new("note.md"),
                "note.md",
                bytes,
                norn_fs::ContentHash::of(bytes).to_string(),
                None,
                &requiring,
                CaseFold::Exact,
            )
        };
        let unread = plan("---\ntitle: never closes\n# heading\n");
        assert_eq!(
            unread
                .findings
                .iter()
                .map(|finding| finding.cause)
                .collect::<Vec<_>>(),
            [Cause::UnreadBlock(UnreadBlock::Unclosed)]
        );
        assert_eq!(
            unread.rule_work,
            RuleWork::default(),
            "the rules were asked"
        );
        let read = plan("---\ntitle: closes\n---\n# heading\n");
        assert_eq!(
            read.findings
                .iter()
                .map(|finding| finding.cause)
                .collect::<Vec<_>>(),
            [Cause::RuleBreach(Breach::RequiredMissing)]
        );
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
            CaseFold::Exact,
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
                &undeclaring(),
                CaseFold::Exact
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
            CaseFold::Exact,
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
                &undeclaring(),
                CaseFold::Exact
            ),
            "the row the observation replaces changed a plan that still derives"
        );
    }

    /// **What a side re-derives is the kinds of the causes that side decides.**
    /// An act that opened nothing concludes what the grammar says about the
    /// names it read, so filing one of those verdicts replaces the path kinds
    /// and nothing else. An act that opened a document's bytes concludes what
    /// those bytes say — the body verdict, every unread block, the tag facet
    /// and every rule breach — and says nothing about the other spellings
    /// rendering onto the same place.
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
                "document/misplaced",
                "document/rules-conflict",
                "document/undeclared-tag",
                "field/forbidden",
                "field/not-one-of",
                "field/required-missing",
                "field/rules-conflict",
                "field/shape-mismatch",
                "field/too-long",
                "field/type-mismatch"
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
                    CaseFold::Exact,
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

    /// **Every heading and every heading anchor carries the readings the
    /// text layer's section resolver matches them by**, so a store's predicate
    /// over the stored readings and a get's section agree. Each reading is its
    /// own field, and each is checked against a value no other field holds. A
    /// Markdown fragment arrives decoded, and a link with no anchor, or an
    /// empty one, carries none.
    #[test]
    fn headings_and_anchors_carry_the_section_resolvers_readings() {
        let source = b"# Design  Notes

[[t#Top#Design  NOTES]] [x](t.md#My%20Top#Sub) [[t#]] [[t#^b]] [[t#^]] [[t]]
";
        let facts = map_document(
            "note.md",
            source,
            norn_fs::ContentHash::of(source).to_string(),
            &ContentModel::none(),
        )
        .unwrap()
        .facts;
        let readings: Vec<String> = facts
            .headings
            .iter()
            .map(|heading| heading.reading.clone())
            .collect();
        assert_eq!(readings, ["design notes"]);
        let anchors: Vec<Option<LinkAnchor>> =
            facts.links.iter().map(|link| link.anchor.clone()).collect();
        let heading = |written: &str, text: &str, marked: &str| {
            Some(LinkAnchor::Heading {
                written: written.to_string(),
                readings: AnchorReadings {
                    text: text.to_string(),
                    marked: Some(marked.to_string()),
                },
            })
        };
        assert_eq!(
            anchors,
            [
                heading("Top#Design  NOTES", "top#design notes", "design notes"),
                heading("My Top#Sub", "my top#sub", "sub"),
                None,
                Some(LinkAnchor::Block {
                    id: "b".to_string()
                }),
                None,
                None,
            ]
        );
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
            CaseFold::Exact,
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
        // Frontmatter tags come before body tags on the row, as they do in
        // the file, and a repeated name is one finding.
        assert_eq!(targets, vec![Some("ephemeral"), Some("draft")]);
        for finding in &plan.findings {
            assert_eq!(finding.cause, Cause::TagBreach(TagBreach::Undeclared));
            assert_eq!(finding.cause.severity(), Severity::Warning);
            assert_eq!(
                finding.subject,
                DocumentPath::new("note.md").expect("a document path")
            );
        }
        assert_eq!(
            plan.findings[0].detail,
            "`#ephemeral`, written in the frontmatter"
        );
        assert_eq!(plan.findings[1].detail, "`#draft`, written in the body");
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
            CaseFold::Exact,
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
            CaseFold::Exact,
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

    /// A facet or rule breach leaves the document whole, so it is reported as
    /// a warning — a rule breach at the floor its rules may raise — rather than
    /// as the error a derivation defect is.
    #[test]
    fn the_two_finding_families_carry_the_severity_their_effect_on_derived_state_has() {
        for cause in CAUSES {
            let expected = match cause {
                Cause::Undecodable(_) | Cause::UnreadBlock(_) => Severity::Error,
                Cause::TagBreach(_) | Cause::RuleBreach(_) => Severity::Warning,
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
                        CaseFold::Exact,
                    )
                    .change
                    .expect("a document that derives")
                })
                .collect();
            store
                .begin_request()
                .apply_increment(
                    norn_store::IncrementProvenance::Derived,
                    changes,
                    &[],
                    declared.content_model(),
                )
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

        /// The find a snapshot over this vault answers to `params`, or the
        /// refusal it answers instead.
        fn try_find(
            &self,
            params: norn_wire::FindParams,
        ) -> Result<norn_store::Found, norn_store::PageRefusal> {
            self.reader
                .try_take()
                .expect("an idle reader")
                .establish()
                .expect("a snapshot")
                .find(&params, self.declared.content_model())
        }

        fn found(&self, params: norn_wire::FindParams) -> norn_store::Found {
            self.try_find(params).expect("a find")
        }

        fn find(&self, params: norn_wire::FindParams) -> Vec<String> {
            self.found(params)
                .rows
                .into_iter()
                .map(|row| row.path.as_str().to_string())
                .collect()
        }

        /// The refusal a find over `params` answers, where the request is
        /// expected to be refused rather than answered.
        fn find_refusal(&self, params: norn_wire::FindParams) -> norn_store::PageRefusal {
            self.try_find(params).expect_err("a find refused")
        }
    }

    /// Hold that find's equality, one-value membership and inequality on each
    /// key and value of `parts` answer what a rule's selector on that value
    /// ([`VaultSchema::selects`], the reference) answers over `documents`,
    /// derived under `fields`: equality and membership find the documents
    /// the selector selects, and inequality every other document.
    fn assert_find_equality_reads_as_the_selectors_do(
        label: &str,
        fields: &str,
        documents: &[(&str, String)],
        parts: &[(&str, &str)],
    ) {
        use norn_wire::Predicate;

        let vault = DerivedVault::new(label, fields.as_bytes(), documents);
        let find = |part: Predicate| vault.find(DerivedVault::request().with_predicates([part]));
        let every: Vec<&str> = {
            let mut paths: Vec<&str> = documents.iter().map(|(path, _)| *path).collect();
            paths.sort_unstable();
            paths
        };

        for (key, value) in parts.iter().copied() {
            let schema = VaultSchema::parse(
                format!("{fields}rules:\n  probe: {{ match: {{ frontmatter: {{ {key}: '{value}' }} }} }}\n")
                    .as_bytes(),
            )
            .expect("a schema selecting on the value");
            let rule = schema.rule("probe").expect("the probe rule");
            let selected: Vec<&str> = every
                .iter()
                .copied()
                .filter(|path| {
                    let (_, bytes) = documents
                        .iter()
                        .find(|(at, _)| at == path)
                        .expect("a corpus document");
                    let document = Document::parse(bytes);
                    let frontmatter = document.frontmatter().map(map_value);
                    schema.selects(
                        rule,
                        path,
                        &authored_fields(frontmatter.as_ref()),
                        CaseFold::Exact,
                    )
                })
                .collect();
            let unselected: Vec<&str> = every
                .iter()
                .copied()
                .filter(|path| !selected.contains(path))
                .collect();
            assert_eq!(
                find(Predicate::equal_to(key, value)),
                selected,
                "`{key}` equal to `{value}`"
            );
            assert_eq!(
                find(Predicate::in_any(key, [value.to_string()])),
                selected,
                "`{key}` in [`{value}`]"
            );
            assert_eq!(
                find(Predicate::not_equal_to(key, value)),
                unselected,
                "`{key}` not equal to `{value}`"
            );
        }
    }

    /// **Find's field equality reads a value as a rule's selector reads it**:
    /// a value not of its key's declared shape reads as no value — a list
    /// under a key declared single, one value under a key declared a list, a
    /// map anywhere — and a tag key, the `tags` carrier declared or not or a
    /// key declared `tags`, compares under the tag fold with its `#` marker
    /// optional, while any other key compares exactly as written.
    #[test]
    fn find_equality_reads_a_value_as_the_rule_selectors_do() {
        let documents: Vec<(&str, String)> = vec![
            (
                "single.md",
                "---\nkind: a\nitems: [x]\nlabels: [Work]\ntags: [Work, '#Play']\ncode: A\n---\n"
                    .to_string(),
            ),
            (
                "listed.md",
                "---\nkind: [a, b]\nitems: x\nlabels: '#work'\ntags: play\ncode: a\n---\n"
                    .to_string(),
            ),
            (
                "nulls.md",
                "---\nkind: null\nitems: []\ntags: [2024]\n---\n".to_string(),
            ),
            ("mapped.md", "---\nkind: { a: 1 }\n---\n".to_string()),
            ("none.md", "# none\n".to_string()),
        ];
        assert_find_equality_reads_as_the_selectors_do(
            "norn-host-equality-reading",
            "version: 1
fields:
  kind: { type: text, shape: single }
  items: { type: text, shape: list }
  labels: { type: tags }
  code: { type: text }
",
            &documents,
            &[
                ("kind", "a"),
                ("items", "x"),
                ("labels", "work"),
                ("labels", "#WORK"),
                ("tags", "work"),
                ("tags", "#play"),
                ("tags", "PLAY"),
                ("tags", "2024"),
                ("code", "a"),
                ("code", "A"),
            ],
        );
    }

    /// **The `tags` carrier declared with a typed order compares under the
    /// tag fold, and only where its value reads as that type**, as its
    /// equality key does: under `type: number`, `5` and `5.0` are one tag and
    /// `05` another, since the fold compares the tag a value names rather
    /// than its number, and `#5` — marked, so no number — and `abc` equal
    /// nothing.
    #[test]
    fn find_equality_on_a_typed_tag_carrier_reads_as_the_selectors_do() {
        let documents: Vec<(&str, String)> = vec![
            ("marked.md", "---\ntags: ['#5']\n---\n".to_string()),
            ("five.md", "---\ntags: [5]\n---\n".to_string()),
            ("padded.md", "---\ntags: ['05']\n---\n".to_string()),
            ("float.md", "---\ntags: [5.0]\n---\n".to_string()),
            ("word.md", "---\ntags: [abc]\n---\n".to_string()),
            ("marked-one.md", "---\ntags: '#1'\n---\n".to_string()),
            ("one.md", "---\ntags: 1\n---\n".to_string()),
            ("none.md", "# none\n".to_string()),
        ];
        assert_find_equality_reads_as_the_selectors_do(
            "norn-host-typed-tag-carrier",
            "version: 1\nfields:\n  tags: { type: number }\n",
            &documents,
            &[("tags", "5"), ("tags", "05"), ("tags", "1")],
        );
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

    /// **A find is advised of a mixed-offset comparison exactly where the
    /// in-process comparison signals one.** Over the same raw text the rule
    /// reads, a sort on each typed key is advised where some pair of the key's
    /// values signals, and a bound, an equality, an inequality and a
    /// membership part are advised where the value they name signals against
    /// some value the key holds — whether or not the documents that value
    /// decided reach the page. A number and a boolean never signal and are
    /// never advised; the dates mix a stated offset with an unstated one, so
    /// a date named with either spelling is advised, and so is the order.
    #[test]
    fn a_find_is_advised_exactly_where_the_in_process_comparison_signals() {
        use norn_config::schema::{ComparisonSignal, FieldType, TypedValue};
        use norn_wire::{AnswerAdvisory, ComparedBy, Direction, Predicate, Sort, SortKey};

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
            "norn-host-comparison-signal",
            b"version: 1\nfields:\n  when:\n    type: date\n  weight:\n    type: number\n  code:\n    type: text\n  flag:\n    type: boolean\n",
            &documents,
        );
        let unquoted = |raw: &str| raw.trim_matches('"').to_string();
        let typed = |kind: FieldType, raw: &str| {
            kind.read(&unquoted(raw))
                .unwrap_or_else(|_| panic!("`{raw}` reads as {kind}"))
        };
        let signals = |left: &TypedValue, right: &TypedValue| {
            left.compare(right).signal == Some(ComparisonSignal::MixedOffset)
        };

        let typed_keys: [(&str, FieldType, Written); 3] = [
            ("when", FieldType::Date, |row| row.1),
            ("weight", FieldType::Number, |row| row.2),
            ("flag", FieldType::Boolean, |row| row.4),
        ];
        for (key, kind, written) in typed_keys {
            let held: Vec<TypedValue> = COMPARED
                .iter()
                .map(|row| typed(kind, written(row)))
                .collect();
            let order_signals = held
                .iter()
                .any(|left| held.iter().any(|right| signals(left, right)));
            let sorted = vault.found(
                DerivedVault::request()
                    .with_sort(Sort::new(SortKey::field(key), Direction::Ascending)),
            );
            assert_eq!(
                sorted.advisories,
                if order_signals {
                    vec![AnswerAdvisory::mixed_offset(key, ComparedBy::Sort)]
                } else {
                    Vec::new()
                },
                "`{key}` sorted"
            );
            for row in &COMPARED {
                let raw = unquoted(written(row));
                let named = typed(kind, &raw);
                let part_signals = held.iter().any(|value| signals(value, &named));
                for part in [
                    Predicate::after(key, raw.clone()),
                    Predicate::before(key, raw.clone()),
                    Predicate::equal_to(key, raw.clone()),
                    Predicate::not_equal_to(key, raw.clone()),
                    Predicate::in_any(key, [raw.clone()]),
                ] {
                    assert_eq!(
                        vault
                            .found(DerivedVault::request().with_predicates([part.clone()]))
                            .advisories,
                        if part_signals {
                            vec![AnswerAdvisory::mixed_offset(key, ComparedBy::Predicate)]
                        } else {
                            Vec::new()
                        },
                        "{part:?}"
                    );
                }
            }
            // The dates are the key the rule signals over, both ways.
            assert_eq!(order_signals, kind == FieldType::Date, "`{key}`");
        }
    }

    /// **Matching is symmetric in the shape of the stored value.** A value
    /// written bare, inside a flow sequence and inside a block sequence are
    /// one value to an equality, an inequality, a range bound and a
    /// membership part, under a declared number and under text alike: every
    /// scalar a key holds is a value of the key wherever it stands among its
    /// siblings, so `[9]` holds nine as `9` does, `[3, 9.0]` and a block list
    /// holding nine hold it too, and a bound met by a value stored second in
    /// a sequence meets the part exactly as one met by its first.
    #[test]
    fn a_value_written_bare_and_inside_a_sequence_meet_one_part_alike() {
        use norn_wire::Predicate;

        let documents: Vec<(&str, String)> = vec![
            (
                "bare.md",
                "---\nweight: 9\ncode: a\n---\nbody\n".to_string(),
            ),
            (
                "bracketed.md",
                "---\nweight: [9]\ncode: [a]\n---\nbody\n".to_string(),
            ),
            (
                "among.md",
                "---\nweight: [3, 9.0]\ncode: [b, a]\n---\nbody\n".to_string(),
            ),
            (
                "block.md",
                "---\nweight:\n  - 9\n  - 3\ncode:\n  - b\n  - a\n---\nbody\n".to_string(),
            ),
            (
                "other.md",
                "---\nweight: 3\ncode: b\n---\nbody\n".to_string(),
            ),
        ];
        let vault = DerivedVault::new(
            "norn-host-stored-shape",
            b"version: 1\nfields:\n  weight:\n    type: number\n  code:\n    type: text\n",
            &documents,
        );
        let find = |part: Predicate| vault.find(DerivedVault::request().with_predicates([part]));

        let holds_the_value = ["among.md", "bare.md", "block.md", "bracketed.md"];
        for (key, value) in [("weight", "9"), ("code", "a")] {
            assert_eq!(
                find(Predicate::equal_to(key, value)),
                holds_the_value,
                "`{key}` equal to {value}"
            );
            assert_eq!(
                find(Predicate::in_any(key, [value.to_string()])),
                holds_the_value,
                "`{key}` in [{value}]"
            );
            assert_eq!(
                find(Predicate::not_equal_to(key, value)),
                ["other.md"],
                "`{key}` not equal to {value}"
            );
        }

        // `weight` holds nine second in `among.md` (`[3, 9.0]`) and `code`
        // holds `a` second in `among.md` and `block.md` (`[b, a]` and its
        // block-list twin, `b` then `a`): a range bound met only by that
        // second value still meets the document.
        assert_eq!(
            find(Predicate::after("weight", "5")),
            holds_the_value,
            "`weight` after 5"
        );
        assert_eq!(
            find(Predicate::before("code", "ab")),
            holds_the_value,
            "`code` before ab"
        );
    }

    /// **A predicate's value is literal text, read as the field's declared
    /// type.** Brackets spelled inside a value's own text are that value's
    /// literal text and nothing else: an equality or a one-value membership
    /// asking for `[draft]` meets a document whose stored value is the quoted
    /// YAML text `[draft]`, and meets no document holding the bare word
    /// `draft`. On a field declared a number, a bracketed spelling such as
    /// `[9]` names no number and is refused as unreadable rather than read as
    /// a list.
    #[test]
    fn bracket_text_in_a_request_value_is_literal_and_read_as_its_declared_type() {
        use norn_wire::Predicate;

        let documents: Vec<(&str, &str)> = vec![
            ("quoted.md", "code: \"[draft]\"\n"),
            ("draft.md", "code: draft\n"),
        ];
        let documents: Vec<(&str, String)> = documents
            .into_iter()
            .map(|(path, frontmatter)| (path, format!("---\n{frontmatter}---\nbody\n")))
            .collect();
        let vault = DerivedVault::new(
            "norn-host-bracketed-request-value",
            b"version: 1\nfields:\n  weight:\n    type: number\n  code:\n    type: text\n",
            &documents,
        );
        let find = |part: Predicate| vault.find(DerivedVault::request().with_predicates([part]));

        assert_eq!(
            find(Predicate::equal_to("code", "[draft]")),
            ["quoted.md"],
            "`code` equal to the literal text [draft]"
        );
        assert_eq!(
            find(Predicate::in_any("code", ["[draft]".to_string()])),
            ["quoted.md"],
            "`code` in the literal text [draft]"
        );

        for part in [
            Predicate::equal_to("weight", "[9]"),
            Predicate::in_any("weight", ["[9]".to_string()]),
        ] {
            assert_eq!(
                vault.find_refusal(DerivedVault::request().with_predicates([part.clone()])),
                norn_store::PageRefusal::UnreadableBound {
                    key: "weight".to_string(),
                    value: "[9]".to_string(),
                },
                "{part:?}"
            );
        }
    }
}
