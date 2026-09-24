//! The get builder: one document, named by a resolution target, answered on a
//! read snapshot as its record, as the section or the block its target's
//! anchor names, or as one page of one of its nested collections.
//!
//! The builder is inherent methods on [`Snapshot`], as the find, count and
//! validate builders are, so every statement it runs reads the one instant
//! the snapshot was established at and is counted on the snapshot's own
//! statement counter. **A get's work follows the one document it answers and
//! the page it reads, never the vault**: the target's class is ranges of a
//! suffix key index, and everything after it is read by the document's row id
//! or its path. [`GetStatement`] names what each statement reads.
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
//! candidate alone, each tried by one statement that stops at a class's second
//! member; a candidate no suffix names alone is named by its path.
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
//! the one document rather than by an index of its own. A block definition is
//! one seek of the document's definitions that stops at the first matching
//! one. A section or a block the document does not carry is answered in band
//! ([`Unsatisfied::MissingSection`], [`Unsatisfied::MissingBlock`]) beside the
//! record of the document's path alone, never the whole document.
//!
//! # A collection page is a keyset page by ordinal
//!
//! One nested collection — links, headings, block definitions or tags — is
//! paged in document order by its ordinal, with the ordinal the page stopped
//! at as its cursor ([`norn_wire::CursorKey::Ordinal`]). The findings
//! standing over the document are paged in `(kind, id)` order at its path, the
//! order a find's findings column and a validate read them in at one path,
//! with a finding's cursor ([`norn_wire::CursorKey::Finding`]). Either reads
//! one row past its bound to learn a next page exists, through the keyset
//! page every read builder reads ([`Snapshot::read_page`]).

mod statement;

use std::ops::Range;

use norn_db::EmittedPlan;
use norn_db::rusqlite::Row;
use norn_wire::{
    Anchor, BodyText, Candidate, CandidateHead, CollectionPage, CollectionSelector, Cursor,
    CursorKey, DocumentRow, FindingKind, GetParams, GetReport, Hint, LinkRow, Page,
    ResolutionTarget, Unsatisfied,
};

use crate::error::{self, StoreError};
use crate::facts::{BlockFact, CANDIDATE_HEAD, HeadingFact, LinkFact, LinkFamily};
use crate::fields::ContentModel;
use crate::find::{
    FindWork, FoundKey, Nested, Projection, block_row, bounded_body, heading_row, tag_row,
    wire_block, wire_heading, wire_span,
};
use crate::read::{
    Lookups, PageRefusal, ReadFilter, ReadStatement, TargetAmbiguity, finding_base, page_limit,
};
use crate::request::{Reading, stored_block, stored_heading, stored_link, unreadable};
use crate::resolve::TargetClass;
use crate::store::Snapshot;

pub use statement::{Collection, GET_STATEMENTS, GetStatement};
use statement::{Spelled, compose};

/// What a get reads a document's text through: the one section resolver and
/// the one block reading, which are `norn-text`'s.
///
/// The store parses no document, so a get is handed its reader as a find is
/// handed its declaration, and the store feeds it the rows it holds of the one
/// document a get answers. An implementation converts those rows to
/// `norn-text`'s headings and hands back what `norn_text::resolve_section`
/// and `norn_text::BodyScan::block_extent` answer, the matched heading's index
/// included.
///
/// **This seam is a dormant carrier** for the host's get handler (NORN-230),
/// which implements it over `norn-text`: no handler calls [`Snapshot::get`]
/// yet, so the call graph reaches no implementation outside the store's own
/// suite, which hands a get the same `norn-text` reading.
pub trait DocumentText {
    /// The section `anchor` names among `headings` — the document's headings
    /// in document order — in `body`, taking the first heading the anchor
    /// matches, and `None` where it matches none.
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

/// What one get read: every statement it ran, and what SQLite counted
/// stepping them, summed over all of them.
///
/// The counters are the get's cost as SQLite ran it, so a pair of gets of
/// the same document over two vault sizes reads whether a get's work grows
/// with the vault by comparing them.
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
    /// a section or a block, and a cursor on anything but a collection page.
    fn of(params: &'a GetParams) -> Result<Self, PageRefusal> {
        let not_taken = |part, answer| Err(PageRefusal::PartNotTaken { part, answer });
        if let Some(selector) = params.collection {
            if params.target.anchor().is_some() {
                return not_taken("an anchor", "a collection page");
            }
            if !params.columns.is_empty() {
                return not_taken("a column", "a collection page");
            }
            return Ok(Shape::Collection(selector));
        }
        if params.after.is_some() {
            return not_taken("a cursor", "a record, a section or a block");
        }
        let anchored = match params.target.anchor() {
            None => {
                return Ok(Shape::Record(if params.columns.is_empty() {
                    Projection::whole()
                } else {
                    Projection::of(&params.columns)?
                }));
            }
            Some(Anchor::Heading { text, .. }) => Shape::Section(text.as_str()),
            Some(Anchor::Block { id, .. }) => Shape::Block(id.as_str()),
            Some(_) => return Err(PageRefusal::UnknownPart { part: "an anchor" }),
        };
        if !params.columns.is_empty() {
            return not_taken(
                "a column",
                match anchored {
                    Shape::Section(_) => "a section",
                    _ => "a block",
                },
            );
        }
        Ok(anchored)
    }
}

/// One page of one collection a get pages by ordinal.
struct OrdinalPage<'a> {
    collection: Collection,
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
    /// it names none; any other answer is not paged and takes no bound.
    ///
    /// Refused: a target naming several documents or none, as the module
    /// states; a declaration read from another schema than the snapshot pins;
    /// a page bound outside `1..=`[`crate::MAX_PAGE`]; a projected column the
    /// store does not project yet; a cursor that names no position in the
    /// collection paged, or that the snapshot's reading refuses; and a part
    /// the answer asked for does not take ([`PageRefusal::PartNotTaken`]).
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
        match self.run_get(params, declared, text, &mut lookups) {
            Ok(_) | Err(PageRefusal::AmbiguousTarget(_) | PageRefusal::UnknownTarget { .. }) => {}
            Err(refusal) => return Err(refusal),
        }
        Ok(
            self.explained(lookups.ran, |statement, filters, plan| GetPlan {
                statement,
                filters,
                plan,
            })?,
        )
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
        let (report, unsatisfied) = match shape {
            Shape::Record(projection) => {
                let mut unknown = Vec::new();
                let fields = self.projected_keys(&projection, declared, lookups, &mut unknown)?;
                let mut rows = self.hydrate_rows(
                    &[FoundKey::unsorted(named.document, named.path.clone())],
                    &projection,
                    &fields,
                    lookups,
                    &mut FindWork::default(),
                )?;
                let unsatisfied = self.resolve(unknown, declared, lookups)?;
                (GetReport::record(rows.remove(0)), unsatisfied)
            }
            Shape::Section(anchor) => self.section(named, anchor, text, lookups)?,
            Shape::Block(id) => self.block(named, id, text, lookups)?,
            Shape::Collection(selector) => {
                let path = named.wire.clone();
                let page = self.collection(
                    named,
                    selector,
                    limit,
                    params.after.as_ref(),
                    &snapshot,
                    lookups,
                )?;
                (GetReport::collection(path, page), Vec::new())
            }
        };
        let mut work = GetWork {
            statements: self.counters().statements_executed() - started,
            ..GetWork::default()
        };
        for ran in &lookups.ran {
            work.full_scan_steps += ran.stepped.full_scan_steps;
            work.sorts += ran.stepped.sorts;
            work.vm_steps += ran.stepped.vm_steps;
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
        const OPERATION: &str = "reading the documents a target names";
        let unknown = || PageRefusal::UnknownTarget {
            target: target.clone(),
        };
        let Ok(class) = TargetClass::compile(
            target.address(),
            self.path_order(),
            declared.ambiguity_ignore(),
        ) else {
            return Err(unknown());
        };
        let head: Vec<(i64, String)> = self
            .run_statement(
                &mut lookups.ran,
                compose(&Spelled::ClassHead {
                    class: &class,
                    rows: CANDIDATE_HEAD,
                }),
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(|problem| error::sql(OPERATION, problem))?;
        match head.as_slice() {
            [] => Err(unknown()),
            [(document, path)] => Ok(Named {
                document: *document,
                path: path.clone(),
                wire: wire_path(path)?,
            }),
            _ => {
                let total = if head.len() < CANDIDATE_HEAD {
                    head.len() as u64
                } else {
                    self.run_statement(
                        &mut lookups.ran,
                        compose(&Spelled::ClassTotal { class: &class }),
                        |row| row.get::<_, u64>(0),
                    )
                    .map_err(|problem| error::sql(OPERATION, problem))?
                    .into_iter()
                    .next()
                    .unwrap_or_default()
                };
                let mut candidates = Vec::with_capacity(head.len());
                for (document, path) in &head {
                    let suffix = self.candidate_suffix(*document, path, declared, lookups)?;
                    candidates.push(Candidate::new(wire_path(path)?, suffix));
                }
                let head = CandidateHead::new(candidates, total).map_err(|problem| {
                    StoreError::Damaged {
                        what: format!("a class's head outgrew its count: {problem}"),
                    }
                })?;
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

    /// The minimal disambiguating suffix of the candidate `document` at
    /// `path`: the first of its suffix spellings whose class holds it alone,
    /// or its path where none does.
    fn candidate_suffix(
        &self,
        document: i64,
        path: &str,
        declared: &ContentModel,
        lookups: &mut Lookups,
    ) -> Result<String, StoreError> {
        let at = crate::path::DocumentPath::new(path)?;
        for spelling in at.suffix_spellings() {
            let Ok(class) =
                TargetClass::compile(&spelling, self.path_order(), declared.ambiguity_ignore())
            else {
                continue;
            };
            let members: Vec<i64> = self
                .run_statement(
                    &mut lookups.ran,
                    compose(&Spelled::CandidateSuffix { class: &class }),
                    |row| row.get(0),
                )
                .map_err(|problem| error::sql("naming a candidate by its suffix", problem))?;
            if members == [document] {
                return Ok(spelling);
            }
        }
        Ok(path.to_string())
    }

    /// The section `anchor` names in the named document, or the record of its
    /// path and the report that it carries no such section.
    fn section(
        &self,
        named: Named,
        anchor: &str,
        text: &dyn DocumentText,
        lookups: &mut Lookups,
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
        let body = self.body_of(named.document, lookups)?;
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
                let marker = usize::try_from(span.byte_offset).unwrap_or(usize::MAX);
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

    /// One page of the collection `selector` names on the named document.
    fn collection(
        &self,
        named: Named,
        selector: CollectionSelector,
        limit: usize,
        after: Option<&Cursor>,
        snapshot: &norn_wire::Snapshot,
        lookups: &mut Lookups,
    ) -> Result<CollectionPage, PageRefusal> {
        let collection = match selector {
            CollectionSelector::Findings => {
                return Ok(CollectionPage::findings(
                    self.finding_page(&named, limit, after, snapshot, lookups)?,
                ));
            }
            CollectionSelector::Links => Collection::Links,
            CollectionSelector::Headings => Collection::Nested(Nested::Headings),
            CollectionSelector::Blocks => Collection::Nested(Nested::Blocks),
            CollectionSelector::Tags => Collection::Nested(Nested::Tags),
            _ => {
                return Err(PageRefusal::UnknownPart {
                    part: "a collection",
                });
            }
        };
        let (after, moved) = self.ordinal_after(after, lookups)?;
        let page = OrdinalPage {
            collection,
            document: named.document,
            limit,
            after,
            snapshot,
        };
        Ok(match collection {
            Collection::Links => {
                let (rows, next) = self.ordinal_page(&page, link_row, lookups)?;
                CollectionPage::links(Page::new(rows, next, moved))
            }
            Collection::Nested(Nested::Headings) => {
                let (rows, next) = self.ordinal_page(&page, heading_row, lookups)?;
                CollectionPage::headings(Page::new(rows, next, moved))
            }
            Collection::Nested(Nested::Blocks) => {
                let (rows, next) = self.ordinal_page(&page, block_row, lookups)?;
                CollectionPage::blocks(Page::new(rows, next, moved))
            }
            Collection::Nested(Nested::Tags) => {
                let (rows, next) = self.ordinal_page(&page, tag_row, lookups)?;
                CollectionPage::tags(Page::new(rows, next, moved))
            }
        })
    }

    /// Where an ordinal page continues from, and what moved since its cursor
    /// was minted; a first page starts at the first row and nothing moved.
    fn ordinal_after(
        &self,
        after: Option<&Cursor>,
        lookups: &mut Lookups,
    ) -> Result<(Option<i64>, Vec<norn_wire::Moved>), PageRefusal> {
        let Some(cursor) = after else {
            return Ok((None, Vec::new()));
        };
        let CursorKey::Ordinal { index, .. } = cursor.key() else {
            return Err(PageRefusal::NotACollectionCursor);
        };
        let index = i64::try_from(*index).map_err(|_| PageRefusal::NotACollectionCursor)?;
        let moved = self.judge_reading(cursor, None, false, lookups)?;
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
                CursorKey::ordinal(u64::try_from(ordinal).unwrap_or_default()),
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
                let CursorKey::Finding { kind, path, id, .. } = cursor.key() else {
                    return Err(PageRefusal::NotACollectionCursor);
                };
                if *path != named.path {
                    return Err(PageRefusal::NotACollectionCursor);
                }
                let id = i64::try_from(*id).map_err(|_| PageRefusal::NotACollectionCursor)?;
                let moved = self.judge_reading(cursor, None, false, lookups)?;
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
                .map_err(|problem| error::sql("reading a page of findings", problem))
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

/// `path` as the wire spells a document's path.
fn wire_path(path: &str) -> Result<norn_wire::DocumentPath, StoreError> {
    norn_wire::DocumentPath::new(path).map_err(|problem| StoreError::Damaged {
        what: format!("`documents.path` holds no document path: {problem}"),
    })
}

/// The document reader named bytes or a heading the document does not hold.
fn outside(what: &str, at: usize) -> StoreError {
    StoreError::Damaged {
        what: format!("the document reader named {what} at {at}, outside the document"),
    }
}

fn link_row(row: &Row<'_>) -> Reading<LinkRow> {
    Ok(stored_link(row)?.map(wire_link))
}

/// A stored link as the wire's row carries it.
///
/// **`targets` is a dormant carrier** for the link index the Layer 3 link
/// index unit lands: a link's target is stored raw and unresolved, and no
/// statement resolves it, so the row carries the empty head out of none, and
/// the health derived from it reads broken whatever the vault holds. The
/// link index resolves each link's target through the one resolver and fills
/// the head; until then the row's syntactic half is the answer.
fn wire_link(link: LinkFact) -> LinkRow {
    let family = match link.family {
        LinkFamily::Wikilink => norn_wire::LinkFamily::Wikilink,
        LinkFamily::Markdown => norn_wire::LinkFamily::Markdown,
    };
    let anchor = match (link.block_ref, link.anchor) {
        (Some(id), _) => Some(Anchor::block(id)),
        (None, Some(text)) => Some(Anchor::heading(text)),
        (None, None) => None,
    };
    LinkRow::new(
        family,
        link.embed,
        link.protocol,
        link.target,
        link.title,
        anchor,
        wire_span(link.span),
        CandidateHead::new([], 0).expect("no candidate heads no document"),
    )
}
