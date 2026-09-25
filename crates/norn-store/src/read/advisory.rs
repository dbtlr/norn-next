//! What a read's applied parts assumed: the advisories an answer carries
//! beside its rows.
//!
//! # A mixed-offset comparison is advised for every value the key holds
//!
//! A dated order reads a date stating no offset at offset zero, so a
//! comparison between a date stating one and a date stating none is decided by
//! that assumption. **Every read verb raises it one way**: a find, a count, a
//! validate and a search each hand the comparisons they made to
//! [`Snapshot::offset_advisories`], and an answer is advised of each by key and
//! by where it compared the key's dates — the order, the grouping, or a
//! predicate. The advisory speaks for **every value the key holds in the
//! snapshot**, never for the page alone:
//!
//! - An order places each row among every date the key holds, so it is advised
//!   where the key holds both spellings, whatever spellings the page's rows
//!   write.
//! - A grouping makes one group of the dates reading as one instant and orders
//!   the groups, so it too is advised where the key holds both spellings.
//! - A predicate — an equality, an inequality, a membership, a `before` or an
//!   `after` bound — compares each value it names against every date the key
//!   holds to decide which documents it keeps, and a document it drops never
//!   reaches the page. So it is advised where some value it names is spelled
//!   otherwise than some date the key holds.
//!
//! Reading the whole key rather than the documents a conjunction's other parts
//! keep makes the advisory an over-statement where those parts narrow a key's
//! dates to one spelling, and never a silence: an answer whose order, grouping
//! or membership a mixed-offset comparison decided is always advised. A
//! conjunction some part empties compared no date, so it is advised of
//! nothing ([`Conjunction::date_comparisons`]).
//!
//! **The cost is one statement per dated key a read compares**
//! ([`FindStatement::OffsetSpellings`]): two existence seeks of the offset
//! index, whatever the vault's size and however many parts name the key. A
//! read comparing no dated key asks nothing.

use std::collections::{BTreeMap, BTreeSet};

use norn_wire::{AnswerAdvisory, ComparedBy};

use super::{Conjunction, Lookups, Ran};
use crate::error::{self, StoreError};
use crate::fields::{ContentModel, OffsetSpelling};
use crate::find::{FindStatement, compose_offset_spellings};
use crate::store::Snapshot;

/// How a read compared a dated key's values, which decides when the
/// comparison met two spellings.
#[derive(Clone, Debug)]
pub(crate) enum Compared {
    /// An order, placing each of the key's dates among the others.
    Order,
    /// A grouping, making one group of the dates reading as one instant and
    /// ordering the groups.
    Group,
    /// A predicate, comparing the key's dates against values it names, of
    /// these spellings.
    Parts(BTreeSet<OffsetSpelling>),
}

impl Compared {
    /// Whether this comparison met two spellings, given the spellings the key
    /// holds: an order or a grouping where the key holds both, and a
    /// predicate where a value it names is spelled otherwise than a date the
    /// key holds.
    fn is_mixed(&self, held: &BTreeSet<OffsetSpelling>) -> bool {
        match self {
            Compared::Order | Compared::Group => held.len() > 1,
            Compared::Parts(named) => named
                .iter()
                .any(|named| held.iter().any(|held| held != named)),
        }
    }

    /// Where the wire says the comparison was made.
    const fn compared_by(&self) -> ComparedBy {
        match self {
            Compared::Order => ComparedBy::Sort,
            Compared::Group => ComparedBy::Group,
            Compared::Parts(_) => ComparedBy::Predicate,
        }
    }
}

/// One comparison a read made over a key with a dated order: the key, and
/// how the request compared it.
#[derive(Clone, Debug)]
pub(crate) struct DateComparison {
    pub(crate) key: String,
    pub(crate) compared: Compared,
}

impl DateComparison {
    /// The comparison of `key`'s values `compared` names, where `declared`
    /// gives `key` a dated order, and `None` where it does not: a key with no
    /// dated order compares no date.
    pub(crate) fn of_dated(key: &str, compared: Compared, declared: &ContentModel) -> Option<Self> {
        declared
            .typed_order(key)
            .is_some_and(|order| order.is_dated())
            .then(|| DateComparison {
                key: key.to_string(),
                compared,
            })
    }
}

impl Conjunction {
    /// The comparisons of dated keys a read made: `whole` — its order or its
    /// grouping — first, then the conjunction's parts in request order. None
    /// where some part empties the match, which leaves no date compared.
    pub(crate) fn date_comparisons(
        &self,
        whole: impl IntoIterator<Item = DateComparison>,
    ) -> Vec<DateComparison> {
        if self.matches_nothing {
            return Vec::new();
        }
        whole
            .into_iter()
            .chain(self.compared_dates.iter().cloned())
            .collect()
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
            let advisory = AnswerAdvisory::mixed_offset(key, comparison.compared.compared_by());
            if comparison.compared.is_mixed(&held[key]) && !advisories.contains(&advisory) {
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

    use super::Compared;
    use crate::fields::OffsetSpelling::{Stated, Unstated};

    /// An order and a grouping are mixed exactly where the key holds both
    /// spellings; a predicate exactly where a value it names differs in
    /// spelling from a date the key holds. A key holding no date is never
    /// mixed.
    #[test]
    fn a_comparison_is_mixed_exactly_where_two_spellings_meet() {
        let none = BTreeSet::new();
        let stated = BTreeSet::from([Stated]);
        let unstated = BTreeSet::from([Unstated]);
        let both = BTreeSet::from([Stated, Unstated]);
        for whole in [Compared::Order, Compared::Group] {
            assert!(!whole.is_mixed(&none), "{whole:?}");
            assert!(!whole.is_mixed(&stated), "{whole:?}");
            assert!(!whole.is_mixed(&unstated), "{whole:?}");
            assert!(whole.is_mixed(&both), "{whole:?}");
        }
        let parts = |named: &[crate::fields::OffsetSpelling]| {
            Compared::Parts(named.iter().copied().collect())
        };
        assert!(!parts(&[Stated]).is_mixed(&stated));
        assert!(!parts(&[Unstated]).is_mixed(&none));
        assert!(parts(&[Unstated]).is_mixed(&stated));
        assert!(!parts(&[Unstated]).is_mixed(&unstated));
        assert!(parts(&[Stated]).is_mixed(&unstated));
        assert!(parts(&[Stated]).is_mixed(&both));
        assert!(parts(&[Stated, Unstated]).is_mixed(&stated));
    }
}
