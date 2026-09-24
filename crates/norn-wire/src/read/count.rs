//! `count`: how many documents match, grouped by the keys a request names.
//!
//! **The grouping tuple has one spelling.** A group member for which the
//! document carries no scalar value — and, under a key the vault declares with
//! a typed order, no scalar that reads as that type — is `null`, on the row and
//! in the cursor key alike, and [`Tally::cursor_key`] is the one function that turns the row into
//! the key, so the two cannot drift into two orders sharing a name.
//!
//! **A tally counts documents, and one document may stand in several
//! groups.** A key holding a set groups the document once per element, and a
//! tag grouping once per tag, so the tallies of one answer can sum past the
//! documents the request matched.
//!
//! **`count` hydrates no document row**, so [`CountParams`] carries no
//! projection: the answer is how many, and a request that wants the documents
//! wants `find`. Tallies of findings are `validate --summary` rather than a
//! grouping here.
//!
//! **A resolution predicate is not applicable.** `resolves` answers which
//! documents a target names, which is a `find`; a `count` carrying one is
//! answered with the tallies it earned and reports the part as
//! [`Unsatisfied::ResolvesNotApplicable`](crate::Unsatisfied::ResolvesNotApplicable)
//! rather than refusing.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::address::VaultAddress;
use crate::cursor::{Cursor, CursorKey, Page};
use crate::predicate::Predicate;

/// What a tally is grouped by.
///
/// On the wire a key is an object tagged `by`: `{"by":"field","key":"type"}`,
/// `{"by":"tag"}`. The two are the whole of it: a count groups by a
/// frontmatter field's value or by a tag, and nothing else in the vocabulary
/// is a grouping a document has one of.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(tag = "by", rename_all = "snake_case")]
#[non_exhaustive]
pub enum GroupKey {
    /// One frontmatter field's value.
    #[non_exhaustive]
    Field {
        /// The frontmatter key.
        key: String,
    },
    /// The tags a document carries, one group per tag.
    Tag {},
}

impl GroupKey {
    /// The frontmatter field `key`.
    pub fn field(key: impl Into<String>) -> Self {
        GroupKey::Field { key: key.into() }
    }

    /// The tags a document carries.
    pub const fn tag() -> Self {
        GroupKey::Tag {}
    }
}

/// One group and how many documents are in it.
///
/// Under a key the vault declares with a typed order, a group is the values
/// sharing one typed value, and its member is labelled by the byte-least
/// spelling among the tally's own documents. So one typed value may carry
/// different labels in two tallies, and under two conjunctions; a page's
/// cursor, and a tally's agreement with a `find` of its labels, hold tally by
/// tally.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[non_exhaustive]
pub struct Tally {
    /// One value per group key the request named, in the request's own order,
    /// and `null` where the document carries no scalar value for that key —
    /// or, under a key declared with a typed order, no scalar that reads as
    /// that type.
    pub group: Vec<Option<String>>,
    /// How many documents are in the group.
    pub count: u64,
}

impl Tally {
    /// The `count` of documents in `group`.
    pub fn new(group: impl IntoIterator<Item = Option<String>>, count: u64) -> Self {
        Tally {
            group: group.into_iter().collect(),
            count,
        }
    }

    /// Where a page of tallies stops at this row.
    ///
    /// The destructuring carries no wildcard, so a field added to a tally does
    /// not compile until this says what the order stops at. The grouping tuple
    /// is spelled one way on the row and in the key alike — a member for which
    /// the document carries no scalar value, or under a key declared with a
    /// typed order none that reads as that type, is `null` in both — so a
    /// continuation names the position the page actually reached.
    pub fn cursor_key(&self) -> CursorKey {
        let Tally { group, count: _ } = self;
        CursorKey::tally(group.clone())
    }
}

/// What `count` answers with: one page of tallies.
///
/// Each tally counts the documents in its group, and a document holding
/// several values under a grouped key stands in the group of each, so the
/// counts can sum past the documents the request matched.
pub type CountReport = Page<Tally>;

/// What a `count` request carries.
///
/// A count answers how many documents match, in one tally per group the
/// request names, and it carries no projection. A `resolves` predicate is
/// answered with the tallies the rest of the request earned and reported back
/// as the unsatisfied part `resolves_not_applicable`.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[non_exhaustive]
pub struct CountParams {
    /// The vault to answer from.
    pub vault: VaultAddress,
    /// The conjunction a document must satisfy to be counted. Empty counts
    /// every document.
    pub predicates: Vec<Predicate>,
    /// What to group by, in the order the groups are reported in. Empty
    /// answers one tally over the whole match.
    pub by: Vec<GroupKey>,
    /// How many tallies at most. `null` leaves the ceiling to the host.
    pub limit: Option<u32>,
    /// Where to continue from. `null` starts at the first tally.
    pub after: Option<Cursor>,
}

impl CountParams {
    /// A `count` over `vault`: every document, in one ungrouped tally.
    pub const fn new(vault: VaultAddress) -> Self {
        CountParams {
            vault,
            predicates: Vec::new(),
            by: Vec::new(),
            limit: None,
            after: None,
        }
    }

    /// The request filtered by `predicates`.
    #[must_use]
    pub fn with_predicates(mut self, predicates: impl IntoIterator<Item = Predicate>) -> Self {
        self.predicates = predicates.into_iter().collect();
        self
    }

    /// The request grouped by `by`.
    #[must_use]
    pub fn with_by(mut self, by: impl IntoIterator<Item = GroupKey>) -> Self {
        self.by = by.into_iter().collect();
        self
    }

    /// The request bounded at `limit` tallies.
    #[must_use]
    pub const fn with_limit(mut self, limit: u32) -> Self {
        self.limit = Some(limit);
        self
    }

    /// The request continuing from `after`.
    #[must_use]
    pub fn with_after(mut self, after: Cursor) -> Self {
        self.after = Some(after);
        self
    }
}
