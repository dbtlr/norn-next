//! A page's rows: the documents a page of keys found, carrying the columns the
//! request projected and nothing else.
//!
//! **A row costs the columns it names.** The keys a page found already carry
//! each document's path, so a projection of the path alone reads nothing more.
//! A field or the body reads the page's document rows by row id, once, and
//! only the columns named; each nested collection is one statement over its
//! own table, and a collection nobody named is a table nobody reads.
//!
//! **The findings column is a nested collection too.** It carries the findings
//! standing at the document's path under the active fingerprint, in `(kind,
//! id)` order — the order `validate` pages them in at one path — each a
//! finding row read through the accessor `validate` reads its rows through,
//! so a finding carries the same head and hint on either verb.
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
    BlockRow, BodyText, Collection, DocumentRow, FieldValue, FindingRow, HeadingRow, TagRow,
    TotalBelowHead,
};

use super::statement::{
    DocumentColumns, FindStatement, Nested, compose_documents, compose_finding_head,
    compose_finding_total, compose_nested_head, compose_nested_total,
};
use super::{FoundKey, Projection};
use crate::error::{self, StoreError};
use crate::facts::{BlockFact, HeadingFact, Span, TagSource};
use crate::json::projected_fields;
use crate::read::{Ran, Stepped, finding_base};
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
    /// The finding rows the findings column read: at most the ceiling per
    /// document.
    pub finding_rows: u64,
}

impl FindWork {
    /// The whole reading, name by name, every count present — the shape a
    /// harness compares, for the reason
    /// [`DerivationCounters::readings`](crate::DerivationCounters::readings)
    /// gives: a count one reading carries and another does not is a
    /// difference rather than a zero.
    ///
    /// The nested rows are one reading per table, so a reading names each
    /// table a hydration can read whether or not this find read it.
    pub fn readings(&self) -> impl Iterator<Item = (&'static str, u64)> + '_ {
        [
            ("find_statements", self.statements),
            ("find_keys_read", self.keys_read),
            ("find_page_full_scan_steps", self.page_full_scan_steps),
            ("find_page_sorts", self.page_sorts),
            ("find_page_vm_steps", self.page_vm_steps),
            ("find_documents_hydrated", self.documents_hydrated),
            ("find_tag_rows", self.nested_rows.tags),
            ("find_heading_rows", self.nested_rows.headings),
            ("find_block_rows", self.nested_rows.blocks),
            ("find_finding_rows", self.finding_rows),
        ]
        .into_iter()
    }

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
    /// the vault's field universe holds, and `findings_under` the fingerprint
    /// the findings column reads under where the projection names it. Each
    /// statement it runs is recorded in `record`.
    pub(super) fn hydrate(
        &self,
        keys: &[FoundKey],
        projection: &Projection<'_>,
        fields: &[&str],
        findings_under: Option<&str>,
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

        if let Some(fingerprint) = findings_under {
            let paths: Vec<&str> = keys.iter().map(FoundKey::path).collect();
            let mut heads = self.read_findings(&paths, fingerprint, work, record)?;
            for (row, path) in rows.iter_mut().zip(&paths) {
                let (items, total) = heads.remove(*path).unwrap_or_default();
                row.findings = Some(Collection::new(items, total).map_err(cut_below_head)?);
            }
        }
        Ok(rows)
    }

    /// The head of the findings standing at each of `paths` under
    /// `fingerprint`, with each head's total: counted where the ceiling
    /// filled the head, and the head's own length where it did not.
    fn read_findings(
        &self,
        paths: &[&str],
        fingerprint: &str,
        work: &mut FindWork,
        record: &mut Vec<Ran>,
    ) -> Result<HashMap<String, (Vec<FindingRow>, u64)>, StoreError> {
        const OPERATION: &str = "reading the findings a page projected";
        if paths.is_empty() {
            return Ok(HashMap::new());
        }
        let head = Ran::new(
            FindStatement::FindingHead,
            compose_finding_head(paths, fingerprint, NESTED_ROW_CEILING)?,
        );
        let mut bases = self
            .run_statement(record, head, finding_base)
            .map_err(|problem| error::sql(OPERATION, problem))?;
        work.finding_rows += bases.len() as u64;
        // The index hands each path's findings back in `(kind, id)` order and
        // the paths in the order they were named; the statement states
        // neither, so the order is taken here, over heads the ceiling bounded.
        let order: HashMap<&str, usize> = paths
            .iter()
            .enumerate()
            .map(|(at, path)| (*path, at))
            .collect();
        bases.sort_by(|one, other| {
            (order.get(one.path.as_str()), &one.kind, one.id).cmp(&(
                order.get(other.path.as_str()),
                &other.kind,
                other.id,
            ))
        });
        let mut filled: HashMap<String, usize> = HashMap::new();
        for base in &bases {
            *filled.entry(base.path.clone()).or_default() += 1;
        }
        let cut: Vec<&str> = filled
            .iter()
            .filter(|(_, held)| **held >= NESTED_ROW_CEILING)
            .map(|(path, _)| path.as_str())
            .collect();
        let totals: HashMap<String, u64> = if cut.is_empty() {
            HashMap::new()
        } else {
            let total = Ran::new(
                FindStatement::FindingTotal,
                compose_finding_total(&cut, fingerprint)?,
            );
            self.run_statement(record, total, |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, u64>(1)?))
            })
            .map_err(|problem| error::sql(OPERATION, problem))?
            .into_iter()
            .collect()
        };

        let at: Vec<String> = bases.iter().map(|base| base.path.clone()).collect();
        let mut heads: HashMap<String, (Vec<FindingRow>, u64)> = HashMap::new();
        for (path, row) in at.into_iter().zip(self.finding_rows(record, bases)?) {
            heads.entry(path).or_default().0.push(row);
        }
        for (path, (items, total)) in &mut heads {
            *total = totals.get(path).copied().unwrap_or(items.len() as u64);
        }
        Ok(heads)
    }

    /// The document rows `ids` name, reading `columns` of each.
    fn read_documents(
        &self,
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
        &self,
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

/// `text` as a row carries it: cut to the last whole character at or before
/// [`BODY_ROW_CEILING`] bytes, beside its whole length in bytes. The one bound a
/// body is held to, whichever part of a document the text is.
pub(crate) fn bounded_body(text: &str) -> BodyText {
    let mut cut = text.len().min(BODY_ROW_CEILING);
    while !text.is_char_boundary(cut) {
        cut -= 1;
    }
    BodyText::new(&text[..cut], text.len() as u64).expect("a head is never longer than its whole")
}

/// A head longer than the whole it heads, which one snapshot's two reads of
/// one document cannot produce.
fn cut_below_head(problem: TotalBelowHead) -> StoreError {
    StoreError::Damaged {
        what: format!("a projected row's head outgrew its whole: {problem}"),
    }
}

pub(crate) fn wire_span(span: Span) -> norn_wire::Span {
    norn_wire::Span::new(span.line, span.column, span.byte_offset)
}

pub(crate) fn tag_row(row: &Row<'_>) -> Reading<TagRow> {
    Ok(stored_tag(row)?.map(|tag| {
        let source = match tag.source {
            TagSource::Body => norn_wire::TagSource::Body,
            TagSource::Frontmatter => norn_wire::TagSource::Frontmatter,
        };
        TagRow::new(tag.name, source, tag.span.map(wire_span))
    }))
}

pub(crate) fn heading_row(row: &Row<'_>) -> Reading<HeadingRow> {
    Ok(stored_heading(row)?.map(wire_heading))
}

/// A stored heading as the wire's row carries it.
pub(crate) fn wire_heading(heading: HeadingFact) -> HeadingRow {
    HeadingRow::new(
        heading.level,
        heading.text,
        heading.slug,
        wire_span(heading.span),
    )
}

pub(crate) fn block_row(row: &Row<'_>) -> Reading<BlockRow> {
    Ok(stored_block(row)?.map(wire_block))
}

/// A stored block definition as the wire's row carries it.
pub(crate) fn wire_block(block: BlockFact) -> BlockRow {
    BlockRow::new(block.block_id, block.span.map(wire_span))
}
