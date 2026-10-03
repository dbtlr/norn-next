//! The statements the write path runs: one registry, read by the code that
//! prepares them and by the plan seam that explains them.
//!
//! **A write statement is spelled here once.** [`WriteStatement::sql`] is the
//! only place these statements' text is written, and [`crate::increment`]'s
//! preparation and [`crate::Request::emitted_plan`] both take it from there, so
//! a plan bar over one of these is a plan of the SQL the increment executes.
//! [`WriteStatement::all`] is the census the plan bar walks, and it is declared
//! with the enum by one macro, so a variant cannot be left out of it.
//!
//! The registry holds the write statements no other
//! [`crate::ExplainedStatement`] names. The increment's remaining statements
//! are explained through their own variants, and the increment prepares them
//! from a closed set rather than from text it is handed.
//!
//! # What is registered
//!
//! The increment's prepared statements — the document upsert, the per-table fact
//! discards and inserts, the document delete, the tombstone record and the row
//! probe — the findings writes, the generation and pinned-scalar writes, and the
//! discard a schema pin runs over findings stamped under another fingerprint. The
//! findings discards an increment runs are
//! [`crate::ExplainedStatement::SubjectDiscard`] and
//! [`crate::ExplainedStatement::PathDiscard`], which carry their own bars, and the
//! pin's typed-value clear is [`crate::ExplainedStatement::TypedValueDiscard`].
//!
//! # What a plan here covers
//!
//! A plan reports how a statement reaches its rows, and it reports the
//! first-level foreign-key actions beside it as searches of the child tables.
//! The bar reads every step a plan shows, those included: none may read a table
//! end to end. A seek by a foreign-key action is not asserted positively, so a
//! cascade that is missing from a plan is not noticed. A plan does not report
//! the triggers a write fires — the full-text maintenance on `documents` and the
//! tombstone clear — or a cascade below the first level, so neither is covered.
//! An `INSERT` reports no search of its own table; for most inserts the plan is
//! empty, and the bar over an empty plan cannot fail. The verification reads
//! that scan by design (`Store::verify_integrity` and the derived-rows digest)
//! are not write-path statements and are not registered.

use norn_db::meta::{NEXT_GENERATION_SQL, PUT_META_SQL};

/// Declares [`WriteStatement`] and its census from one list of variants, so a
/// statement the enum names is in [`WriteStatement::all`] by construction.
macro_rules! registry {
    ($($(#[$doc:meta])* $variant:ident),+ $(,)?) => {
        /// One statement the write path prepares or executes.
        #[derive(Clone, Copy, Debug, Eq, PartialEq)]
        pub enum WriteStatement {
            $($(#[$doc])* $variant,)+
        }

        /// How many statements [`WriteStatement::all`] holds.
        pub const WRITE_STATEMENTS: usize = [$(WriteStatement::$variant),+].len();

        impl WriteStatement {
            /// Every statement the write path registers, in declaration order.
            pub fn all() -> [Self; WRITE_STATEMENTS] {
                [$(Self::$variant),+]
            }
        }
    };
}

registry! {
    /// The document row's upsert: updated on a path conflict, so the row keeps
    /// its identity across a re-derivation.
    UpsertDocument,
    /// The replacement of a document's link rows, discarded before they are
    /// written. The five fact discards below are one statement per table.
    DiscardLinks,
    DiscardHeadings,
    DiscardBlocks,
    DiscardTags,
    DiscardFields,
    /// One link row.
    InsertLink,
    /// One key a link is held under.
    InsertLinkKey,
    InsertHeading,
    InsertBlock,
    InsertTag,
    InsertField,
    /// The death of a document: the row removed, its content hash returned for
    /// the tombstone.
    DeleteDocument,
    /// A tombstone, updated on a path conflict.
    RecordTombstone,
    /// Whether a document row stands at a path.
    RowAt,
    /// One finding row, stamped with the generation and fingerprint it lands
    /// under.
    InsertFinding,
    /// One candidate of a finding's head.
    InsertFindingCandidate,
    /// One class a finding is a member of.
    InsertFindingClass,
    /// One path key a finding is held under.
    InsertFindingPath,
    /// The discard a schema pin runs over findings stamped under another
    /// fingerprint.
    DiscardStaleFindings,
    /// The write generation's increment, which takes and records it in one
    /// statement.
    NextGeneration,
    /// A pinned scalar's upsert, which a schema pin writes three of.
    PutMeta,
}

impl WriteStatement {
    /// The fact discards, one per fact table, in the order the increment runs
    /// them.
    pub const FACT_DISCARDS: [Self; 5] = [
        Self::DiscardLinks,
        Self::DiscardHeadings,
        Self::DiscardBlocks,
        Self::DiscardTags,
        Self::DiscardFields,
    ];

    /// The statement's text: the one spelling the write path prepares and the
    /// plan seam explains.
    pub(crate) fn sql(self) -> &'static str {
        match self {
            Self::UpsertDocument => {
                "INSERT INTO documents (
                     path, suffix_key, folded_suffix_key, admitting_segments, content_hash,
                     byte_length, body, body_hash, body_offset, frontmatter,
                     frontmatter_projection_hash, frontmatter_diagnostic_count, generation,
                     derived_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)
                 ON CONFLICT(path) DO UPDATE SET
                     suffix_key                   = excluded.suffix_key,
                     folded_suffix_key            = excluded.folded_suffix_key,
                     admitting_segments           = excluded.admitting_segments,
                     content_hash                 = excluded.content_hash,
                     byte_length                  = excluded.byte_length,
                     body                         = excluded.body,
                     body_hash                    = excluded.body_hash,
                     body_offset                  = excluded.body_offset,
                     frontmatter                  = excluded.frontmatter,
                     frontmatter_projection_hash  = excluded.frontmatter_projection_hash,
                     frontmatter_diagnostic_count = excluded.frontmatter_diagnostic_count,
                     generation                   = excluded.generation,
                     derived_at                   = excluded.derived_at
                 RETURNING id"
            }
            Self::DiscardLinks => "DELETE FROM links WHERE document = ?1",
            Self::DiscardHeadings => "DELETE FROM headings WHERE document = ?1",
            Self::DiscardBlocks => "DELETE FROM blocks WHERE document = ?1",
            Self::DiscardTags => "DELETE FROM document_tags WHERE document = ?1",
            Self::DiscardFields => "DELETE FROM document_fields WHERE document = ?1",
            Self::InsertLink => {
                "INSERT INTO links (
                     document, ordinal, family, embed, protocol, target, title, anchor,
                     anchor_text, anchor_marked, block_ref, address, span_line, span_column,
                     span_offset
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)
                 RETURNING id"
            }
            Self::InsertLinkKey => {
                "INSERT INTO link_keys (link, document, key, folded_key, segments)
                 VALUES (?1, ?2, ?3, ?4, ?5)"
            }
            Self::InsertHeading => {
                "INSERT INTO headings (
                     document, ordinal, text, reading, slug, level, span_line, span_column,
                     span_offset, body_offset, inside_container
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)"
            }
            Self::InsertBlock => {
                "INSERT INTO blocks (
                     document, ordinal, block_id, span_line, span_column, span_offset
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)"
            }
            Self::InsertTag => {
                "INSERT INTO document_tags (
                     document, ordinal, name, folded_name, source, span_line, span_column,
                     span_offset
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)"
            }
            Self::InsertField => {
                "INSERT INTO document_fields (
                     document, key, ordinal, path, container, raw, typed, least_raw,
                     least_typed, offset_stated
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)"
            }
            // The hash the tombstone carries comes back from the row this
            // removes, so the delete and the record read one value between them.
            Self::DeleteDocument => "DELETE FROM documents WHERE path = ?1 RETURNING content_hash",
            Self::RecordTombstone => {
                "INSERT INTO tombstones (
                     path, last_content_hash, provenance, generation, recorded_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(path) DO UPDATE SET
                     last_content_hash = COALESCE(
                         excluded.last_content_hash, tombstones.last_content_hash
                     ),
                     provenance        = excluded.provenance,
                     generation        = excluded.generation,
                     recorded_at       = excluded.recorded_at"
            }
            Self::RowAt => "SELECT EXISTS (SELECT 1 FROM documents WHERE path = ?1)",
            Self::InsertFinding => {
                "INSERT INTO findings (
                     vault_schema_fingerprint, generation, kind, severity, path, target,
                     span_line, span_column, span_offset, candidates_total, message, detail,
                     ordinal
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
                 RETURNING id"
            }
            Self::InsertFindingCandidate => {
                "INSERT INTO finding_candidates (finding, rank, path, suffix)
                 VALUES (?1, ?2, ?3, ?4)"
            }
            Self::InsertFindingClass => {
                "INSERT INTO finding_classes (finding, class_key) VALUES (?1, ?2)"
            }
            Self::InsertFindingPath => {
                "INSERT INTO finding_paths (finding, path_key) VALUES (?1, ?2)"
            }
            // Two open ranges rather than `<>`: an inequality is not a predicate
            // an index can answer, so it would read every finding in the table
            // on every pin — including the pins that discard nothing.
            Self::DiscardStaleFindings => {
                "DELETE FROM findings
                 WHERE vault_schema_fingerprint < ?1 OR vault_schema_fingerprint > ?1"
            }
            Self::NextGeneration => NEXT_GENERATION_SQL,
            Self::PutMeta => PUT_META_SQL,
        }
    }

    /// How many values the statement binds: the highest `?n` its text names.
    ///
    /// Read off the text so it cannot drift from it. The plan seam binds that
    /// many nulls, because a plan is a report about the statement and not a run
    /// of it, and no write statement's text branches on what it is bound to.
    pub(crate) fn parameter_count(self) -> usize {
        let sql = self.sql();
        sql.match_indices('?')
            .map(|(at, _)| {
                sql[at + 1..]
                    .chars()
                    .take_while(char::is_ascii_digit)
                    .collect::<String>()
                    .parse::<usize>()
                    .expect("every placeholder a write statement names is numbered")
            })
            .max()
            .unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_statements_parameter_count_is_its_highest_placeholder() {
        assert_eq!(WriteStatement::UpsertDocument.parameter_count(), 14);
        assert_eq!(WriteStatement::InsertLink.parameter_count(), 15);
        assert_eq!(WriteStatement::DiscardStaleFindings.parameter_count(), 1);
        assert_eq!(WriteStatement::PutMeta.parameter_count(), 2);
    }
}
