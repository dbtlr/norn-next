//! The links one document holds, read on a snapshot without its body.
//!
//! **What the store derived from a document's bytes, beside the hash of the
//! bytes it derived them from.** A caller that holds a file's content hash —
//! from a streamed read that kept no bytes — and wants the links those bytes
//! hold reads them here instead of parsing the file: where the hash it holds
//! is the hash answered here, the links are the ones a parse of those bytes
//! yields, because the store's are what the host's one reading of a
//! document's links derived from them. Where the hashes differ the answer is
//! about other bytes, and deciding what that means is the caller's.
//!
//! The read is two statements on the snapshot, each named by
//! [`HeldLinksStatement`] and run through [`Snapshot::run_statement`], so it
//! is counted on the snapshot and explained by [`Snapshot::held_links_plans`]
//! as every read builder's statements are: the document's row by its path,
//! and its links by its row id, in document order. Neither names the body,
//! so a document's weight is not what reading its links costs.

use norn_db::EmittedPlan;
use norn_db::rusqlite::types::Value;

use crate::error::{self, StoreError};
use crate::facts::LinkFact;
use crate::find::Nested;
use crate::path::DocumentPath;
use crate::read::{Ran, ReadStatement};
use crate::request::stored_link;
use crate::store::Snapshot;

/// Every statement shape the held-links read runs, named.
///
/// The same discipline as [`crate::GetStatement`]: [`HeldLinksStatement::all`]
/// holds each shape once, [`HeldLinksStatement::slot`] is exhaustive over the
/// enum, and [`HELD_LINKS_STATEMENTS`] is the count a census is checked
/// against.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HeldLinksStatement {
    /// The holder's row id and content hash, by its path as stored: one
    /// equality seek of `documents_path`.
    Holder,
    /// Every link the holder holds, in document order: one range seek of
    /// `links_document_ordinal` at the holder, which hands the rows back in
    /// order. Not run where no document stands at the path.
    Links,
}

/// How many statement shapes [`HeldLinksStatement::all`] holds.
pub const HELD_LINKS_STATEMENTS: usize = 2;

impl HeldLinksStatement {
    /// Every statement shape, in slot order, which is the order a read runs
    /// them in.
    pub fn all() -> [Self; HELD_LINKS_STATEMENTS] {
        [Self::Holder, Self::Links]
    }

    /// Where this statement stands in [`Self::all`]. Exhaustive, so a shape
    /// added to the enum has to take a slot.
    pub fn slot(self) -> usize {
        match self {
            Self::Holder => 0,
            Self::Links => 1,
        }
    }
}

impl From<HeldLinksStatement> for ReadStatement {
    fn from(statement: HeldLinksStatement) -> Self {
        ReadStatement::HeldLinks(statement)
    }
}

/// The links one document holds, as the store derived them, beside the
/// content hash of the bytes it derived them from.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HeldLinks {
    /// The hash the document's row records, spelled as every stored content
    /// hash is.
    pub content_hash: String,
    /// Every link the document holds, in the order its facts list them —
    /// its frontmatter's wikilinks, then its body's links — as the host's
    /// one reading of a document's links hands them over.
    pub links: Vec<LinkFact>,
}

/// A statement the held-links read ran, with the plan SQLite reported for the
/// text and the values it ran with.
#[derive(Clone, Debug)]
pub struct HeldLinksPlan {
    pub statement: ReadStatement,
    pub plan: EmittedPlan,
}

impl Snapshot {
    /// The links the document at `path` holds and the content hash they were
    /// derived from, or `None` where no document stands at `path` — no file
    /// there, a file whose bytes do not decode as a document, or another
    /// spelling of the path than the one stored.
    ///
    /// **Its body is never read.** Two seeks, whatever the document weighs:
    /// its row by path, and its links by row id
    /// ([`HeldLinksStatement`]). A store that holds a link row it cannot read
    /// refuses as damaged.
    ///
    /// **What a moved document's links are read from.** A move whose
    /// document the plan carries byte for byte holds no body to parse, so
    /// the host's planner and applier take the moved document's own links
    /// from here where the hash they streamed is the hash answered.
    pub fn held_links(&self, path: &DocumentPath) -> Result<Option<HeldLinks>, StoreError> {
        self.read_held_links(path, &mut Vec::new())
    }

    /// Every statement [`Snapshot::held_links`] runs for `path`, in the order
    /// it runs them, each with the plan SQLite reported for it: the read
    /// itself, run on this snapshot, and each plan taken of the very text and
    /// values its statement ran with.
    pub fn held_links_plans(&self, path: &DocumentPath) -> Result<Vec<HeldLinksPlan>, StoreError> {
        let mut record = Vec::new();
        self.read_held_links(path, &mut record)?;
        self.explained(record, |statement, _, plan| HeldLinksPlan {
            statement,
            plan,
        })
    }

    /// The read [`Snapshot::held_links`] answers and
    /// [`Snapshot::held_links_plans`] explains, recording every statement it
    /// runs in `record`.
    fn read_held_links(
        &self,
        path: &DocumentPath,
        record: &mut Vec<Ran>,
    ) -> Result<Option<HeldLinks>, StoreError> {
        let holder = Ran::new(
            HeldLinksStatement::Holder,
            (
                "SELECT d.id, d.content_hash FROM documents AS d WHERE d.path = ?1".to_string(),
                vec![Value::Text(path.as_str().to_string())],
            ),
        );
        let Some((document, content_hash)) = self
            .run_statement(record, holder, |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(|problem| error::sql("reading a holder's row", problem))?
            .into_iter()
            .next()
        else {
            return Ok(None);
        };
        let links = Ran::new(
            HeldLinksStatement::Links,
            (
                format!(
                    "SELECT {columns} FROM links AS n WHERE n.document = ?1 ORDER BY n.ordinal",
                    columns = Nested::Links.columns(),
                ),
                vec![Value::Integer(document)],
            ),
        );
        let links = self
            .run_statement(record, links, stored_link)
            .map_err(|problem| error::sql("reading the links a document holds", problem))?
            .into_iter()
            .collect::<Result<Vec<LinkFact>, StoreError>>()?;
        Ok(Some(HeldLinks {
            content_hash,
            links,
        }))
    }
}
