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

use crate::read::{
    Binder, FINDING_ROW_COLUMNS, Filter, PathGlob, answer_ordering, answer_place, glob_test,
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
/// path order and then by id — `(path COLLATE NOCASE, path, id)`, on every
/// root — and the page is in `(kind, path, id)` order.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ValidateStatement {
    /// One kind's findings standing under the active fingerprint, from the
    /// page's position on, in `(path COLLATE NOCASE, path, id)` order: a seek
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
}

/// How many statement shapes [`ValidateStatement::all`] holds.
pub const VALIDATE_STATEMENTS: usize = 2;

impl ValidateStatement {
    /// Every statement shape, in slot order.
    pub fn all() -> [Self; VALIDATE_STATEMENTS] {
        [Self::KindPage, Self::Summary]
    }

    /// Where this statement stands in [`Self::all`]. Exhaustive, so a shape
    /// added to the enum has to take a slot.
    pub fn slot(self) -> usize {
        let slot = match self {
            Self::KindPage => 0,
            Self::Summary => 1,
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
    /// The kinds read: the one kind of a [`ValidateStatement::KindPage`]
    /// section, and every kind a [`ValidateStatement::Summary`] tallies.
    pub(crate) kinds: &'a [&'a str],
    /// The severities admitted, as stored; `None` where every severity is.
    pub(crate) severities: Option<&'a [&'a str]>,
    /// The `(path, id)` a section resumes after; `None` starts at its first
    /// finding.
    pub(crate) after: Option<(&'a str, i64)>,
    /// The conjunction's parts: a path part judges the finding's own path,
    /// and every other part the document row at it.
    pub(crate) filters: &'a [Filter],
    /// Whether the conjunction names a part that judges a document and
    /// filters nothing among documents — a predicate key outside the field
    /// universe — so a finding stands in the answer only on a document row.
    pub(crate) on_a_document: bool,
    pub(crate) rows: usize,
}

/// One validate statement and its parameters, in the numbering the text
/// states.
///
/// **A path part judges the finding's own path**, so it reaches a finding
/// standing where no document row does. Its glob matches under the root's
/// fold, as every path part's does, and its range is its glob's folded range
/// on every root, because a section's findings stand in the answer's path
/// order and the index that holds that order compares paths under `NOCASE`.
/// Every path part's range, and on a page the position a section resumes
/// after, fold into one lower bound on `(path COLLATE NOCASE, path, id)` and
/// one upper bound on `path COLLATE NOCASE` — the greatest lower and the least
/// upper — so a section seeks its kind's findings from the tightest place
/// whichever the request named, sorts nothing, and costs the findings its page
/// reads. Where the root tells spellings apart, the folded range also reaches
/// the findings at paths that spell the glob's prefix in another case, which
/// the glob then rejects.
///
/// **Every other part judges the document row at the finding's path**, which
/// a finding standing where no document row does never satisfies — a part
/// on a key outside the field universe included, which filters nothing among
/// documents and so asks only that the row stands. Where some such part
/// keeps what it seeks, the matched documents drive the statement:
/// `CROSS JOIN` keeps them the outer loop, reached by the part's seek, and
/// each one's findings are one seek at its path, so the statement costs what
/// the part matched and sorts that. Where every such part excludes or filters
/// nothing, the statement seeks its kind as an unnarrowed one does and tests
/// each finding's document row, one seek of `documents_path` at its path.
pub(crate) fn compose_findings(findings: &Findings<'_>) -> (String, Vec<Value>) {
    let mut binder = Binder::default();
    let (paths, documents): (Vec<&Filter>, Vec<&Filter>) = findings
        .filters
        .iter()
        .partition(|filter| filter.path_glob().is_some());
    let globs: Vec<PathGlob<'_>> = paths
        .iter()
        .filter_map(|filter| filter.path_glob())
        .collect();
    let driven = documents.iter().any(|filter| !filter.shape.excludes());
    let on_a_document = findings.on_a_document || !documents.is_empty();

    let mut conditions = vec![format!(
        "f.vault_schema_fingerprint = {}",
        binder.bind(Value::Text(findings.fingerprint.to_string()))
    )];
    match (findings.statement, findings.kinds) {
        (ValidateStatement::KindPage, [kind]) => conditions.push(format!(
            "f.kind = {}",
            binder.bind(Value::Text((*kind).to_string()))
        )),
        (ValidateStatement::KindPage, _) => unreachable!("a page section reads one kind"),
        (ValidateStatement::Summary, kinds) => {
            conditions.push(format!("f.kind IN ({})", listed(&mut binder, kinds)))
        }
    }
    // A summary names every severity it admits, so each `(kind, severity)`
    // cell is a seek of its own and a path part's range bounds each; a page
    // names them where it narrows by one.
    let every_severity: Vec<&str> = Severity::ALL.iter().map(Severity::as_str).collect();
    let severities = match (findings.statement, findings.severities) {
        (_, Some(severities)) => Some(severities),
        (ValidateStatement::Summary, None) => Some(every_severity.as_slice()),
        (ValidateStatement::KindPage, None) => None,
    };
    if let Some(severities) = severities {
        conditions.push(format!(
            "f.severity IN ({})",
            listed(&mut binder, severities)
        ));
    }

    // The greatest lower bound and the least upper bound of the path parts'
    // folded ranges, and on a page, the section's position. A lower bound is
    // a place in the answer's path order — the folded path, the path, then an
    // id — and a text orders below the empty blob a range with no upper bound
    // is bounded by. Each bound picked is one some part's own range states, so
    // the range holds every path all the parts admit; a part's range opens at
    // the least place its folded prefix begins, before every path folding to
    // it whatever its bytes, which the empty path and id 0 name.
    let mut lower: (String, String, i64) = match findings.after {
        Some((path, id)) => {
            let (folded, path) = answer_place(path);
            (folded, path, id)
        }
        None => (String::new(), String::new(), 0),
    };
    let mut upper: Option<Value> = None;
    for glob in &globs {
        let (from, to) = glob.folded_range();
        let opening = (from, String::new(), 0);
        if opening > lower {
            lower = opening;
        }
        upper = Some(match (upper, to) {
            (Some(Value::Text(held)), Value::Text(to)) => Value::Text(held.min(to)),
            (Some(Value::Text(held)), _) => Value::Text(held),
            (_, to) => to,
        });
    }
    // Where matched documents drive the statement, each one's findings are
    // sought at its path, and the folded range is a test of what that seek
    // reaches rather than a second seek to weigh against it.
    let ranged = if driven { "+f.path" } else { "f.path" };
    let (folded, path, id) = lower;
    match findings.statement {
        // The position and the range's lower bound are one place in the
        // answer's path order, compared key by key: the folded path first,
        // which is what an index holding that order seeks from.
        ValidateStatement::KindPage => {
            let (folded, path, id) = (
                binder.bind(Value::Text(folded)),
                binder.bind(Value::Text(path)),
                binder.bind(Value::Integer(id)),
            );
            conditions.push(format!(
                "({ranged}, f.path, f.id) > ({folded} COLLATE NOCASE, {path}, {id})"
            ));
        }
        ValidateStatement::Summary => {
            if !globs.is_empty() {
                conditions.push(format!(
                    "{ranged} >= {} COLLATE NOCASE",
                    binder.bind(Value::Text(folded))
                ));
            }
        }
    }
    if let Some(upper) = upper {
        conditions.push(format!("{ranged} < {} COLLATE NOCASE", binder.bind(upper)));
    }
    for glob in &globs {
        let pattern = binder.bind(glob.pattern.clone());
        let recorded = binder.bind(glob.recorded_order.clone());
        conditions.push(glob_test(&pattern, "f.path", &recorded));
    }

    let tests: Vec<String> = documents
        .iter()
        .map(|filter| filter.spell("dv.id", &mut binder))
        .collect();
    let from = if driven {
        conditions.extend(tests);
        // The path compared folded as well as bytewise: the folded equality
        // is implied by the bytewise one, and it is what lets a seek at a
        // matched document's path run down an index holding the answer's
        // path order, so a summary's cell stays covered at each document.
        "FROM documents AS dv
                     CROSS JOIN findings AS f
                         ON f.path = dv.path COLLATE NOCASE AND f.path = dv.path"
    } else {
        if on_a_document {
            let tested: Vec<String> = std::iter::once("dv.path = f.path".to_string())
                .chain(tests)
                .collect();
            conditions.push(format!(
                "EXISTS (SELECT 1 FROM documents AS dv
                     WHERE {})",
                tested.join("\n                       AND ")
            ));
        }
        "FROM findings AS f"
    };

    let conditions = conditions.join("\n                       AND ");
    match findings.statement {
        ValidateStatement::KindPage => {
            let limit = binder.bind(Value::Integer(
                i64::try_from(findings.rows).expect("a page's row count fits i64"),
            ));
            let ordered = format!("{}, f.id", answer_ordering("f.path", ""));
            (
                format!(
                    "SELECT {FINDING_ROW_COLUMNS} {from}
                     WHERE {conditions}
                     ORDER BY {ordered}
                     LIMIT {limit}"
                ),
                binder.into_values(),
            )
        }
        ValidateStatement::Summary => (
            format!(
                "SELECT f.kind, f.severity, COUNT(*) {from}
                     WHERE {conditions}
                     GROUP BY f.kind, f.severity
                     ORDER BY f.kind, f.severity"
            ),
            binder.into_values(),
        ),
    }
}

/// One placeholder per value, bound in order, as an `IN` list's members.
fn listed(binder: &mut Binder, values: &[&str]) -> String {
    values
        .iter()
        .map(|value| binder.bind(Value::Text((*value).to_string())))
        .collect::<Vec<String>>()
        .join(", ")
}
