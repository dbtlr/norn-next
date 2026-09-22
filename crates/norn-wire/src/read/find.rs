//! `find`: the documents matching a conjunction, in a stated order.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::address::VaultAddress;
use crate::cursor::{Cursor, Page};
use crate::document::{Column, DocumentRow};
use crate::predicate::Predicate;

/// Which way an order runs.
///
/// On the wire a direction is the flat string itself: `"ascending"`,
/// `"descending"`.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Direction {
    /// Smallest first. A document missing the sort field orders before every
    /// document that carries it.
    Ascending,
    /// Largest first. A document missing the sort field orders after every
    /// document that carries it.
    Descending,
}

/// What an order sorts by.
///
/// On the wire a key is an object tagged `by`: `{"by":"field","key":"due"}`,
/// `{"by":"path"}`.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(tag = "by", rename_all = "snake_case")]
#[non_exhaustive]
pub enum SortKey {
    /// One frontmatter field, compared under the type the vault's schema
    /// declares for it.
    #[non_exhaustive]
    Field {
        /// The frontmatter key.
        key: String,
    },
    /// The document's path.
    Path {},
}

impl SortKey {
    /// The frontmatter field `key`.
    pub fn field(key: impl Into<String>) -> Self {
        SortKey::Field { key: key.into() }
    }

    /// The document's path.
    pub const fn path() -> Self {
        SortKey::Path {}
    }
}

/// The order a page of documents is in.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[non_exhaustive]
pub struct Sort {
    /// What the order sorts by.
    pub key: SortKey,
    /// Which way it runs.
    pub direction: Direction,
}

impl Sort {
    /// The order that sorts by `key`, running `direction`.
    pub const fn new(key: SortKey, direction: Direction) -> Self {
        Sort { key, direction }
    }
}

/// What `find` answers with: one page of document rows.
pub type FindReport = Page<DocumentRow>;

/// What a `find` request carries.
///
/// **`find` is the sole verb that answers document rows**, and the sole verb
/// a resolution predicate is meaningful on: a `resolves` part here selects the
/// documents a target names, where the same part on `count` or `validate` is
/// reported as an unsatisfied part instead.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[non_exhaustive]
pub struct FindParams {
    /// The vault to answer from.
    pub vault: VaultAddress,
    /// The conjunction a document must satisfy. Empty matches every document.
    pub predicates: Vec<Predicate>,
    /// The order the rows come back in. `null` orders by path, ascending.
    pub sort: Option<Sort>,
    /// The columns each row carries. Empty projects the path alone.
    pub columns: Vec<Column>,
    /// How many rows at most. `null` leaves the ceiling to the host.
    pub limit: Option<u32>,
    /// Where to continue from. `null` starts at the first row.
    pub after: Option<Cursor>,
}

impl FindParams {
    /// A `find` over `vault`: every document, ordered by path, projecting the
    /// path alone.
    pub const fn new(vault: VaultAddress) -> Self {
        FindParams {
            vault,
            predicates: Vec::new(),
            sort: None,
            columns: Vec::new(),
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

    /// The request ordered by `sort`.
    #[must_use]
    pub fn with_sort(mut self, sort: Sort) -> Self {
        self.sort = Some(sort);
        self
    }

    /// The request projecting `columns`.
    #[must_use]
    pub fn with_columns(mut self, columns: impl IntoIterator<Item = Column>) -> Self {
        self.columns = columns.into_iter().collect();
        self
    }

    /// The request bounded at `limit` rows.
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
