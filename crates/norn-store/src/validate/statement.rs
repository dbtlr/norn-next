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

use crate::facts::StoredPathOrder;
use crate::read::{Binder, FINDING_ROW_COLUMNS, Filter, PathGlob, glob_test};

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
/// kind's code, so each kind is a section whose findings stand in `(path, id)`
/// order, and the page is in `(kind, path, id)` order.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ValidateStatement {
    /// One kind's findings standing under the active fingerprint, from the
    /// page's position on, in `(path, id)` order: a seek of
    /// `findings_vault_schema_fingerprint` at `(fingerprint, kind)` bounded
    /// below by the position, or of `findings_fingerprint_kind_severity` at
    /// `(fingerprint, kind, severity)` where the request admits one severity.
    /// A path part bounds that seek by the range its glob's literal prefix
    /// opens where the root tells spellings apart; where it folds ASCII case,
    /// the section seeks `findings_fingerprint_kind_severity_nocase` at
    /// `(fingerprint, kind, severity)` for each severity it admits over the
    /// folded prefix's range, and sorts what that range reached. A document
    /// part that keeps what it seeks drives the statement instead: the
    /// documents it matched each reach their findings by one seek at their
    /// path, and those are sorted.
    KindPage,
    /// How many findings stand, one tally per kind and severity: an aggregate
    /// over `findings_fingerprint_kind_severity`, which covers every column a
    /// tally reads, so it reads no finding row. It names every kind and every
    /// severity it admits, so each `(kind, severity)` cell is one seek of the
    /// index, which a path part's range bounds, and the tallies are grouped
    /// in the order the index holds them. Where the root folds ASCII case, a
    /// path part's folded range bounds the same cells of
    /// `findings_fingerprint_kind_severity_nocase`, which covers a tally as
    /// well. A document part that keeps what it
    /// seeks drives the statement instead: the documents it matched each take
    /// one seek of the index at their path per cell, and the groups are
    /// sorted.
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
/// standing where no document row does. Every path part's range folds into one
/// lower bound and one upper bound on `path` — the greatest lower and the least
/// upper — so the index seek takes the tightest range whichever the request
/// named, and each part's glob is tested on the paths that range reaches. The
/// parts match under the snapshot's path order, and the range is read in it:
///
/// - **Where the root tells spellings apart**, the range is bytewise, and the
///   position a page section resumes after folds into the same lower bound on
///   `(path, id)`, so a section seeks its kind's findings in `(path, id)` order
///   from the tighter of the two and sorts nothing.
/// - **Where the root folds ASCII case**, the range is the folded prefixes'
///   `NOCASE` range, which `findings_fingerprint_kind_severity_nocase` seeks at
///   each `(fingerprint, kind, severity)` cell the statement admits — so a
///   page names every severity it admits, as a summary does. That index holds
///   a kind's findings in folded path order, so the position is a bytewise
///   test of the rows the range reaches rather than a bound on the seek, and a
///   page section sorts what the range handed it into `(path, id)` order. A
///   section therefore costs the findings its path parts match, whatever page
///   it reads.
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
    // Every path part is compiled under the snapshot's one order.
    let folded = globs
        .iter()
        .any(|glob| glob.order == StoredPathOrder::AsciiCaseInsensitive);
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
    // names them where it narrows by one, and where a folded range is sought
    // through the index that leads with the severity.
    let every_severity: Vec<&str> = Severity::ALL.iter().map(Severity::as_str).collect();
    let severities = match (findings.statement, findings.severities) {
        (_, Some(severities)) => Some(severities),
        (ValidateStatement::Summary, None) => Some(every_severity.as_slice()),
        (ValidateStatement::KindPage, None) if folded => Some(every_severity.as_slice()),
        (ValidateStatement::KindPage, None) => None,
    };
    if let Some(severities) = severities {
        conditions.push(format!(
            "f.severity IN ({})",
            listed(&mut binder, severities)
        ));
    }

    // The greatest lower bound and the least upper bound of the path parts'
    // ranges, and where the range is bytewise, the section's position. A text
    // orders below the empty blob a range with no upper bound is bounded by.
    // Folded bounds are folded text, so their byte order is their `NOCASE`
    // order, and either bound picked is one some part's own range states.
    let position = findings
        .after
        .map_or((String::new(), 0), |(path, id)| (path.to_string(), id));
    let mut lower: (String, i64) = if folded {
        (String::new(), 0)
    } else {
        position.clone()
    };
    let mut upper: Option<Value> = None;
    for glob in &globs {
        if let Value::Text(from) = glob.lower
            && (from.as_str(), 0) > (lower.0.as_str(), lower.1)
        {
            lower = (from.clone(), 0);
        }
        upper = Some(match (upper, glob.upper) {
            (Some(Value::Text(held)), Value::Text(to)) => Value::Text(held.min(to.clone())),
            (Some(Value::Text(held)), _) => Value::Text(held),
            (_, to) => to.clone(),
        });
    }
    let collation = if folded {
        StoredPathOrder::AsciiCaseInsensitive.collation()
    } else {
        StoredPathOrder::Sensitive.collation()
    };
    // Where matched documents drive the statement, each one's findings are
    // sought at its path, and a folded range is a test of what that seek
    // reaches rather than a second seek to weigh against it.
    let ranged = if folded && driven {
        "+f.path"
    } else {
        "f.path"
    };
    match (findings.statement, folded) {
        (ValidateStatement::KindPage, false) => {
            let (path, id) = (
                binder.bind(Value::Text(lower.0)),
                binder.bind(Value::Integer(lower.1)),
            );
            conditions.push(format!("(f.path, f.id) > ({path}, {id})"));
        }
        (ValidateStatement::KindPage, true) => {
            conditions.push(format!(
                "{ranged} >= {}{collation}",
                binder.bind(Value::Text(lower.0))
            ));
            let (path, id) = (
                binder.bind(Value::Text(position.0)),
                binder.bind(Value::Integer(position.1)),
            );
            conditions.push(format!("(+f.path, f.id) > ({path}, {id})"));
        }
        (ValidateStatement::Summary, _) => {
            if folded || !lower.0.is_empty() {
                conditions.push(format!(
                    "{ranged} >= {}{collation}",
                    binder.bind(Value::Text(lower.0))
                ));
            }
        }
    }
    if let Some(upper) = upper {
        conditions.push(format!("{ranged} < {}{collation}", binder.bind(upper)));
    }
    for glob in &globs {
        let pattern = binder.bind(glob.pattern.clone());
        let order = binder.bind(glob.recorded_order.clone());
        conditions.push(glob_test(&pattern, "f.path", &order));
    }

    let tests: Vec<String> = documents
        .iter()
        .map(|filter| filter.spell("dv.id", &mut binder))
        .collect();
    let from = if driven {
        conditions.extend(tests);
        "FROM documents AS dv
                     CROSS JOIN findings AS f ON f.path = dv.path"
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
            // A folded range is read in folded order, so the page's order is
            // no index's and the unary `+` says so: the seek is the range's.
            let ordered = if folded {
                "+f.path, f.id"
            } else {
                "f.path, f.id"
            };
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
