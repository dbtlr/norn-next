//! The statements the get builder emits, named, and the one composer that
//! spells each of them with its parameters.
//!
//! **One function writes a statement's text and binds its parameters.**
//! [`compose`] takes what one statement reads and spells it, numbering each
//! parameter as it writes the placeholder for it, so the text and the values
//! cannot drift apart, and the plan a bar judges is the plan of the statement
//! the get ran.

use norn_db::rusqlite::types::Value;

use crate::find::Nested;
use crate::read::{Binder, FINDING_ROW_COLUMNS, Ran};
use crate::request::DOCUMENT_HEADINGS_SQL;

/// Every statement shape the get builder runs, named.
///
/// The same discipline as [`crate::FindStatement`]: [`GetStatement::all`]
/// holds each shape once, [`GetStatement::slot`] is exhaustive over the enum,
/// and [`GET_STATEMENTS`] is the count a census is checked against. A get
/// resolves its target as every read resolves one, answers a record through
/// the hydration a find's rows are read through, resolves a links page's
/// links as a row's links are resolved, and reads the active fingerprint and
/// the field universe as a find does; the statements those run are
/// [`crate::FindStatement`]s and are named there.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GetStatement {
    /// Every heading one document carries, in document order: one range seek
    /// of `headings_document_ordinal` at the document. What a heading anchor
    /// is matched against, so its cost is the document's headings, and
    /// [`crate::GetWork::anchor_headings`] counts the rows it hands back.
    DocumentHeadings,
    /// The first definition of one block identifier in one document, in
    /// document order: a range seek of `blocks_document_ordinal` at the
    /// document that tests each definition's identifier and stops at the
    /// first that matches.
    BlockDefinition,
    /// One document's whole body, by row id: what a section's or a block's
    /// bytes are read out of.
    DocumentBody,
    /// One page of one nested collection, in document order: a seek of the
    /// collection's `(document, ordinal)` index at the document, from the
    /// ordinal the page continues after, that stops at the page's bound.
    CollectionPage(Nested),
    /// One page of the findings standing at the document's path under the
    /// active fingerprint, in `(kind, id)` order: a seek of `findings_path`
    /// at `(path, fingerprint)` from the kind the page continues in — the id
    /// it continues after tested within that kind — that stops at the page's
    /// bound.
    FindingPage,
}

/// How many statement shapes [`GetStatement::all`] holds.
pub const GET_STATEMENTS: usize = 5;

impl GetStatement {
    /// Every statement shape, in slot order. The collection a page reads is
    /// not part of its slot: a bar that cares ranges over it itself.
    pub fn all() -> [Self; GET_STATEMENTS] {
        [
            Self::DocumentHeadings,
            Self::BlockDefinition,
            Self::DocumentBody,
            Self::CollectionPage(Nested::Links),
            Self::FindingPage,
        ]
    }

    /// Where this statement stands in [`Self::all`]. Exhaustive, so a shape
    /// added to the enum has to take a slot.
    pub fn slot(self) -> usize {
        let slot = match self {
            Self::DocumentHeadings => 0,
            Self::BlockDefinition => 1,
            Self::DocumentBody => 2,
            Self::CollectionPage(_) => 3,
            Self::FindingPage => 4,
        };
        assert!(
            slot < GET_STATEMENTS,
            "slot {slot} is outside the enumeration: grow `all` and `GET_STATEMENTS` with the \
             statement that took it"
        );
        slot
    }
}

/// What one get statement reads.
pub(crate) enum Spelled<'a> {
    /// [`GetStatement::DocumentHeadings`].
    DocumentHeadings { document: i64 },
    /// [`GetStatement::BlockDefinition`].
    BlockDefinition { document: i64, id: &'a str },
    /// [`GetStatement::DocumentBody`].
    DocumentBody { document: i64 },
    /// [`GetStatement::CollectionPage`]: at most `rows` of `collection`,
    /// after the ordinal `after`, or from the first where it is `None`.
    CollectionPage {
        collection: Nested,
        document: i64,
        after: Option<i64>,
        rows: usize,
    },
    /// [`GetStatement::FindingPage`]: at most `rows` findings at `path` under
    /// `fingerprint`, after the `(kind, id)` `after`, or from the first where
    /// it is `None`.
    FindingPage {
        path: &'a str,
        fingerprint: &'a str,
        after: Option<(&'a str, i64)>,
        rows: usize,
    },
}

impl Spelled<'_> {
    /// The statement this spells.
    fn statement(&self) -> GetStatement {
        match self {
            Spelled::DocumentHeadings { .. } => GetStatement::DocumentHeadings,
            Spelled::BlockDefinition { .. } => GetStatement::BlockDefinition,
            Spelled::DocumentBody { .. } => GetStatement::DocumentBody,
            Spelled::CollectionPage { collection, .. } => GetStatement::CollectionPage(*collection),
            Spelled::FindingPage { .. } => GetStatement::FindingPage,
        }
    }
}

/// The statement `spelled` names, with its parameters in the numbering its
/// text states, ready to run.
///
/// **A page resumes from its lower bound**, not from a test of the rows it
/// read: an unset position binds a bound below every row — the ordinal `-1`,
/// the empty kind — so the text does not branch on whether the page
/// continues, and the plan is the same either way.
pub(crate) fn compose(spelled: &Spelled<'_>) -> Ran {
    let integer =
        |value: usize| Value::Integer(i64::try_from(value).expect("a row count fits i64"));
    let composed = match spelled {
        Spelled::DocumentHeadings { document } => (
            DOCUMENT_HEADINGS_SQL.to_string(),
            vec![Value::Integer(*document)],
        ),
        Spelled::BlockDefinition { document, id } => (
            "SELECT b.block_id, b.span_line, b.span_column, b.span_offset FROM blocks AS b
             WHERE b.document = ?1 AND b.block_id = ?2
             ORDER BY b.ordinal
             LIMIT 1"
                .to_string(),
            vec![Value::Integer(*document), Value::Text((*id).to_string())],
        ),
        Spelled::DocumentBody { document } => (
            "SELECT d.body FROM documents AS d WHERE d.id = ?1".to_string(),
            vec![Value::Integer(*document)],
        ),
        Spelled::CollectionPage {
            collection,
            document,
            after,
            rows,
        } => {
            let mut binder = Binder::default();
            let document = binder.bind(Value::Integer(*document));
            let after = binder.bind(Value::Integer(after.unwrap_or(-1)));
            let limit = binder.bind(integer(*rows));
            (
                format!(
                    "SELECT {columns}, n.ordinal FROM {table} AS n
                     WHERE n.document = {document} AND n.ordinal > {after}
                     ORDER BY n.ordinal
                     LIMIT {limit}",
                    columns = collection.columns(),
                    table = collection.table(),
                ),
                binder.into_values(),
            )
        }
        Spelled::FindingPage {
            path,
            fingerprint,
            after,
            rows,
        } => {
            let mut binder = Binder::default();
            let (kind, id) = after.unwrap_or(("", 0));
            let path = binder.bind(Value::Text((*path).to_string()));
            let fingerprint = binder.bind(Value::Text((*fingerprint).to_string()));
            let kind = binder.bind(Value::Text(kind.to_string()));
            let id = binder.bind(Value::Integer(id));
            let limit = binder.bind(integer(*rows));
            (
                format!(
                    "SELECT {FINDING_ROW_COLUMNS} FROM findings AS f
                     WHERE f.path = {path} AND f.vault_schema_fingerprint = {fingerprint}
                       AND (f.kind, f.id) > ({kind}, {id})
                     ORDER BY f.kind, f.id
                     LIMIT {limit}"
                ),
                binder.into_values(),
            )
        }
    };
    Ran::new(spelled.statement(), composed)
}
