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
/// is in `(kind, path, position, id)` order. A repair batch reads the same
/// findings with the kinds merged into that one path order instead
/// ([`ValidateStatement::MergedPage`]).
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
    /// The rules of each rule set a page's finding rows cite: one seek of
    /// `rule_sets`' row id per set, its names walked out of its spelling in
    /// byte order. A page citing no set runs none. Every verb answering
    /// finding rows runs it through the one resolution the finding row
    /// accessor holds, as a validate runs the find's finding-detail
    /// statements.
    RuleSets,
    /// The findings a repair batch selects, every kind read merged into one
    /// path order, from a place on: one arm per kind, each the seek a
    /// [`ValidateStatement::KindPage`] section is — of
    /// `findings_fingerprint_kind_nocase` at `(fingerprint, kind)`, or of
    /// `findings_fingerprint_kind_severity_nocase` where the request admits
    /// one severity — or, where a rule is selected, the seek a
    /// [`ValidateStatement::RulePage`] section is over the rule's rows. The
    /// arms are joined by `UNION ALL` and ordered together in `(path COLLATE
    /// NOCASE, path, position, id)`, which SQLite runs as a merge of the arms'
    /// own orders, so nothing sorts and a bounded read costs the rows it hands
    /// back. A path part bounds every arm as it bounds a section; a document
    /// part that keeps what it seeks drives each arm instead, and those are
    /// sorted.
    MergedPage,
    /// The findings of one document after a place in it, merged across the
    /// kinds read: [`ValidateStatement::MergedPage`] bounded to exactly one
    /// path, which a repair batch reads to take a document's remaining
    /// findings whole. Each arm pins the path by its folded and its bytewise
    /// equality, so it still seeks the index that holds the answer's path order,
    /// and seeks past the finding's position and id; the arms are ordered by
    /// position and id, which the index hands back within the one path, and
    /// nothing sorts. A document part that keeps what it seeks drives each arm
    /// as it does a merged page's.
    DocumentTail,
}

/// How many statement shapes [`ValidateStatement::all`] holds.
pub const VALIDATE_STATEMENTS: usize = 7;

impl ValidateStatement {
    /// Every statement shape, in slot order.
    pub fn all() -> [Self; VALIDATE_STATEMENTS] {
        [
            Self::KindPage,
            Self::Summary,
            Self::RulePage,
            Self::RuleSummary,
            Self::RuleSets,
            Self::MergedPage,
            Self::DocumentTail,
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
            Self::MergedPage => 5,
            Self::DocumentTail => 6,
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
    /// The one rule whose findings a rule statement reads — a
    /// [`ValidateStatement::RulePage`] or a [`ValidateStatement::RuleSummary`],
    /// and a merged read that selects a rule — and `None` for every finding.
    pub(crate) rule: Option<&'a str>,
    /// The kinds read: the one kind of a page's section, every kind a summary
    /// tallies, and the kind of each arm a merged read merges.
    pub(crate) kinds: &'a [&'a str],
    /// The severities admitted, as stored; `None` where every severity is.
    pub(crate) severities: Option<&'a [&'a str]>,
    /// The `(path, position, id)` a section resumes after; `None` starts at
    /// its first finding. A [`ValidateStatement::DocumentTail`] names it, and
    /// reads the path's findings after the position and id.
    pub(crate) after: Option<(&'a str, i64, i64)>,
    /// The conjunction's parts: a path part judges the finding's own path,
    /// and every other part the document row at it.
    pub(crate) filters: &'a [Filter],
    /// The rows a page reads at most; a summary and a document's tail read
    /// them all.
    pub(crate) rows: usize,
}

impl Findings<'_> {
    /// Whether the statement reads the rule's own rows rather than the
    /// findings'.
    fn ruled(&self) -> bool {
        match self.statement {
            ValidateStatement::KindPage | ValidateStatement::Summary => {
                assert!(
                    self.rule.is_none(),
                    "a statement over every rule names none"
                );
                false
            }
            ValidateStatement::RulePage | ValidateStatement::RuleSummary => {
                assert!(self.rule.is_some(), "a rule's statement names its rule");
                true
            }
            ValidateStatement::MergedPage | ValidateStatement::DocumentTail => self.rule.is_some(),
            ValidateStatement::RuleSets => {
                unreachable!("the rule sets are read by `compose_rule_sets`")
            }
        }
    }
}

/// The columns a merged read's arms add after a finding row's, each arm
/// spelling the keys its own table orders the findings by. A compound
/// statement orders by its result columns alone, and a rule's arm orders by
/// its rule rows' path, position and finding, which no column of the finding
/// itself states.
const MERGE_PATH: &str = "order_path";
const MERGE_POSITION: &str = "order_position";
const MERGE_ID: &str = "order_id";

/// The table one arm seeks, the alias it takes, and the column that holds the
/// finding's id there.
struct Seeked {
    table: &'static str,
    alias: &'static str,
    id: &'static str,
}

impl Seeked {
    fn of(ruled: bool) -> Self {
        if ruled {
            Seeked {
                table: "finding_rules",
                alias: "fr",
                id: "fr.finding",
            }
        } else {
            Seeked {
                table: "findings",
                alias: "f",
                id: "f.id",
            }
        }
    }

    fn column(&self, name: &str) -> String {
        format!("{}.{name}", self.alias)
    }
}

/// The conjunction's parts, sorted by what they judge.
struct Narrowed<'a> {
    /// The path parts' patterns, which judge the finding's own path.
    parts: Vec<&'a PathPart>,
    /// The parts that judge the document row at the finding's path.
    documents: Vec<&'a Filter>,
    /// Whether some document part keeps what it seeks, so the matched
    /// documents drive the statement.
    driven: bool,
}

impl<'a> Narrowed<'a> {
    fn of(filters: &'a [Filter]) -> Self {
        let (paths, documents): (Vec<&Filter>, Vec<&Filter>) = filters
            .iter()
            .partition(|filter| filter.path_part().is_some());
        Narrowed {
            parts: paths
                .iter()
                .filter_map(|filter| filter.path_part())
                .collect(),
            driven: documents.iter().any(|filter| !filter.shape().excludes()),
            documents,
        }
    }
}

/// One validate statement and its parameters, in the numbering the text
/// states.
///
/// **A statement is its arms, ordered and bounded.** [`compose_arm`] writes one
/// arm — one kind's findings, or every kind a summary tallies — and this adds
/// what the statement does with them: a page orders one arm and bounds it, a
/// summary groups one, and a merged read joins an arm per kind with `UNION ALL`
/// and orders and bounds the join. So a kind's page and the arm it contributes
/// to a merge are one text, written once.
pub(crate) fn compose_findings(findings: &Findings<'_>) -> (String, Vec<Value>) {
    let mut binder = Binder::default();
    let narrowed = Narrowed::of(findings.filters);
    let seeked = Seeked::of(findings.ruled());
    let limit = |binder: &mut Binder| {
        binder.bind(Value::Integer(
            i64::try_from(findings.rows).expect("a page's row count fits i64"),
        ))
    };
    let text = match findings.statement {
        ValidateStatement::KindPage | ValidateStatement::RulePage => {
            let arm = compose_arm(findings, &narrowed, findings.kinds, &mut binder);
            let ordered = format!(
                "{}, {}, {}",
                answer_ordering(&seeked.column("path"), ""),
                seeked.column("position"),
                seeked.id
            );
            let limit = limit(&mut binder);
            format!(
                "{arm}\n                     ORDER BY {ordered}\n                     LIMIT {limit}"
            )
        }
        ValidateStatement::MergedPage | ValidateStatement::DocumentTail => {
            assert!(!findings.kinds.is_empty(), "a merged read reads a kind");
            let arms: Vec<String> = findings
                .kinds
                .iter()
                .map(|kind| {
                    compose_arm(findings, &narrowed, std::slice::from_ref(kind), &mut binder)
                })
                .collect();
            let arms = arms.join("\n                     UNION ALL\n                     ");
            if findings.statement == ValidateStatement::MergedPage {
                let ordered = answer_ordering(MERGE_PATH, "");
                let limit = limit(&mut binder);
                format!(
                    "{arms}\n                     ORDER BY {ordered}, {MERGE_POSITION}, {MERGE_ID}\n                     LIMIT {limit}"
                )
            } else {
                // Every finding of a tail stands at the one path, so the
                // arms are ordered by the keys that remain. Ordering by the
                // path as well would sort: the index holds the path's
                // equalities constant, and SQLite does not take the terms
                // naming it as satisfied.
                format!("{arms}\n                     ORDER BY {MERGE_POSITION}, {MERGE_ID}")
            }
        }
        ValidateStatement::Summary | ValidateStatement::RuleSummary => {
            let arm = compose_arm(findings, &narrowed, findings.kinds, &mut binder);
            let (kind, severity) = (seeked.column("kind"), seeked.column("severity"));
            format!(
                "{arm}\n                     GROUP BY {kind}, {severity}\n                     ORDER BY {kind}, {severity}"
            )
        }
        ValidateStatement::RuleSets => {
            unreachable!("the rule sets are read by `compose_rule_sets`")
        }
    };
    (text, binder.into_values())
}

/// One arm of a statement: a `SELECT` of `kinds` — the one kind of a page's
/// section and of each arm of a merge, and every kind a summary tallies —
/// under the fingerprint, the rule, the severities and the place the
/// statement reads from, with its parameters numbered in `binder`.
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
/// **A document's tail pins the path instead**, by the folded and the
/// bytewise equality together, which is what lets the seek run down an index
/// holding the answer's path order, and resumes after a position and an id.
///
/// **Every other part judges the document row at the finding's path**, which
/// a finding standing where no document row does never satisfies. Where some
/// such part keeps what it seeks, the matched documents drive the statement:
/// `CROSS JOIN` keeps them the outer loop, reached by the part's seek, and
/// each one's findings are one seek at its path, so the statement costs what
/// the part matched and sorts that. Where every such part excludes, the
/// statement seeks its kind as an unnarrowed one does and tests each
/// finding's document row, one seek of `documents_path` at its path.
fn compose_arm(
    findings: &Findings<'_>,
    narrowed: &Narrowed<'_>,
    kinds: &[&str],
    binder: &mut Binder,
) -> String {
    let Narrowed {
        parts,
        documents,
        driven,
    } = narrowed;
    let on_a_document = !documents.is_empty();
    let statement = findings.statement;
    let rows = matches!(
        statement,
        ValidateStatement::KindPage
            | ValidateStatement::RulePage
            | ValidateStatement::MergedPage
            | ValidateStatement::DocumentTail
    );
    let ruled = findings.ruled();
    let seeked = Seeked::of(ruled);
    let (table, seek, id) = (seeked.table, seeked.alias, seeked.id);
    let column = |name: &str| seeked.column(name);

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
    match (rows, kinds) {
        (true, [kind]) => conditions.push(format!(
            "{} = {}",
            column("kind"),
            binder.bind(Value::Text((*kind).to_string()))
        )),
        (true, _) => unreachable!("a page section reads one kind"),
        (false, kinds) => {
            conditions.push(format!("{} IN ({})", column("kind"), listed(binder, kinds)))
        }
    }
    // A summary names every severity it admits, so each `(kind, severity)`
    // cell is a seek of its own and a path part's range bounds each; a page
    // names them where it narrows by one.
    let every_severity: Vec<&str> = Severity::ALL.iter().map(Severity::as_str).collect();
    let severities = match (rows, findings.severities) {
        (_, Some(severities)) => Some(severities),
        (false, None) => Some(every_severity.as_slice()),
        (true, None) => None,
    };
    if let Some(severities) = severities {
        conditions.push(format!(
            "{} IN ({})",
            column("severity"),
            listed(binder, severities)
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
    for part in parts {
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
    if statement == ValidateStatement::DocumentTail {
        // The tail is the one path's findings after a position and an id, so
        // the path parts' lower bounds hold already: the path was admitted
        // by the reading that named it.
        let (path, position, after_id) = findings
            .after
            .expect("a document's tail names the place it resumes after");
        let (folded, path) = answer_place(path);
        let (folded, path, position, after_id) = (
            binder.bind(Value::Text(folded)),
            binder.bind(Value::Text(path)),
            binder.bind(Value::Integer(position)),
            binder.bind(Value::Integer(after_id)),
        );
        conditions.push(format!("{path_column} = {folded} COLLATE NOCASE"));
        conditions.push(format!("{path_column} = {path}"));
        conditions.push(format!(
            "({position_column}, {id}) > ({position}, {after_id} COLLATE BINARY)"
        ));
    } else if rows {
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
    for part in parts {
        conditions.push(part.glob_test(&path_column, binder));
    }

    let tests: Vec<String> = documents
        .iter()
        .map(|filter| filter.spell("dv.id", binder))
        .collect();
    // A rule's page reaches each finding it pages from the rule's row, by the
    // finding's row id; its tally reads the rule's rows alone.
    let reached = if ruled && rows {
        "\n                     CROSS JOIN findings AS f ON f.id = fr.finding"
    } else {
        ""
    };
    let from = if *driven {
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
    let selected = match statement {
        ValidateStatement::KindPage | ValidateStatement::RulePage => {
            FINDING_ROW_COLUMNS.to_string()
        }
        ValidateStatement::MergedPage | ValidateStatement::DocumentTail => format!(
            "{FINDING_ROW_COLUMNS}, {path_column} AS {MERGE_PATH}, \
             {position_column} AS {MERGE_POSITION}, {id} AS {MERGE_ID}"
        ),
        ValidateStatement::Summary | ValidateStatement::RuleSummary => {
            format!("{}, {}, COUNT(*)", column("kind"), column("severity"))
        }
        ValidateStatement::RuleSets => {
            unreachable!("the rule sets are read by `compose_rule_sets`")
        }
    };
    format!(
        "SELECT {selected} {from}
                     WHERE {conditions}"
    )
}

/// The statement reading the rules of each rule set `ids` names: one seek of
/// `rule_sets`' row id per set, in the order of its identity, the names of
/// each walked out of its spelling in order ([`crate::rule_set`]).
pub(crate) fn compose_rule_sets(ids: &[i64]) -> (String, Vec<Value>) {
    let mut binder = Binder::default();
    let listed: Vec<String> = ids
        .iter()
        .map(|id| binder.bind(Value::Integer(*id)))
        .collect();
    (
        format!(
            "SELECT rs.id, {} FROM rule_sets AS rs
                     LEFT JOIN json_each(rs.rules) AS j
                     WHERE rs.id IN ({})
                     ORDER BY rs.id",
            crate::rule_set::walked_columns("rs", "j"),
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
