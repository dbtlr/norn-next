//! `get`: one document, addressed by a resolution target.
//!
//! **A target that names no one document is a refusal, not a report.** The
//! verb answers about one document, so a target that resolves to more than one
//! is `vault/ambiguous-target` and a target that resolves to none is
//! `vault/unknown-target`. Neither is a report shape: a report saying "here
//! are four documents" would make every consumer of `get` write the branch the
//! refusal already is.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::address::VaultAddress;
use crate::cursor::{Cursor, Page};
use crate::document::{BlockRow, Column, DocumentPath, DocumentRow, HeadingRow, LinkRow, TagRow};
use crate::finding_row::FindingRow;
use crate::target::ResolutionTarget;

/// Which nested collection a request pages by ordinal.
///
/// On the wire a selector is the flat string itself: `"links"`,
/// `"headings"`.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum CollectionSelector {
    /// The links the document carries.
    Links,
    /// The headings the document carries.
    Headings,
    /// The block identifiers the document defines.
    Blocks,
    /// The tags the document carries.
    Tags,
    /// The findings standing over the document.
    Findings,
}

/// One page of one nested collection.
///
/// On the wire a page is an object tagged `of`:
/// `{"of":"links","page":{"rows":[…],"next":null,"moved":[]}}`. The tag names
/// which collection was paged, so a consumer reads the row type from the value
/// rather than from the request it sent.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(tag = "of", rename_all = "snake_case")]
#[non_exhaustive]
pub enum CollectionPage {
    /// A page of the document's links.
    #[non_exhaustive]
    Links {
        /// The page.
        page: Page<LinkRow>,
    },
    /// A page of the document's headings.
    #[non_exhaustive]
    Headings {
        /// The page.
        page: Page<HeadingRow>,
    },
    /// A page of the document's block identifiers.
    #[non_exhaustive]
    Blocks {
        /// The page.
        page: Page<BlockRow>,
    },
    /// A page of the document's tags.
    #[non_exhaustive]
    Tags {
        /// The page.
        page: Page<TagRow>,
    },
    /// A page of the findings standing over the document.
    #[non_exhaustive]
    Findings {
        /// The page.
        page: Page<FindingRow>,
    },
}

impl CollectionPage {
    /// A page of links.
    pub const fn links(page: Page<LinkRow>) -> Self {
        CollectionPage::Links { page }
    }

    /// A page of headings.
    pub const fn headings(page: Page<HeadingRow>) -> Self {
        CollectionPage::Headings { page }
    }

    /// A page of block identifiers.
    pub const fn blocks(page: Page<BlockRow>) -> Self {
        CollectionPage::Blocks { page }
    }

    /// A page of tags.
    pub const fn tags(page: Page<TagRow>) -> Self {
        CollectionPage::Tags { page }
    }

    /// A page of findings.
    pub const fn findings(page: Page<FindingRow>) -> Self {
        CollectionPage::Findings { page }
    }

    /// Which collection this is a page of.
    ///
    /// The match carries no wildcard, so a page minted without a selector does
    /// not compile: [`CollectionSelector`] stays the flat value a request names
    /// a collection by, and this is the one place the two lists are held
    /// together.
    pub const fn selector(&self) -> CollectionSelector {
        match self {
            CollectionPage::Links { .. } => CollectionSelector::Links,
            CollectionPage::Headings { .. } => CollectionSelector::Headings,
            CollectionPage::Blocks { .. } => CollectionSelector::Blocks,
            CollectionPage::Tags { .. } => CollectionSelector::Tags,
            CollectionPage::Findings { .. } => CollectionSelector::Findings,
        }
    }
}

/// What `get` answers with.
///
/// On the wire a report is an object tagged `shape`, and which shape it takes
/// follows from what the request asked for: a whole record, the section a
/// heading anchor named, or one page of one nested collection.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(tag = "shape", rename_all = "snake_case")]
#[non_exhaustive]
pub enum GetReport {
    /// The document, projected onto the columns the request asked for, or
    /// whole under the per-row ceiling where it asked for none.
    #[non_exhaustive]
    Record {
        /// The document's row.
        document: DocumentRow,
    },
    /// The section the target's heading anchor named.
    #[non_exhaustive]
    Section {
        /// The document the section is in.
        path: DocumentPath,
        /// The heading that opens it.
        heading: HeadingRow,
        /// The section body, from the heading to the next one at its level or
        /// above.
        body: String,
    },
    /// One page of one nested collection.
    #[non_exhaustive]
    Collection {
        /// The document the collection is on.
        path: DocumentPath,
        /// Which collection was paged.
        selector: CollectionSelector,
        /// The page.
        page: CollectionPage,
    },
}

impl GetReport {
    /// The whole record `document`.
    pub const fn record(document: DocumentRow) -> Self {
        GetReport::Record { document }
    }

    /// The section `heading` opens in the document at `path`, holding `body`.
    pub fn section(path: DocumentPath, heading: HeadingRow, body: impl Into<String>) -> Self {
        GetReport::Section {
            path,
            heading,
            body: body.into(),
        }
    }

    /// The `page` of one collection on the document at `path`.
    ///
    /// The selector is read off the page rather than passed in, so the two
    /// name one collection.
    pub const fn collection(path: DocumentPath, page: CollectionPage) -> Self {
        GetReport::Collection {
            path,
            selector: page.selector(),
            page,
        }
    }
}

/// What a `get` request carries.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[non_exhaustive]
pub struct GetParams {
    /// The vault to answer from.
    pub vault: VaultAddress,
    /// The document to answer about, and the place inside it where the target
    /// names one.
    pub target: ResolutionTarget,
    /// The columns the record carries. Empty answers the whole record under
    /// the per-row ceiling.
    pub columns: Vec<Column>,
    /// The one nested collection to page instead of answering a record.
    /// `null` answers a record or a section.
    pub collection: Option<CollectionSelector>,
    /// Where to continue the paged collection from. `null` starts at its first
    /// row.
    pub after: Option<Cursor>,
}

impl GetParams {
    /// A `get` of `target` from `vault`, answering the whole record.
    pub const fn new(vault: VaultAddress, target: ResolutionTarget) -> Self {
        GetParams {
            vault,
            target,
            columns: Vec::new(),
            collection: None,
            after: None,
        }
    }

    /// The request projecting `columns`.
    #[must_use]
    pub fn with_columns(mut self, columns: impl IntoIterator<Item = Column>) -> Self {
        self.columns = columns.into_iter().collect();
        self
    }

    /// The request paging the `collection` instead of answering a record.
    #[must_use]
    pub const fn with_collection(mut self, collection: CollectionSelector) -> Self {
        self.collection = Some(collection);
        self
    }

    /// The request continuing from `after`.
    #[must_use]
    pub fn with_after(mut self, after: Cursor) -> Self {
        self.after = Some(after);
        self
    }
}
