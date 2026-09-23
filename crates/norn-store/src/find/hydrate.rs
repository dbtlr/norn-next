//! A page's rows: the documents a page of keys found, carrying the columns the
//! request projected and nothing else.
//!
//! **A row costs the columns it names.** The keys a page found already carry
//! each document's path, so a projection of the path alone reads nothing more.
//! A field or the body reads the page's document rows by row id, once, and
//! only the columns named; each nested collection is one statement over its
//! own table, and a collection nobody named is a table nobody reads.
//!
//! **A row is bounded, and says where it was cut.** A nested collection
//! carries at most [`NESTED_ROW_CEILING`] items and a body at most
//! [`BODY_ROW_CEILING`] bytes, and the bound is applied in the statement, so
//! a document with a million tags costs its page the ceiling rather than the
//! million. What the whole held rides beside the head: a collection's
//! [`Collection::total`] is counted — only for a document whose head the
//! ceiling filled, since a head below the ceiling is the whole — and a body's
//! [`BodyText::byte_length`] is the whole body's length in bytes.

use std::collections::{BTreeMap, HashMap};

use norn_db::rusqlite::Row;
use norn_wire::{
    BlockRow, BodyText, Collection, DocumentRow, FieldValue, HeadingRow, TagRow, TotalBelowHead,
};

use super::statement::{
    DocumentColumns, FindStatement, Nested, compose_documents, compose_nested_head,
    compose_nested_total,
};
use super::{FoundKey, Projection, Ran, Stepped};
use crate::error::{self, StoreError};
use crate::facts::{Span, TagSource};
use crate::json::projected_fields;
use crate::request::{Reading, stored_block, stored_heading, stored_tag};
use crate::store::Snapshot;

/// How many items one nested collection carries on one row.
///
/// The store's per-row bound: a collection longer than this is cut to its
/// first this-many items, in document order, and the row's
/// [`Collection::total`] says how many there were.
pub const NESTED_ROW_CEILING: usize = 256;

/// How many bytes of a document's body one row carries.
///
/// The store's per-row bound: a longer body is cut to the last whole character
/// at or before this many bytes, and the row's [`BodyText::byte_length`] says
/// how long the whole body is.
pub const BODY_ROW_CEILING: usize = 65_536;

/// What one find read, by kind.
///
/// The instrument a caller reads what a page cost off: how many statements the
/// find ran on its snapshot, how many keys its page statements handed back —
/// one past the bound where a next page exists — what SQLite counted stepping
/// those page statements, how many document rows it hydrated, and how many
/// rows of each nested table it read.
///
/// **The page counters are the page's cost as SQLite ran it**, read off each
/// page statement's own status once its rows are read, and summed over the
/// page statements the find ran. They are counters rather than a plan's
/// words, so a pair of finds over two vault sizes reads whether a page's work
/// grows with the vault by comparing them.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct FindWork {
    /// The statements the find ran, each counted on the snapshot as it ran.
    pub statements: u64,
    /// The document keys the page statements handed back.
    pub keys_read: u64,
    /// Steps the page statements took through a loop no constraint bounds —
    /// a table or an index read end to end. A page seeks, so this is zero.
    pub page_full_scan_steps: u64,
    /// Sorts the page statements ran: a temporary B-tree an order filled
    /// because no index handed its rows back in that order.
    pub page_sorts: u64,
    /// Virtual-machine operations the page statements ran: the whole of what
    /// SQLite did for the page, whatever it did it on.
    pub page_vm_steps: u64,
    /// The document rows the hydration read.
    pub documents_hydrated: u64,
    /// The nested-table rows the hydration read, by table.
    pub nested_rows: NestedRows,
}

impl FindWork {
    /// Add what SQLite counted stepping one page statement.
    pub(super) fn page_stepped(&mut self, stepped: Stepped) {
        self.page_full_scan_steps += stepped.full_scan_steps;
        self.page_sorts += stepped.sorts;
        self.page_vm_steps += stepped.vm_steps;
    }
}

/// Nested-table rows read, one count per table.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct NestedRows {
    /// Rows of `document_tags`.
    pub tags: u64,
    /// Rows of `headings`.
    pub headings: u64,
    /// Rows of `blocks`.
    pub blocks: u64,
}

impl NestedRows {
    /// The rows read of `nested`'s table.
    pub fn of(&self, nested: Nested) -> u64 {
        match nested {
            Nested::Tags => self.tags,
            Nested::Headings => self.headings,
            Nested::Blocks => self.blocks,
        }
    }

    fn add(&mut self, nested: Nested, rows: u64) {
        let count = match nested {
            Nested::Tags => &mut self.tags,
            Nested::Headings => &mut self.headings,
            Nested::Blocks => &mut self.blocks,
        };
        *count += rows;
    }
}

/// One document row's own columns, as the hydration read them.
#[derive(Default)]
struct DocumentColumnsRead {
    frontmatter: Option<String>,
    body_head: Option<Vec<u8>>,
    body_length: Option<u64>,
}

/// One nested collection's head for each document, each item beside its
/// ordinal.
type Heads<T> = HashMap<i64, Vec<(i64, T)>>;

impl Snapshot {
    /// The rows of the documents `keys` name, in their order, carrying the
    /// columns `projection` names; `fields` is the field keys it names that
    /// the vault's field universe holds. Each statement it runs is recorded in
    /// `record`.
    pub(super) fn hydrate(
        &mut self,
        keys: &[FoundKey],
        projection: &Projection<'_>,
        fields: &[&str],
        work: &mut FindWork,
        record: &mut Vec<Ran>,
    ) -> Result<Vec<DocumentRow>, StoreError> {
        let ids: Vec<i64> = keys.iter().map(FoundKey::document).collect();
        let columns = DocumentColumns {
            frontmatter: projection.all_fields || !fields.is_empty(),
            body: projection.body,
        };
        let mut read = if ids.is_empty() || columns == DocumentColumns::default() {
            HashMap::new()
        } else {
            self.read_documents(&ids, columns, work, record)?
        };

        let mut rows = Vec::with_capacity(keys.len());
        for key in keys {
            let path = norn_wire::DocumentPath::new(key.path()).map_err(|problem| {
                StoreError::Damaged {
                    what: format!("`documents.path` holds no document path: {problem}"),
                }
            })?;
            let mut row = DocumentRow::new(path);
            let own = read.remove(&key.document()).unwrap_or_default();
            if projection.names_fields() {
                row = row.with_fields(project_fields(
                    own.frontmatter.as_deref(),
                    projection.all_fields,
                    fields,
                )?);
            }
            if projection.body {
                row = row.with_body(body_text(own.body_head, own.body_length)?);
            }
            rows.push(row);
        }

        for nested in projection.nested.iter().copied() {
            match nested {
                Nested::Tags => {
                    let mut heads = self.read_nested(nested, &ids, tag_row, work, record)?;
                    for (row, id) in rows.iter_mut().zip(&ids) {
                        let (items, total) = heads.remove(id).unwrap_or_default();
                        row.tags = Some(Collection::new(items, total).map_err(cut_below_head)?);
                    }
                }
                Nested::Headings => {
                    let mut heads = self.read_nested(nested, &ids, heading_row, work, record)?;
                    for (row, id) in rows.iter_mut().zip(&ids) {
                        let (items, total) = heads.remove(id).unwrap_or_default();
                        row.headings = Some(Collection::new(items, total).map_err(cut_below_head)?);
                    }
                }
                Nested::Blocks => {
                    let mut heads = self.read_nested(nested, &ids, block_row, work, record)?;
                    for (row, id) in rows.iter_mut().zip(&ids) {
                        let (items, total) = heads.remove(id).unwrap_or_default();
                        row.blocks = Some(Collection::new(items, total).map_err(cut_below_head)?);
                    }
                }
            }
        }
        Ok(rows)
    }

    /// The document rows `ids` name, reading `columns` of each.
    fn read_documents(
        &mut self,
        ids: &[i64],
        columns: DocumentColumns,
        work: &mut FindWork,
        record: &mut Vec<Ran>,
    ) -> Result<HashMap<i64, DocumentColumnsRead>, StoreError> {
        const OPERATION: &str = "reading the document rows a page found";
        let statement = Ran::new(
            FindStatement::HydrateDocuments,
            compose_documents(ids, columns, BODY_ROW_CEILING),
        );
        let read = self
            .run_statement(record, statement, |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    DocumentColumnsRead {
                        frontmatter: row.get(1)?,
                        body_head: row.get(2)?,
                        body_length: row.get(3)?,
                    },
                ))
            })
            .map_err(|problem| error::sql(OPERATION, problem))?;
        work.documents_hydrated += read.len() as u64;
        Ok(read.into_iter().collect())
    }

    /// The head of `nested` for each document `ids` names, with each head's
    /// total: counted where the ceiling filled the head, and the head's own
    /// length where it did not.
    fn read_nested<T>(
        &mut self,
        nested: Nested,
        ids: &[i64],
        item: fn(&Row<'_>) -> Reading<T>,
        work: &mut FindWork,
        record: &mut Vec<Ran>,
    ) -> Result<HashMap<i64, (Vec<T>, u64)>, StoreError> {
        const OPERATION: &str = "reading the nested rows a page projected";
        if ids.is_empty() {
            return Ok(HashMap::new());
        }
        let width = nested.width();
        let head = Ran::new(
            FindStatement::NestedHead(nested),
            compose_nested_head(nested, ids, NESTED_ROW_CEILING),
        );
        let read = self
            .run_statement(record, head, |row| {
                Ok((
                    row.get::<_, i64>(width)?,
                    row.get::<_, i64>(width + 1)?,
                    item(row)?,
                ))
            })
            .map_err(|problem| error::sql(OPERATION, problem))?;
        work.nested_rows.add(nested, read.len() as u64);
        let mut heads: Heads<T> = HashMap::new();
        for (document, ordinal, item) in read {
            heads.entry(document).or_default().push((ordinal, item?));
        }

        let cut: Vec<i64> = heads
            .iter()
            .filter(|(_, items)| items.len() >= NESTED_ROW_CEILING)
            .map(|(document, _)| *document)
            .collect();
        let totals: HashMap<i64, u64> = if cut.is_empty() {
            HashMap::new()
        } else {
            let total = Ran::new(
                FindStatement::NestedTotal(nested),
                compose_nested_total(nested, &cut),
            );
            self.run_statement(record, total, |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, u64>(1)?))
            })
            .map_err(|problem| error::sql(OPERATION, problem))?
            .into_iter()
            .collect()
        };

        Ok(heads
            .into_iter()
            .map(|(document, mut items)| {
                // The index hands a document's rows back in ordinal order; the
                // statement states none, so the order is taken here, over the
                // head the ceiling already bounded.
                items.sort_by_key(|(ordinal, _)| *ordinal);
                let head: Vec<T> = items.into_iter().map(|(_, item)| item).collect();
                let total = totals.get(&document).copied().unwrap_or(head.len() as u64);
                (document, (head, total))
            })
            .collect())
    }
}

/// A row's fields: every field its projection carries where `all` asks for
/// them, and each of `keys` — [`FieldValue::Absent`] where the document does
/// not carry it.
fn project_fields(
    frontmatter: Option<&str>,
    all: bool,
    keys: &[&str],
) -> Result<BTreeMap<String, FieldValue>, StoreError> {
    let mut carried = match frontmatter {
        Some(text) => projected_fields(text)?,
        None => BTreeMap::new(),
    };
    if all {
        for key in keys {
            carried
                .entry((*key).to_string())
                .or_insert_with(FieldValue::absent);
        }
        return Ok(carried);
    }
    Ok(keys
        .iter()
        .map(|key| {
            (
                (*key).to_string(),
                carried.remove(*key).unwrap_or_else(FieldValue::absent),
            )
        })
        .collect())
}

/// The body a row carries: the head the statement read, cut back to its last
/// whole character, and the whole body's length.
fn body_text(head: Option<Vec<u8>>, length: Option<u64>) -> Result<BodyText, StoreError> {
    let head = head.unwrap_or_default();
    let text = match std::str::from_utf8(&head) {
        Ok(text) => text,
        // A head cut inside a character ends in an incomplete sequence, which
        // is the one invalid shape a cut makes; anything else is a body this
        // store did not write.
        Err(cut) if cut.error_len().is_none() => {
            std::str::from_utf8(&head[..cut.valid_up_to()]).expect("valid up to the cut")
        }
        Err(problem) => {
            return Err(StoreError::Damaged {
                what: format!("`documents.body` holds text that is not UTF-8: {problem}"),
            });
        }
    };
    BodyText::new(text, length.unwrap_or_default()).map_err(cut_below_head)
}

/// A head longer than the whole it heads, which one snapshot's two reads of
/// one document cannot produce.
fn cut_below_head(problem: TotalBelowHead) -> StoreError {
    StoreError::Damaged {
        what: format!("a projected row's head outgrew its whole: {problem}"),
    }
}

fn wire_span(span: Span) -> norn_wire::Span {
    norn_wire::Span::new(span.line, span.column, span.byte_offset)
}

fn tag_row(row: &Row<'_>) -> Reading<TagRow> {
    Ok(stored_tag(row)?.map(|tag| {
        let source = match tag.source {
            TagSource::Body => norn_wire::TagSource::Body,
            TagSource::Frontmatter => norn_wire::TagSource::Frontmatter,
        };
        TagRow::new(tag.name, source, tag.span.map(wire_span))
    }))
}

fn heading_row(row: &Row<'_>) -> Reading<HeadingRow> {
    Ok(stored_heading(row)?.map(|heading| {
        HeadingRow::new(
            heading.level,
            heading.text,
            heading.slug,
            wire_span(heading.span),
        )
    }))
}

fn block_row(row: &Row<'_>) -> Reading<BlockRow> {
    Ok(stored_block(row)?.map(|block| BlockRow::new(block.block_id, block.span.map(wire_span))))
}
