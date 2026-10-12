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
//! one keeps at most [`MATCHED_DRIVE_FACTOR`] documents per row the page
//! reads ([`crate::FindStatement::PartCeiling`]); the first that does drives
//! the page from what it matched, and where none does the page walks its own
//! order and tests every part at each document's key.
//!
//! **What a page costs is therefore bounded, and a drain is linear.** A page
//! driven from what a part matched costs at most the ceiling's documents. A
//! page that walks its order costs the documents it walks to fill its rows:
//! where every part keeps more than the ceiling, those are the documents
//! between the place it resumes at and its last row, and a drain walks each
//! once. A page past a few documents a part keeps among many it walks pays for
//! the many, a declared limit: no index holds a range's, a full-text match's,
//! a resolution's or a links-to part's documents in a page's order.

use crate::error::StoreError;
use crate::find::FindStatement;
use crate::store::Snapshot;

use super::{Filter, Lookups, Ran};

/// How many documents, per row a page reads, a part may keep and still drive
/// the page from what it matched.
///
/// A page driven from what a part matched costs those documents, sorted, and a
/// page that walks its order costs the documents it walks to its rows, so the
/// factor is where the two meet for a part keeping one document in that many
/// of the page's order.
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
    /// keeps at most [`MATCHED_DRIVE_FACTOR`] documents per row, asked in the
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
        let ceiling = rows.saturating_mul(MATCHED_DRIVE_FACTOR);
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
}
