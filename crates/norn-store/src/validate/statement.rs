//! The statements the validate builder emits, named, and the one composer that
//! spells each of them with its parameters.
//!
//! **One function writes a statement's text and binds its parameters.**
//! [`compose_findings`] numbers each parameter as it writes the placeholder for
//! it, through the binder every read builder's composer numbers with, and
//! spells a document part through the filter fragments every read builder
//! shares, so a part narrows a validate's subjects exactly as it narrows a
//! find's documents.

use norn_db::rusqlite::types::Value;

use norn_wire::Severity;

use crate::ddl::findings::DOCUMENT_POSITION;
use crate::read::{
    AnswerSeek, Binder, FINDING_ROW_COLUMNS, Filter, PathPart, Term, answer_ordering, answer_place,
    answer_range,
};

/// Every statement shape the validate builder runs, named.
///
/// The same discipline as [`crate::FindStatement`]: [`ValidateStatement::all`]
/// holds each shape once, [`ValidateStatement::slot`] is exhaustive over the
/// enum, and [`VALIDATE_STATEMENTS`] is the count a census is checked against.
/// A validate compiles its conjunction through the compilation every read
/// builder shares, and reads a page's finding rows through the accessor a
/// find's findings column reads through; the statements both run are
/// [`crate::FindStatement`]s and are named there.
///
/// A page of findings reads one kind after another, in the byte order of the
/// kind's code, so each kind is a section whose findings stand in the answer's
/// path order, then by position among their path's findings and then by id —
/// `(path COLLATE NOCASE, path, position, id)`, on every root — and the page
/// is in `(kind, path, position, id)` order.
///
/// **A request selecting one rule reads the same order off the rule's own
/// rows.** `finding_rules` holds one row per finding and rule it cites, with
/// the finding's fingerprint, kind, severity, path and position copied beside
/// the rule, and its two indexes lead with `(fingerprint, rule, kind)` where
/// the findings' lead with `(fingerprint, kind)`, so a rule's page and its
/// tally are the same seeks one column further in.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ValidateStatement {
    /// One kind's findings standing under the active fingerprint, from the
    /// page's position on, in `(path COLLATE NOCASE, path, position, id)`
    /// order: a seek
    /// of `findings_fingerprint_kind_nocase` at `(fingerprint, kind)` bounded
    /// below by the position, or of `findings_fingerprint_kind_severity_nocase`
    /// at `(fingerprint, kind, severity)` where the request admits one
    /// severity. A path part bounds the seek by its glob's folded range, and
    /// nothing sorts. A document part that keeps what it seeks drives the
    /// statement instead: the documents it matched each reach their findings
    /// by one seek at their path, and those are sorted.
    KindPage,
    /// How many findings stand, one tally per kind and severity: an aggregate
    /// over `findings_fingerprint_kind_severity_nocase`, which covers every
    /// column a tally reads, so it reads no finding row. It names every kind
    /// and every severity it admits, so each `(kind, severity)` cell is one
    /// seek of the index, which a path part's folded range bounds, and the
    /// tallies are grouped in the order the index holds them. A document part
    /// that keeps what it seeks drives the statement instead: the documents it
    /// matched each take one covering seek of the index at their path per
    /// cell, and the groups are sorted.
    Summary,
    /// One kind's findings citing one rule, from the page's position on, in
    /// the order [`ValidateStatement::KindPage`] reads them: a seek of
    /// `finding_rules_fingerprint_rule_kind_nocase` at `(fingerprint, rule,
    /// kind)` bounded below by the position, or of
    /// `finding_rules_fingerprint_rule_kind_severity_nocase` at `(fingerprint,
    /// rule, kind, severity)` where the request admits one severity, each
    /// rule row reaching its finding by row id. A path part bounds the seek
    /// as it bounds a kind page's; a document part that keeps what it seeks
    /// drives the statement, each matched document reaching the rule's rows by
    /// one seek at its path.
    RulePage,
    /// How many findings citing one rule stand, one tally per kind and
    /// severity: an aggregate over
    /// `finding_rules_fingerprint_rule_kind_severity_nocase`, which covers
    /// every column a tally reads, one seek per `(kind, severity)` cell, so it
    /// reads neither a finding nor a rule row. A path part bounds each cell
    /// and a document part drives it as they do a summary.
    RuleSummary,
    /// The rules of each rule set a page's findings cite: one seek of
    /// `rule_set_rules`' primary key per set, its rules in byte order. A page
    /// citing no set runs none.
    RuleSets,
}

/// How many statement shapes [`ValidateStatement::all`] holds.
pub const VALIDATE_STATEMENTS: usize = 5;

impl ValidateStatement {
    /// Every statement shape, in slot order.
    pub fn all() -> [Self; VALIDATE_STATEMENTS] {
        [
            Self::KindPage,
            Self::Summary,
            Self::RulePage,
            Self::RuleSummary,
            Self::RuleSets,
        ]
    }

    /// Where this statement stands in [`Self::all`]. Exhaustive, so a shape
    /// added to the enum has to take a slot.
    pub fn slot(self) -> usize {
        let slot = match self {
            Self::KindPage => 0,
            Self::Summary => 1,
            Self::RulePage => 2,
            Self::RuleSummary => 3,
            Self::RuleSets => 4,
        };
        assert!(
            slot < VALIDATE_STATEMENTS,
            "slot {slot} is outside the enumeration: grow `all` and `VALIDATE_STATEMENTS` with \
             the statement that took it"
        );
        slot
    }
}

/// What one validate statement reads.
pub(crate) struct Findings<'a> {
    pub(crate) statement: ValidateStatement,
    /// The fingerprint the findings stand under: the active one, and the empty
    /// text where no schema is pinned.
    pub(crate) fingerprint: &'a str,
    /// The one rule whose findings a [`ValidateStatement::RulePage`] or a
    /// [`ValidateStatement::RuleSummary`] reads, and `None` for the others.
    pub(crate) rule: Option<&'a str>,
    /// The kinds read: the one kind of a page's section, and every kind a
    /// summary tallies.
    pub(crate) kinds: &'a [&'a str],
    /// The severities admitted, as stored; `None` where every severity is.
    pub(crate) severities: Option<&'a [&'a str]>,
    /// The `(path, position, id)` a section resumes after; `None` starts at
    /// its first finding.
    pub(crate) after: Option<(&'a str, i64, i64)>,
    /// The conjunction's parts: a path part judges the finding's own path,
    /// and every other part the document row at it.
    pub(crate) filters: &'a [Filter],
    pub(crate) rows: usize,
}

/// One validate statement and its parameters, in the numbering the text
/// states.
///
/// **A path part judges the finding's own path**, so it reaches a finding
/// standing where no document row does. Its glob matches under the root's fold,
/// as every path part's does, and its range is the answer order's
/// ([`answer_range`]) on every root, because a section's findings stand in the
/// answer's path order and the index that holds that order compares paths under
/// `NOCASE`. Every path part's range, and on a page the position a section
/// resumes after, fold into one lower bound on `(path COLLATE NOCASE, path,
/// position, id)` and one upper bound on `path COLLATE NOCASE` — the greatest lower and
/// the least upper — so a section seeks its kind's findings exactly past the
/// tightest place whichever the request named ([`AnswerSeek`]), sorts nothing,
/// and costs the findings its page reads. Where the root tells spellings apart,
/// the folded range also reaches the findings at paths that spell the glob's
/// prefix in another case, which the glob then rejects.
///
/// **Every other part judges the document row at the finding's path**, which
/// a finding standing where no document row does never satisfies. Where some
/// such part keeps what it seeks, the matched documents drive the statement:
/// `CROSS JOIN` keeps them the outer loop, reached by the part's seek, and
/// each one's findings are one seek at its path, so the statement costs what
/// the part matched and sorts that. Where every such part excludes, the
/// statement seeks its kind as an unnarrowed one does and tests each
/// finding's document row, one seek of `documents_path` at its path.
pub(crate) fn compose_findings(findings: &Findings<'_>) -> (String, Vec<Value>) {
    let mut binder = Binder::default();
    let (paths, documents): (Vec<&Filter>, Vec<&Filter>) = findings
        .filters
        .iter()
        .partition(|filter| filter.path_part().is_some());
    let parts: Vec<&PathPart> = paths
        .iter()
        .filter_map(|filter| filter.path_part())
        .collect();
    let driven = documents.iter().any(|filter| !filter.shape().excludes());
    let on_a_document = !documents.is_empty();
    // A rule's statements seek the rule's own rows, which carry the finding's
    // key beside the rule; every other statement seeks the findings.
    let paged = matches!(
        findings.statement,
        ValidateStatement::KindPage | ValidateStatement::RulePage
    );
    let (table, seek, id) = match findings.statement {
        ValidateStatement::KindPage | ValidateStatement::Summary => ("findings", "f", "f.id"),
        ValidateStatement::RulePage | ValidateStatement::RuleSummary => {
            ("finding_rules", "fr", "fr.finding")
        }
        ValidateStatement::RuleSets => {
            unreachable!("the rule sets are read by `compose_rule_sets`")
        }
    };
    let column = |name: &str| format!("{seek}.{name}");

    let mut conditions = vec![format!(
        "{} = {}",
        column("vault_schema_fingerprint"),
        binder.bind(Value::Text(findings.fingerprint.to_string()))
    )];
    if let Some(rule) = findings.rule {
        conditions.push(format!(
            "{} = {}",
            column("rule"),
            binder.bind(Value::Text(rule.to_string()))
        ));
    }
    match (paged, findings.kinds) {
        (true, [kind]) => conditions.push(format!(
            "{} = {}",
            column("kind"),
            binder.bind(Value::Text((*kind).to_string()))
        )),
        (true, _) => unreachable!("a page section reads one kind"),
        (false, kinds) => conditions.push(format!(
            "{} IN ({})",
            column("kind"),
            listed(&mut binder, kinds)
        )),
    }
    // A summary names every severity it admits, so each `(kind, severity)`
    // cell is a seek of its own and a path part's range bounds each; a page
    // names them where it narrows by one.
    let every_severity: Vec<&str> = Severity::ALL.iter().map(Severity::as_str).collect();
    let severities = match (paged, findings.severities) {
        (_, Some(severities)) => Some(severities),
        (false, None) => Some(every_severity.as_slice()),
        (true, None) => None,
    };
    if let Some(severities) = severities {
        conditions.push(format!(
            "{} IN ({})",
            column("severity"),
            listed(&mut binder, severities)
        ));
    }

    // The greatest lower bound and the least upper bound of the path parts'
    // ranges in the answer order, and on a page, the section's position. A
    // lower bound is a place in the answer order — the folded path, the path,
    // a position among the path's findings, then an id — and a text orders
    // below the empty blob a range with no upper bound is bounded by. Each
    // bound picked is one some part's own range states, so the range holds
    // every path all the parts admit; a part's range opens at the least place
    // its folded prefix begins, before every path folding to it whatever its
    // bytes, which the empty path, the document's position and id 0 name.
    let mut lower: (String, String, i64, i64) = match findings.after {
        Some((path, position, id)) => {
            let (folded, path) = answer_place(path);
            (folded, path, position, id)
        }
        None => (String::new(), String::new(), DOCUMENT_POSITION, 0),
    };
    let mut upper: Option<Value> = None;
    for part in &parts {
        let (from, to) = answer_range(&part.pattern);
        let opening = (from, String::new(), DOCUMENT_POSITION, 0);
        if opening > lower {
            lower = opening;
        }
        upper = Some(match (upper, to) {
            (Some(Value::Text(held)), Value::Text(to)) => Value::Text(held.min(to)),
            (Some(Value::Text(held)), _) => Value::Text(held),
            (_, to) => to,
        });
    }
    let path_column = column("path");
    let position_column = column("position");
    // Where matched documents drive the statement, each one's findings are
    // sought at its path by equality on both path columns, which leaves the
    // position and the range no column to seek, so they test what that seek
    // reaches.
    let (folded, path, position, after_id) = lower;
    if paged {
        // The position and the range's lower bound are one place in the
        // answer order, which the kind's index is sought past.
        let (folded, path, position, after_id) = (
            binder.bind(Value::Text(folded)),
            binder.bind(Value::Text(path)),
            binder.bind(Value::Integer(position)),
            binder.bind(Value::Integer(after_id)),
        );
        conditions.push(
            AnswerSeek {
                reach: "",
                comparison: ">",
                lead: None,
                path: &path_column,
                folded: &folded,
                bytewise: &path,
                trail: Some(Term {
                    column: &position_column,
                    bound: &position,
                }),
                id: Some(Term {
                    column: id,
                    bound: &after_id,
                }),
            }
            .spelled(),
        );
    } else if !parts.is_empty() {
        conditions.push(format!(
            "{path_column} >= {} COLLATE NOCASE",
            binder.bind(Value::Text(folded))
        ));
    }
    if let Some(upper) = upper {
        conditions.push(format!(
            "{path_column} < {} COLLATE NOCASE",
            binder.bind(upper)
        ));
    }
    for part in &parts {
        conditions.push(part.glob_test(&path_column, &mut binder));
    }

    let tests: Vec<String> = documents
        .iter()
        .map(|filter| filter.spell("dv.id", &mut binder))
        .collect();
    // A rule's page reaches each finding it pages from the rule's row, by the
    // finding's row id; its tally reads the rule's rows alone.
    let reached = if findings.statement == ValidateStatement::RulePage {
        "\n                     CROSS JOIN findings AS f ON f.id = fr.finding"
    } else {
        ""
    };
    let from = if driven {
        conditions.extend(tests);
        // The path compared folded as well as bytewise: the folded equality
        // is implied by the bytewise one, and it is what lets a seek at a
        // matched document's path run down an index holding the answer's
        // path order, so a summary's cell stays covered at each document. The
        // bytewise equality matches a finding to the document at its exact
        // path.
        format!(
            "FROM documents AS dv
                     CROSS JOIN {table} AS {seek}
                         ON {path_column} = dv.path COLLATE NOCASE AND {path_column} = dv.path{reached}"
        )
    } else {
        if on_a_document {
            let tested: Vec<String> = std::iter::once(format!("dv.path = {path_column}"))
                .chain(tests)
                .collect();
            conditions.push(format!(
                "EXISTS (SELECT 1 FROM documents AS dv
                     WHERE {})",
                tested.join("\n                       AND ")
            ));
        }
        format!("FROM {table} AS {seek}{reached}")
    };

    let conditions = conditions.join("\n                       AND ");
    if paged {
        let limit = binder.bind(Value::Integer(
            i64::try_from(findings.rows).expect("a page's row count fits i64"),
        ));
        let ordered = format!(
            "{}, {position_column}, {id}",
            answer_ordering(&path_column, "")
        );
        (
            format!(
                "SELECT {FINDING_ROW_COLUMNS} {from}
                     WHERE {conditions}
                     ORDER BY {ordered}
                     LIMIT {limit}"
            ),
            binder.into_values(),
        )
    } else {
        let (kind, severity) = (column("kind"), column("severity"));
        (
            format!(
                "SELECT {kind}, {severity}, COUNT(*) {from}
                     WHERE {conditions}
                     GROUP BY {kind}, {severity}
                     ORDER BY {kind}, {severity}"
            ),
            binder.into_values(),
        )
    }
}

/// The statement reading the rules of each rule set `ids` names: one seek of
/// `rule_set_rules`' primary key per set, the rules of each in byte order.
pub(crate) fn compose_rule_sets(ids: &[i64]) -> (String, Vec<Value>) {
    let mut binder = Binder::default();
    let listed: Vec<String> = ids
        .iter()
        .map(|id| binder.bind(Value::Integer(*id)))
        .collect();
    (
        format!(
            "SELECT rule_set, rule FROM rule_set_rules
                     WHERE rule_set IN ({})
                     ORDER BY rule_set, rule",
            listed.join(", ")
        ),
        binder.into_values(),
    )
}

/// One placeholder per value, bound in order, as an `IN` list's members.
fn listed(binder: &mut Binder, values: &[&str]) -> String {
    values
        .iter()
        .map(|value| binder.bind(Value::Text((*value).to_string())))
        .collect::<Vec<String>>()
        .join(", ")
}
