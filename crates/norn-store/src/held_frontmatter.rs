//! The frontmatter one document holds, read on a snapshot without its body.
//!
//! **What the store derived from a document's bytes, beside the hash of the
//! bytes it derived it from**, as [`crate::held_links`] reads a document's
//! links: a caller that holds a file's content hash from a streamed read that
//! kept no bytes, and wants what those bytes' frontmatter says, reads the
//! canonical projection here instead of reading the file. Where the hash it
//! holds is the hash answered here, the projection is the one a parse of
//! those bytes derived; where the hashes differ the answer is about other
//! bytes, and deciding what that means is the caller's.
//!
//! **Its one consumer is the write gate's judgment of a carried move**: a
//! document a move carries byte for byte is judged again at its destination,
//! since a schema rule's path selectors and allowed paths read where it
//! stands, and the host reads its frontmatter here rather than its bytes, so
//! the move holds the projection and no copy of the document.
//!
//! The read is one statement on the snapshot, named by
//! [`HeldFrontmatterStatement`] and run through [`Snapshot::run_statement`],
//! so it is counted on the snapshot and explained by
//! [`Snapshot::held_frontmatter_plans`] as every read builder's statements
//! are: the document's row by its path. It names no body, so a document's
//! body is not what reading its frontmatter costs; the projection is.

use norn_db::EmittedPlan;
use norn_db::rusqlite::types::Value;

use crate::error::{self, StoreError};
use crate::json::{FrontmatterValue, frontmatter_of_projection};
use crate::path::DocumentPath;
use crate::read::{Ran, ReadStatement};
use crate::store::Snapshot;

/// Every statement shape the held-frontmatter read runs, named.
///
/// The same discipline as [`crate::HeldLinksStatement`]:
/// [`HeldFrontmatterStatement::all`] holds each shape once,
/// [`HeldFrontmatterStatement::slot`] is exhaustive over the enum, and
/// [`HELD_FRONTMATTER_STATEMENTS`] is the count a census is checked against.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HeldFrontmatterStatement {
    /// The holder's content hash, projection and frontmatter diagnostic count,
    /// by its path as stored: one equality seek of `documents_path`.
    Holder,
}

/// How many statement shapes [`HeldFrontmatterStatement::all`] holds.
pub const HELD_FRONTMATTER_STATEMENTS: usize = 1;

impl HeldFrontmatterStatement {
    /// Every statement shape, in slot order, which is the order a read runs
    /// them in.
    pub fn all() -> [Self; HELD_FRONTMATTER_STATEMENTS] {
        [Self::Holder]
    }

    /// Where this statement stands in [`Self::all`]. Exhaustive, so a shape
    /// added to the enum has to take a slot.
    pub fn slot(self) -> usize {
        match self {
            Self::Holder => 0,
        }
    }
}

impl From<HeldFrontmatterStatement> for ReadStatement {
    fn from(statement: HeldFrontmatterStatement) -> Self {
        ReadStatement::HeldFrontmatter(statement)
    }
}

/// What one document's frontmatter block came to, as the store derived it.
#[derive(Clone, Debug, PartialEq)]
pub enum HeldBlock {
    /// The block read, as the value tree its canonical projection holds
    /// ([`frontmatter_of_projection`]).
    Read(FrontmatterValue),
    /// The document carries no frontmatter block.
    None,
    /// The document carries a block nothing read: its fields are unknown
    /// rather than absent.
    Unread,
}

/// The frontmatter one document holds, as the store derived it, beside the
/// content hash of the bytes it derived it from.
#[derive(Clone, Debug, PartialEq)]
pub struct HeldFrontmatter {
    /// The hash the document's row records, spelled as every stored content
    /// hash is.
    pub content_hash: String,
    /// What its frontmatter block came to.
    pub block: HeldBlock,
}

/// A statement the held-frontmatter read ran, with the plan SQLite reported
/// for the text and the values it ran with.
#[derive(Clone, Debug)]
pub struct HeldFrontmatterPlan {
    pub statement: ReadStatement,
    pub plan: EmittedPlan,
}

impl Snapshot {
    /// The frontmatter the document at `path` holds and the content hash it
    /// was derived from, or `None` where no document stands at `path` — no
    /// file there, a file whose bytes do not decode as a document, or another
    /// spelling of the path than the one stored.
    ///
    /// **Its body is never read.** One seek, its row by path
    /// ([`HeldFrontmatterStatement`]). A projection that is not one this store
    /// writes refuses as damaged. A row with no projection reads as no block
    /// where its parse raised no frontmatter-scoped diagnostic, and as a
    /// block nothing read where it raised one, as the row's own columns state.
    pub fn held_frontmatter(
        &self,
        path: &DocumentPath,
    ) -> Result<Option<HeldFrontmatter>, StoreError> {
        self.read_held_frontmatter(path, &mut Vec::new())
    }

    /// Every statement [`Snapshot::held_frontmatter`] runs for `path`, in the
    /// order it runs them, each with the plan SQLite reported for it.
    pub fn held_frontmatter_plans(
        &self,
        path: &DocumentPath,
    ) -> Result<Vec<HeldFrontmatterPlan>, StoreError> {
        let mut record = Vec::new();
        self.read_held_frontmatter(path, &mut record)?;
        self.explained(record, |statement, _, plan| HeldFrontmatterPlan {
            statement,
            plan,
        })
    }

    /// The read [`Snapshot::held_frontmatter`] answers and
    /// [`Snapshot::held_frontmatter_plans`] explains, recording every
    /// statement it runs in `record`.
    fn read_held_frontmatter(
        &self,
        path: &DocumentPath,
        record: &mut Vec<Ran>,
    ) -> Result<Option<HeldFrontmatter>, StoreError> {
        let holder = Ran::new(
            HeldFrontmatterStatement::Holder,
            (
                "SELECT d.content_hash, d.frontmatter, d.frontmatter_diagnostic_count \
                 FROM documents AS d WHERE d.path = ?1"
                    .to_string(),
                vec![Value::Text(path.as_str().to_string())],
            ),
        );
        let Some((content_hash, projection, diagnostics)) = self
            .run_statement(record, holder, |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            })
            .map_err(|problem| error::sql("reading a holder's frontmatter", problem))?
            .into_iter()
            .next()
        else {
            return Ok(None);
        };
        let block = match projection {
            Some(text) => HeldBlock::Read(frontmatter_of_projection(&text)?),
            None if diagnostics == 0 => HeldBlock::None,
            None => HeldBlock::Unread,
        };
        Ok(Some(HeldFrontmatter {
            content_hash,
            block,
        }))
    }
}
