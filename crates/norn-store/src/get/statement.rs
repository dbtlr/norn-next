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
use crate::resolve::{self, TargetClass};

/// Every statement shape the get builder runs, named.
///
/// The same discipline as [`crate::FindStatement`]: [`GetStatement::all`]
/// holds each shape once, [`GetStatement::slot`] is exhaustive over the enum,
/// and [`GET_STATEMENTS`] is the count a census is checked against. A get
/// answers a record through the hydration a find's rows are read through, and
/// reads the active fingerprint and the field universe as a find does; the
/// statements those run are [`crate::FindStatement`]s and are named there.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GetStatement {
    /// The head of the class a target opens, in the resolution ladder's
    /// order: the target's suffix ranges on `documents_suffix_key` where the
    /// root tells spellings apart and on `documents_folded_suffix_key` where
    /// it folds ASCII case, less the places the schema ignores, at most
    /// [`crate::CANDIDATE_HEAD`] of them. The ladder's tie-break on the path
    /// sorts the rows the ranges reached, so it costs the class.
    ClassHead,
    /// How many documents the class holds, run only where the head filled
    /// [`crate::CANDIDATE_HEAD`]: a count over the same ranges.
    ClassTotal,
    /// Whether a suffix of a candidate's path names that candidate alone: at
    /// most two documents of the suffix's class, over the same ranges, in no
    /// order, so it stops at the second.
    CandidateSuffix,
    /// Every heading one document carries, in document order: one range seek
    /// of `headings_document_ordinal` at the document. What a heading anchor
    /// is matched against, so its cost is the document's headings.
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
    CollectionPage(Collection),
    /// One page of the findings standing at the document's path under the
    /// active fingerprint, in `(kind, id)` order: a seek of `findings_path`
    /// at `(path, fingerprint)` from the kind the page continues in — the id
    /// it continues after tested within that kind — that stops at the page's
    /// bound.
    FindingPage,
}

/// How many statement shapes [`GetStatement::all`] holds.
pub const GET_STATEMENTS: usize = 8;

impl GetStatement {
    /// Every statement shape, in slot order. The collection a page reads is
    /// not part of its slot: a bar that cares ranges over it itself.
    pub fn all() -> [Self; GET_STATEMENTS] {
        [
            Self::ClassHead,
            Self::ClassTotal,
            Self::CandidateSuffix,
            Self::DocumentHeadings,
            Self::BlockDefinition,
            Self::DocumentBody,
            Self::CollectionPage(Collection::Links),
            Self::FindingPage,
        ]
    }

    /// Where this statement stands in [`Self::all`]. Exhaustive, so a shape
    /// added to the enum has to take a slot.
    pub fn slot(self) -> usize {
        let slot = match self {
            Self::ClassHead => 0,
            Self::ClassTotal => 1,
            Self::CandidateSuffix => 2,
            Self::DocumentHeadings => 3,
            Self::BlockDefinition => 4,
            Self::DocumentBody => 5,
            Self::CollectionPage(_) => 6,
            Self::FindingPage => 7,
        };
        assert!(
            slot < GET_STATEMENTS,
            "slot {slot} is outside the enumeration: grow `all` and `GET_STATEMENTS` with the \
             statement that took it"
        );
        slot
    }
}

/// A nested collection a get pages by ordinal.
///
/// Each is a table whose rows a document owns, keyed by the document and the
/// row's position in it, so a page of one is a range of that key.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Collection {
    /// The links the document carries, in `links`.
    Links,
    /// A collection a find's row projects too.
    Nested(Nested),
}

impl Collection {
    /// Every collection a get pages by ordinal.
    pub const ALL: [Collection; 4] = [
        Collection::Links,
        Collection::Nested(Nested::Headings),
        Collection::Nested(Nested::Blocks),
        Collection::Nested(Nested::Tags),
    ];

    /// The table the collection's rows live in.
    pub const fn table(self) -> &'static str {
        match self {
            Collection::Links => "links",
            Collection::Nested(nested) => nested.table(),
        }
    }

    /// The columns an item is read from, in the order the stored-fact reader
    /// of the same table reads them.
    const fn columns(self) -> &'static str {
        match self {
            Collection::Links => {
                "n.family, n.embed, n.protocol, n.target, n.title, n.anchor, n.block_ref, \
                 n.span_line, n.span_column, n.span_offset"
            }
            Collection::Nested(nested) => nested.columns(),
        }
    }

    /// How many columns [`Collection::columns`] names, which is where the
    /// ordinal stands after them.
    pub(crate) const fn width(self) -> usize {
        match self {
            Collection::Links => 10,
            Collection::Nested(nested) => nested.width(),
        }
    }
}

/// What one get statement reads.
pub(crate) enum Spelled<'a> {
    /// [`GetStatement::ClassHead`]: at most `rows` of `class`.
    ClassHead { class: &'a TargetClass, rows: usize },
    /// [`GetStatement::ClassTotal`].
    ClassTotal { class: &'a TargetClass },
    /// [`GetStatement::CandidateSuffix`]: the class a suffix spelling opens.
    CandidateSuffix { class: &'a TargetClass },
    /// [`GetStatement::DocumentHeadings`].
    DocumentHeadings { document: i64 },
    /// [`GetStatement::BlockDefinition`].
    BlockDefinition { document: i64, id: &'a str },
    /// [`GetStatement::DocumentBody`].
    DocumentBody { document: i64 },
    /// [`GetStatement::CollectionPage`]: at most `rows` of `collection`,
    /// after the ordinal `after`, or from the first where it is `None`.
    CollectionPage {
        collection: Collection,
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
            Spelled::ClassHead { .. } => GetStatement::ClassHead,
            Spelled::ClassTotal { .. } => GetStatement::ClassTotal,
            Spelled::CandidateSuffix { .. } => GetStatement::CandidateSuffix,
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
/// **A class is read as every read of a class reads it**: its rows are
/// [`resolve::class_rows`], and a head of it is in [`resolve::ladder_order`].
/// **A page resumes from its lower bound**, not from a test of the rows it
/// read: an unset position binds a bound below every row — the ordinal `-1`,
/// the empty kind — so the text does not branch on whether the page
/// continues, and the plan is the same either way.
pub(crate) fn compose(spelled: &Spelled<'_>) -> Ran {
    let integer =
        |value: usize| Value::Integer(i64::try_from(value).expect("a row count fits i64"));
    let composed = match spelled {
        Spelled::ClassHead { class, rows } => {
            let mut values = class.parameters();
            let limit = values.len() + 1;
            values.push(integer(*rows));
            (
                format!(
                    "SELECT dr.id, dr.path {} {} LIMIT ?{limit}",
                    resolve::class_rows(class),
                    resolve::ladder_order(class)
                ),
                values,
            )
        }
        Spelled::ClassTotal { class } => (
            format!("SELECT COUNT(*) {}", resolve::class_rows(class)),
            class.parameters(),
        ),
        Spelled::CandidateSuffix { class } => {
            let values = class.parameters();
            (
                format!("SELECT dr.id {} LIMIT 2", resolve::class_rows(class)),
                values,
            )
        }
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
