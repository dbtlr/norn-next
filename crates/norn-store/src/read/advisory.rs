//! What a read's applied parts assumed: the advisories an answer carries
//! beside its rows.
//!
//! # A mixed-offset comparison is advised for every value the key holds
//!
//! A dated order reads a date stating no offset at offset zero, so a
//! comparison between a date stating one and a date stating none is decided by
//! that assumption. A read is advised of it by key and by where it compared
//! the key's dates — the order, or a predicate — and the advisory speaks for
//! **every value the key holds in the snapshot**, never for the page alone:
//!
//! - An order places each row among every date the key holds, so it is advised
//!   where the key holds both spellings, whatever spellings the page's rows
//!   write.
//! - A predicate — an equality, an inequality, a membership, a `before` or an
//!   `after` bound — compares each value it names against every date the key
//!   holds to decide which documents it keeps, and a document it drops never
//!   reaches the page. So it is advised where some value it names is spelled
//!   otherwise than some date the key holds.
//!
//! Reading the whole key rather than the documents a conjunction's other parts
//! keep makes the advisory an over-statement where those parts narrow a key's
//! dates to one spelling, and never a silence: an answer whose order or
//! membership a mixed-offset comparison decided is always advised.
//!
//! **The cost is one statement per dated key a read compares**
//! ([`FindStatement::OffsetSpellings`]): two existence seeks of the offset
//! index, whatever the vault's size and however many parts name the key. A
//! read comparing no dated key asks nothing.

use std::collections::{BTreeMap, BTreeSet};

use norn_wire::{AnswerAdvisory, ComparedBy};

use super::{Lookups, Ran};
use crate::error::{self, StoreError};
use crate::fields::OffsetSpelling;
use crate::find::{FindStatement, compose_offset_spellings};
use crate::store::Snapshot;

/// One comparison a read made over a key with a dated order: the key, where
/// the request compared it, and the spellings of the values the request
/// named — none for an order, which compares the key's dates with each other.
#[derive(Clone, Debug)]
pub(crate) struct DateComparison {
    pub(crate) key: String,
    pub(crate) by: ComparedBy,
    pub(crate) named: BTreeSet<OffsetSpelling>,
}

impl DateComparison {
    /// Whether this comparison met two spellings, given the spellings the key
    /// holds: an order where the key holds both, and a predicate where a value
    /// it names is spelled otherwise than a date the key holds.
    fn is_mixed(&self, held: &BTreeSet<OffsetSpelling>) -> bool {
        match self.by {
            ComparedBy::Predicate => self
                .named
                .iter()
                .any(|named| held.iter().any(|held| held != named)),
            // An order, which compares the key's dates with each other.
            _ => held.len() > 1,
        }
    }
}

impl Snapshot {
    /// The advisories `comparisons` earn, once per key and place, in the order
    /// they are listed: each key's spellings are read once
    /// ([`FindStatement::OffsetSpellings`]), however many comparisons name it.
    pub(crate) fn offset_advisories(
        &self,
        comparisons: &[DateComparison],
        lookups: &mut Lookups,
    ) -> Result<Vec<AnswerAdvisory>, StoreError> {
        let mut held: BTreeMap<&str, BTreeSet<OffsetSpelling>> = BTreeMap::new();
        let mut advisories = Vec::new();
        for comparison in comparisons {
            let key = comparison.key.as_str();
            if !held.contains_key(key) {
                held.insert(key, self.offset_spellings(key, lookups)?);
            }
            let advisory = AnswerAdvisory::mixed_offset(key, comparison.by);
            if comparison.is_mixed(&held[key]) && !advisories.contains(&advisory) {
                advisories.push(advisory);
            }
        }
        Ok(advisories)
    }

    /// The spellings the typed dates under `key` are written in.
    fn offset_spellings(
        &self,
        key: &str,
        lookups: &mut Lookups,
    ) -> Result<BTreeSet<OffsetSpelling>, StoreError> {
        let answered = self
            .run_statement(
                &mut lookups.ran,
                Ran::new(
                    FindStatement::OffsetSpellings,
                    compose_offset_spellings(key),
                ),
                |row| Ok((row.get::<_, bool>(0)?, row.get::<_, bool>(1)?)),
            )
            .map_err(|problem| {
                error::sql("reading the offset spellings a date key holds", problem)
            })?;
        let (unstated, stated) = answered.into_iter().next().unwrap_or_default();
        Ok([
            (unstated, OffsetSpelling::Unstated),
            (stated, OffsetSpelling::Stated),
        ]
        .into_iter()
        .filter_map(|(holds, spelling)| holds.then_some(spelling))
        .collect())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use norn_wire::ComparedBy;

    use super::DateComparison;
    use crate::fields::OffsetSpelling::{Stated, Unstated};

    fn compared(by: ComparedBy, named: &[crate::fields::OffsetSpelling]) -> DateComparison {
        DateComparison {
            key: "due".to_string(),
            by,
            named: named.iter().copied().collect(),
        }
    }

    /// An order is mixed exactly where the key holds both spellings; a
    /// predicate exactly where a value it names differs in spelling from a
    /// date the key holds. A key holding no date is never mixed.
    #[test]
    fn a_comparison_is_mixed_exactly_where_two_spellings_meet() {
        let none = BTreeSet::new();
        let stated = BTreeSet::from([Stated]);
        let unstated = BTreeSet::from([Unstated]);
        let both = BTreeSet::from([Stated, Unstated]);
        let order = compared(ComparedBy::Sort, &[]);
        assert!(!order.is_mixed(&none));
        assert!(!order.is_mixed(&stated));
        assert!(order.is_mixed(&both));
        assert!(!compared(ComparedBy::Predicate, &[Stated]).is_mixed(&stated));
        assert!(!compared(ComparedBy::Predicate, &[Unstated]).is_mixed(&none));
        assert!(compared(ComparedBy::Predicate, &[Unstated]).is_mixed(&stated));
        assert!(!compared(ComparedBy::Predicate, &[Unstated]).is_mixed(&unstated));
        assert!(compared(ComparedBy::Predicate, &[Stated]).is_mixed(&unstated));
        assert!(compared(ComparedBy::Predicate, &[Stated]).is_mixed(&both));
        assert!(compared(ComparedBy::Predicate, &[Stated, Unstated]).is_mixed(&stated));
    }
}
