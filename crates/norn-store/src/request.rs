//! One request against one store, and everything it derived.
//!
//! Every operation the store performs happens inside a request, which is what
//! makes derivation attributable: the counters belong to the request rather than
//! to the process, so "what did this cost" is answerable without subtracting two
//! readings of a shared number.
//!
//! # Writes replace one document's facts wholesale
//!
//! A document is re-derived by writing its row and replacing every fact row
//! derived from it, in one transaction. Nothing diffs and nothing merges: the
//! text layer's output for a document is the complete answer for that document,
//! so replacing is both correct and the only shape in which a fact row's
//! ordinal means what it says.
//!
//! **The document row keeps its identity across a re-derivation.** It is updated
//! rather than replaced, which is what lets a finding or a vector reference a
//! document that has been re-read since.
//!
//! What is *not* here is the increment: which documents a change reaches, how a
//! changeset is ordered, how a torn write is detected, and when a tombstone has
//! outlived its purpose. Those compose these operations rather than replacing
//! them.
//!
//! # Reads at rest, and the read builders that are not here
//!
//! The `stored_*` readers answer *what is at rest for this one path*, and take
//! no predicate, no sort and no page. That is the whole family: they are how a
//! heal decides whether a document changed and how a re-derivation compares what
//! it is about to write.
//!
//! The two probe readers — [`Request::suffix_candidates`] and
//! [`Request::findings_in_class`] — are the range reads the resolution ladder
//! and the findings pillar are defined in terms of. They take a
//! [`SuffixProbe`], which is a pair of index bounds, and read exactly the class
//! it opens.
//!
//! Compiling request parameters into SQL — predicates, sorts, pages, the query
//! shapes that carry `EXPLAIN` bars — is the read builders' job, and they
//! compose these primitives rather than re-spelling the ranges.

use rusqlite::{OptionalExtension, Params, Row, TransactionBehavior, params};

use crate::counters::{Counter, DerivationCounters};
use crate::ddl;
use crate::error::{self, StoreError};
use crate::facts::{
    BlockFact, CANDIDATE_HEAD, CandidateFact, Deletion, DocumentFacts, FindingFacts, HeadingFact,
    Invalidation, LinkFact, LinkFamily, PillarReport, Provenance, Span, StoredDocument,
    StoredFacts, StoredFinding, StoredTombstone, TagFact, TagSource, VaultSchemaPin, VectorFacts,
};
use crate::json;
use crate::path::{DocumentPath, SuffixProbe};
use crate::store::{self, Store};

/// A row reading that can fail twice: the driver may not produce the row, and
/// the row may hold a value this schema does not describe.
type Reading<T> = rusqlite::Result<Result<T, StoreError>>;

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

    /// Write one document's facts, replacing everything derived from it.
    ///
    /// One transaction, taken `IMMEDIATE` so that the write lock is held from
    /// the first statement: a deferred transaction that upgrades halfway through
    /// can fail after the fact rows have been discarded.
    pub fn upsert_document(&mut self, facts: &DocumentFacts) -> Result<(), StoreError> {
        let projection = facts.frontmatter.as_ref().map(json::canonical_json);
        let derived_at = unix_seconds();
        let transaction = self
            .store
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| error::sql("opening the upsert transaction", error))?;
        let generation = store::next_generation(&transaction)?;

        let document: i64 = transaction
            .query_row(
                "INSERT INTO documents (
                     path, suffix_key, stem, depth, content_hash, byte_length, body,
                     body_offset, frontmatter, diagnostic_count, generation, derived_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
                 ON CONFLICT(path) DO UPDATE SET
                     suffix_key       = excluded.suffix_key,
                     stem             = excluded.stem,
                     depth            = excluded.depth,
                     content_hash     = excluded.content_hash,
                     byte_length      = excluded.byte_length,
                     body             = excluded.body,
                     body_offset      = excluded.body_offset,
                     frontmatter      = excluded.frontmatter,
                     diagnostic_count = excluded.diagnostic_count,
                     generation       = excluded.generation,
                     derived_at       = excluded.derived_at
                 RETURNING id",
                params![
                    facts.path.as_str(),
                    facts.path.suffix_key(),
                    facts.path.stem(),
                    facts.path.depth() as i64,
                    facts.content_hash,
                    facts.byte_length,
                    facts.body,
                    facts.body_offset,
                    projection,
                    facts.diagnostic_count,
                    generation,
                    derived_at,
                ],
                |row| row.get(0),
            )
            .map_err(|error| error::sql("writing a document row", error))?;

        let mut discarded = 0_u64;
        for statement in [
            "DELETE FROM links WHERE document = ?1",
            "DELETE FROM headings WHERE document = ?1",
            "DELETE FROM blocks WHERE document = ?1",
            "DELETE FROM document_tags WHERE document = ?1",
        ] {
            discarded += transaction
                .execute(statement, params![document])
                .map_err(|error| error::sql("discarding a document's fact rows", error))?
                as u64;
        }

        {
            let mut insert = transaction
                .prepare(
                    "INSERT INTO links (
                         document, ordinal, family, embed, protocol, target, title, anchor,
                         block_ref, span_line, span_column, span_offset
                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
                )
                .map_err(|error| error::sql("preparing a link write", error))?;
            for (ordinal, link) in facts.links.iter().enumerate() {
                insert
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
        }

        {
            let mut insert = transaction
                .prepare(
                    "INSERT INTO headings (
                         document, ordinal, text, slug, level, span_line, span_column,
                         span_offset, body_offset, inside_container
                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                )
                .map_err(|error| error::sql("preparing a heading write", error))?;
            for (ordinal, heading) in facts.headings.iter().enumerate() {
                insert
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
        }

        {
            let mut insert = transaction
                .prepare(
                    "INSERT INTO blocks (
                         document, ordinal, block_id, span_line, span_column, span_offset
                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                )
                .map_err(|error| error::sql("preparing a block write", error))?;
            for (ordinal, block) in facts.blocks.iter().enumerate() {
                insert
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
        }

        {
            let mut insert = transaction
                .prepare(
                    "INSERT INTO document_tags (
                         document, ordinal, name, source, span_line, span_column, span_offset
                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                )
                .map_err(|error| error::sql("preparing a tag write", error))?;
            for (ordinal, tag) in facts.tags.iter().enumerate() {
                insert
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
        }

        transaction
            .commit()
            .map_err(|error| error::sql("committing a document write", error))?;

        // Counters record what happened, so they move after the commit that
        // made it happen.
        self.counters.add(Counter::DocumentsUpserted, 1);
        self.counters.add(Counter::FactRowsDiscarded, discarded);
        self.counters
            .add(Counter::LinkRowsWritten, facts.links.len() as u64);
        self.counters
            .add(Counter::HeadingRowsWritten, facts.headings.len() as u64);
        self.counters
            .add(Counter::BlockRowsWritten, facts.blocks.len() as u64);
        self.counters
            .add(Counter::TagRowsWritten, facts.tags.len() as u64);
        if projection.is_some() {
            self.counters.add(Counter::FrontmatterProjections, 1);
        }
        Ok(())
    }

    /// Delete one document and record its death.
    ///
    /// The document row goes, the cascade takes every fact row derived from it,
    /// and a tombstone records the path, the hash it was last derived at, and
    /// where the news came from — all in one transaction, so there is no instant
    /// at which the document is gone and nothing says why.
    ///
    /// A path nothing had derived still gets a tombstone. The ordering it
    /// carries is the point, and it is worth most exactly when a derivation
    /// never happened: a late event then has something to compare against
    /// instead of guessing.
    pub fn delete_document(
        &mut self,
        path: &DocumentPath,
        provenance: Provenance,
    ) -> Result<Deletion, StoreError> {
        let recorded_at = unix_seconds();
        let transaction = self
            .store
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| error::sql("opening the delete transaction", error))?;
        let generation = store::next_generation(&transaction)?;

        let last_content_hash: Option<String> = transaction
            .query_row(
                "DELETE FROM documents WHERE path = ?1 RETURNING content_hash",
                params![path.as_str()],
                |row| row.get(0),
            )
            .optional()
            .map_err(|error| error::sql("deleting a document row", error))?;
        let removed = last_content_hash.is_some();

        transaction
            .execute(
                "INSERT INTO tombstones (
                     path, suffix_key, stem, last_content_hash, provenance, generation, recorded_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                 ON CONFLICT(path) DO UPDATE SET
                     suffix_key        = excluded.suffix_key,
                     stem              = excluded.stem,
                     last_content_hash = excluded.last_content_hash,
                     provenance        = excluded.provenance,
                     generation        = excluded.generation,
                     recorded_at       = excluded.recorded_at",
                params![
                    path.as_str(),
                    path.suffix_key(),
                    path.stem(),
                    last_content_hash,
                    provenance.as_str(),
                    generation,
                    recorded_at,
                ],
            )
            .map_err(|error| error::sql("recording a tombstone", error))?;

        transaction
            .commit()
            .map_err(|error| error::sql("committing a document delete", error))?;

        if removed {
            self.counters.add(Counter::DocumentsDeleted, 1);
        }
        self.counters.add(Counter::TombstonesRecorded, 1);
        Ok(Deletion {
            removed,
            generation,
        })
    }

    /// Record one finding, with the head of its candidates.
    ///
    /// The finding is stamped with the vault-schema fingerprint currently
    /// pinned, which is what a schema change invalidates it by. A store with no
    /// pinned schema stamps the empty fingerprint: a schema arriving later is a
    /// schema change, and these findings are invalidated by it exactly as they
    /// should be.
    pub fn record_finding(&mut self, finding: &FindingFacts) -> Result<(), StoreError> {
        if finding.candidates.len() > CANDIDATE_HEAD {
            return Err(StoreError::Bound {
                what: "a finding's candidate head",
                limit: CANDIDATE_HEAD,
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
                     vault_schema_fingerprint, generation, kind, severity, path, document,
                     class_key, target, span_line, span_column, span_offset, candidates_total,
                     message, detail
                 ) VALUES (
                     ?1, ?2, ?3, ?4, ?5, (SELECT id FROM documents WHERE path = ?5),
                     ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13
                 ) RETURNING id",
                params![
                    fingerprint,
                    generation,
                    finding.kind,
                    finding.severity,
                    finding.path.as_str(),
                    finding.class_key,
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

        transaction
            .commit()
            .map_err(|error| error::sql("committing a finding", error))?;
        self.counters.add(Counter::FindingsWritten, 1);
        Ok(())
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

    /// Pin the vault schema's projection: the bytes, their fingerprint, and the
    /// generation the pin happened at.
    ///
    /// The file remains the schema's sole authority. What this records is which
    /// schema derived state was derived under.
    pub fn pin_vault_schema(&mut self, bytes: &[u8], fingerprint: &str) -> Result<i64, StoreError> {
        let transaction = self
            .store
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| error::sql("opening the schema-pin transaction", error))?;
        let generation = store::next_generation(&transaction)?;
        store::put_meta(&transaction, ddl::meta::VAULT_SCHEMA_BYTES, bytes)?;
        store::put_meta(
            &transaction,
            ddl::meta::VAULT_SCHEMA_FINGERPRINT,
            fingerprint,
        )?;
        store::put_meta(&transaction, ddl::meta::VAULT_SCHEMA_GENERATION, generation)?;
        transaction
            .commit()
            .map_err(|error| error::sql("committing a schema pin", error))?;
        self.counters.add(Counter::VaultSchemaPins, 1);
        Ok(generation)
    }

    /// Discard the derived state the pinned vault schema keys, and report what
    /// went.
    ///
    /// The fingerprint is read from the pin rather than passed in, so what is
    /// kept cannot disagree with what is pinned. Parse-fact rows carry no schema
    /// key and are not touched: a schema edit re-derives exactly the tables it
    /// keys.
    pub fn discard_schema_dependent(&mut self) -> Result<Invalidation, StoreError> {
        let transaction = self
            .store
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| error::sql("opening the invalidation transaction", error))?;
        let fingerprint: String =
            store::get_meta(&transaction, ddl::meta::VAULT_SCHEMA_FINGERPRINT)?.unwrap_or_default();
        let discarded = transaction
            .execute(
                "DELETE FROM findings WHERE vault_schema_fingerprint <> ?1",
                params![fingerprint],
            )
            .map_err(|error| error::sql("discarding schema-dependent state", error))?
            as u64;
        transaction
            .commit()
            .map_err(|error| error::sql("committing an invalidation", error))?;
        self.counters.add(Counter::FindingsDiscarded, discarded);
        Ok(Invalidation {
            findings_discarded: discarded,
        })
    }

    // ---- reads at rest ----

    /// One document's row, without its body or its fact rows.
    pub fn stored_document(
        &self,
        path: &DocumentPath,
    ) -> Result<Option<StoredDocument>, StoreError> {
        self.read_one(
            "SELECT path, content_hash, byte_length, body_offset, frontmatter,
                    diagnostic_count, generation, derived_at
             FROM documents WHERE path = ?1",
            params![path.as_str()],
            stored_document,
            "reading a document row",
        )
    }

    /// One document's row, its body, and every fact row derived from it, in
    /// ordinal order.
    pub fn stored_facts(&self, path: &DocumentPath) -> Result<Option<StoredFacts>, StoreError> {
        let Some(document) = self.stored_document(path)? else {
            return Ok(None);
        };
        let body: String = self
            .store
            .connection
            .query_row(
                "SELECT body FROM documents WHERE path = ?1",
                params![path.as_str()],
                |row| row.get(0),
            )
            .map_err(|error| error::sql("reading a document body", error))?;
        Ok(Some(StoredFacts {
            document,
            body,
            links: self.read_all(
                "SELECT family, embed, protocol, target, title, anchor, block_ref,
                        span_line, span_column, span_offset
                 FROM links WHERE document = (SELECT id FROM documents WHERE path = ?1)
                 ORDER BY ordinal",
                params![path.as_str()],
                stored_link,
                "reading a document's links",
            )?,
            headings: self.read_all(
                "SELECT text, slug, level, span_line, span_column, span_offset, body_offset,
                        inside_container
                 FROM headings WHERE document = (SELECT id FROM documents WHERE path = ?1)
                 ORDER BY ordinal",
                params![path.as_str()],
                stored_heading,
                "reading a document's headings",
            )?,
            blocks: self.read_all(
                "SELECT block_id, span_line, span_column, span_offset
                 FROM blocks WHERE document = (SELECT id FROM documents WHERE path = ?1)
                 ORDER BY ordinal",
                params![path.as_str()],
                stored_block,
                "reading a document's block ids",
            )?,
            tags: self.read_all(
                "SELECT name, source, span_line, span_column, span_offset
                 FROM document_tags WHERE document = (SELECT id FROM documents WHERE path = ?1)
                 ORDER BY ordinal",
                params![path.as_str()],
                stored_tag,
                "reading a document's tags",
            )?,
        }))
    }

    /// The death recorded for one path, if any.
    pub fn stored_tombstone(
        &self,
        path: &DocumentPath,
    ) -> Result<Option<StoredTombstone>, StoreError> {
        self.read_one(
            "SELECT path, last_content_hash, provenance, generation, recorded_at
             FROM tombstones WHERE path = ?1",
            params![path.as_str()],
            stored_tombstone,
            "reading a tombstone",
        )
    }

    /// The findings recorded at one path, oldest generation first.
    pub fn stored_findings(&self, path: &DocumentPath) -> Result<Vec<StoredFinding>, StoreError> {
        self.findings(
            "WHERE path = ?1 ORDER BY generation, id",
            params![path.as_str()],
        )
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
    /// membership of an ambiguity class: one range over `documents(suffix_key)`,
    /// costing the class rather than the vault.
    pub fn suffix_candidates(&self, probe: &SuffixProbe) -> Result<Vec<DocumentPath>, StoreError> {
        self.read_all(
            "SELECT path FROM documents
             WHERE suffix_key >= ?1 AND suffix_key < ?2
             ORDER BY suffix_key",
            params![probe.lower(), probe.upper()],
            |row| Ok(DocumentPath::new(&row.get::<_, String>(0)?)),
            "reading suffix candidates",
        )
    }

    /// Every finding whose ambiguity class the probe opens.
    ///
    /// This is what scopes findings maintenance: a changed path names a class,
    /// and the findings a change to it can invalidate are exactly the ones in
    /// this range. Nothing outside the class is read, and no finding is
    /// revisited because the document it was written in happened to change.
    pub fn findings_in_class(&self, probe: &SuffixProbe) -> Result<Vec<StoredFinding>, StoreError> {
        self.findings(
            "WHERE class_key >= ?1 AND class_key < ?2 ORDER BY class_key, id",
            params![probe.lower(), probe.upper()],
        )
    }

    // ---- readers ----

    /// The findings one predicate selects, each with the head of its candidates.
    fn findings(
        &self,
        predicate: &str,
        parameters: impl Params,
    ) -> Result<Vec<StoredFinding>, StoreError> {
        let sql = format!(
            "SELECT id, kind, severity, path, class_key, target, span_line, span_column,
                    span_offset, candidates_total, message, detail, vault_schema_fingerprint,
                    generation
             FROM findings {predicate}"
        );
        let found = self.read_all(&sql, parameters, stored_finding, "reading findings")?;
        let mut findings = Vec::with_capacity(found.len());
        for (id, mut finding) in found {
            finding.candidates = self.read_all(
                "SELECT path, suffix FROM finding_candidates WHERE finding = ?1 ORDER BY rank",
                params![id],
                stored_candidate,
                "reading a finding's candidates",
            )?;
            findings.push(finding);
        }
        Ok(findings)
    }

    fn read_one<T>(
        &self,
        sql: &str,
        parameters: impl Params,
        read: impl FnOnce(&Row<'_>) -> Reading<T>,
        operation: &'static str,
    ) -> Result<Option<T>, StoreError> {
        self.store
            .connection
            .query_row(sql, parameters, read)
            .optional()
            .map_err(|error| error::sql(operation, error))?
            .transpose()
    }

    fn read_all<T>(
        &self,
        sql: &str,
        parameters: impl Params,
        read: impl FnMut(&Row<'_>) -> Reading<T>,
        operation: &'static str,
    ) -> Result<Vec<T>, StoreError> {
        let mut statement = self
            .store
            .connection
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

fn stored_document(row: &Row<'_>) -> Reading<StoredDocument> {
    let path: String = row.get(0)?;
    let content_hash: String = row.get(1)?;
    let byte_length: u64 = row.get(2)?;
    let body_offset: u64 = row.get(3)?;
    let frontmatter: Option<String> = row.get(4)?;
    let diagnostic_count: u32 = row.get(5)?;
    let generation: i64 = row.get(6)?;
    let derived_at: i64 = row.get(7)?;
    Ok(DocumentPath::new(&path).map(|path| StoredDocument {
        path,
        content_hash,
        byte_length,
        body_offset,
        frontmatter,
        diagnostic_count,
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

/// A finding and its row id, which is what its candidates are read by.
fn stored_finding(row: &Row<'_>) -> Reading<(i64, StoredFinding)> {
    let id: i64 = row.get(0)?;
    let kind: String = row.get(1)?;
    let severity: String = row.get(2)?;
    let path: String = row.get(3)?;
    let class_key: Option<String> = row.get(4)?;
    let target: Option<String> = row.get(5)?;
    let span = optional_span(row, 6)?;
    let candidates_total: u64 = row.get(9)?;
    let message: String = row.get(10)?;
    let detail: Option<String> = row.get(11)?;
    let vault_schema_fingerprint: String = row.get(12)?;
    let generation: i64 = row.get(13)?;
    Ok(DocumentPath::new(&path).map(|path| {
        (
            id,
            StoredFinding {
                kind,
                severity,
                path,
                class_key,
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

fn stored_candidate(row: &Row<'_>) -> Reading<CandidateFact> {
    let path: String = row.get(0)?;
    let suffix: String = row.get(1)?;
    Ok(DocumentPath::new(&path).map(|path| CandidateFact { path, suffix }))
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

/// A span read from three nullable columns, which are written together and are
/// therefore all present or all absent.
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
fn unix_seconds() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_secs() as i64)
}
