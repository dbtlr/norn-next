//! Which part drives a page, chosen as the page reads.
//!
//! **A page narrowed by a part that keeps what it seeks is driven by one of
//! those parts, or walks its own order.** A part that keeps its documents in
//! the page's order drives it from its source, from the place the page
//! resumes at, so a page costs the documents it reads
//! ([`crate::ReadFilter::is_path_ordered`]). Any other part reaches its
//! documents in another order, so driving a page from it costs every document
//! it matched, sorted, on every page, which over a drain is quadratic in what
//! it matched; and walking the page's order costs the documents the walk
//! passes, which is the vault where the part keeps few. Which is cheaper is
//! not known before the page reads, and asking a part what it keeps costs
//! what is asked.
//!
//! **So the page asks once, then walks and asks in rounds, against a budget
//! that doubles** ([`Snapshot::drive_page`]). It first asks the parts in the
//! request's order whether one keeps no more documents than the page reads
//! ([`FindStatement::PartCeiling`]); the first that does drives the page from
//! what it matched, walking nothing. Otherwise, a round walks at most a budget of rows of the
//! page's order past where the last round stopped — the row its window ends
//! at is read first, narrowed by no part ([`Paged::window`]), and the walk
//! tests every part at each row up to it — and where that leaves the page
//! short, asks the parts again whether one keeps at most the budget; the
//! first that does drives the
//! rest of the page from what it matched, and otherwise the budget doubles.
//! The budget starts at [`MATCHED_DRIVE_FACTOR`] documents per row the page
//! reads and stops at the ceiling: the greater of that and the square root of
//! the rows the page reads times the documents the vault holds
//! ([`FindStatement::DocumentSpan`], read once a page first asks). A page
//! asked at the ceiling that no part drives walks on unbounded. A field
//! sort's missing section walks every document in path order, passing the
//! ones that carry the key, so its window is counted in documents too.
//!
//! **What a page costs.** The first count costs at most the rows the page
//! reads, one document past them per part asked. A round costs its budget, in rows walked, in the
//! window's rows and in each part's count, which reads its index seek until
//! one document past the budget; the budgets double, so a page costs about
//! the last budget it reached, which is never past the ceiling. A part asked
//! its documents among the rows its seek reaches alone over-counts where the
//! seek reaches documents the part then rejects, and is only ever taken to
//! keep more than it does.
//!
//! - **A part spread through the vault**, keeping a share of its documents,
//!   leaves a page to walk its rows over that share; the rounds stop at the
//!   first budget past that walk, so the page costs about the walk it needs —
//!   at least eight documents per row — and its counts no more than that, the
//!   first one a document per row it reads.
//!   The rows a round's window passes past the page's last row, which the
//!   next page walks again, are at most that budget: eight per row on the
//!   first round, and otherwise twice what the page walked before it. So a
//!   drain walks each document a bounded number of times, and is linear in
//!   the vault.
//! - **A part keeping few documents** — no more than the ceiling — drives a
//!   page from what it matched once the budget reaches them, or at once where
//!   it keeps no more than the page reads, so the page
//!   costs about what the part keeps, and a drain of it its pages times that:
//!   the documents it keeps squared over the rows a page reads — at most the
//!   vault where the vault's term sets the ceiling, and eight documents per
//!   row it reads where the floor does.
//! - **A selective page**, whose part keeps no more documents than the page
//!   reads, is driven from what the part matched by its first count, reading
//!   no window and no span of the vault: it costs what the part keeps, as it
//!   did before any walk.
//!
//! **What stays a declared limit** is a page that walks past many documents
//! to reach the next of a few once the budget reaches the ceiling: a part
//! keeping more than the ceiling with its documents bunched together, or a
//! part whose seek reaches many documents it rejects — findings at paths no
//! document stands at, values of the wrong declared shape or failing a folded
//! key's typed order, links whose key names another document, documents the
//! ambiguity-ignore set leaves out, paths under a glob's literal prefix the
//! glob rejects — which its count takes for documents it keeps, so the page
//! walks on. Each such document is walked once over a drain past the
//! ceiling's rounds: no index holds a range's, a full-text match's, a
//! resolution's or a links-to part's documents in a page's order, and a
//! count that judged each row would read without bound.

use std::collections::VecDeque;

use crate::error::{self, StoreError};
use crate::find::{FindStatement, compose_document_span};
use crate::store::Snapshot;

use super::page::{KeysetPage, fill_from};
use super::{Filter, Lookups, Ran, Stepped};

/// The fewest documents, per row a page reads, a part may keep and still
/// drive the page from what it matched, however small the vault: a floor on
/// the ceiling, so a page in a small vault sorts at most this many documents
/// per row rather than walking it. It is also the walk's first budget, per
/// row: what a page walks before it asks any part what it keeps.
pub const MATCHED_DRIVE_FACTOR: usize = 8;

/// How a page reaches the documents its parts keep, by the part's position
/// among the parts a page tests.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Driver {
    /// The part keeps its documents in the page's order and drives the page
    /// from its source ([`Filter::source`]).
    Ordered(usize),
    /// The part keeps at most the budget's documents, and drives the page
    /// from what it matched, which is sorted.
    Matched(usize),
    /// No part drives: the page walks its own order and tests each part at
    /// every document it reads.
    Walked,
}

/// The sections of one page, read as the drive reads them.
///
/// A section is one statement's reach in the page's order and the place it
/// resumes past; a row is what the statement hands back, which names a place
/// in that order too, so a section resumes past any row it handed back.
pub(crate) trait Paged {
    type Section: Clone;
    type Row;

    /// At most `rows` of `section`'s rows, driven by `driver`, and where
    /// `until` names one, none past it in the page's order: the walk's
    /// window, which only a walked read is given.
    fn read(
        &self,
        snapshot: &Snapshot,
        record: &mut Vec<Ran>,
        section: &Self::Section,
        driver: Driver,
        until: Option<&Self::Row>,
        rows: usize,
    ) -> Result<Vec<Self::Row>, StoreError>;

    /// The row `offset` rows past `section`'s place in its order, which no
    /// document part narrows — the last row a walk of `offset + 1` rows
    /// reaches — or `None` where the section holds no more than `offset`.
    fn window(
        &self,
        snapshot: &Snapshot,
        record: &mut Vec<Ran>,
        section: &Self::Section,
        offset: usize,
    ) -> Result<Option<Self::Row>, StoreError>;

    /// `section`, resumed past `row`.
    fn resumed(&self, section: &Self::Section, row: &Self::Row) -> Self::Section;
}

/// What a read of a page has established about how its request is driven,
/// which a later read of the same request, further on in the same order,
/// takes up rather than establish again.
#[derive(Clone, Debug)]
pub(crate) enum Settled<R> {
    /// Nothing yet: the read asks from its first count.
    Unasked,
    /// The driver the read settled on, which a later read takes as it is.
    Driver(Driver),
    /// The read filled — or read to its end — within a round of its walk,
    /// before any part was found to drive it or its budget reached the
    /// ceiling; a later read walks on from that round.
    Walking(Round<R>),
}

impl<R> Settled<R> {
    /// This, for a later read that resumes where `ahead` says whether it
    /// resumes before a row: a round's window stays the walk's where the
    /// later read resumes before the row the window ends at, and is read
    /// again from where the later read resumes otherwise.
    pub(crate) fn resumed_where(self, ahead: impl FnOnce(&R) -> bool) -> Self {
        match self {
            Settled::Walking(Round {
                budget,
                ceiling,
                window: Window::EndsAt(end),
            }) if !ahead(&end) => Settled::Walking(Round {
                budget,
                ceiling,
                window: Window::Unread,
            }),
            settled => settled,
        }
    }
}

/// A round of a page's walk, as far as it went.
#[derive(Clone, Debug)]
pub(crate) struct Round<R> {
    /// The round's budget: every part keeping what it seeks has been asked
    /// at the rows the page reads and at each smaller budget, and kept more,
    /// and none has been asked at this one.
    budget: usize,
    /// The ceiling the budget stops at, once a round has read the vault's
    /// span.
    ceiling: Option<usize>,
    /// Where the round's window ends.
    window: Window<R>,
}

impl<R> Round<R> {
    /// The first round of a page of `scale` rows, which reads its window.
    fn first(scale: usize) -> Self {
        Round {
            budget: scale.saturating_mul(MATCHED_DRIVE_FACTOR).max(1),
            ceiling: None,
            window: Window::Unread,
        }
    }
}

/// Where a round's window ends.
#[derive(Clone, Debug)]
pub(crate) enum Window<R> {
    /// Not read: the round reads it first, its budget past where the round
    /// starts ([`Paged::window`]).
    Unread,
    /// At this row, which the walk reads up to.
    EndsAt(R),
    /// Past the section's end, which the walk reads to.
    SectionEnds,
}

/// What a page the drive read holds and cost.
pub(crate) struct Driven<T> {
    /// The rows, at most the page's bound, in the page's order.
    pub(crate) rows: Vec<T>,
    /// What the read established about how its request is driven.
    pub(crate) settled: Settled<T>,
    /// What SQLite counted stepping every statement the drive ran: the
    /// walk's windows, the parts' counts and the vault's span beside the
    /// page statements.
    pub(crate) stepped: Stepped,
}

impl<T: Clone> Driven<T> {
    /// The page of at most `limit` rows these are, read one past it.
    pub(crate) fn into_page(self, limit: usize) -> KeysetPage<T> {
        KeysetPage::cut(self.rows, limit, self.stepped)
    }
}

impl Snapshot {
    /// One page of at most `rows` rows narrowed by `parts`, read from
    /// `sections` in order by the driver it settles on.
    ///
    /// A part that keeps its documents in the page's order — `descending`
    /// names which way the page reads the answer's path order, and is `None`
    /// for a page in any other order — drives it from its source, the first
    /// such part in the request's order. Where none does and some part keeps
    /// what it seeks, the page asks the parts whether one keeps no more
    /// documents than `scale`, the rows the page reads, and the first that
    /// does drives it from what it matched; otherwise the page walks its own
    /// order in rounds: each walks at
    /// most a budget of rows past where the last stopped
    /// ([`Paged::window`]), and where that leaves the page short, asks the
    /// parts in the request's order whether one keeps at most the budget
    /// ([`FindStatement::PartCeiling`]); the first that does drives the rest
    /// of the page from what it matched, and otherwise the budget doubles.
    /// The budget starts at [`MATCHED_DRIVE_FACTOR`] per row of `scale` and
    /// stops at the ceiling ([`Snapshot::drive_ceiling`]); a page no part
    /// within the ceiling drives walks on unbounded past it. `settled` is
    /// what an earlier read of the same request established, which this read
    /// takes up without asking again: the driver it settled on, or the round
    /// its walk filled in, which this read walks on from.
    #[allow(clippy::too_many_arguments)] // One page's every input, each named at its call.
    pub(crate) fn drive_page<P: Paged>(
        &self,
        paged: &P,
        parts: &[&Filter],
        descending: Option<bool>,
        scale: usize,
        sections: Vec<P::Section>,
        rows: usize,
        settled: Settled<P::Row>,
        lookups: &mut Lookups,
    ) -> Result<Driven<P::Row>, StoreError> {
        let ran_before = lookups.ran.len();
        let mut found: Vec<P::Row> = Vec::new();
        let mut queue: VecDeque<P::Section> = sections.into();
        let settled = match settled {
            Settled::Unasked => match settled_driver(parts, descending) {
                Some(driver) => Settled::Driver(driver),
                None => Settled::Unasked,
            },
            settled => settled,
        };
        let settled = match settled {
            Settled::Driver(driver) => {
                self.read_rest(paged, rows, driver, &mut found, &mut queue, lookups)?;
                Settled::Driver(driver)
            }
            // A page with no section to read asks nothing.
            settled if queue.is_empty() => settled,
            // A part keeping no more documents than the page reads drives it
            // from what it matched before anything is walked.
            Settled::Unasked if let Some(at) = self.first_within(parts, scale, lookups)? => {
                let driver = Driver::Matched(at);
                self.read_rest(paged, rows, driver, &mut found, &mut queue, lookups)?;
                Settled::Driver(driver)
            }
            Settled::Unasked => {
                let first = Round::first(scale);
                self.walk_rounds(
                    paged, parts, scale, rows, first, &mut found, &mut queue, lookups,
                )?
            }
            Settled::Walking(round) => self.walk_rounds(
                paged, parts, scale, rows, round, &mut found, &mut queue, lookups,
            )?,
        };
        let mut stepped = Stepped::default();
        for ran in &lookups.ran[ran_before..] {
            stepped.add(ran.stepped);
        }
        Ok(Driven {
            rows: found,
            settled,
            stepped,
        })
    }

    /// `found` filled to `rows` from the sections `queue` holds, each read
    /// whole by `driver`, as a page reads its sections ([`fill_from`]).
    fn read_rest<P: Paged>(
        &self,
        paged: &P,
        rows: usize,
        driver: Driver,
        found: &mut Vec<P::Row>,
        queue: &mut VecDeque<P::Section>,
        lookups: &mut Lookups,
    ) -> Result<(), StoreError> {
        fill_from(
            queue.drain(..),
            rows,
            found,
            &mut lookups.ran,
            |record, section, room| paged.read(self, record, &section, driver, None, room),
        )?;
        Ok(())
    }

    /// `found` filled to `rows` from the sections `queue` holds by a walk in
    /// rounds from `round`, for a page of `scale` rows no part drove before
    /// it walked: each round walks at most its budget past where the last
    /// stopped — up to its window's end, read first where it is not known —
    /// and where that leaves the page short, asks `parts` whether one keeps
    /// at most the budget; the first that does drives the rest of the page,
    /// and otherwise the budget doubles, to the ceiling, where the page walks
    /// on. The driver the page settled on, or the round it filled — or read
    /// every section — in.
    #[allow(clippy::too_many_arguments)] // One walk's every input, each named at its call.
    fn walk_rounds<P: Paged>(
        &self,
        paged: &P,
        parts: &[&Filter],
        scale: usize,
        rows: usize,
        mut round: Round<P::Row>,
        found: &mut Vec<P::Row>,
        queue: &mut VecDeque<P::Section>,
        lookups: &mut Lookups,
    ) -> Result<Settled<P::Row>, StoreError> {
        loop {
            let Some(section) = queue.front().cloned() else {
                return Ok(Settled::Walking(round));
            };
            let end = match std::mem::replace(&mut round.window, Window::Unread) {
                Window::Unread => {
                    paged.window(self, &mut lookups.ran, &section, round.budget - 1)?
                }
                Window::EndsAt(end) => Some(end),
                Window::SectionEnds => None,
            };
            let walked = paged.read(
                self,
                &mut lookups.ran,
                &section,
                Driver::Walked,
                end.as_ref(),
                rows - found.len(),
            )?;
            found.extend(walked);
            if found.len() >= rows {
                round.window = match end {
                    Some(end) => Window::EndsAt(end),
                    None => Window::SectionEnds,
                };
                return Ok(Settled::Walking(round));
            }
            let Some(end) = end else {
                // The section ended inside the window, so the walk read all
                // of it: the next one starts a window of its own.
                queue.pop_front();
                continue;
            };
            queue[0] = paged.resumed(&section, &end);
            let ceiling = match round.ceiling {
                Some(ceiling) => ceiling,
                None => *round.ceiling.insert(self.drive_ceiling(scale, lookups)?),
            };
            let cap = round.budget.min(ceiling);
            let driver = match self.first_within(parts, cap, lookups)? {
                Some(at) => Driver::Matched(at),
                None if cap >= ceiling => Driver::Walked,
                None => {
                    round.budget = round.budget.saturating_mul(2).min(ceiling);
                    continue;
                }
            };
            self.read_rest(paged, rows, driver, found, queue, lookups)?;
            return Ok(Settled::Driver(driver));
        }
    }

    /// The first of `parts` that keeps what it seeks and keeps at most `cap`
    /// documents, by its position, asked in the request's order and no
    /// further than the first that does: one [`FindStatement::PartCeiling`]
    /// per part asked.
    fn first_within(
        &self,
        parts: &[&Filter],
        cap: usize,
        lookups: &mut Lookups,
    ) -> Result<Option<usize>, StoreError> {
        for (at, part) in keeping(parts) {
            let probe = Ran::new(FindStatement::PartCeiling, part.kept_within(cap))
                .narrowed_by(vec![part.shape()]);
            if self.ask(
                &mut lookups.ran,
                probe,
                "asking how many documents a part keeps",
            )? {
                return Ok(Some(at));
            }
        }
        Ok(None)
    }

    /// The most documents a part may keep and still drive a page reading
    /// `rows` rows from what it matched, which is where the walk's budget
    /// stops doubling: the greater of [`MATCHED_DRIVE_FACTOR`] per row and
    /// the square root of `rows` times the vault's greatest document row id.
    /// One [`FindStatement::DocumentSpan`].
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

/// The parts that keep what they seek, each beside its position among
/// `parts`. A part that excludes drives nothing.
fn keeping<'a>(parts: &'a [&'a Filter]) -> impl Iterator<Item = (usize, &'a Filter)> + 'a {
    parts
        .iter()
        .copied()
        .enumerate()
        .filter(|(_, part)| !part.shape().excludes())
}

/// The driver a page takes without walking or asking: the first part that
/// keeps its documents in the page's order where the page reads in the
/// answer's path order, and a walk where no part keeps what it seeks.
fn settled_driver(parts: &[&Filter], descending: Option<bool>) -> Option<Driver> {
    if let Some(descending) = descending
        && let Some((at, _)) = keeping(parts).find(|(_, part)| part.is_path_ordered(descending))
    {
        return Some(Driver::Ordered(at));
    }
    match keeping(parts).next() {
        None => Some(Driver::Walked),
        Some(_) => None,
    }
}
