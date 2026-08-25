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
//! **The end-state input set is wider than this one.** A lane-1 projection
//! keyed on the vault schema derives under the pinned schema, so the planner
//! takes that pin beside the document. The vault schema's content model — the
//! layer the `#tag` facet graduates into — adds the parameter when it lands.
//! Until then a plan is a function of the observation alone.
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

use std::path::Path;

use norn_store::{
    BlockFact, Change, DiscardScope, DocumentFacts, DocumentPath, FrontmatterValue, HeadingFact,
    LinkFact, LinkFamily, Provenance, Span, TagFact, TagSource,
};
use norn_text::{BlockRefusal, Document, SourceSpan, Value};
use norn_wire::{FindingKind, FindingScope};

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
}

impl Cause {
    /// The finding kind this cause is recorded under.
    pub(crate) const fn kind(self) -> FindingKind {
        match self {
            Cause::Undecodable(cause) => cause.kind(),
            Cause::UnreadBlock(cause) => cause.kind(),
        }
    }

    /// What an act has to read to conclude this cause.
    pub(crate) const fn decided(self) -> Decided {
        match self {
            Cause::Undecodable(cause) => cause.decided(),
            // A block is read out of the document's own bytes, so concluding
            // that nothing read it means having opened them.
            Cause::UnreadBlock(_) => Decided::ByBytes,
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
const CAUSES: [Cause; 6] = [
    Cause::Undecodable(Undecodable::PathBytes),
    Cause::Undecodable(Undecodable::PathSpelling),
    Cause::Undecodable(Undecodable::BodyBytes),
    Cause::UnreadBlock(UnreadBlock::Unclosed),
    Cause::UnreadBlock(UnreadBlock::Unreadable),
    Cause::UnreadBlock(UnreadBlock::TooLarge),
];

/// The finding kinds no cause above carries.
///
/// Quarantine and the unread block are the only producers recording findings
/// today, so the list is empty. A kind minted for another producer — an
/// ambiguity a resolution reads, a field a schema refuses — is named here, which
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

pub(crate) fn map_document(path: &str, bytes: &[u8], hash: String) -> Result<Derived, Quarantine> {
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
    let mut facts = DocumentFacts::new(document_path, hash, document.body(), bytes.len() as u64);
    facts.body_offset = document.body_start() as u64;
    facts.frontmatter = document.frontmatter().map(map_value);
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
    subject: DocumentPath,
    cause: Cause,
    detail: String,
}

impl PlannedFinding {
    /// Splits the finding into its subject, its cause, and its formatted detail.
    pub(crate) fn into_parts(self) -> (DocumentPath, Cause, String) {
        (self.subject, self.cause, self.detail)
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
    pub(crate) finding: Option<PlannedFinding>,
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
pub(crate) fn plan_document(
    path: &Path,
    spelling: &str,
    bytes: &[u8],
    hash: String,
    stored: Option<&DocumentPath>,
) -> Plan {
    match map_document(spelling, bytes, hash) {
        Ok(derived) => {
            let subject = derived.facts.path.clone();
            Plan {
                change: Some(Change::Upsert(derived.facts)),
                finding: derived.unread_frontmatter.map(|unread| {
                    let detail = match unread.problem {
                        Some(problem) => format!("{path:?}: {problem}"),
                        None => format!("{path:?}"),
                    };
                    PlannedFinding {
                        subject,
                        cause: Cause::UnreadBlock(unread.cause),
                        detail,
                    }
                }),
            }
        }
        Err(quarantine) => Plan {
            change: stored.map(|row| Change::Death {
                path: row.clone(),
                provenance: Provenance::Quarantine,
            }),
            finding: Some(plan_quarantine(path, quarantine)),
        },
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
                Cause::UnreadBlock(_) => FindingScope::Document,
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

            let unheld = plan_document(path, spelling, bytes, hash(), None);
            let finding = unheld
                .finding
                .expect("a refused document states why it contributes no facts");
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
            let held = plan_document(path, spelling, bytes, hash(), Some(&stored));
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
            let problem = map_document("note.md", bytes, hash())
                .expect("a document whose block went unread still derives")
                .unread_frontmatter
                .expect("the block was read by nothing")
                .problem;
            assert_eq!(
                problem.is_some(),
                states_a_problem,
                "{cause:?} accounts for its refusal another way"
            );

            let plan = plan_document(Path::new("note.md"), "note.md", bytes, hash(), None);
            assert!(
                matches!(plan.change, Some(Change::Upsert(_))),
                "{cause:?} cost the document the row it derives"
            );
            let finding = plan.finding.expect("the unknown fields are stated");
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
        let derived = map_document("note.md", whole, hash.clone()).expect("a document derives");
        let plan = plan_document(
            Path::new("note.md"),
            "note.md",
            whole,
            hash.clone(),
            Some(&stored),
        );
        assert_eq!(
            plan.change,
            Some(Change::Upsert(derived.facts)),
            "the upsert carries something other than the facts the act derived"
        );
        assert_eq!(
            plan.finding, None,
            "a document that derives whole states a cause"
        );
        assert_eq!(
            plan,
            plan_document(Path::new("note.md"), "note.md", whole, hash, None),
            "the row the observation replaces changed a plan that still derives"
        );

        let unread = b"---\ntitle: note\n# Heading\n".as_slice();
        let hash = norn_fs::ContentHash::of(unread).to_string();
        let derived = map_document("note.md", unread, hash.clone()).expect("a document derives");
        let plan = plan_document(
            Path::new("note.md"),
            "note.md",
            unread,
            hash.clone(),
            Some(&stored),
        );
        assert!(
            matches!(plan.change, Some(Change::Upsert(_))),
            "a block nothing read cost the document its row"
        );
        assert_eq!(
            plan.finding
                .as_ref()
                .expect("the unknown fields are stated")
                .subject,
            derived.facts.path,
            "a derived document's finding stands at another identity"
        );
        assert_eq!(
            plan,
            plan_document(Path::new("note.md"), "note.md", unread, hash, None),
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
                "document/frontmatter-unreadable"
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
            )
            .expect("a document whose block went unread still derives")
        };
        let plain = derive("---\nk: 1\nk: 2\n---\n# heading\n");
        let tagged = derive("---\n!x k: 1\nk: 2\n---\n# heading\n");
        for (spelling, derived) in [("plain", &plain), ("tagged", &tagged)] {
            assert!(
                derived.facts.frontmatter.is_none(),
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
            )
            .expect("a document whose block went unread still derives")
        };

        let refused = (1..=norn_store::MAX_FRONTMATTER_DEPTH)
            .find(|depth| derive(&block_nesting(*depth)).unread_frontmatter.is_some())
            .expect("the text layer reads every block the store's bound admits");
        let deepest = derive(&block_nesting(refused - 1));
        let projection = deepest
            .facts
            .frontmatter
            .as_ref()
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
        )
        .unwrap();
        let facts = derived.facts;
        assert!(derived.unread_frontmatter.is_none());
        assert!(facts.frontmatter.is_some());
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
            )
            .unwrap()
            .facts;
            let read = String::from_utf8(source).unwrap();
            assert_eq!(
                facts.frontmatter.is_some(),
                projection,
                "the projection of `{read}` is not what the block is"
            );
            assert_eq!(
                facts.frontmatter_diagnostic_count, notes,
                "`{read}` raised another count of block-scoped notes"
            );
        }
    }
}
