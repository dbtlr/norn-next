//! Which part drives a page, chosen once per request.
//!
//! **A page narrowed by a part that keeps what it seeks is driven by one of
//! those parts, or walks its own order.** A part that keeps its documents in
//! the page's order drives it from its source, from the place the page
//! resumes at, so a page costs the documents it reads
//! ([`crate::ReadFilter::is_path_ordered`]). Any other part reaches its
//! documents in another order, so driving a page from it costs every document
//! it matched, sorted, on every page, which over a drain is quadratic in what
//! it matched. So the page asks, part by part in the request's order, whether
//! one keeps at most a ceiling of documents ([`FindStatement::PartCeiling`]);
//! the first that does drives the page from what it matched, and where none
//! does the page walks its own order and tests every part at each document's
//! key.
//!
//! **The ceiling scales with the page and the vault**: the greater of
//! [`MATCHED_DRIVE_FACTOR`] documents per row the page reads and the square
//! root of the rows the page reads times the documents the vault holds
//! ([`FindStatement::DocumentSpan`]). A part is asked its distinct documents
//! among the rows its index seek reaches alone, which over-counts where the
//! seek reaches documents the part then rejects. A page driven from what a
//! part matched costs at most the ceiling's documents, and a drain of it its
//! pages times that: at most the vault where the vault's term sets the
//! ceiling, and eight documents per row it reads where the floor does. A page
//! that walks its order costs the documents it walks to fill its rows: a part
//! keeping more than the ceiling spread through the vault leaves about the
//! vault over the ceiling documents between two of its own, so a page walks
//! about its rows times that, which is at most the ceiling, and a drain walks
//! each document once. **What stays a declared limit** is a page that walks
//! past many documents to reach the next of a few: a part's documents bunched
//! together, or a part whose seek reaches many documents it rejects — findings
//! at paths no document stands at, values of the wrong declared shape or
//! failing a folded key's typed order, links whose key names another document,
//! documents the ambiguity-ignore set leaves out, paths under a glob's literal
//! prefix the glob rejects — which its count takes for documents it keeps, so
//! the page walks. Each such document is walked once over a drain: no index
//! holds a range's, a full-text match's, a resolution's or a links-to part's
//! documents in a page's order, and a count that judged each row would read
//! without bound.

use crate::error::{self, StoreError};
use crate::find::{FindStatement, compose_document_span};
use crate::store::Snapshot;

use super::{Filter, Lookups, Ran};

/// The fewest documents, per row a page reads, a part may keep and still
/// drive the page from what it matched, however small the vault: a floor on
/// the ceiling, so a page in a small vault sorts at most this many documents
/// per row rather than walking it.
pub const MATCHED_DRIVE_FACTOR: usize = 8;

/// How a page reaches the documents its parts keep, by the part's position
/// among the parts a page tests.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Driver {
    /// The part keeps its documents in the page's order and drives the page
    /// from its source ([`Filter::source`]).
    Ordered(usize),
    /// The part keeps at most the ceiling's documents, and drives the page
    /// from what it matched, which is sorted.
    Matched(usize),
    /// No part drives: the page walks its own order and tests each part at
    /// every document it reads.
    Walked,
}

impl Snapshot {
    /// The part that drives a page reading `rows` rows narrowed by `parts`:
    /// the first that keeps its documents in the page's order where the page
    /// reads in the answer's path order — `descending` names which way, and is
    /// `None` for a page in any other order — and otherwise the first that
    /// keeps at most the ceiling ([`Snapshot::drive_ceiling`]), asked in the
    /// request's order and no further than the first that does. A part that
    /// excludes drives nothing.
    pub(crate) fn choose_driver(
        &self,
        parts: &[&Filter],
        descending: Option<bool>,
        rows: usize,
        lookups: &mut Lookups,
    ) -> Result<Driver, StoreError> {
        let keeping = || {
            parts
                .iter()
                .enumerate()
                .filter(|(_, part)| !part.shape().excludes())
        };
        if let Some(descending) = descending
            && let Some((at, _)) = keeping().find(|(_, part)| part.is_path_ordered(descending))
        {
            return Ok(Driver::Ordered(at));
        }
        if keeping().next().is_none() {
            return Ok(Driver::Walked);
        }
        let ceiling = self.drive_ceiling(rows, lookups)?;
        for (at, part) in keeping() {
            let probe = Ran::new(FindStatement::PartCeiling, part.kept_within(ceiling))
                .narrowed_by(vec![part.shape()]);
            if self.ask(
                &mut lookups.ran,
                probe,
                "asking how many documents a part keeps",
            )? {
                return Ok(Driver::Matched(at));
            }
        }
        Ok(Driver::Walked)
    }

    /// How many documents a part may keep and still drive a page reading
    /// `rows` rows from what it matched: the greater of
    /// [`MATCHED_DRIVE_FACTOR`] per row and the square root of `rows` times
    /// the vault's greatest document row id. One
    /// [`FindStatement::DocumentSpan`].
    fn drive_ceiling(&self, rows: usize, lookups: &mut Lookups) -> Result<usize, StoreError> {
        const OPERATION: &str = "reading how many documents the vault may hold";
        let span: i64 = self
            .run_statement(
                &mut lookups.ran,
                Ran::new(FindStatement::DocumentSpan, compose_document_span()),
                |row| row.get(0),
            )
            .map_err(|problem| error::sql(OPERATION, problem))?
            .into_iter()
            .next()
            .unwrap_or(0);
        let span = usize::try_from(span).unwrap_or(0);
        Ok(rows
            .saturating_mul(MATCHED_DRIVE_FACTOR)
            .max(rows.saturating_mul(span).isqrt()))
    }
}
