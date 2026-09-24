//! The statements the count builder emits, named, and the one composer that
//! spells each of them with its parameters.
//!
//! **One function writes a statement's text and binds its parameters.**
//! [`compose_tallies`] numbers each parameter as it writes the placeholder for
//! it, through the same binder the find builder's composer numbers with, and
//! spells a conjunction's filters through the find builder's own filter
//! fragments, so a part narrows a count exactly as it narrows a find.

use norn_db::rusqlite::types::Value;

use crate::find::{Binder, FieldOrder, Filter};

/// Every statement shape the count builder runs, named.
///
/// The same discipline as [`crate::FindStatement`]: [`CountStatement::all`]
/// holds each shape once, [`CountStatement::slot`] is exhaustive over the
/// enum, and [`COUNT_STATEMENTS`] is the count a census is checked against. A
/// count compiles its conjunction through the find builder, so the probes that
/// compilation runs are [`crate::FindStatement`]s and are named there.
///
/// A grouped count reads its tallies in two sections, split on the grouping's
/// leading member: the tallies whose leading member is `null` first, then the
/// ones whose leading member holds a value, which is the order a tuple with a
/// `null` member sorts in.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CountStatement {
    /// The one tally of a count grouped by nothing: the documents the
    /// conjunction matches, counted over `documents`. With no filter it is a
    /// count of that table, which SQLite answers from the b-tree's own count
    /// through its narrowest index, and with one it reaches the documents the
    /// filter's seek hands it by row id.
    Total,
    /// The tallies whose leading member is `null`: the documents carrying no
    /// value under the leading key, grouped by the rest of the tuple. It walks
    /// the documents — every one, with no filter that keeps what it seeks —
    /// and probes each for a value under the leading key by its document, so
    /// no read of the leading key's rows passes a document that does not hold
    /// it. The member names what the leading key is read from.
    NullLead(GroupMember),
    /// The tallies whose leading member holds a value, grouped by the whole
    /// tuple. With no filter that keeps what it seeks, a seek of the leading
    /// member's value index from the page's position; with one, the matched
    /// documents each reaching their leading values by the document. The
    /// member names what the leading key is read from.
    ValuedLead(GroupMember),
}

/// How many statement shapes [`CountStatement::all`] holds.
pub const COUNT_STATEMENTS: usize = 3;

impl CountStatement {
    /// Every statement shape, in slot order.
    ///
    /// The member a section's leading key is read from is not part of its
    /// slot: a bar that cares about that axis ranges over it itself.
    pub fn all() -> [Self; COUNT_STATEMENTS] {
        [
            Self::Total,
            Self::NullLead(GroupMember::Field(FieldOrder::Raw)),
            Self::ValuedLead(GroupMember::Field(FieldOrder::Raw)),
        ]
    }

    /// Where this statement stands in [`Self::all`]. Exhaustive, so a shape
    /// added to the enum has to take a slot.
    pub fn slot(self) -> usize {
        let slot = match self {
            Self::Total => 0,
            Self::NullLead(_) => 1,
            Self::ValuedLead(_) => 2,
        };
        assert!(
            slot < COUNT_STATEMENTS,
            "slot {slot} is outside the enumeration: grow `all` and `COUNT_STATEMENTS` with the \
             statement that took it"
        );
        slot
    }
}

/// What one member of a grouping tuple is read from.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GroupMember {
    /// A frontmatter field's value rows in `document_fields`, grouped and
    /// ordered under the order: the raw text, or the typed sort key where the
    /// declaration gives the key a typed order.
    Field(FieldOrder),
    /// The document's tags in `document_tags`, by name.
    Tag,
}

impl GroupMember {
    /// Every member, in the order a bar ranging over them reads them.
    pub const ALL: [GroupMember; 3] = [
        GroupMember::Field(FieldOrder::Raw),
        GroupMember::Field(FieldOrder::Typed),
        GroupMember::Tag,
    ];

    /// The table the member's rows live in.
    pub const fn table(self) -> &'static str {
        match self {
            GroupMember::Field(_) => "document_fields",
            GroupMember::Tag => "document_tags",
        }
    }

    /// The column the member groups and orders by.
    fn sort(self) -> &'static str {
        match self {
            GroupMember::Field(FieldOrder::Raw) => "raw",
            GroupMember::Field(FieldOrder::Typed) => "typed",
            GroupMember::Tag => "name",
        }
    }

    /// The column a group's label is the least of.
    fn label(self) -> &'static str {
        match self {
            GroupMember::Field(_) => "raw",
            GroupMember::Tag => "name",
        }
    }
}

/// One member of a grouping tuple as a statement reads it: what it is read
/// from, and the key a field member reads.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Member<'a> {
    pub(crate) shape: GroupMember,
    /// The frontmatter key, for a field member.
    pub(crate) key: Option<&'a str>,
}

impl Member<'_> {
    /// The rows of this member, under `alias`, that belong to the document
    /// `id` names and hold a value, as a join condition or a probe's `WHERE`.
    ///
    /// The value test stands behind a unary `+`, which SQLite reads as the
    /// same value and never as an index constraint, so the rows are reached by
    /// the document: a field's by its primary key's `(document, key)` prefix,
    /// a tag's by its `(document, ordinal)` index.
    fn of_document(self, alias: &str, id: &str, binder: &mut Binder) -> String {
        match self.shape {
            GroupMember::Field(_) => {
                let key = binder.bind(key_value(self.key));
                format!(
                    "{alias}.document = {id} AND {alias}.key = {key} AND +{alias}.{sort} IS NOT NULL",
                    sort = self.shape.sort()
                )
            }
            GroupMember::Tag => format!("{alias}.document = {id}"),
        }
    }
}

/// A field member's key as a bound value.
fn key_value(key: Option<&str>) -> Value {
    Value::Text(key.expect("a field member names its key").to_string())
}

/// What one tally statement reads: the statement, the grouping tuple's
/// members, where it resumes, the filters it narrows by, and how many tallies.
pub(crate) struct Tallies<'a> {
    pub(crate) statement: CountStatement,
    /// Every member of the grouping tuple, in the request's order.
    pub(crate) members: &'a [Member<'a>],
    /// The sort key, or `None` for `null`, of each member the statement groups
    /// by — every member for [`CountStatement::ValuedLead`], every member
    /// after the leading one for [`CountStatement::NullLead`] — that the
    /// statement resumes after. `None` starts at its first tally.
    pub(crate) after: Option<&'a [Option<String>]>,
    pub(crate) filters: &'a [Filter],
    pub(crate) rows: usize,
}

/// One tally statement and its parameters, in the numbering the text states.
///
/// Every grouped statement selects each grouped member's label — the least
/// raw spelling in the group, `NULL` for a `null` member — and then how many
/// distinct documents the group holds, grouped and ordered by the members'
/// sort columns in the request's order, which puts `null` first. A document
/// holding several values under a key joins once per value, so a group counts
/// distinct documents and one document may stand in several groups.
///
/// **Where a statement resumes is a bound on the grouped tuple.** Each member
/// compares as its sort column with `null` read as the integer `0`, which
/// SQLite orders before every text, and an unset position binds `-1` to every
/// member, which orders before that; so the text does not branch on whether
/// the statement resumes. The valued section's leading member is also a seek:
/// its index is searched from the position's leading value, the empty text
/// where none is set.
///
/// **A statement narrowed by a filter that keeps what it seeks is driven from
/// that seek**: the matched documents are the outer loop and every member is
/// reached by its document, so the statement's cost is the matched set's, not
/// the vault's. A statement with no filter, or narrowed only by filters that
/// exclude, reads its own leading rows and tests each against them.
pub(crate) fn compose_tallies(tallies: &Tallies<'_>) -> (String, Vec<Value>) {
    let mut binder = Binder::default();
    let driven = tallies
        .filters
        .iter()
        .any(|filter| !filter.shape.excludes());
    let alias = |at: usize| format!("g{}", at + 1);
    let (grouped, lead): (Vec<(usize, Member<'_>)>, Option<Member<'_>>) = match tallies.statement {
        CountStatement::Total => (Vec::new(), None),
        CountStatement::NullLead(_) => (
            tallies
                .members
                .iter()
                .copied()
                .enumerate()
                .skip(1)
                .collect(),
            tallies.members.first().copied(),
        ),
        CountStatement::ValuedLead(_) => (
            tallies.members.iter().copied().enumerate().collect(),
            tallies.members.first().copied(),
        ),
    };

    let mut conditions: Vec<String> = Vec::new();
    let (from, id) = match (tallies.statement, lead) {
        (CountStatement::Total, _) => ("FROM documents AS d".to_string(), "d.id".to_string()),
        (CountStatement::NullLead(_), Some(lead)) => {
            let probe = lead.of_document("m", "d.id", &mut binder);
            conditions.push(format!(
                "NOT EXISTS (SELECT 1 FROM {table} AS m WHERE {probe})",
                table = lead.shape.table()
            ));
            ("FROM documents AS d".to_string(), "d.id".to_string())
        }
        (CountStatement::ValuedLead(_), Some(lead)) => {
            let (table, sort) = (lead.shape.table(), lead.shape.sort());
            let from_position = binder.bind(Value::Text(
                tallies
                    .after
                    .and_then(|after| after.first().cloned().flatten())
                    .unwrap_or_default(),
            ));
            if driven {
                let key = match lead.shape {
                    GroupMember::Field(_) => {
                        format!(" AND g1.key = {}", binder.bind(key_value(lead.key)))
                    }
                    GroupMember::Tag => String::new(),
                };
                (
                    format!(
                        "FROM documents AS d
                     CROSS JOIN {table} AS g1
                       ON g1.document = d.id{key} AND +g1.{sort} >= {from_position}"
                    ),
                    "d.id".to_string(),
                )
            } else {
                if let GroupMember::Field(_) = lead.shape {
                    conditions.push(format!("g1.key = {}", binder.bind(key_value(lead.key))));
                }
                conditions.push(format!("g1.{sort} >= {from_position}"));
                (format!("FROM {table} AS g1"), "g1.document".to_string())
            }
        }
        (CountStatement::NullLead(_) | CountStatement::ValuedLead(_), None) => {
            unreachable!("a grouped section groups by a leading member")
        }
    };
    let joins: String = grouped
        .iter()
        .filter(|(at, _)| matches!(tallies.statement, CountStatement::NullLead(_)) || *at > 0)
        .map(|(at, member)| {
            let alias = alias(*at);
            format!(
                "\n                     LEFT JOIN {table} AS {alias} ON {on}",
                table = member.shape.table(),
                on = member.of_document(&alias, &id, &mut binder)
            )
        })
        .collect();
    conditions.extend(
        tallies
            .filters
            .iter()
            .map(|filter| filter.spell(&id, &mut binder)),
    );

    let sorts: Vec<String> = grouped
        .iter()
        .map(|(at, member)| format!("{}.{}", alias(*at), member.shape.sort()))
        .collect();
    let mut columns: Vec<String> = grouped
        .iter()
        .map(|(at, member)| format!("MIN({}.{})", alias(*at), member.shape.label()))
        .collect();
    columns.push(match tallies.statement {
        // One row per document: the table's own count.
        CountStatement::Total => "COUNT(*)".to_string(),
        CountStatement::NullLead(_) | CountStatement::ValuedLead(_) => {
            format!("COUNT(DISTINCT {id})")
        }
    });

    let mut having = vec!["COUNT(*) > 0".to_string()];
    if !sorts.is_empty() {
        let bound: Vec<String> = (0..sorts.len())
            .map(|at| {
                binder.bind(match tallies.after {
                    None => Value::Integer(-1),
                    Some(after) => match &after[at] {
                        None => Value::Integer(0),
                        Some(sort) => Value::Text(sort.clone()),
                    },
                })
            })
            .collect();
        let tuple: Vec<String> = sorts
            .iter()
            .map(|sort| format!("COALESCE({sort}, 0)"))
            .collect();
        having.push(format!("({}) > ({})", tuple.join(", "), bound.join(", ")));
    }

    let mut sql = format!(
        "SELECT {}\n                     {from}{joins}",
        columns.join(", ")
    );
    if !conditions.is_empty() {
        sql.push_str(&format!(
            "\n                     WHERE {}",
            conditions.join("\n                       AND ")
        ));
    }
    if matches!(tallies.statement, CountStatement::Total) {
        return (sql, binder.into_values());
    }
    if !sorts.is_empty() {
        sql.push_str(&format!(
            "\n                     GROUP BY {}",
            sorts.join(", ")
        ));
    }
    sql.push_str(&format!(
        "\n                     HAVING {}",
        having.join(" AND ")
    ));
    if !sorts.is_empty() {
        sql.push_str(&format!(
            "\n                     ORDER BY {}",
            sorts.join(", ")
        ));
    }
    let limit = binder.bind(Value::Integer(
        i64::try_from(tallies.rows).expect("a page's tally count fits i64"),
    ));
    sql.push_str(&format!("\n                     LIMIT {limit}"));
    (sql, binder.into_values())
}
