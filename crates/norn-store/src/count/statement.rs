//! The statements the count builder emits, named, and the one composer that
//! spells each of them with its parameters.
//!
//! **One function writes a statement's text and binds its parameters.**
//! [`compose_tallies`] numbers each parameter as it writes the placeholder for
//! it, through the binder every read builder's composer numbers with, and
//! spells a conjunction's filters through the filter fragments every read
//! builder shares, so a part narrows a count exactly as it narrows a find.

use norn_db::rusqlite::types::Value;

use crate::read::{Binder, Driver, FieldOrder, Filter};

/// Every statement shape the count builder runs, named.
///
/// The same discipline as [`crate::FindStatement`]: [`CountStatement::all`]
/// holds each shape once, [`CountStatement::slot`] is exhaustive over the
/// enum, and [`COUNT_STATEMENTS`] is the count a census is checked against. A
/// count compiles its conjunction through the compilation every read builder
/// shares, whose probes are [`crate::FindStatement`]s and are named there.
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
    /// The tallies whose leading member is `null`: the readable documents
    /// carrying no value under the leading key, grouped by the rest of the
    /// tuple. It walks the documents — the matched ones whenever a part keeps
    /// what it seeks, every one otherwise — and probes each for a value under
    /// the leading key, so no read of the leading key's rows passes a document
    /// that does not hold it. The member names what the leading key is read from.
    NullLead(GroupMember),
    /// The tallies whose leading member holds a value, grouped by the whole
    /// tuple. A part keeping at most the page drive's budget drives from its
    /// matched documents, each reaching its leading values by the document;
    /// otherwise a seek of the leading value index tests every part at each
    /// document, within the walk's window. The member names what the leading
    /// key is read from.
    ValuedLead(GroupMember),
    /// One leading value at an offset past a section's position, read from
    /// its value index with no document part: where a walk's window ends.
    ValuedWindow(GroupMember),
}

/// How many statement shapes [`CountStatement::all`] holds.
pub const COUNT_STATEMENTS: usize = 4;

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
            Self::ValuedWindow(GroupMember::Field(FieldOrder::Raw)),
        ]
    }

    /// Where this statement stands in [`Self::all`]. Exhaustive, so a shape
    /// added to the enum has to take a slot.
    pub fn slot(self) -> usize {
        let slot = match self {
            Self::Total => 0,
            Self::NullLead(_) => 1,
            Self::ValuedLead(_) => 2,
            Self::ValuedWindow(_) => 3,
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
    /// The document's tags in `document_tags`, by folded name: every
    /// spelling of one tag under the tag fold is one group.
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
            GroupMember::Tag => "folded_name",
        }
    }

    /// The aggregate a group's label is selected by, over the member's rows
    /// under `alias`. `path` is the holding document's path where the
    /// statement already joins `documents` to each row, and `None` where it
    /// reads the member's rows alone, so the path is looked up by the row's
    /// document.
    ///
    /// A field's label is the least raw spelling in the group. A tag's is the
    /// spelling written first in the answer order — the holding document's
    /// path under `COLLATE NOCASE`, then its bytes, then the tag's position
    /// in the document — which is the least of one text per row that orders
    /// as that tuple does and carries the name after it:
    /// `hex(lower(path)) hex(path) ordinal name`, space-separated. `lower`
    /// folds ASCII alone, as `NOCASE` does; a hex digit sorts above the space,
    /// so a shorter path sorts first as it does unencoded; and the ordinal is
    /// zero-padded to a fixed width. [`GroupMember::read_label`] reads the
    /// name back off the least one.
    fn label(self, alias: &str, path: Option<&str>) -> String {
        let ordered_path = |path: &str| format!("hex(lower({path})) || ' ' || hex({path})");
        match self {
            GroupMember::Field(_) => format!("MIN({alias}.raw)"),
            GroupMember::Tag => {
                let path = match path {
                    Some(path) => ordered_path(path),
                    None => format!(
                        "(SELECT {} FROM documents AS lp WHERE lp.id = {alias}.document)",
                        ordered_path("lp.path")
                    ),
                };
                format!(
                    "MIN({path} || ' ' || printf('%020d', {alias}.ordinal) || ' ' || {alias}.name)"
                )
            }
        }
    }

    /// The label a group reads as, from the value [`GroupMember::label`]
    /// selected for it: `None` for a `null` member.
    pub(crate) fn read_label(self, selected: Option<String>) -> Option<String> {
        match self {
            GroupMember::Field(_) => selected,
            GroupMember::Tag => selected.map(|ordered| {
                ordered
                    .splitn(4, ' ')
                    .nth(3)
                    .expect("a tag label carries its name after three ordering fields")
                    .to_string()
            }),
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
    pub(crate) driver: Driver,
    /// A round resumes past this whole leading group, independently of the
    /// tuple position the page started at.
    pub(crate) past: Option<&'a str>,
    /// The leading group the walk reads through, every row at it included.
    pub(crate) until: Option<&'a str>,
    /// Source rows passed before a window reads its one leading value.
    pub(crate) offset: usize,
    /// None streams the complete relation into retained storage.
    pub(crate) rows: Option<usize>,
}

/// One tally statement and its parameters, in the numbering the text states.
///
/// Every grouped statement selects each grouped member's label — the least
/// raw spelling in a field's group, the first-written spelling in a tag's
/// ([`GroupMember::label`]), `NULL` for a `null` member — and then how many
/// distinct documents the group holds, grouped and ordered by the members'
/// sort columns in the request's order, which puts `null` first. A document
/// holding several values under a key joins once per value, so a group counts
/// distinct documents and one document may stand in several groups.
///
/// **Where a statement resumes is a bound on the grouped tuple.** Each member
/// compares as its sort column with `null` read as the integer `0`, which
/// SQLite orders before every text, and an unset position binds `-1` to every
/// member, which orders before that. The valued section's leading member is
/// also a seek: a single-key continuation and a resumed round seek strictly
/// past their position; a tuple with trailing members includes its leading
/// value so those members can resume within it. An unset position starts at
/// the empty text.
///
/// **The page drive chooses which part drives a grouped statement.** A part
/// keeping at most its budget drives from its matched documents: the outer
/// loop, every member reached by its document, every other part tested there.
/// Otherwise the statement walks its leading index from the position and
/// tests every part at each document, through the window's leading value
/// inclusive. A round resumes past that whole leading group. A window reads
/// one value at an offset on the same seek, narrowed by no part and grouped
/// by nothing.
pub(crate) fn compose_tallies(tallies: &Tallies<'_>) -> (String, Vec<Value>) {
    let mut binder = Binder::default();
    let driving = match tallies.driver {
        Driver::Matched(at) => Some(at),
        Driver::Walked => None,
        Driver::Ordered(_) => unreachable!("a grouped order is not a path order"),
    };
    let driven = driving.is_some();
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
        CountStatement::ValuedLead(_) | CountStatement::ValuedWindow(_) => (
            tallies.members.iter().copied().enumerate().collect(),
            tallies.members.first().copied(),
        ),
    };

    let mut conditions: Vec<String> = Vec::new();
    // The document path is `d.path` in every form that joins `documents` as
    // `d`, and `None` in the one that reads the leading member's rows alone.
    let (from, id, path) = match (tallies.statement, lead) {
        (CountStatement::Total, _) => (
            "FROM documents AS d".to_string(),
            "d.id".to_string(),
            Some("d.path"),
        ),
        (CountStatement::NullLead(_), Some(lead)) => {
            // Unknown content is not absence. Keep readability a test of the
            // documents the existing drive reaches, rather than a new range.
            conditions.push("+d.content_hash IS NOT NULL".to_string());
            let probe = lead.of_document("m", "d.id", &mut binder);
            conditions.push(format!(
                "NOT EXISTS (SELECT 1 FROM {table} AS m WHERE {probe})",
                table = lead.shape.table()
            ));
            (
                "FROM documents AS d".to_string(),
                "d.id".to_string(),
                Some("d.path"),
            )
        }
        (CountStatement::ValuedLead(_) | CountStatement::ValuedWindow(_), Some(lead)) => {
            let (table, sort) = (lead.shape.table(), lead.shape.sort());
            let from_position = binder.bind(Value::Text(
                tallies
                    .past
                    .map(str::to_string)
                    .or_else(|| {
                        tallies
                            .after
                            .and_then(|after| after.first().cloned().flatten())
                    })
                    .unwrap_or_default(),
            ));
            let comparison = if tallies.past.is_some()
                || (tallies.members.len() == 1 && tallies.after.is_some())
            {
                ">"
            } else {
                ">="
            };
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
                       ON g1.document = d.id{key} AND +g1.{sort} {comparison} {from_position}"
                    ),
                    "d.id".to_string(),
                    Some("d.path"),
                )
            } else {
                if let GroupMember::Field(_) = lead.shape {
                    conditions.push(format!("g1.key = {}", binder.bind(key_value(lead.key))));
                }
                conditions.push(format!("g1.{sort} {comparison} {from_position}"));
                (
                    format!("FROM {table} AS g1"),
                    "g1.document".to_string(),
                    None,
                )
            }
        }
        (
            CountStatement::NullLead(_)
            | CountStatement::ValuedLead(_)
            | CountStatement::ValuedWindow(_),
            None,
        ) => {
            unreachable!("a grouped section groups by a leading member")
        }
    };
    if let Some(lead) = lead
        && matches!(
            tallies.statement,
            CountStatement::ValuedLead(_) | CountStatement::ValuedWindow(_)
        )
    {
        let sort = lead.shape.sort();
        if let Some(until) = tallies.until {
            let until = binder.bind(Value::Text(until.to_string()));
            conditions.push(format!("g1.{sort} <= {until}"));
        }
        if matches!(tallies.statement, CountStatement::ValuedWindow(_)) {
            let offset = binder.bind(Value::Integer(
                i64::try_from(tallies.offset).expect("a window offset fits i64"),
            ));
            return (
                format!(
                    "SELECT g1.{sort} {from} WHERE {} ORDER BY g1.{sort} LIMIT 1 OFFSET {offset}",
                    conditions.join(" AND "),
                ),
                binder.into_values(),
            );
        }
    }
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
    conditions.extend(tallies.filters.iter().enumerate().map(|(at, filter)| {
        if matches!(tallies.statement, CountStatement::Total) || driving == Some(at) {
            filter.spell(&id, &mut binder)
        } else {
            filter.test(&id, &mut binder)
        }
    }));

    let sorts: Vec<String> = grouped
        .iter()
        .map(|(at, member)| format!("{}.{}", alias(*at), member.shape.sort()))
        .collect();
    let mut columns: Vec<String> = grouped
        .iter()
        .map(|(at, member)| member.shape.label(&alias(*at), path))
        .collect();
    columns.push(match tallies.statement {
        // One row per document: the table's own count.
        CountStatement::Total => "COUNT(*)".to_string(),
        CountStatement::NullLead(_) | CountStatement::ValuedLead(_) => {
            format!("COUNT(DISTINCT {id})")
        }
        CountStatement::ValuedWindow(_) => unreachable!("a window reads its leading value alone"),
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
    if let Some(rows) = tallies.rows {
        let limit = binder.bind(Value::Integer(
            i64::try_from(rows).expect("a page's tally count fits i64"),
        ));
        sql.push_str(&format!("\n                     LIMIT {limit}"));
    }
    (sql, binder.into_values())
}
