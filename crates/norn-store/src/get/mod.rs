//! The get builder: one document, named by a resolution target, answered on a
//! read snapshot as its record, as the section or the block its target's
//! anchor names, or as one page of one of its nested collections.
//!
//! The builder is inherent methods on [`Snapshot`], as the find, count and
//! validate builders are, so every statement it runs reads the one instant
//! the snapshot was established at and is counted on the snapshot's own
//! statement counter. **A get's work follows the target's class, the one
//! document it answers and the page it reads**: resolving the target reads
//! the class its address opens — ranges of a suffix key index whose head
//! sorts every document the ranges reach — so it costs the class, which grows
//! with the vault only where the vault adds documents the target names.
//! Everything after it is read by the document's row id or its path.
//! [`GetStatement`] names what each statement reads.
//!
//! # A target names one document, or the get refuses
//!
//! The target's address is compiled by the one resolver,
//! [`crate::TargetClass`], under the snapshot's path order and the
//! declaration's ambiguity-ignore set, with the anchor split off first — the
//! class a find's `resolves` part reads on the same root. Exactly one document
//! in it answers. More than one refuses
//! ([`PageRefusal::AmbiguousTarget`]) with the head of the class in the
//! resolution ladder's order, at most [`crate::CANDIDATE_HEAD`] of it, how many
//! there were, and the hint naming the target whose `find` resolves the whole
//! class. None refuses ([`PageRefusal::UnknownTarget`]). On a root that folds
//! ASCII case every folded spelling is one class, so an exact-case spelling
//! takes no precedence there.
//!
//! Each candidate in a head is named by its **minimal disambiguating
//! suffix**: the first of its path's suffix spellings
//! ([`crate::DocumentPath::suffix_spellings`]) whose class holds that
//! candidate alone, every spelling tried by one statement whose each range
//! stops at its second member; a candidate no suffix names alone is named by
//! its path. A target is resolved as every read resolves one — a links-to
//! part's, and a page's links — so the statements it runs are
//! [`crate::FindStatement`]s.
//!
//! # A record is a find's row
//!
//! A target with no anchor answers the document's row, projected onto the
//! columns the request names — or onto every column a find projects, where it
//! names none — through the hydration a find's rows are read through
//! ([`Snapshot::find`]): each column costs what it costs on a find's row, a
//! nested collection is cut at [`crate::NESTED_ROW_CEILING`] with its true
//! total beside it, and a body at [`crate::BODY_ROW_CEILING`] bytes. A
//! projected key outside the field universe is reported as a find reports it.
//!
//! # A section and a block are read through the document's text
//!
//! The store parses no document. What a heading anchor names, and what bytes
//! a block definition's block holds, are answered by the document reader the
//! caller hands a get ([`DocumentText`]) — `norn-text`'s one section resolver
//! and its one block reading — over the rows the store holds of the one
//! document: its headings, read in document order by one range seek, and its
//! body, read by row id. **Matching a heading anchor costs the document's
//! headings**: the anchor's text reading compares text with case and
//! whitespace folded, which no index the store holds orders by, so the lookup
//! is a seek of the document's heading rows and a pass over them, bounded by
//! the one document rather than by an index of its own. That pass is the one
//! in-memory match a get runs over more rows than it answers, and
//! [`GetWork::anchor_headings`] counts the rows it is handed: at most the
//! named document's headings, each once. A block definition is
//! one seek of the document's definitions that stops at the first matching
//! one. A section or a block the document does not carry is answered in band
//! ([`Unsatisfied::MissingSection`], [`Unsatisfied::MissingBlock`]) beside the
//! record of the document's path alone, never the whole document.
//!
//! # A collection page is a keyset page by ordinal
//!
//! One nested collection — links, headings, block definitions or tags — is
//! paged in document order by its ordinal, with the collection and the
//! ordinal the page stopped at as its cursor
//! ([`norn_wire::CursorKey::Ordinal`]). Each collection is its own row type,
//! so a cursor minted paging one refuses on another
//! ([`PageRefusal::CursorNotTaken`], naming the two). An ordinal cursor names its
//! collection and a position in it, and no document, so it continues the same
//! collection of any document from that position, and one past the last row
//! answers an empty page. The findings standing over the document are paged
//! in `(kind, id)` order at its path, the order a find's findings column and a
//! validate read them in at one path, with a finding's cursor
//! ([`norn_wire::CursorKey::Finding`]). That cursor names the path it was
//! minted at, so at another document's path it names no position
//! ([`PageRefusal::CursorNotTaken`], a finding's cursor on findings paged),
//! and an ordinal cursor naming the findings names none either, since the
//! findings are paged by a finding's cursor. Either reads one row past its bound to learn a next page
//! exists, through the keyset page every read builder reads
//! ([`Snapshot::read_page`]).
//!
//! A page of links resolves the links it holds as a find's links column
//! resolves a page's: what each link names now, and the health that gives it,
//! read as one set in a fixed number of statements, however many links the
//! page holds.

mod statement;

use std::ops::Range;

use norn_db::EmittedPlan;
use norn_db::rusqlite::Row;
use norn_wire::{
    Anchor, AnswerShape, BodyText, CollectionPage, CollectionSelector, Cursor, CursorKey,
    DocumentRow, FindingKind, GetParams, GetReport, Hint, Page, PagedRows, RequestPart,
    ResolutionTarget, Unsatisfied,
};

use crate::error::{self, StoreError};
use crate::facts::{BlockFact, HeadingFact};
use crate::fields::ContentModel;
use crate::find::{
    FindStatement, FindWork, FoundKey, Nested, Projection, block_row, bounded_body, heading_row,
    identified_link, tag_row, wire_block, wire_heading,
};
use crate::read::{
    Lookups, Naming, PageRefusal, ReadFilter, ReadStatement, Stepped, TargetAmbiguity,
    finding_base, page_limit, wire_path,
};
use crate::request::{Reading, stored_block, stored_heading, unreadable};
use crate::store::Snapshot;

pub use statement::{GET_STATEMENTS, GetStatement};
use statement::{Spelled, compose};

/// What a get reads a document's text through: the one section resolver and
/// the one block reading, which are `norn-text`'s.
///
/// The store parses no document, so a get is handed its reader as a find is
/// handed its declaration, and the store feeds it the rows it holds of the one
/// document a get answers. An implementation converts those rows to
/// `norn-text`'s headings and hands back what `norn_text::resolve_section`
/// and `norn_text::BodyScan::block_extent` answer, the matched heading's index
/// included. The host's get handler implements it so, and the store's own
/// suite hands a get the same `norn-text` reading.
///
/// **Every offset a reader is handed is a position in the body it is handed
/// with**: each heading's offset and body offset and each block marker is at
/// or before the body's end and on a character boundary. A get checks the
/// stored offsets against the body before it calls the reader and refuses the
/// read as damage where one names no position, so no reader slices a body at
/// an offset its bytes cannot hold.
pub trait DocumentText {
    /// The section `anchor` names among `headings` — the document's headings
    /// in document order — in `body`, taking the first heading the anchor
    /// matches, and `None` where it matches none.
    ///
    /// **An implementation makes at most four linear passes over
    /// `headings`**, visiting each heading at most once a pass, and reads no
    /// heading it is not handed, so what the match compares is at most four
    /// times what [`GetWork::anchor_headings`] counts.
    /// `norn_text::resolve_section` states the same bound of itself.
    fn section(&self, headings: &[HeadingFact], body: &str, anchor: &str) -> Option<SectionAt>;

    /// The bytes of `body` the block holds whose definition's `^` marker
    /// stands at `marker`.
    fn block(&self, body: &str, marker: usize) -> Range<usize>;
}

/// The section a heading anchor names: which heading opens it, by its index
/// in document order, and the bytes of the body below the heading that it
/// owns — to the start of the line holding the next heading at its level or
/// above, or the end of the body.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SectionAt {
    pub heading: usize,
    pub body: Range<usize>,
}

/// What [`Snapshot::get`] answers: the report, and what the request could not
/// apply.
#[derive(Clone, Debug, PartialEq)]
pub struct Gotten {
    /// The record, the section, the block, or the collection page.
    pub report: GetReport,
    /// The parts of the request that could not be applied as asked: an
    /// unknown projected key, a section or a block the document does not
    /// carry.
    pub unsatisfied: Vec<Unsatisfied>,
    /// The reading the answer was established under, as a cursor carries it.
    /// A get's order is no field's, so it names no fingerprint.
    pub snapshot: norn_wire::Snapshot,
    /// What the get read.
    pub work: GetWork,
}

impl Gotten {
    /// The unsatisfied parts and the report a handler wraps in a
    /// [`norn_wire::VaultAnswer`].
    pub fn into_report(self) -> (Vec<Unsatisfied>, GetReport) {
        (self.unsatisfied, self.report)
    }
}

/// What one get read: every statement it ran, what SQLite counted stepping
/// them, summed over all of them, and the heading rows a section lookup
/// matched its anchor over in memory.
///
/// The statement counters are the get's cost as SQLite ran it, so a pair of
/// gets of the same document over two vault sizes reads whether a get's work
/// grows with the vault by comparing them.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct GetWork {
    /// The statements the get ran, each counted on the snapshot as it ran.
    pub statements: u64,
    /// Steps the statements took through a loop no constraint bounds.
    pub full_scan_steps: u64,
    /// Sorts the statements ran.
    pub sorts: u64,
    /// Virtual-machine operations the statements ran.
    pub vm_steps: u64,
    /// The heading rows the section lookup handed the document reader to
    /// match its anchor over: the named document's headings, each once. What
    /// the in-memory match compares is bounded by this count times the
    /// passes [`DocumentText::section`] is held to. Zero where the get looks
    /// up no section.
    pub anchor_headings: u64,
    /// The candidate rows the resolution of the links the get answers read:
    /// each one document a link's head names, at most
    /// [`crate::CANDIDATE_HEAD`] per link, cut in the statement that reads
    /// them. Zero where the get answers no link.
    pub link_candidates_read: u64,
}

impl GetWork {
    /// The whole reading, name by name, every count present: the shape a
    /// harness compares, for the reason
    /// [`FindWork::readings`](crate::FindWork::readings) gives.
    pub fn readings(&self) -> impl Iterator<Item = (&'static str, u64)> + '_ {
        [
            ("get_statements", self.statements),
            ("get_full_scan_steps", self.full_scan_steps),
            ("get_sorts", self.sorts),
            ("get_vm_steps", self.vm_steps),
            ("get_anchor_headings", self.anchor_headings),
            ("get_link_candidates_read", self.link_candidates_read),
        ]
        .into_iter()
    }

    /// The work of one statement SQLite counted `stepped` for.
    fn of(stepped: Stepped) -> Self {
        let mut work = GetWork {
            statements: 1,
            ..GetWork::default()
        };
        work.add(stepped);
        work
    }

    /// Add what SQLite counted stepping one statement, leaving the statement
    /// count to the caller.
    fn add(&mut self, stepped: Stepped) {
        self.full_scan_steps += stepped.full_scan_steps;
        self.sorts += stepped.sorts;
        self.vm_steps += stepped.vm_steps;
    }

    /// Move onto this one statement's work the rows `get` counts that
    /// `statement` handed back: the heading rows a section lookup matched
    /// over are the document's headings statement's, and the candidate rows
    /// a link resolution read are the link-target statement's. Each count
    /// moves once, so a get's plans sum to its work.
    fn take_rows_of(&mut self, statement: ReadStatement, get: &mut GetWork) {
        match statement {
            ReadStatement::Get(GetStatement::DocumentHeadings) => {
                self.anchor_headings = std::mem::take(&mut get.anchor_headings);
            }
            ReadStatement::Find(FindStatement::LinkTargets) => {
                self.link_candidates_read = std::mem::take(&mut get.link_candidates_read);
            }
            _ => {}
        }
    }
}

/// A statement a get ran, with the plan SQLite reported for the text and the
/// values it ran with.
#[derive(Clone, Debug)]
pub struct GetPlan {
    /// The statement, named by the builder that names it: a get statement, or
    /// a statement the hydration or a lookup a find shares ran.
    pub statement: ReadStatement,
    /// The filters the statement narrows by: a get's narrow by none.
    pub filters: Vec<ReadFilter>,
    pub plan: EmittedPlan,
    /// What SQLite counted stepping the statement as the get ran it, one
    /// statement's [`GetWork`]: the work of a get it refused is read here.
    /// The document's headings statement carries the heading rows the
    /// section lookup matched over, and the link-target statement the
    /// candidate rows it read, so the plans of a get sum to its work.
    pub work: GetWork,
}

/// What a request asks of its one document.
enum Shape<'a> {
    Record(Projection<'a>),
    Section(&'a str),
    Block(&'a str),
    Collection(CollectionSelector),
}

impl<'a> Shape<'a> {
    /// What `params` asks for, or the refusal of a part the answer it asks for
    /// does not take: an anchor or a column on a collection page, a column on
    /// a section or a block, and a cursor or a limit on anything but a
    /// collection page.
    ///
    /// The answer is classified before any part is judged against it, so a
    /// refused part always names the answer that does not take it. An anchor
    /// this build does not know classifies as no answer, so it is refused
    /// ([`PageRefusal::UnknownPart`]) ahead of any other part the request
    /// carries.
    fn of(params: &'a GetParams) -> Result<Self, PageRefusal> {
        let not_taken = |part, answer| Err(PageRefusal::PartNotTaken { part, answer });
        if let Some(selector) = params.collection {
            if params.target.anchor().is_some() {
                return not_taken(RequestPart::Anchor, AnswerShape::CollectionPage);
            }
            if !params.columns.is_empty() {
                return not_taken(RequestPart::Column, AnswerShape::CollectionPage);
            }
            return Ok(Shape::Collection(selector));
        }
        let (answer, anchored) = match params.target.anchor() {
            None => (AnswerShape::Record, None),
            Some(Anchor::Heading { text, .. }) => {
                (AnswerShape::Section, Some(Shape::Section(text.as_str())))
            }
            Some(Anchor::Block { id, .. }) => (AnswerShape::Block, Some(Shape::Block(id.as_str()))),
            Some(_) => {
                return Err(PageRefusal::UnknownPart {
                    part: RequestPart::unknown("an anchor"),
                });
            }
        };
        if params.after.is_some() {
            return not_taken(RequestPart::Cursor, answer);
        }
        if params.limit.is_some() {
            return not_taken(RequestPart::Limit, answer);
        }
        match anchored {
            None if params.columns.is_empty() => Ok(Shape::Record(Projection::whole())),
            None => Ok(Shape::Record(Projection::of(&params.columns)?)),
            Some(_) if !params.columns.is_empty() => not_taken(RequestPart::Column, answer),
            Some(anchored) => Ok(anchored),
        }
    }
}

/// One page of one collection a get pages by ordinal.
struct OrdinalPage<'a> {
    /// The collection as the request named it, which the next page's cursor
    /// names.
    selector: CollectionSelector,
    collection: Nested,
    document: i64,
    limit: usize,
    /// The ordinal the page continues after, and `None` on a first page.
    after: Option<i64>,
    /// The reading the next page's cursor is minted under.
    snapshot: &'a norn_wire::Snapshot,
}

/// The document a target named.
struct Named {
    document: i64,
    path: String,
    wire: norn_wire::DocumentPath,
}

impl Snapshot {
    /// The document `params.target` names, answered as `params` asks.
    ///
    /// `declared` is the vault's declaration, read from the schema the
    /// snapshot pins: its ambiguity-ignore set narrows the target's class, and
    /// — beside the keys documents carry — it says which projected keys are
    /// known. `text` reads a section or a block out of the document's text. A
    /// collection page holds `params.limit` rows, [`crate::DEFAULT_PAGE`] where
    /// it names none; any other answer is not paged, and a bound on one is
    /// refused.
    ///
    /// Refused: a target naming several documents or none, as the module
    /// states; a declaration read from another schema than the snapshot pins;
    /// a page bound outside `1..=`[`crate::MAX_PAGE`]; a projected column this
    /// build of the store does not know; a cursor that names no position in
    /// the collection paged, that was minted paging another collection, or
    /// that the snapshot's reading refuses; and a part the answer asked for
    /// does not take ([`PageRefusal::PartNotTaken`]).
    pub fn get(
        &self,
        params: &GetParams,
        declared: &ContentModel,
        text: &dyn DocumentText,
    ) -> Result<Gotten, PageRefusal> {
        self.run_get(params, declared, text, &mut Lookups::default())
    }

    /// Every statement [`Snapshot::get`] runs for `params`, in the order it
    /// runs them, each with the plan SQLite reported for it.
    ///
    /// This is the get itself, run on this snapshot, and each plan is taken of
    /// the very text and values its statement ran with, as
    /// [`Snapshot::find_plans`] takes a find's. A statement the get did not
    /// run is not listed. A get refused because its target names several
    /// documents or none ran the statements that decided so, and those are
    /// explained; any other refusal is returned.
    pub fn get_plans(
        &self,
        params: &GetParams,
        declared: &ContentModel,
        text: &dyn DocumentText,
    ) -> Result<Vec<GetPlan>, PageRefusal> {
        let mut lookups = Lookups::default();
        let mut counted = match self.run_get(params, declared, text, &mut lookups) {
            Ok(gotten) => gotten.work,
            Err(PageRefusal::AmbiguousTarget(_) | PageRefusal::UnknownTarget { .. }) => {
                GetWork::default()
            }
            Err(refusal) => return Err(refusal),
        };
        let mut stepped = lookups
            .ran
            .iter()
            .map(|ran| ran.stepped)
            .collect::<Vec<_>>()
            .into_iter();
        Ok(self.explained(lookups.ran, |statement, filters, plan| {
            let mut work = GetWork::of(stepped.next().unwrap_or_default());
            work.take_rows_of(statement, &mut counted);
            GetPlan {
                statement,
                filters,
                plan,
                work,
            }
        })?)
    }

    /// The get [`Snapshot::get`] answers and [`Snapshot::get_plans`]
    /// explains, recording every statement it runs in `lookups`.
    fn run_get(
        &self,
        params: &GetParams,
        declared: &ContentModel,
        text: &dyn DocumentText,
        lookups: &mut Lookups,
    ) -> Result<Gotten, PageRefusal> {
        let started = self.counters().statements_executed();
        let shape = Shape::of(params)?;
        let limit = match shape {
            Shape::Collection(_) => page_limit(params.limit)?,
            _ => 0,
        };
        self.declaration_pinned(declared, lookups)?;
        let named = self.named(&params.target, declared, lookups)?;
        let snapshot = self.reading_facts(None, lookups)?;
        let mut work = GetWork::default();
        let (report, unsatisfied) = match shape {
            Shape::Record(projection) => {
                let mut unknown = Vec::new();
                let fields = self.projected_keys(&projection, declared, lookups, &mut unknown)?;
                let mut hydration = FindWork::default();
                let mut rows = self.hydrate_rows(
                    &[FoundKey::unsorted(named.document, named.path.clone())],
                    &projection,
                    &fields,
                    declared,
                    lookups,
                    &mut hydration,
                )?;
                work.link_candidates_read += hydration.link_candidates_read;
                let unsatisfied = self.resolve(unknown, declared, lookups)?;
                (GetReport::record(rows.remove(0)), unsatisfied)
            }
            Shape::Section(anchor) => self.section(named, anchor, text, lookups, &mut work)?,
            Shape::Block(id) => self.block(named, id, text, lookups)?,
            Shape::Collection(selector) => {
                let path = named.wire.clone();
                let page = self.collection(
                    named,
                    selector,
                    limit,
                    params.after.as_ref(),
                    &snapshot,
                    declared,
                    lookups,
                    &mut work,
                )?;
                (GetReport::collection(path, page), Vec::new())
            }
        };
        work.statements = self.counters().statements_executed() - started;
        for ran in &lookups.ran {
            work.add(ran.stepped);
        }
        Ok(Gotten {
            report,
            unsatisfied,
            snapshot,
            work,
        })
    }

    /// The one document `target` names, or the refusal of a target naming
    /// several or none.
    fn named(
        &self,
        target: &ResolutionTarget,
        declared: &ContentModel,
        lookups: &mut Lookups,
    ) -> Result<Named, PageRefusal> {
        match self.name_target(
            target.address(),
            declared.ambiguity_ignore(),
            &mut lookups.ran,
        )? {
            Naming::Nothing => Err(PageRefusal::UnknownTarget {
                target: target.clone(),
            }),
            Naming::One { document, path } => Ok(Named {
                document,
                wire: wire_path(&path)?,
                path,
            }),
            Naming::Several(head) => {
                let address = ResolutionTarget::new(target.address())
                    .expect("a target's address is a target of its own");
                Err(PageRefusal::AmbiguousTarget(Box::new(TargetAmbiguity {
                    target: target.clone(),
                    head,
                    hint: Hint::resolves(address),
                })))
            }
        }
    }

    /// The section `anchor` names in the named document, or the record of its
    /// path and the report that it carries no such section, counting in
    /// `work` the heading rows the anchor is matched over.
    fn section(
        &self,
        named: Named,
        anchor: &str,
        text: &dyn DocumentText,
        lookups: &mut Lookups,
        work: &mut GetWork,
    ) -> Result<(GetReport, Vec<Unsatisfied>), StoreError> {
        let headings: Vec<HeadingFact> = self
            .run_statement(
                &mut lookups.ran,
                compose(&Spelled::DocumentHeadings {
                    document: named.document,
                }),
                stored_heading,
            )
            .map_err(|problem| error::sql("reading a document's headings", problem))?
            .into_iter()
            .collect::<Result<_, StoreError>>()?;
        work.anchor_headings += headings.len() as u64;
        let body = self.body_of(named.document, lookups)?;
        for heading in &headings {
            let at = held_offset(&body, heading.span.byte_offset, "a heading's offset")?;
            let body_at = held_offset(&body, heading.body_offset, "a heading's body offset")?;
            if body_at < at {
                return Err(StoreError::Damaged {
                    what: format!(
                        "the store holds a heading at {at} whose body starts before it, at {body_at}"
                    ),
                });
            }
        }
        let Some(at) = text.section(&headings, &body, anchor) else {
            return Ok((
                GetReport::record(DocumentRow::new(named.wire)),
                vec![Unsatisfied::missing_section(anchor)],
            ));
        };
        let heading = headings
            .get(at.heading)
            .cloned()
            .ok_or_else(|| outside("a heading index", at.heading))?;
        let owned = body
            .get(at.body.clone())
            .ok_or_else(|| outside("a section's bytes", at.body.end))?;
        Ok((
            GetReport::section(named.wire, wire_heading(heading), bounded_body(owned)),
            Vec::new(),
        ))
    }

    /// The block `id` names in the named document, or the record of its path
    /// and the report that it defines no such block.
    fn block(
        &self,
        named: Named,
        id: &str,
        text: &dyn DocumentText,
        lookups: &mut Lookups,
    ) -> Result<(GetReport, Vec<Unsatisfied>), StoreError> {
        let definition: Option<BlockFact> = self
            .run_statement(
                &mut lookups.ran,
                compose(&Spelled::BlockDefinition {
                    document: named.document,
                    id,
                }),
                stored_block,
            )
            .map_err(|problem| error::sql("reading a block definition", problem))?
            .into_iter()
            .next()
            .transpose()?;
        let Some(definition) = definition else {
            return Ok((
                GetReport::record(DocumentRow::new(named.wire)),
                vec![Unsatisfied::missing_block(id)],
            ));
        };
        // A definition recorded with no position names no marker, so no
        // block's bytes can be read for it.
        let body = match definition.span {
            None => BodyText::new("", 0).expect("an empty head heads an empty whole"),
            Some(span) => {
                let body = self.body_of(named.document, lookups)?;
                let marker = held_offset(&body, span.byte_offset, "a block's marker")?;
                let range = text.block(&body, marker);
                let held = body
                    .get(range.clone())
                    .ok_or_else(|| outside("a block's bytes", range.end))?;
                bounded_body(held)
            }
        };
        Ok((
            GetReport::block(named.wire, wire_block(definition), body),
            Vec::new(),
        ))
    }

    /// One document's whole body.
    fn body_of(&self, document: i64, lookups: &mut Lookups) -> Result<String, StoreError> {
        self.run_statement(
            &mut lookups.ran,
            compose(&Spelled::DocumentBody { document }),
            |row| row.get::<_, String>(0),
        )
        .map_err(|problem| error::sql("reading a document's body", problem))?
        .into_iter()
        .next()
        .ok_or_else(|| StoreError::Damaged {
            what: format!("the document at row {document} has no row"),
        })
    }

    /// One page of the collection `selector` names on the named document, a
    /// links page resolving its links under `declared`'s ambiguity-ignore
    /// set and counting in `work` the candidate rows that read.
    #[allow(clippy::too_many_arguments)] // A page is named by each of these, and none of them groups with another.
    fn collection(
        &self,
        named: Named,
        selector: CollectionSelector,
        limit: usize,
        after: Option<&Cursor>,
        snapshot: &norn_wire::Snapshot,
        declared: &ContentModel,
        lookups: &mut Lookups,
        work: &mut GetWork,
    ) -> Result<CollectionPage, PageRefusal> {
        let collection = match selector {
            CollectionSelector::Findings => {
                return Ok(CollectionPage::findings(
                    self.finding_page(&named, limit, after, snapshot, lookups)?,
                ));
            }
            CollectionSelector::Links => Nested::Links,
            CollectionSelector::Headings => Nested::Headings,
            CollectionSelector::Blocks => Nested::Blocks,
            CollectionSelector::Tags => Nested::Tags,
            _ => {
                return Err(PageRefusal::UnknownPart {
                    part: RequestPart::unknown("a collection"),
                });
            }
        };
        let (after, moved) = self.ordinal_after(selector, after, lookups)?;
        let page = OrdinalPage {
            selector,
            collection,
            document: named.document,
            limit,
            after,
            snapshot,
        };
        Ok(match collection {
            Nested::Links => {
                let (links, next) = self.ordinal_page(&page, identified_link, lookups)?;
                let rows = self.link_rows(
                    links,
                    declared.ambiguity_ignore(),
                    &mut lookups.ran,
                    &mut work.link_candidates_read,
                )?;
                CollectionPage::links(Page::new(rows, next, moved))
            }
            Nested::Headings => {
                let (rows, next) = self.ordinal_page(&page, heading_row, lookups)?;
                CollectionPage::headings(Page::new(rows, next, moved))
            }
            Nested::Blocks => {
                let (rows, next) = self.ordinal_page(&page, block_row, lookups)?;
                CollectionPage::blocks(Page::new(rows, next, moved))
            }
            Nested::Tags => {
                let (rows, next) = self.ordinal_page(&page, tag_row, lookups)?;
                CollectionPage::tags(Page::new(rows, next, moved))
            }
        })
    }

    /// Where a page of `paged` continues from, and what moved since its
    /// cursor was minted; a first page starts at the first row and nothing
    /// moved. A cursor minted paging another collection is refused.
    fn ordinal_after(
        &self,
        paged: CollectionSelector,
        after: Option<&Cursor>,
        lookups: &mut Lookups,
    ) -> Result<(Option<i64>, Vec<norn_wire::Moved>), PageRefusal> {
        let Some(cursor) = after else {
            return Ok((None, Vec::new()));
        };
        let not_taken =
            || PageRefusal::cursor_not_taken(cursor, PagedRows::Collection { of: paged });
        let CursorKey::Ordinal { of, index, .. } = cursor.key() else {
            return Err(not_taken());
        };
        if *of != paged {
            return Err(not_taken());
        }
        let index = i64::try_from(*index).map_err(|_| not_taken())?;
        let moved =
            self.judge_unordered_reading(cursor, PagedRows::Collection { of: paged }, lookups)?;
        Ok((Some(index), moved))
    }

    /// One page of the rows `page` names, each read through `item`, and the
    /// cursor the next page continues from.
    fn ordinal_page<T: Clone>(
        &self,
        page: &OrdinalPage<'_>,
        item: fn(&Row<'_>) -> Reading<T>,
        lookups: &mut Lookups,
    ) -> Result<(Vec<T>, Option<Cursor>), PageRefusal> {
        let width = page.collection.width();
        let read = self.read_page(
            [page.after],
            page.limit,
            &mut lookups.ran,
            |record, after, rows| {
                let ran = compose(&Spelled::CollectionPage {
                    collection: page.collection,
                    document: page.document,
                    after,
                    rows,
                });
                self.run_statement(record, ran, |row| {
                    Ok((row.get::<_, i64>(width)?, item(row)?))
                })
                .map_err(|problem| error::sql("reading a page of a collection", problem))?
                .into_iter()
                .map(|(ordinal, item)| item.map(|item| (ordinal, item)))
                .collect()
            },
        )?;
        let next = read.next.map(|(ordinal, _)| {
            Cursor::new(
                page.snapshot.clone(),
                CursorKey::ordinal(page.selector, u64::try_from(ordinal).unwrap_or_default()),
            )
        });
        Ok((read.rows.into_iter().map(|(_, item)| item).collect(), next))
    }

    /// One page of at most `limit` of the findings standing at the named
    /// document's path under the active fingerprint, after `after`.
    fn finding_page(
        &self,
        named: &Named,
        limit: usize,
        after: Option<&Cursor>,
        snapshot: &norn_wire::Snapshot,
        lookups: &mut Lookups,
    ) -> Result<Page<norn_wire::FindingRow>, PageRefusal> {
        // A finding recorded under no schema is stamped with the empty
        // fingerprint.
        let fingerprint = self.fingerprint(lookups)?.unwrap_or_default();
        let (resume, moved) = match after {
            None => (None, Vec::new()),
            Some(cursor) => {
                // The findings are paged by a finding's cursor, so an ordinal
                // naming them was minted by no page, and one at another path
                // names no position among this document's findings.
                let not_taken = || PageRefusal::cursor_not_taken(cursor, PagedRows::Finding);
                let CursorKey::Finding { kind, path, id, .. } = cursor.key() else {
                    return Err(not_taken());
                };
                if *path != named.path {
                    return Err(not_taken());
                }
                let id = i64::try_from(*id).map_err(|_| not_taken())?;
                let moved = self.judge_unordered_reading(cursor, PagedRows::Finding, lookups)?;
                (Some((kind.as_str(), id)), moved)
            }
        };
        let page = self.read_page([resume], limit, &mut lookups.ran, |record, after, rows| {
            let ran = compose(&Spelled::FindingPage {
                path: &named.path,
                fingerprint: &fingerprint,
                after,
                rows,
            });
            self.run_statement(record, ran, finding_base)
                .map_err(|problem| error::sql("reading a page of findings", problem))?
                .into_iter()
                .collect()
        })?;
        let next = page
            .next
            .map(|last| -> Result<Cursor, StoreError> {
                let kind = FindingKind::try_from(last.kind.as_str())
                    .map_err(|_| unreadable("findings.kind", &last.kind))?;
                let id = u64::try_from(last.id)
                    .map_err(|_| unreadable("findings.id", &last.id.to_string()))?;
                Ok(Cursor::new(
                    snapshot.clone(),
                    CursorKey::finding(kind, last.path, id),
                ))
            })
            .transpose()?;
        let rows = self.finding_rows(&mut lookups.ran, page.rows)?;
        Ok(Page::new(rows, next, moved))
    }
}

/// `offset` as a position in `body`: at or before its end, on a character
/// boundary. The headings and the blocks are read out of the body when it is
/// derived, so a stored offset that names no position in it is a row that
/// disagrees with the body it was derived from, which is damage.
fn held_offset(body: &str, offset: u64, what: &str) -> Result<usize, StoreError> {
    usize::try_from(offset)
        .ok()
        .filter(|&at| body.is_char_boundary(at))
        .ok_or_else(|| StoreError::Damaged {
            what: format!(
                "the store holds {what} at {offset}, which is no position in a body of {} bytes",
                body.len()
            ),
        })
}

/// The document reader named bytes or a heading the document does not hold.
fn outside(what: &str, at: usize) -> StoreError {
    StoreError::Damaged {
        what: format!("the document reader named {what} at {at}, outside the document"),
    }
}
