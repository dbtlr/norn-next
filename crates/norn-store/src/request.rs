//! One request against one store, and everything it derived.
//!
//! # A request is an attribution scope; the increment is the atomicity scope
//!
//! Every operation the store performs happens inside a request, and what that
//! buys is attribution: the counters belong to the request rather than to the
//! process, so "what did this cost" is answerable without subtracting two
//! readings of a shared number.
//!
//! It buys nothing about atomicity, and it is not where atomicity is decided.
//! **[`Request::apply_increment`] is where documents are written, and a
//! changeset lands whole or not at all** — see [`crate::increment`], which is
//! also where the shape of that guarantee is stated. Every other write here is
//! whole on its own: a finding and its candidate and class rows, a schema pin
//! and the discard the new key implies, a vector. So a request that performed
//! three acts and then failed has three whole acts at rest, and what a request
//! never was is a way to group them into one.
//!
//! # Writes replace one document's facts wholesale
//!
//! A document is re-derived by writing its row and replacing every fact row
//! derived from it. Nothing diffs and nothing merges: the text layer's output
//! for a document is the complete answer for that document, so replacing is both
//! correct and the only shape in which a fact row's ordinal means what it says.
//!
//! **The document row keeps its identity across a re-derivation.** It is updated
//! rather than replaced, which is what lets a vector reference a document that
//! has been re-read since.
//!
//! What is *not* here is retention: when a tombstone has outlived the disorder
//! it was recorded to survive is a policy over generations, and nothing in this
//! crate decides it.
//!
//! # Reads at rest, and the read builders that are not here
//!
//! The `stored_*` readers answer *what is at rest for this one path*, and take
//! no predicate, no sort and no page. That is the whole family: they are how a
//! heal decides whether a document changed and how a re-derivation compares what
//! it is about to write.
//!
//! The probe readers — [`Request::suffix_candidates`] and
//! [`Request::findings_in_class`] — are the range reads the resolution ladder
//! and the findings pillar are defined in terms of. They take a
//! [`SuffixProbe`], which is a set of index bounds, and read exactly the class it
//! opens. [`Request::probe_reader_plan`] hands their emitted SQL and its query
//! plan out, because a plan bar over hand-copied SQL judges a string nobody runs
//! — and no crate outside this one may open a connection to take a plan itself.
//!
//! Compiling request parameters into SQL — predicates, sorts, pages, the query
//! shapes that carry `EXPLAIN` bars — is the read builders' job, and they
//! compose these primitives rather than re-spelling the ranges.

use std::collections::BTreeSet;

use rusqlite::{
    Connection, OptionalExtension, Params, Row, TransactionBehavior, params, params_from_iter,
};

use crate::counters::{Counter, DerivationCounters};
use crate::ddl;
use crate::error::{self, StoreError};
use crate::facts::{
    BlockFact, CANDIDATE_HEAD, CandidateFact, FindingFacts, HeadingFact, Invalidation, LinkFact,
    LinkFamily, PillarReport, Provenance, SchemaPin, Span, StoredDocument, StoredFacts,
    StoredFinding, StoredTombstone, TagFact, TagSource, VaultSchemaPin, VectorFacts,
};
use crate::increment::{self, Change, IncrementOutcome, IncrementProvenance};
use crate::path::{ClassKey, DocumentPath, SuffixProbe};
use crate::store::{self, Store};

/// A row reading that can fail twice: the driver may not produce the row, and
/// the row may hold a value this schema does not describe.
type Reading<T> = rusqlite::Result<Result<T, StoreError>>;

/// The columns every finding reader selects, in the order [`stored_finding`]
/// reads them.
const FINDING_COLUMNS: &str = "id, kind, severity, path, target, span_line, span_column,
            span_offset, candidates_total, message, detail, vault_schema_fingerprint, generation";

/// How many finding ids one further-query batches its `IN` list by.
///
/// A class or a path can hold more findings than SQLite's 32766-parameter
/// bound leaves room for in one statement — the candidate and class reads bind
/// one parameter per id — so [`Request::findings`] chunks rather than binding
/// the whole list at once.
#[cfg(not(test))]
const FINDING_ID_CHUNK: usize = 500;
/// Shrunk under test, so a unit test can cross a chunk boundary without
/// writing tens of thousands of rows to do it.
#[cfg(test)]
const FINDING_ID_CHUNK: usize = 4;

/// One request's worth of work against a store.
pub struct Request<'a> {
    store: &'a mut Store,
    counters: DerivationCounters,
}

impl<'a> Request<'a> {
    pub(crate) fn new(store: &'a mut Store) -> Self {
        Request {
            store,
            counters: DerivationCounters::default(),
        }
    }

    /// What this request has derived so far.
    pub fn counters(&self) -> &DerivationCounters {
        &self.counters
    }

    /// End the request and hand back what it derived.
    pub fn finish(self) -> DerivationCounters {
        self.counters
    }

    // ---- writes ----

    /// Apply one changeset — document upserts and deaths — and report what it
    /// did.
    ///
    /// **The changeset is the unit of atomicity**: every entry lands, or none of
    /// them does. There is no scope to open and add to, because this entry
    /// point's granularity *is* the guarantee — a caller that needs two changes
    /// to land together hands them over together. Internally that is one
    /// `IMMEDIATE` transaction, so the write lock is held from the first
    /// statement rather than acquired halfway through.
    ///
    /// One generation stamps every row the changeset writes, entries apply in
    /// the order they arrive, and the findings in every ambiguity class the
    /// changed paths belong to are discarded before it commits — reported in
    /// [`IncrementOutcome::affected_classes`], because re-recording what holds
    /// now is the caller's. `provenance` marks where the post-state came from;
    /// it changes no statement, and what it binds is how a counter reading is
    /// read.
    ///
    /// [`crate::increment`] states all of it in full, the empty changeset and
    /// the duplicate-path rule included.
    pub fn apply_increment(
        &mut self,
        provenance: IncrementProvenance,
        changes: impl IntoIterator<Item = Change>,
    ) -> Result<IncrementOutcome, StoreError> {
        increment::apply(self.store, &mut self.counters, provenance, changes)
    }

    /// Record one finding, with the head of its candidates.
    ///
    /// The finding is stamped with the vault-schema fingerprint currently
    /// pinned, which is what a schema change invalidates it by. A store with no
    /// pinned schema stamps the empty fingerprint: a schema arriving later is a
    /// schema change, and these findings are invalidated by it exactly as they
    /// should be.
    ///
    /// Two shapes of head are refused rather than stored: one longer than
    /// [`CANDIDATE_HEAD`], and one longer than the total it claims to be the head
    /// of. The total is what makes the head a head, so a total below the head's
    /// own length describes no vault.
    ///
    /// **Every class the finding is in is written**, one `finding_classes` row
    /// each, because a target's reductions are disjoint classes and a finding
    /// filed under one of them is invisible to the other's maintenance. A finding
    /// that is not about resolution carries no class and writes no such row.
    pub fn record_finding(&mut self, finding: &FindingFacts) -> Result<(), StoreError> {
        if finding.candidates.len() > CANDIDATE_HEAD {
            return Err(StoreError::Bound {
                what: "a finding's candidate head",
                limit: CANDIDATE_HEAD,
                given: finding.candidates.len(),
            });
        }
        if finding.candidates_total < finding.candidates.len() as u64 {
            return Err(StoreError::Bound {
                what: "a finding's candidate total, against the head it heads",
                limit: finding.candidates_total as usize,
                given: finding.candidates.len(),
            });
        }
        let transaction = self
            .store
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| error::sql("opening the finding transaction", error))?;
        let generation = store::next_generation(&transaction)?;
        let fingerprint: String =
            store::get_meta(&transaction, ddl::meta::VAULT_SCHEMA_FINGERPRINT)?.unwrap_or_default();

        let id: i64 = transaction
            .query_row(
                "INSERT INTO findings (
                     vault_schema_fingerprint, generation, kind, severity, path, target,
                     span_line, span_column, span_offset, candidates_total, message, detail
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
                 RETURNING id",
                params![
                    fingerprint,
                    generation,
                    finding.kind,
                    finding.severity,
                    finding.path.as_str(),
                    finding.target,
                    finding.span.map(|span| span.line),
                    finding.span.map(|span| span.column),
                    finding.span.map(|span| span.byte_offset),
                    finding.candidates_total,
                    finding.message,
                    finding.detail,
                ],
                |row| row.get(0),
            )
            .map_err(|error| error::sql("writing a finding", error))?;

        {
            let mut insert = transaction
                .prepare(
                    "INSERT INTO finding_candidates (finding, rank, path, suffix)
                     VALUES (?1, ?2, ?3, ?4)",
                )
                .map_err(|error| error::sql("preparing a candidate write", error))?;
            for (rank, candidate) in finding.candidates.iter().enumerate() {
                insert
                    .execute(params![
                        id,
                        rank as i64,
                        candidate.path.as_str(),
                        candidate.suffix,
                    ])
                    .map_err(|error| error::sql("writing a finding candidate", error))?;
            }
        }

        {
            let mut insert = transaction
                .prepare("INSERT INTO finding_classes (finding, class_key) VALUES (?1, ?2)")
                .map_err(|error| error::sql("preparing a finding class write", error))?;
            for class_key in &finding.class_keys {
                insert
                    .execute(params![id, class_key.as_str()])
                    .map_err(|error| error::sql("writing a finding's class", error))?;
            }
        }

        transaction
            .commit()
            .map_err(|error| error::sql("committing a finding", error))?;
        self.counters.add(Counter::FindingsWritten, 1);
        Ok(())
    }

    /// Discard every finding in the class a probe opens, and report what went.
    ///
    /// The write side of class-scoped maintenance reached **by a class rather
    /// than by a change**: a caller that knows which class to re-derive says so
    /// here, where a changeset says it by naming the paths that moved and
    /// [`Request::apply_increment`] discards the classes those paths are in.
    /// Both run the same statement.
    ///
    /// This is also the reason findings need no cascade: a finding is keyed by
    /// path and class, outlives the document it is about, and leaves the table
    /// through this discard — counted, rather than as a side effect of a delete
    /// somewhere else.
    ///
    /// **Discard, then record, is the idempotence story.** Re-deriving a class is
    /// a discard followed by a [`Request::record_finding`] for each finding that
    /// holds now, so two derivations of one class cannot leave two copies and
    /// there is no dedupe rule for a caller to keep.
    ///
    /// A finding in more than one class goes **whole**: the membership row this
    /// range matched identifies the finding, and deleting the finding takes its
    /// other memberships with it. The count is findings rather than rows, because
    /// a cascade is not what `changes()` reports.
    pub fn discard_findings_in_class(
        &mut self,
        probe: &SuffixProbe,
    ) -> Result<Invalidation, StoreError> {
        let discarded = self
            .store
            .connection
            .execute(
                &class_discard_sql(probe.range_count()),
                probe_parameters(probe),
            )
            .map_err(|error| error::sql("discarding a class's findings", error))?
            as u64;
        self.counters.add(Counter::FindingsDiscarded, discarded);
        Ok(Invalidation {
            findings_discarded: discarded,
        })
    }

    /// Store one document's embedding under one model.
    ///
    /// An embedding for a `(document, model, version)` that already has one
    /// replaces it: a vector is a pure function of its inputs, so a second
    /// answer for the same inputs is the same answer recomputed.
    pub fn store_vector(&mut self, vector: &VectorFacts) -> Result<(), StoreError> {
        let transaction = self
            .store
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| error::sql("opening the vector transaction", error))?;
        let generation = store::next_generation(&transaction)?;
        let document: Option<i64> = transaction
            .query_row(
                "SELECT id FROM documents WHERE path = ?1",
                params![vector.path.as_str()],
                |row| row.get(0),
            )
            .optional()
            .map_err(|error| error::sql("reading a document row", error))?;
        let Some(document) = document else {
            return Err(StoreError::UnknownDocument {
                path: vector.path.as_str().to_string(),
            });
        };
        transaction
            .execute(
                "INSERT INTO document_vectors (
                     document, model_id, model_version, content_hash, dimensions, embedding,
                     generation
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                 ON CONFLICT(document, model_id, model_version) DO UPDATE SET
                     content_hash = excluded.content_hash,
                     dimensions   = excluded.dimensions,
                     embedding    = excluded.embedding,
                     generation   = excluded.generation",
                params![
                    document,
                    vector.model_id,
                    vector.model_version,
                    vector.content_hash,
                    vector.dimensions,
                    vector.embedding,
                    generation,
                ],
            )
            .map_err(|error| error::sql("writing a vector", error))?;
        transaction
            .commit()
            .map_err(|error| error::sql("committing a vector", error))?;
        self.counters.add(Counter::VectorsWritten, 1);
        Ok(())
    }

    /// Pin the vault schema's projection — the bytes, their fingerprint, and the
    /// generation the pin happened at — and discard the derived state the new key
    /// invalidates.
    ///
    /// The file remains the schema's sole authority. What this records is which
    /// schema derived state was derived under.
    ///
    /// **The pin and the discard are one transaction.** Splitting them leaves a
    /// generation in which the pinned key says a set of findings is dead and a
    /// request can still read them, and nothing outside the store can close that
    /// window. Parse-fact rows carry no schema key and are not touched: a schema
    /// edit re-derives exactly the tables it keys.
    ///
    /// Pinning a schema whose bytes and fingerprint already match the standing
    /// pin is **not a schema change**. It takes no generation, writes nothing and
    /// discards nothing, so a caller that pins on every attach does not move the
    /// store's write sequence for a schema that did not move.
    pub fn pin_vault_schema(
        &mut self,
        bytes: &[u8],
        fingerprint: &str,
    ) -> Result<SchemaPin, StoreError> {
        let transaction = self
            .store
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| error::sql("opening the schema-pin transaction", error))?;

        let pinned_fingerprint: Option<String> =
            store::get_meta(&transaction, ddl::meta::VAULT_SCHEMA_FINGERPRINT)?;
        let pinned_bytes: Option<Vec<u8>> =
            store::get_meta(&transaction, ddl::meta::VAULT_SCHEMA_BYTES)?;
        if pinned_fingerprint.as_deref() == Some(fingerprint)
            && pinned_bytes.as_deref() == Some(bytes)
        {
            let generation: i64 =
                store::get_meta(&transaction, ddl::meta::VAULT_SCHEMA_GENERATION)?
                    .unwrap_or_default();
            return Ok(SchemaPin {
                generation,
                repinned: false,
                invalidated: Invalidation::default(),
            });
        }

        let generation = store::next_generation(&transaction)?;
        store::put_meta(&transaction, ddl::meta::VAULT_SCHEMA_BYTES, bytes)?;
        store::put_meta(
            &transaction,
            ddl::meta::VAULT_SCHEMA_FINGERPRINT,
            fingerprint,
        )?;
        store::put_meta(&transaction, ddl::meta::VAULT_SCHEMA_GENERATION, generation)?;

        // Two open ranges rather than `<>`: an inequality is not a predicate an
        // index can answer, so it would read every finding in the table on every
        // pin — including the pins that discard nothing.
        let discarded = transaction
            .execute(
                "DELETE FROM findings
                 WHERE vault_schema_fingerprint < ?1 OR vault_schema_fingerprint > ?1",
                params![fingerprint],
            )
            .map_err(|error| error::sql("discarding schema-dependent state", error))?
            as u64;

        transaction
            .commit()
            .map_err(|error| error::sql("committing a schema pin", error))?;
        self.counters.add(Counter::VaultSchemaPins, 1);
        self.counters.add(Counter::FindingsDiscarded, discarded);
        Ok(SchemaPin {
            generation,
            repinned: true,
            invalidated: Invalidation {
                findings_discarded: discarded,
            },
        })
    }

    // ---- reads at rest ----

    /// One document's row, without its body or its fact rows.
    pub fn stored_document(
        &self,
        path: &DocumentPath,
    ) -> Result<Option<StoredDocument>, StoreError> {
        Self::read_one(
            &self.store.connection,
            "SELECT path, content_hash, byte_length, body_offset, frontmatter,
                    frontmatter_diagnostic_count, generation, derived_at
             FROM documents WHERE path = ?1",
            params![path.as_str()],
            |row| stored_document(row, 0),
            "reading a document row",
        )
    }

    /// One document's row, its body, and every fact row derived from it, in
    /// ordinal order.
    ///
    /// The row, the body and the document's id come back in one statement, and
    /// the four fact reads are keyed by that id — so the path is looked up once
    /// rather than once per fact table.
    ///
    /// **All five reads run inside one `DEFERRED` transaction, so they all see
    /// one WAL snapshot.** Without it, a second `Store` on the same path — the
    /// per-vault file lock that would rule that out is a later crate's, not
    /// this one's (NORN-33) — could commit a re-derivation between the document
    /// read and a fact read, pairing an old document row with the new fact
    /// rows. A snapshot taken once at the first statement is what a torn read
    /// like that cannot happen inside.
    pub fn stored_facts(&mut self, path: &DocumentPath) -> Result<Option<StoredFacts>, StoreError> {
        let transaction = self
            .store
            .connection
            .transaction_with_behavior(TransactionBehavior::Deferred)
            .map_err(|error| error::sql("opening the stored-facts snapshot", error))?;
        let found = Self::read_one(
            &transaction,
            "SELECT id, body, path, content_hash, byte_length, body_offset, frontmatter,
                    frontmatter_diagnostic_count, generation, derived_at
             FROM documents WHERE path = ?1",
            params![path.as_str()],
            |row| {
                let id: i64 = row.get(0)?;
                let body: String = row.get(1)?;
                Ok(stored_document(row, 2)?.map(|document| (id, body, document)))
            },
            "reading a document row",
        )?;
        let Some((id, body, document)) = found else {
            return Ok(None);
        };
        Ok(Some(StoredFacts {
            document,
            body,
            links: Self::read_all(
                &transaction,
                "SELECT family, embed, protocol, target, title, anchor, block_ref,
                        span_line, span_column, span_offset
                 FROM links WHERE document = ?1 ORDER BY ordinal",
                params![id],
                stored_link,
                "reading a document's links",
            )?,
            headings: Self::read_all(
                &transaction,
                "SELECT text, slug, level, span_line, span_column, span_offset, body_offset,
                        inside_container
                 FROM headings WHERE document = ?1 ORDER BY ordinal",
                params![id],
                stored_heading,
                "reading a document's headings",
            )?,
            blocks: Self::read_all(
                &transaction,
                "SELECT block_id, span_line, span_column, span_offset
                 FROM blocks WHERE document = ?1 ORDER BY ordinal",
                params![id],
                stored_block,
                "reading a document's block ids",
            )?,
            tags: Self::read_all(
                &transaction,
                "SELECT name, source, span_line, span_column, span_offset
                 FROM document_tags WHERE document = ?1 ORDER BY ordinal",
                params![id],
                stored_tag,
                "reading a document's tags",
            )?,
        }))
        // The transaction is never committed. A read takes no write lock worth
        // keeping and discards nothing on rollback, so letting it drop here
        // is the same outcome as a `COMMIT` without a statement that says so.
    }

    /// The death recorded for one path, if any.
    pub fn stored_tombstone(
        &self,
        path: &DocumentPath,
    ) -> Result<Option<StoredTombstone>, StoreError> {
        Self::read_one(
            &self.store.connection,
            "SELECT path, last_content_hash, provenance, generation, recorded_at
             FROM tombstones WHERE path = ?1",
            params![path.as_str()],
            stored_tombstone,
            "reading a tombstone",
        )
    }

    /// The findings recorded at one path, oldest generation first.
    pub fn stored_findings(&self, path: &DocumentPath) -> Result<Vec<StoredFinding>, StoreError> {
        let sql = format!(
            "SELECT {FINDING_COLUMNS} FROM findings WHERE path = ?1 ORDER BY generation, id"
        );
        self.findings(&sql, params![path.as_str()])
    }

    /// The pinned vault-schema projection, if a schema has been pinned.
    pub fn vault_schema_pin(&self) -> Result<Option<VaultSchemaPin>, StoreError> {
        let connection = &self.store.connection;
        let Some(bytes): Option<Vec<u8>> =
            store::get_meta(connection, ddl::meta::VAULT_SCHEMA_BYTES)?
        else {
            return Ok(None);
        };
        Ok(Some(VaultSchemaPin {
            bytes,
            fingerprint: store::get_meta(connection, ddl::meta::VAULT_SCHEMA_FINGERPRINT)?
                .unwrap_or_default(),
            generation: store::get_meta(connection, ddl::meta::VAULT_SCHEMA_GENERATION)?
                .unwrap_or_default(),
        }))
    }

    /// How much each pillar is holding.
    pub fn pillars(&self) -> Result<PillarReport, StoreError> {
        let count = |sql: &str| -> Result<u64, StoreError> {
            self.store
                .connection
                .query_row(sql, [], |row| row.get(0))
                .map_err(|error| error::sql("counting rows at rest", error))
        };
        Ok(PillarReport {
            documents: count("SELECT count(*) FROM documents")?,
            tombstones: count("SELECT count(*) FROM tombstones")?,
            findings: count("SELECT count(*) FROM findings")?,
            finding_candidates: count("SELECT count(*) FROM finding_candidates")?,
            vectors: count("SELECT count(*) FROM document_vectors")?,
            migrations_applied: count("SELECT count(*) FROM migrations")?,
        })
    }

    // ---- probe reads ----

    /// Every document in the class a probe opens, in suffix-key order.
    ///
    /// The full candidate enumeration behind a finding's bounded head, and the
    /// membership of an ambiguity class: ranges over `documents(suffix_key)`,
    /// costing the class rather than the vault.
    ///
    /// The order is total — `suffix_key` then `path` — because equal suffix keys
    /// are exactly what an ambiguity class is made of, and a ladder whose ties
    /// fall out in row-insertion order is a ladder that reorders itself when a
    /// document is re-derived.
    pub fn suffix_candidates(&self, probe: &SuffixProbe) -> Result<Vec<DocumentPath>, StoreError> {
        Self::read_all(
            &self.store.connection,
            &suffix_candidates_sql(probe.range_count()),
            probe_parameters(probe),
            |row| Ok(DocumentPath::new(&row.get::<_, String>(0)?)),
            "reading suffix candidates",
        )
    }

    /// Every finding belonging to an ambiguity class the probe opens.
    ///
    /// This is what scopes findings maintenance: a changed path names a class,
    /// and the findings a change to it can invalidate are exactly the ones with a
    /// membership row in this range. A finding belongs to one class per reduction
    /// of the target it is about, and **any** of them reaches it — which is what
    /// makes the no-miss direction true for a target whose two reductions are
    /// disjoint classes. A finding in two of the probe's own classes still comes
    /// back once.
    ///
    /// The read is **correct in the no-miss direction and a superset**: a
    /// longer-suffix finding in the same class is read even where the particular
    /// change cannot have reached it, which costs a re-decision rather than a
    /// stale finding nothing revisits.
    pub fn findings_in_class(&self, probe: &SuffixProbe) -> Result<Vec<StoredFinding>, StoreError> {
        self.findings(
            &findings_in_class_sql(probe.range_count()),
            probe_parameters(probe),
        )
    }

    /// The full-text matches for one FTS5 match expression, in path order.
    ///
    /// The range primitive the full-text read builder composes: it takes a match
    /// expression and returns the documents whose body the index says holds those
    /// terms. Snippets, ranking and paging are the builder's.
    ///
    /// It reads the index rather than the column, which is what makes it the
    /// pillar's observable behaviour: a `MATCH` that answers about text
    /// `documents.body` no longer carries is an index that has drifted, and
    /// [`crate::Store::verify_integrity`] is what states that as damage.
    pub fn full_text_matches(&self, expression: &str) -> Result<Vec<DocumentPath>, StoreError> {
        Self::read_all(
            &self.store.connection,
            "SELECT documents.path FROM documents_fts
             JOIN documents ON documents.id = documents_fts.rowid
             WHERE documents_fts MATCH ?1
             ORDER BY documents.path",
            params![expression],
            |row| Ok(DocumentPath::new(&row.get::<_, String>(0)?)),
            "reading full-text matches",
        )
    }

    /// The statement one probe reader emits for one probe, with the query plan
    /// SQLite reported for **that** statement.
    ///
    /// A plan bar is worth something only against the SQL that actually ran, and
    /// no crate outside this one may open a connection to take a plan of its own.
    /// So the store hands the pair out: the reader chooses the statement, the
    /// caller judges the plan. It is deliberately not a way to explain arbitrary
    /// SQL — the readers are named, and their statements stay this crate's.
    pub fn probe_reader_plan(
        &self,
        reader: ProbeReader,
        probe: &SuffixProbe,
    ) -> Result<EmittedPlan, StoreError> {
        let sql = match reader {
            ProbeReader::SuffixCandidates => suffix_candidates_sql(probe.range_count()),
            ProbeReader::FindingsInClass => findings_in_class_sql(probe.range_count()),
        };
        let steps = Self::read_all(
            &self.store.connection,
            &format!("EXPLAIN QUERY PLAN {sql}"),
            probe_parameters(probe),
            |row| {
                Ok(Ok(PlanStep {
                    id: row.get(0)?,
                    parent: row.get(1)?,
                    detail: row.get(3)?,
                }))
            },
            "explaining a probe reader",
        )?;
        Ok(EmittedPlan { sql, steps })
    }

    // ---- readers ----

    /// The findings one statement selects, each with the head of its candidates
    /// and the set of classes it is in.
    ///
    /// The candidates and the classes come back in one further statement each
    /// **per chunk of [`FINDING_ID_CHUNK`] ids** rather than one per finding, so
    /// reading a class costs a small, bounded number of round trips whatever the
    /// class holds — and never one `IN` list wide enough to trip SQLite's
    /// 32766-parameter cap.
    fn findings(
        &self,
        sql: &str,
        parameters: impl Params,
    ) -> Result<Vec<StoredFinding>, StoreError> {
        let found = Self::read_all(
            &self.store.connection,
            sql,
            parameters,
            stored_finding,
            "reading findings",
        )?;
        if found.is_empty() {
            return Ok(Vec::new());
        }
        let ids: Vec<i64> = found.iter().map(|(id, _)| *id).collect();

        let mut findings: Vec<StoredFinding> = Vec::with_capacity(found.len());
        let mut positions = std::collections::HashMap::with_capacity(found.len());
        for (position, (id, finding)) in found.into_iter().enumerate() {
            positions.insert(id, position);
            findings.push(finding);
        }

        for chunk in ids.chunks(FINDING_ID_CHUNK) {
            let placeholders = (1..=chunk.len())
                .map(|index| format!("?{index}"))
                .collect::<Vec<String>>()
                .join(", ");
            let candidates = Self::read_all(
                &self.store.connection,
                &format!(
                    "SELECT finding, path, suffix FROM finding_candidates
                     WHERE finding IN ({placeholders}) ORDER BY finding, rank"
                ),
                params_from_iter(chunk.iter()),
                stored_candidate,
                "reading a finding's candidates",
            )?;
            for (finding, candidate) in candidates {
                if let Some(position) = positions.get(&finding) {
                    findings[*position].candidates.push(candidate);
                }
            }
            let classes = Self::read_all(
                &self.store.connection,
                &format!(
                    "SELECT finding, class_key FROM finding_classes
                     WHERE finding IN ({placeholders}) ORDER BY finding, class_key"
                ),
                params_from_iter(chunk.iter()),
                stored_class,
                "reading a finding's classes",
            )?;
            for (finding, class_key) in classes {
                if let Some(position) = positions.get(&finding) {
                    findings[*position].class_keys.insert(class_key);
                }
            }
        }
        Ok(findings)
    }

    fn read_one<T>(
        connection: &Connection,
        sql: &str,
        parameters: impl Params,
        read: impl FnOnce(&Row<'_>) -> Reading<T>,
        operation: &'static str,
    ) -> Result<Option<T>, StoreError> {
        connection
            .query_row(sql, parameters, read)
            .optional()
            .map_err(|error| error::sql(operation, error))?
            .transpose()
    }

    fn read_all<T>(
        connection: &Connection,
        sql: &str,
        parameters: impl Params,
        read: impl FnMut(&Row<'_>) -> Reading<T>,
        operation: &'static str,
    ) -> Result<Vec<T>, StoreError> {
        let mut statement = connection
            .prepare(sql)
            .map_err(|error| error::sql(operation, error))?;
        let rows = statement
            .query_map(parameters, read)
            .map_err(|error| error::sql(operation, error))?;
        let mut found = Vec::new();
        for row in rows {
            found.push(row.map_err(|error| error::sql(operation, error))??);
        }
        Ok(found)
    }
}

/// Which probe reader [`Request::probe_reader_plan`] is asked about.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProbeReader {
    /// [`Request::suffix_candidates`].
    SuffixCandidates,
    /// [`Request::findings_in_class`].
    FindingsInClass,
}

/// One row of `EXPLAIN QUERY PLAN`, as SQLite reports it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlanStep {
    pub id: i64,
    pub parent: i64,
    pub detail: String,
}

/// The statement a reader emitted, paired with its plan.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EmittedPlan {
    pub sql: String,
    pub steps: Vec<PlanStep>,
}

/// The statement [`Request::suffix_candidates`] emits for a probe of
/// `ranges` ranges.
fn suffix_candidates_sql(ranges: usize) -> String {
    format!(
        "SELECT path FROM documents WHERE {} ORDER BY suffix_key, path",
        range_predicate("suffix_key", ranges)
    )
}

/// The statement [`Request::findings_in_class`] emits for a probe of `ranges`
/// ranges.
///
/// The membership rows are read through their own index and the findings are
/// reached by row id, which is why a finding in two of the probe's classes comes
/// back once rather than twice and why the `id` order needs no sorter.
fn findings_in_class_sql(ranges: usize) -> String {
    format!(
        "SELECT {FINDING_COLUMNS} FROM findings
         WHERE id IN (SELECT finding FROM finding_classes WHERE {})
         ORDER BY id",
        range_predicate("class_key", ranges)
    )
}

/// The statement that discards every finding in the classes a probe of `ranges`
/// ranges opens.
///
/// One builder, two execution sites: [`Request::discard_findings_in_class`] runs
/// it over a probe a caller named, and an increment runs it over each class its
/// changed paths are in. A second spelling of it would be a second answer to
/// "which findings does re-deriving a class take".
pub(crate) fn class_discard_sql(ranges: usize) -> String {
    format!(
        "DELETE FROM findings WHERE id IN (
             SELECT finding FROM finding_classes WHERE {}
         )",
        range_predicate("class_key", ranges)
    )
}

/// The predicate one probe's ranges open over `column`: a half-open range per
/// range, `OR`ed, each bound its own parameter.
///
/// Ranges rather than a pattern, and `OR`ed ranges rather than a wider one,
/// because each range is an index seek and their union is what a two-reduction
/// target opens.
fn range_predicate(column: &str, ranges: usize) -> String {
    (0..ranges)
        .map(|index| {
            let (lower, upper) = (index * 2 + 1, index * 2 + 2);
            format!("({column} >= ?{lower} AND {column} < ?{upper})")
        })
        .collect::<Vec<String>>()
        .join(" OR ")
}

/// A probe's bounds in the order [`range_predicate`] numbers them.
pub(crate) fn probe_parameters(probe: &SuffixProbe) -> impl Params {
    params_from_iter(
        probe
            .ranges()
            .flat_map(|(lower, upper)| [lower.to_string(), upper.to_string()])
            .collect::<Vec<String>>(),
    )
}

/// A document row, read from `first` onwards.
fn stored_document(row: &Row<'_>, first: usize) -> Reading<StoredDocument> {
    let path: String = row.get(first)?;
    let content_hash: String = row.get(first + 1)?;
    let byte_length: u64 = row.get(first + 2)?;
    let body_offset: u64 = row.get(first + 3)?;
    let frontmatter: Option<String> = row.get(first + 4)?;
    let frontmatter_diagnostic_count: u32 = row.get(first + 5)?;
    let generation: i64 = row.get(first + 6)?;
    let derived_at: i64 = row.get(first + 7)?;
    Ok(DocumentPath::new(&path).map(|path| StoredDocument {
        path,
        content_hash,
        byte_length,
        body_offset,
        frontmatter,
        frontmatter_diagnostic_count,
        generation,
        derived_at,
    }))
}

fn stored_tombstone(row: &Row<'_>) -> Reading<StoredTombstone> {
    let path: String = row.get(0)?;
    let last_content_hash: Option<String> = row.get(1)?;
    let written: String = row.get(2)?;
    let generation: i64 = row.get(3)?;
    let recorded_at: i64 = row.get(4)?;
    let Some(provenance) = Provenance::from_str(&written) else {
        return Ok(Err(unreadable("tombstones.provenance", &written)));
    };
    Ok(DocumentPath::new(&path).map(|path| StoredTombstone {
        path,
        last_content_hash,
        provenance,
        generation,
        recorded_at,
    }))
}

/// A finding and its row id, which is what its candidates and its classes are
/// read by.
fn stored_finding(row: &Row<'_>) -> Reading<(i64, StoredFinding)> {
    let id: i64 = row.get(0)?;
    let kind: String = row.get(1)?;
    let severity: String = row.get(2)?;
    let path: String = row.get(3)?;
    let target: Option<String> = row.get(4)?;
    let span = optional_span(row, 5)?;
    let candidates_total: u64 = row.get(8)?;
    let message: String = row.get(9)?;
    let detail: Option<String> = row.get(10)?;
    let vault_schema_fingerprint: String = row.get(11)?;
    let generation: i64 = row.get(12)?;
    Ok(DocumentPath::new(&path).map(|path| {
        (
            id,
            StoredFinding {
                kind,
                severity,
                path,
                class_keys: BTreeSet::new(),
                target,
                span,
                candidates: Vec::new(),
                candidates_total,
                message,
                detail,
                vault_schema_fingerprint,
                generation,
            },
        )
    }))
}

/// A candidate and the finding it heads.
fn stored_candidate(row: &Row<'_>) -> Reading<(i64, CandidateFact)> {
    let finding: i64 = row.get(0)?;
    let path: String = row.get(1)?;
    let suffix: String = row.get(2)?;
    Ok(DocumentPath::new(&path).map(|path| (finding, CandidateFact { path, suffix })))
}

/// A class and the finding that belongs to it.
///
/// The key is read back through [`ClassKey::new`] rather than trusted: a stored
/// key that no probe opens is a finding at rest, so it is damage rather than a
/// value to hand on.
fn stored_class(row: &Row<'_>) -> Reading<(i64, ClassKey)> {
    let finding: i64 = row.get(0)?;
    let written: String = row.get(1)?;
    Ok(match ClassKey::new(&written) {
        Ok(class_key) => Ok((finding, class_key)),
        Err(_) => Err(unreadable("finding_classes.class_key", &written)),
    })
}

fn stored_link(row: &Row<'_>) -> Reading<LinkFact> {
    let written: String = row.get(0)?;
    let Some(family) = LinkFamily::from_str(&written) else {
        return Ok(Err(unreadable("links.family", &written)));
    };
    Ok(Ok(LinkFact {
        family,
        embed: row.get(1)?,
        protocol: row.get(2)?,
        target: row.get(3)?,
        title: row.get(4)?,
        anchor: row.get(5)?,
        block_ref: row.get(6)?,
        span: Span {
            line: row.get(7)?,
            column: row.get(8)?,
            byte_offset: row.get(9)?,
        },
    }))
}

fn stored_heading(row: &Row<'_>) -> Reading<HeadingFact> {
    Ok(Ok(HeadingFact {
        text: row.get(0)?,
        slug: row.get(1)?,
        level: row.get(2)?,
        span: Span {
            line: row.get(3)?,
            column: row.get(4)?,
            byte_offset: row.get(5)?,
        },
        body_offset: row.get(6)?,
        inside_container: row.get(7)?,
    }))
}

fn stored_block(row: &Row<'_>) -> Reading<BlockFact> {
    Ok(Ok(BlockFact {
        block_id: row.get(0)?,
        span: optional_span(row, 1)?,
    }))
}

fn stored_tag(row: &Row<'_>) -> Reading<TagFact> {
    let written: String = row.get(1)?;
    let Some(source) = TagSource::from_str(&written) else {
        return Ok(Err(unreadable("document_tags.source", &written)));
    };
    Ok(Ok(TagFact {
        name: row.get(0)?,
        source,
        span: optional_span(row, 2)?,
    }))
}

/// A span read from three nullable columns.
///
/// The columns are written together and a `CHECK` refuses a row where only some
/// of them are set, so "any one absent means no span" reads every row that can
/// exist rather than repairing a half-recorded position.
fn optional_span(row: &Row<'_>, first: usize) -> rusqlite::Result<Option<Span>> {
    let line: Option<u64> = row.get(first)?;
    let column: Option<u64> = row.get(first + 1)?;
    let byte_offset: Option<u64> = row.get(first + 2)?;
    Ok(match (line, column, byte_offset) {
        (Some(line), Some(column), Some(byte_offset)) => Some(Span {
            line,
            column,
            byte_offset,
        }),
        _ => None,
    })
}

/// A stored value outside the vocabulary its column holds. Damaged rather than
/// merely unexpected: the writer is this crate, so a value nothing here writes
/// means the row was not written by it.
fn unreadable(column: &str, written: &str) -> StoreError {
    StoreError::Damaged {
        what: format!("`{column}` holds `{written}`, which is not a value this schema writes"),
    }
}

/// Unix seconds, for the columns a person reads. Nothing orders by it.
pub(crate) fn unix_seconds() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_secs() as i64)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// [`findings`] chunks its two follow-up `IN` lists at [`FINDING_ID_CHUNK`],
    /// which this build shrinks to 4. Ten findings, each with one candidate and
    /// one class of its own, cross that boundary twice, and every one has to
    /// come back paired with the candidate and class it — and only it — wrote.
    #[test]
    fn findings_reassemble_correctly_across_a_chunk_boundary() {
        let root =
            std::env::temp_dir().join(format!("norn-store-request-chunk-{}", std::process::id()));
        let mut store = Store::open_throwaway(root.join("store.sqlite3")).expect("opening a store");
        let subject = DocumentPath::new("notes.md").expect("a document path");

        let mut request = store.begin_request();
        for index in 0..10 {
            let class_key = ClassKey::new(&format!("class-{index}/")).expect("a class key");
            request
                .record_finding(&FindingFacts {
                    kind: "test/chunking".to_string(),
                    severity: "warning".to_string(),
                    path: subject.clone(),
                    class_keys: [class_key].into_iter().collect(),
                    target: None,
                    span: None,
                    candidates: vec![CandidateFact {
                        path: DocumentPath::new(&format!("candidate-{index}.md")).unwrap(),
                        suffix: format!("candidate-{index}"),
                    }],
                    candidates_total: 1,
                    message: format!("finding {index}"),
                    detail: None,
                })
                .expect("recording a finding");
        }

        let found = request.stored_findings(&subject).expect("reading findings");
        assert_eq!(found.len(), 10);
        for (index, finding) in found.iter().enumerate() {
            assert_eq!(
                finding.candidates.len(),
                1,
                "finding {index} lost or gained a candidate at a chunk boundary"
            );
            assert_eq!(
                finding.candidates[0].path.as_str(),
                format!("candidate-{index}.md")
            );
            let expected = ClassKey::new(&format!("class-{index}/")).expect("a class key");
            assert!(
                finding.class_keys.contains(&expected),
                "finding {index} lost its class at a chunk boundary"
            );
        }
    }
}
