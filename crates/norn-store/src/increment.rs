//! The write-through increment: one changeset, applied whole.
//!
//! # The changeset is the unit of atomicity
//!
//! [`crate::Request::apply_increment`] takes a changeset — document upserts and
//! deaths, in the order they are handed over — and applies **all of it or none
//! of it**. There is no way to open a scope and add writes to it, because the
//! entry point's granularity *is* the atomicity contract: a caller that needs
//! two changes to land together hands them over together, and a caller that
//! needs them to land separately calls twice.
//!
//! What that buys is the rung-2 story. A process that dies partway through
//! leaves the previous generation whole: nothing partial is ever at rest, so
//! there is no state saying a document was re-derived while the fact rows
//! derived from it were not.
//!
//! # One generation stamps everything the changeset wrote
//!
//! The generation is taken once, before the first entry, and every row the
//! changeset writes carries it. Generations are what order writes in the store,
//! and a changeset is one write however many documents it names — so a reader
//! comparing generations sees the changeset arrive at an instant rather than as
//! a run of numbers it has to recognize as one act.
//!
//! An **empty changeset takes no generation.** It writes nothing, so moving the
//! store's write sequence for it would make the sequence report an act that
//! never happened; the outcome says so by carrying no generation at all.
//!
//! # Entries apply in order, and the last entry for a path decides
//!
//! A changeset may name one path more than once, and the entries are applied in
//! the order they arrive. An upsert followed by a death for the same path leaves
//! a tombstone and no document; the reverse leaves the document, beside the
//! tombstone the death recorded. Nothing is coalesced ahead of time, because
//! coalescing would have to decide which of two facts about one path is the
//! true one — and the caller already said, by ordering them.
//!
//! # Dependent state is composed inside the same act
//!
//! **Full text is trigger-maintained.** Three triggers on `documents` carry
//! `documents_fts`, the store schema's full-text pillar, so the document
//! statements a changeset runs are the whole of its full-text maintenance. There
//! is no explicit index write here, and adding one would be a second maintainer
//! of the same rows.
//!
//! **Findings are discarded on two axes, both inside the one transaction.**
//!
//! - *By subject path.* Every changed path — upserted or dead — takes the
//!   findings recorded about it. A re-derivation's findings were read off facts
//!   the changeset has just replaced, and a death's describe a document that is
//!   gone; the caller re-mints what holds now from the facts it just handed
//!   over, which is the changeset itself, entry by entry.
//! - *By affected ambiguity class.* Every changed path also names the class it
//!   belongs to, and the findings in the union of those classes go with it —
//!   the resolution axis, where a document joining or leaving a class
//!   invalidates findings written in documents that did not change.
//!
//! The two axes are what make **discard-then-record** total. The store discards
//! and never records: minting a finding is a reading of the vault the caller
//! performs, so the outcome reports the classes, and the subject axis needs no
//! report because its scope is the changeset the caller just built.
//!
//! A finding with no class — a vault-schema violation, say — is out of the
//! resolution axis and reachable on the subject axis alone: it dies when the
//! document it is about changes, and survives every change to any other.
//!
//! This is maintenance rather than a cascade. Nothing in the schema references
//! `documents` from `findings`, so a finding about a path no document has is
//! recordable and no row-existence ordering matters; what reaches a finding is
//! a statement this crate runs and counts.
//!
//! # Memory is the changeset's, never the vault's
//!
//! Entries are consumed from an iterator one at a time and nothing collects
//! them, so a caller streaming a heal hands over something that yields documents
//! and the store holds one. What lives across entries is the prepared
//! statements, the tally, and the set of affected classes — the statements are a
//! fixed cost, and the other two are bounded by the changeset rather than by the
//! vault behind it.

use std::collections::BTreeSet;

use rusqlite::{CachedStatement, OptionalExtension, Transaction, TransactionBehavior, params};

use crate::counters::{Counter, DerivationCounters};
use crate::error::{self, StoreError};
use crate::facts::{DocumentFacts, Invalidation, Provenance};
use crate::json;
use crate::path::{ClassKey, DocumentPath};
use crate::request;
use crate::store::{self, Store};

/// One entry in a changeset.
///
/// The two shapes are the two things that happen to a path: it is derived, or
/// it is gone.
#[derive(Clone, Debug, PartialEq)]
pub enum Change {
    /// A document as it now stands, replacing everything derived from its path.
    Upsert(DocumentFacts),
    /// A path's death, recorded with how it was learned.
    ///
    /// The hash the tombstone carries is not supplied: it is the one the store
    /// last derived the path at, read out of the row this entry removes. A death
    /// for a path nothing had derived has none, and keeps the one already
    /// recorded where a tombstone is already there.
    Death {
        path: DocumentPath,
        provenance: Provenance,
    },
}

/// Where a changeset's post-state came from.
///
/// **This is the changeset's mark, and it is a different concept from
/// [`Provenance`], which is one tombstone's.** A tombstone's provenance says how
/// a single death was learned — a heal prune, a watcher removal, an applied
/// plan. This says how the whole changeset's facts were arrived at, and a
/// changeset carrying deaths carries both.
///
/// **The mark changes no statement the increment runs, and that is the contract
/// rather than an omission.** The store composes supplied facts under either
/// mark and re-derives nothing under either. What the mark changes is what a
/// counter reading is held to: see [`crate::DerivationCounters`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IncrementProvenance {
    /// The host read the documents from disk and derived these facts from the
    /// bytes it read. This is the heal and watcher path.
    Derived,
    /// An applier composed the post-state and the store writes it. Nothing here
    /// recomputes any part of what was composed, which is what the write-through
    /// contract means by the worker having already done the work.
    Composed,
}

/// What applying a changeset did.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IncrementOutcome {
    /// The mark the changeset was applied under.
    pub provenance: IncrementProvenance,
    /// The generation every row the changeset wrote is stamped with, or `None`
    /// for an empty changeset — which writes nothing and takes none.
    pub generation: Option<i64>,
    /// Documents written, counting a path named twice once per entry.
    pub documents_upserted: u64,
    /// Document rows removed. A death for a path nothing had derived removes
    /// none of them and still records a tombstone, so this is not the number of
    /// deaths.
    pub documents_deleted: u64,
    /// Deaths recorded, which is one per [`Change::Death`] entry.
    pub tombstones_recorded: u64,
    /// Every ambiguity class the changed paths are in — the resolution axis of
    /// the findings maintenance this changeset implies, and what a caller
    /// re-records against.
    ///
    /// The subject axis carries no field beside it: the paths whose findings
    /// went are the changeset's own entries, which the caller processed one by
    /// one to build it.
    pub affected_classes: BTreeSet<ClassKey>,
    /// The findings the changeset discarded on both axes — those recorded about
    /// a changed path, and those in [`IncrementOutcome::affected_classes`]. A
    /// finding in both is counted once, because the first discard is what
    /// removed it.
    pub invalidated: Invalidation,
}

/// Apply one changeset and report what it did.
///
/// The transaction is taken `IMMEDIATE`, so the write lock is held from the
/// first statement: a deferred transaction that upgrades halfway through can
/// fail after fact rows have already been discarded.
pub(crate) fn apply(
    store: &mut Store,
    counters: &mut DerivationCounters,
    provenance: IncrementProvenance,
    changes: impl IntoIterator<Item = Change>,
) -> Result<IncrementOutcome, StoreError> {
    // The first entry is what decides whether there is an act at all, so it is
    // taken before anything is opened: an empty changeset writes nothing, takes
    // no generation and holds no lock.
    let mut entries = changes.into_iter();
    let Some(first) = entries.next() else {
        return Ok(IncrementOutcome {
            provenance,
            generation: None,
            documents_upserted: 0,
            documents_deleted: 0,
            tombstones_recorded: 0,
            affected_classes: BTreeSet::new(),
            invalidated: Invalidation::default(),
        });
    };

    // One clock reading for the changeset, as there is one generation. Nothing
    // orders by it; it is what a person reads in a report.
    let recorded_at = request::unix_seconds();
    let transaction = store
        .connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|error| error::sql("opening the increment transaction", error))?;
    let generation = store::next_generation(&transaction)?;

    let mut tally = Tally::default();
    {
        let mut statements = Statements::prepare(&transaction)?;
        for (index, change) in std::iter::once(first).chain(entries).enumerate() {
            let subject = subject(&change);
            // Every refusal from here is wrapped in the entry that caused it.
            // A heal streaming fifty thousand entries fails on whichever one is
            // pathological, and the operation alone reads the same for all of
            // them.
            let applied = match &change {
                Change::Upsert(facts) => {
                    upsert(&mut statements, generation, recorded_at, facts, &mut tally)
                }
                // The death's own provenance, which is a different thing from
                // the changeset's mark this function was called with.
                Change::Death { path, provenance } => record_death(
                    &mut statements,
                    generation,
                    recorded_at,
                    path,
                    *provenance,
                    &mut tally,
                ),
            }
            .and_then(|()| discard_the_subject(&mut statements.discard_subject, subject));
            tally.findings_discarded +=
                applied.map_err(|problem| error::in_entry(index, subject, problem))?;
            torn_here(index + 1);
        }
        let discarded =
            discard_affected_classes(&mut statements.discard_class, &tally.affected_classes)?;
        tally.findings_discarded += discarded;
    }

    transaction
        .commit()
        .map_err(|error| error::sql("committing an increment", error))?;

    // Counters record what happened, so they move after the commit that made it
    // happen.
    counters.add(Counter::DocumentsUpserted, tally.documents_upserted);
    counters.add(Counter::DocumentsDeleted, tally.documents_deleted);
    counters.add(Counter::TombstonesRecorded, tally.tombstones_recorded);
    counters.add(Counter::FactRowsDiscarded, tally.fact_rows_discarded);
    counters.add(Counter::LinkRowsWritten, tally.link_rows);
    counters.add(Counter::HeadingRowsWritten, tally.heading_rows);
    counters.add(Counter::BlockRowsWritten, tally.block_rows);
    counters.add(Counter::TagRowsWritten, tally.tag_rows);
    counters.add(Counter::FrontmatterProjections, tally.projections);
    counters.add(Counter::FindingsDiscarded, tally.findings_discarded);

    Ok(IncrementOutcome {
        provenance,
        generation: Some(generation),
        documents_upserted: tally.documents_upserted,
        documents_deleted: tally.documents_deleted,
        tombstones_recorded: tally.tombstones_recorded,
        affected_classes: tally.affected_classes,
        invalidated: Invalidation {
            findings_discarded: tally.findings_discarded,
        },
    })
}

/// The point a changeset can be torn at, and the only one: between two entries,
/// with the transaction open and nothing committed.
///
/// It is a call to nothing without the `induced-failure` feature, so a shipped
/// build carries no check here.
#[inline]
fn torn_here(applied: usize) {
    #[cfg(feature = "induced-failure")]
    store::abort_if_the_changeset_is_torn(applied as u64);
    #[cfg(not(feature = "induced-failure"))]
    let _ = applied;
}

/// What one changeset accumulated, to be reported and counted once it committed.
///
/// It grows with the changeset and with nothing else: the counts are scalars and
/// the class set holds one key per distinct stem among the changed paths.
#[derive(Default)]
struct Tally {
    documents_upserted: u64,
    documents_deleted: u64,
    tombstones_recorded: u64,
    fact_rows_discarded: u64,
    link_rows: u64,
    heading_rows: u64,
    block_rows: u64,
    tag_rows: u64,
    projections: u64,
    findings_discarded: u64,
    affected_classes: BTreeSet<ClassKey>,
}

/// The statements one changeset runs, prepared once and reused for every entry.
///
/// **This is the whole of what lives across entries.** Preparing inside the loop
/// would compile the same SQL once per document, and holding the entries would
/// make a changeset cost its own size in facts on top of the rows it writes —
/// the iterator hands one over at a time and nothing here keeps it.
struct Statements<'t> {
    upsert_document: CachedStatement<'t>,
    /// One per fact table, in the order [`FACT_DISCARDS`] names them.
    discard_facts: Vec<CachedStatement<'t>>,
    insert_link: CachedStatement<'t>,
    insert_heading: CachedStatement<'t>,
    insert_block: CachedStatement<'t>,
    insert_tag: CachedStatement<'t>,
    delete_document: CachedStatement<'t>,
    record_tombstone: CachedStatement<'t>,
    /// The subject-scoped findings discard, over one changed path at a time.
    discard_subject: CachedStatement<'t>,
    /// The class-scoped findings discard, over one class at a time.
    discard_class: CachedStatement<'t>,
}

/// The fact rows a re-derivation replaces, one statement per table.
///
/// Nothing diffs and nothing merges: the text layer's output for a document is
/// the complete answer for that document, so replacing is both correct and the
/// only shape in which a fact row's ordinal means what it says.
const FACT_DISCARDS: &[&str] = &[
    "DELETE FROM links WHERE document = ?1",
    "DELETE FROM headings WHERE document = ?1",
    "DELETE FROM blocks WHERE document = ?1",
    "DELETE FROM document_tags WHERE document = ?1",
];

impl<'t> Statements<'t> {
    fn prepare(transaction: &'t Transaction<'_>) -> Result<Self, StoreError> {
        let prepared = |sql: &str, what: &'static str| {
            transaction
                .prepare_cached(sql)
                .map_err(move |error| error::sql(what, error))
        };
        Ok(Statements {
            // The document row keeps its identity across a re-derivation: it is
            // updated rather than replaced, which is what lets a vector
            // reference a document that has been re-read since.
            upsert_document: prepared(
                "INSERT INTO documents (
                     path, suffix_key, content_hash, byte_length, body, body_offset, frontmatter,
                     frontmatter_diagnostic_count, generation, derived_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
                 ON CONFLICT(path) DO UPDATE SET
                     suffix_key                   = excluded.suffix_key,
                     content_hash                 = excluded.content_hash,
                     byte_length                  = excluded.byte_length,
                     body                         = excluded.body,
                     body_offset                  = excluded.body_offset,
                     frontmatter                  = excluded.frontmatter,
                     frontmatter_diagnostic_count = excluded.frontmatter_diagnostic_count,
                     generation                   = excluded.generation,
                     derived_at                   = excluded.derived_at
                 RETURNING id",
                "preparing a document write",
            )?,
            discard_facts: FACT_DISCARDS
                .iter()
                .map(|sql| prepared(sql, "preparing a fact-row discard"))
                .collect::<Result<Vec<CachedStatement<'t>>, StoreError>>()?,
            insert_link: prepared(
                "INSERT INTO links (
                     document, ordinal, family, embed, protocol, target, title, anchor,
                     block_ref, span_line, span_column, span_offset
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
                "preparing a link write",
            )?,
            insert_heading: prepared(
                "INSERT INTO headings (
                     document, ordinal, text, slug, level, span_line, span_column,
                     span_offset, body_offset, inside_container
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                "preparing a heading write",
            )?,
            insert_block: prepared(
                "INSERT INTO blocks (
                     document, ordinal, block_id, span_line, span_column, span_offset
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                "preparing a block write",
            )?,
            insert_tag: prepared(
                "INSERT INTO document_tags (
                     document, ordinal, name, source, span_line, span_column, span_offset
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                "preparing a tag write",
            )?,
            // The hash the tombstone carries comes back from the row this
            // removes, so the delete and the record read one value between them.
            delete_document: prepared(
                "DELETE FROM documents WHERE path = ?1 RETURNING content_hash",
                "preparing a document delete",
            )?,
            record_tombstone: prepared(
                "INSERT INTO tombstones (
                     path, last_content_hash, provenance, generation, recorded_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(path) DO UPDATE SET
                     last_content_hash = COALESCE(
                         excluded.last_content_hash, tombstones.last_content_hash
                     ),
                     provenance        = excluded.provenance,
                     generation        = excluded.generation,
                     recorded_at       = excluded.recorded_at",
                "preparing a tombstone write",
            )?,
            discard_subject: prepared(
                request::SUBJECT_DISCARD_SQL,
                "preparing a path's findings discard",
            )?,
            discard_class: prepared(
                &request::class_discard_sql(1),
                "preparing a class's findings discard",
            )?,
        })
    }
}

/// Write one document's facts, replacing everything derived from it.
fn upsert(
    statements: &mut Statements<'_>,
    generation: i64,
    derived_at: i64,
    facts: &DocumentFacts,
    tally: &mut Tally,
) -> Result<(), StoreError> {
    refuse_a_document_that_does_not_add_up(facts)?;
    let projection = facts
        .frontmatter
        .as_ref()
        .map(json::canonical_json)
        .transpose()?;

    let document: i64 = statements
        .upsert_document
        .query_row(
            params![
                facts.path.as_str(),
                facts.path.suffix_key(),
                facts.content_hash,
                facts.byte_length,
                facts.body,
                facts.body_offset,
                projection,
                facts.frontmatter_diagnostic_count,
                generation,
                derived_at,
            ],
            |row| row.get(0),
        )
        .map_err(|error| error::sql("writing a document row", error))?;

    for statement in &mut statements.discard_facts {
        tally.fact_rows_discarded += statement
            .execute(params![document])
            .map_err(|error| error::sql("discarding a document's fact rows", error))?
            as u64;
    }

    for (ordinal, link) in facts.links.iter().enumerate() {
        statements
            .insert_link
            .execute(params![
                document,
                ordinal as i64,
                link.family.as_str(),
                link.embed,
                link.protocol,
                link.target,
                link.title,
                link.anchor,
                link.block_ref,
                link.span.line,
                link.span.column,
                link.span.byte_offset,
            ])
            .map_err(|error| error::sql("writing a link row", error))?;
    }

    for (ordinal, heading) in facts.headings.iter().enumerate() {
        statements
            .insert_heading
            .execute(params![
                document,
                ordinal as i64,
                heading.text,
                heading.slug,
                heading.level,
                heading.span.line,
                heading.span.column,
                heading.span.byte_offset,
                heading.body_offset,
                heading.inside_container,
            ])
            .map_err(|error| error::sql("writing a heading row", error))?;
    }

    for (ordinal, block) in facts.blocks.iter().enumerate() {
        statements
            .insert_block
            .execute(params![
                document,
                ordinal as i64,
                block.block_id,
                block.span.map(|span| span.line),
                block.span.map(|span| span.column),
                block.span.map(|span| span.byte_offset),
            ])
            .map_err(|error| error::sql("writing a block row", error))?;
    }

    for (ordinal, tag) in facts.tags.iter().enumerate() {
        statements
            .insert_tag
            .execute(params![
                document,
                ordinal as i64,
                tag.name,
                tag.source.as_str(),
                tag.span.map(|span| span.line),
                tag.span.map(|span| span.column),
                tag.span.map(|span| span.byte_offset),
            ])
            .map_err(|error| error::sql("writing a tag row", error))?;
    }

    tally.documents_upserted += 1;
    tally.link_rows += facts.links.len() as u64;
    tally.heading_rows += facts.headings.len() as u64;
    tally.block_rows += facts.blocks.len() as u64;
    tally.tag_rows += facts.tags.len() as u64;
    if projection.is_some() {
        tally.projections += 1;
    }
    tally.affected_classes.insert(facts.path.class_key());
    Ok(())
}

/// Delete one document and record its death.
///
/// The document row goes, the cascade takes every fact row derived from it, and
/// a tombstone records the path, the hash it was last derived at, and where the
/// news came from — so there is no instant at which the document is gone and
/// nothing says why.
///
/// A path nothing had derived still gets a tombstone. The ordering it carries is
/// the point, and it is worth most exactly when a derivation never happened: a
/// late event then has something to compare against instead of guessing.
///
/// A path that dies twice **keeps the hash already recorded** where the second
/// death has none of its own. The recorded hash is the comparison basis the
/// tombstone exists to carry, and a death learned from an absent file carries
/// nothing better to put in its place.
fn record_death(
    statements: &mut Statements<'_>,
    generation: i64,
    recorded_at: i64,
    path: &DocumentPath,
    provenance: Provenance,
    tally: &mut Tally,
) -> Result<(), StoreError> {
    let last_content_hash: Option<String> = statements
        .delete_document
        .query_row(params![path.as_str()], |row| row.get(0))
        .optional()
        .map_err(|error| error::sql("deleting a document row", error))?;
    if last_content_hash.is_some() {
        tally.documents_deleted += 1;
    }

    statements
        .record_tombstone
        .execute(params![
            path.as_str(),
            last_content_hash,
            provenance.as_str(),
            generation,
            recorded_at,
        ])
        .map_err(|error| error::sql("recording a tombstone", error))?;

    tally.tombstones_recorded += 1;
    tally.affected_classes.insert(path.class_key());
    Ok(())
}

/// Refuse a document whose frame and body do not account for its size.
///
/// A document is its frontmatter block and then its body: `body_offset` is where
/// the body begins and the body runs to the end of the document, so
/// `body_offset` plus the body's length **is** the document's byte length. That
/// holds for every shape the text layer produces — no block, an unclosed block,
/// a byte-order mark, `CRLF` line endings — because the body it reports is a
/// slice of the document that ends where the document does.
///
/// So the three numbers are checkable against each other, and a set of them that
/// does not add up describes no file: a body offset past the end of the document
/// turns every stored span into an offset outside it, and a byte length that
/// disagrees with the body is a size a heal would compare against. This is the
/// one write path for a document row, so it is where the check belongs.
fn refuse_a_document_that_does_not_add_up(facts: &DocumentFacts) -> Result<(), StoreError> {
    let accounted = facts.body_offset.saturating_add(facts.body.len() as u64);
    if accounted == facts.byte_length {
        return Ok(());
    }
    let widest = |value: u64| usize::try_from(value).unwrap_or(usize::MAX);
    Err(StoreError::Bound {
        what: "a document's byte length, against the body offset and body accounting for it",
        limit: widest(facts.byte_length),
        given: widest(accounted),
    })
}

/// The path one entry is about, which is the subject its findings are recorded
/// under.
fn subject(change: &Change) -> &DocumentPath {
    match change {
        Change::Upsert(facts) => &facts.path,
        Change::Death { path, .. } => path,
    }
}

/// Discard the findings recorded about one changed path, and report how many
/// went.
///
/// A seek over `findings_path` rather than a predicate over the table: the
/// discard costs the path's own findings, and it runs once per entry, so a
/// changeset of fifty thousand entries is fifty thousand seeks rather than fifty
/// thousand scans.
///
/// A path named twice discards twice, and the second run finds nothing: the
/// entry ahead of it took the rows, and the caller records what holds now once
/// the whole changeset has committed.
fn discard_the_subject(
    statement: &mut CachedStatement<'_>,
    path: &DocumentPath,
) -> Result<u64, StoreError> {
    Ok(statement
        .execute(params![path.as_str()])
        .map_err(|error| error::sql("discarding a path's findings", error))? as u64)
}

/// Discard the findings in every class the changeset's paths affect, and report
/// how many went.
///
/// One seek per class rather than one predicate over all of them: each class is
/// a range over `finding_classes(class_key)`, and the statement that opens one
/// is prepared once and run per class. The count is findings rather than rows,
/// because a finding a previous range — or the subject axis — already took is
/// found gone rather than counted twice.
fn discard_affected_classes(
    statement: &mut CachedStatement<'_>,
    classes: &BTreeSet<ClassKey>,
) -> Result<u64, StoreError> {
    let mut discarded = 0_u64;
    for class in classes {
        let probe = class.probe();
        // The statement was compiled for one range and binds two parameters. A
        // class key is its own probe's single lower bound, so this holds by
        // construction — and a probe that ever opened two would bind half its
        // bounds and discard the wrong rows.
        debug_assert_eq!(
            probe.range_count(),
            1,
            "the class discard is prepared for one range"
        );
        discarded += statement
            .execute(request::probe_parameters(&probe))
            .map_err(|error| error::sql("discarding a class's findings", error))?
            as u64;
    }
    Ok(discarded)
}
