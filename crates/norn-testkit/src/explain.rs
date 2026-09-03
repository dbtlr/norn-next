//! Query-plan assertions, stated over the SQL that was actually emitted.
//!
//! A plan assertion is only worth anything against the statement the builder
//! produced: a gate written over hand-copied SQL tests a string nobody
//! executes. [`QueryPlan`] is the pairing that makes the coupling structural
//! — it holds the emitted statement together with the `EXPLAIN QUERY PLAN`
//! rows for *that* statement, and every assertion here is a method on the
//! pair, so there is no way to assert about a plan without naming the SQL it
//! came from. A failure prints the statement for the same reason.
//!
//! The pairing earns its keep twice over: SQLite reports a plan in terms of
//! the *aliases* the statement used, so `documents AS d` becomes `SCAN d`,
//! and only the statement says what `d` is. The alias resolution here reads
//! the SQL for exactly that.
//!
//! # What counts as unbounded
//!
//! A `SCAN` row reads its relation end to end, and it does so whether or not
//! it reads through an index: `SCAN t USING COVERING INDEX i` visits one
//! entry per row of `t`, which is the cost the bar is about. So **the scan
//! bar fails on any scan of a table**, covering index included, unless the
//! row's detail carries a constraint. Three forms are not table reads and do
//! not fail it:
//!
//! - `SCAN t VIRTUAL TABLE INDEX 1:` or `SCAN t VIRTUAL TABLE INDEX 0:M1` —
//!   a virtual table consulted through its own index selection, which is how
//!   an FTS5 `MATCH` or a table-valued function's argument is planned. The
//!   pair is `idxNum:idxStr`, and either half being non-trivial bounds the
//!   scan: a nonzero `idxNum` on its own, or a non-empty `idxStr` on its own.
//!   Only the exact pair `0:` constrains nothing, and that one is unbounded.
//! - `SCAN 2-ROW VALUES CLAUSE` — an inline list whose length is in the
//!   statement, not in the data.
//! - A scan of a co-routine or materialized subquery, whose own steps appear
//!   as separate rows and are judged there.
//!
//! Nothing here opens a database. The rows are handed in by whoever ran the
//! `EXPLAIN`, which is the store's own API.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

/// One row of `EXPLAIN QUERY PLAN`, in SQLite's shape.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlanRow {
    pub id: i64,
    pub parent: i64,
    pub detail: String,
}

/// What a `SCAN` row reads.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ScanTarget<'a> {
    /// A named relation: a table, an alias for one, or the name of a
    /// co-routine or materialized subquery.
    Relation(&'a str),
    /// A virtual table read through its own index, as
    /// `VIRTUAL TABLE INDEX <idxNum>:<idxStr>`.
    VirtualTable {
        name: &'a str,
        /// `idxNum`: the index the virtual table's module chose, by number.
        /// Nonzero bounds the scan on its own, whatever `idxStr` says.
        index_number: &'a str,
        /// `idxStr`: the virtual table's own encoding of the constraints it
        /// was given. Non-empty means it was given some.
        specification: &'a str,
    },
    /// An inline `VALUES` list. It names no relation.
    ValuesClause,
}

impl PlanRow {
    pub fn new(id: i64, parent: i64, detail: impl Into<String>) -> Self {
        PlanRow {
            id,
            parent,
            detail: detail.into(),
        }
    }

    /// What this row scans, if it scans anything.
    ///
    /// SQLite spells the older form `SCAN TABLE t` and the current one
    /// `SCAN t`. The name is whatever the statement called the relation, so
    /// it is an alias wherever the statement used one; [`QueryPlan`] resolves
    /// that against the SQL.
    pub fn scan_target(&self) -> Option<ScanTarget<'_>> {
        let rest = self.detail.strip_prefix("SCAN ")?;
        let rest = rest.strip_prefix("TABLE ").unwrap_or(rest);
        if rest.ends_with("VALUES CLAUSE") {
            return Some(ScanTarget::ValuesClause);
        }
        let name = rest.split_whitespace().next()?;
        let tail = rest[name.len()..].trim_start();
        if let Some(spec) = tail.strip_prefix("VIRTUAL TABLE INDEX ") {
            let (index_number, specification) = spec.split_once(':').unwrap_or((spec, ""));
            return Some(ScanTarget::VirtualTable {
                name,
                index_number: index_number.trim(),
                specification: specification.trim(),
            });
        }
        Some(ScanTarget::Relation(name))
    }

    /// The relation this row scans, if it scans a named one. A `VALUES`
    /// clause names nothing and reports `None`.
    pub fn scans(&self) -> Option<&str> {
        match self.scan_target()? {
            ScanTarget::Relation(name) | ScanTarget::VirtualTable { name, .. } => Some(name),
            ScanTarget::ValuesClause => None,
        }
    }

    /// The relation this row searches, if it searches one. A search uses an
    /// index or a rowid to reach the rows it wants.
    pub fn searches(&self) -> Option<&str> {
        let rest = self.detail.strip_prefix("SEARCH ")?;
        let rest = rest.strip_prefix("TABLE ").unwrap_or(rest);
        rest.split_whitespace().next()
    }

    /// The index this row reads through, if it names one.
    ///
    /// An index SQLite built for this statement has no name; the row reads
    /// `USING AUTOMATIC COVERING INDEX` and reports `None` here and `true`
    /// from [`PlanRow::uses_automatic_index`]. So does a rowid lookup, which
    /// reads `USING INTEGER PRIMARY KEY`.
    pub fn index(&self) -> Option<&str> {
        let (_, rest) = self.detail.split_once(" USING ")?;
        let rest = rest.strip_prefix("COVERING ").unwrap_or(rest);
        let rest = rest.strip_prefix("INDEX ")?;
        let name = rest.split_whitespace().next().unwrap_or(rest);
        (!name.starts_with('(')).then_some(name)
    }

    /// Whether this row reads through an index SQLite built for the
    /// statement rather than one the schema declares.
    pub fn uses_automatic_index(&self) -> bool {
        self.detail.contains(" USING AUTOMATIC ")
    }

    /// The constraint the row was given, as SQLite prints it: the
    /// parenthesised tail of the detail, such as `(stem=?)`.
    pub fn constraint(&self) -> Option<&str> {
        let open = self.detail.rfind('(')?;
        let close = self.detail[open..].find(')')? + open;
        Some(&self.detail[open..=close])
    }

    /// Whether this row reads its relation end to end.
    ///
    /// True for any scan of a named relation that carries no constraint,
    /// index or no index, and for a virtual table asked for everything.
    /// False for a `VALUES` clause and for a constrained virtual-table read.
    pub fn is_unbounded_scan(&self) -> bool {
        match self.scan_target() {
            None | Some(ScanTarget::ValuesClause) => false,
            Some(ScanTarget::VirtualTable {
                index_number,
                specification,
                ..
            }) => index_number == "0" && specification.is_empty(),
            Some(ScanTarget::Relation(_)) => self.constraint().is_none(),
        }
    }

    /// Whether this row reads a table's own rows without an index — the
    /// weaker bar. A covering-index scan is not a table scan: it never
    /// touches the table's own pages, and it is unbounded all the same.
    pub fn is_table_scan(&self) -> bool {
        matches!(self.scan_target(), Some(ScanTarget::Relation(_))) && self.index().is_none()
    }
}

impl fmt::Display for PlanRow {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}|{}|{}", self.id, self.parent, self.detail)
    }
}

/// The statement a builder emitted, paired with its query plan.
#[derive(Clone, Debug)]
pub struct QueryPlan {
    sql: String,
    rows: Vec<PlanRow>,
    aliases: BTreeMap<String, String>,
}

impl QueryPlan {
    /// Pair an emitted statement with the plan SQLite reported for it.
    pub fn new(sql: impl Into<String>, rows: Vec<PlanRow>) -> Self {
        let sql = sql.into();
        let aliases = aliases(&sql);
        QueryPlan { sql, rows, aliases }
    }

    /// The statement the assertions are about.
    pub fn sql(&self) -> &str {
        &self.sql
    }

    pub fn rows(&self) -> &[PlanRow] {
        &self.rows
    }

    /// The table a plan row's name refers to, resolved through the aliases
    /// the statement declared. SQLite echoes a schema qualifier back in the
    /// row (`SCAN main.documents`), so it is stripped here before the alias
    /// lookup, the same as it is read off the SQL side. A name the reader
    /// cannot bind is its own bare form.
    pub fn table_of<'a>(&'a self, name: &'a str) -> &'a str {
        let name = bare_name(name);
        self.aliases.get(name).map_or(name, String::as_str)
    }

    /// The names the statement's plan introduces for subqueries: a
    /// co-routine or a materialized common table expression. A scan of one is
    /// not a table read, and the subquery's own steps are separate rows.
    fn subqueries(&self) -> BTreeSet<&str> {
        self.rows
            .iter()
            .filter_map(|row| {
                let detail = &row.detail;
                detail
                    .strip_prefix("CO-ROUTINE ")
                    .or_else(|| detail.strip_prefix("MATERIALIZE "))
            })
            .map(str::trim)
            .collect()
    }

    /// Every step that reads a relation end to end.
    pub fn unbounded_steps(&self) -> Vec<&PlanRow> {
        let subqueries = self.subqueries();
        self.rows
            .iter()
            .filter(|row| row.is_unbounded_scan())
            .filter(|row| !row.scans().is_some_and(|name| subqueries.contains(name)))
            .collect()
    }

    /// Every step that reads a table's own rows without an index.
    pub fn table_scans(&self) -> Vec<&PlanRow> {
        let subqueries = self.subqueries();
        self.rows
            .iter()
            .filter(|row| row.is_table_scan())
            .filter(|row| !row.scans().is_some_and(|name| subqueries.contains(name)))
            .collect()
    }

    /// **The scan bar.** No step of this plan reads `table` end to end.
    pub fn assert_no_full_scan_of(&self, table: &str) {
        let table = bare_name(table);
        let scans: Vec<&PlanRow> = self
            .unbounded_steps()
            .into_iter()
            .filter(|row| row.scans().map(|name| self.table_of(name)) == Some(table))
            .collect();
        assert!(
            scans.is_empty(),
            "the plan reads `{table}` end to end: {}\nemitted SQL: {}",
            rows_display(&scans),
            self.sql
        );
    }

    /// No step of this plan reads any relation end to end.
    pub fn assert_no_full_scan(&self) {
        let scans = self.unbounded_steps();
        assert!(
            scans.is_empty(),
            "the plan reads a relation end to end: {}\nemitted SQL: {}",
            rows_display(&scans),
            self.sql
        );
    }

    /// The weaker bar: no step reads a table's own rows without an index. A
    /// covering-index scan passes this and fails [`QueryPlan::assert_no_full_scan`].
    pub fn assert_no_table_scan(&self) {
        let scans = self.table_scans();
        assert!(
            scans.is_empty(),
            "the plan reads a table without an index: {}\nemitted SQL: {}",
            rows_display(&scans),
            self.sql
        );
    }

    /// **The index bar.** The plan reaches `table` by a search rather than a
    /// scan, which is the difference an index makes.
    pub fn assert_searches(&self, table: &str) {
        let table = bare_name(table);
        assert!(
            self.rows
                .iter()
                .any(|row| row.searches().map(|name| self.table_of(name)) == Some(table)),
            "the plan does not search `{table}`: {}\nemitted SQL: {}",
            rows_display(&self.rows.iter().collect::<Vec<_>>()),
            self.sql
        );
    }

    /// **The equality bar.** Every step that searches `table` was given this
    /// constraint, in the spelling SQLite prints it.
    ///
    /// A search is not a point read on its own. `path>?` opens a cursor on the
    /// same index as `path=?` and reports the same `SEARCH … USING INDEX` row,
    /// so [`QueryPlan::assert_uses_index`] and [`QueryPlan::assert_searches`]
    /// both green a reader that seeks to a key and then walks the rest of the
    /// table. The constraint text is where the two come apart, and it is the
    /// only place a `WITHOUT ROWID` primary-key seek can be judged at all —
    /// that row names no index.
    ///
    /// The constraint is read off the steps that search `table` and nowhere
    /// else, so an equality seek of some other relation in the same plan
    /// cannot answer for this one. A plan that does not search the table at all
    /// fails, for the same reason [`QueryPlan::assert_searches`] does.
    pub fn assert_search_constraint(&self, table: &str, constraint: &str) {
        let table = bare_name(table);
        let searches: Vec<&PlanRow> = self
            .rows
            .iter()
            .filter(|row| row.searches().map(|name| self.table_of(name)) == Some(table))
            .collect();
        assert!(
            !searches.is_empty(),
            "the plan does not search `{table}`: {}\nemitted SQL: {}",
            rows_display(&self.rows.iter().collect::<Vec<_>>()),
            self.sql
        );
        let wrong: Vec<&PlanRow> = searches
            .into_iter()
            .filter(|row| row.constraint() != Some(constraint))
            .collect();
        assert!(
            wrong.is_empty(),
            "a step searching `{table}` does not carry the constraint `{constraint}`: {}\n\
             emitted SQL: {}",
            rows_display(&wrong),
            self.sql
        );
    }

    /// The plan reads through the named index.
    pub fn assert_uses_index(&self, index: &str) {
        assert!(
            self.rows.iter().any(|row| row.index() == Some(index)),
            "the plan does not use index `{index}`: {}\nemitted SQL: {}",
            rows_display(&self.rows.iter().collect::<Vec<_>>()),
            self.sql
        );
    }

    /// The plan sorts or groups without materializing a temporary B-tree.
    pub fn assert_no_temp_btree(&self) {
        let temporary: Vec<&PlanRow> = self
            .rows
            .iter()
            .filter(|row| row.detail.contains("TEMP B-TREE"))
            .collect();
        assert!(
            temporary.is_empty(),
            "the plan builds a temporary B-tree: {}\nemitted SQL: {}",
            rows_display(&temporary),
            self.sql
        );
    }
}

fn rows_display(rows: &[&PlanRow]) -> String {
    if rows.is_empty() {
        return "(no rows)".to_string();
    }
    rows.iter()
        .map(|row| row.to_string())
        .collect::<Vec<_>>()
        .join("; ")
}

/// The alias-to-table bindings a statement declares, as `alias -> table`.
///
/// The reader takes the shape a builder emits: after `FROM` or `JOIN`, a bare
/// table name, optionally followed by `AS alias` or by a bare alias. A schema
/// qualifier is dropped here on the SQL side, and [`QueryPlan::table_of`]
/// drops it again from the plan row's name — SQLite echoes the qualifier
/// back as `SCAN main.documents` rather than reporting the bare name. Its
/// limits are exact and worth stating, because an alias it cannot bind is
/// left to stand for itself rather than guessed at:
///
/// - A subquery or table-valued function in a `FROM` clause binds nothing;
///   the reader stops at the `(` and resumes at the next `FROM` or `JOIN`.
/// - A quoted identifier holding whitespace is not understood.
/// - A comment is not recognized; its words are read as SQL.
/// - The reader does not scope aliases: a statement whose subquery reuses an
///   outer alias for a different table poisons that alias rather than
///   rebinding it — both bindings are dropped, and `table_of` falls back to
///   the alias itself. So a scoped bar (`assert_no_full_scan_of`,
///   `assert_searches`) neither greens the shadowed table nor blames the
///   wrong one; it simply cannot vouch for either name once it is ambiguous.
///   `assert_no_full_scan` is unscoped and judges every row by whether it
///   scans at all, so it is the bar that still catches a shadowed scan.
fn aliases(sql: &str) -> BTreeMap<String, String> {
    const AFTER_A_TABLE: &[&str] = &[
        "ON",
        "USING",
        "WHERE",
        "GROUP",
        "ORDER",
        "LIMIT",
        "HAVING",
        "WINDOW",
        "RETURNING",
        "SET",
        "VALUES",
        "UNION",
        "INTERSECT",
        "EXCEPT",
        "NATURAL",
        "LEFT",
        "RIGHT",
        "FULL",
        "INNER",
        "OUTER",
        "CROSS",
        "JOIN",
        "FROM",
        "INDEXED",
        "NOT",
        "AS",
        "SELECT",
    ];

    let mut bindings: BTreeMap<String, String> = BTreeMap::new();
    let mut poisoned: BTreeSet<String> = BTreeSet::new();
    let tokens = tokenize(sql);
    let mut index = 0;
    while index < tokens.len() {
        let token = tokens[index];
        index += 1;
        if !token.eq_ignore_ascii_case("FROM") && !token.eq_ignore_ascii_case("JOIN") {
            continue;
        }
        while let Some(&table) = tokens.get(index) {
            if table == "(" || AFTER_A_TABLE.iter().any(|k| table.eq_ignore_ascii_case(k)) {
                break;
            }
            index += 1;
            let table = bare_name(table);
            if let Some(&next) = tokens.get(index) {
                let alias = if next.eq_ignore_ascii_case("AS") {
                    index += 1;
                    tokens.get(index).copied()
                } else if next == "," || AFTER_A_TABLE.iter().any(|k| next.eq_ignore_ascii_case(k))
                {
                    None
                } else {
                    Some(next)
                };
                if let Some(alias) = alias {
                    index += 1;
                    let alias = bare_name(alias);
                    if is_identifier(alias) {
                        bind(&mut bindings, &mut poisoned, alias, table);
                    }
                }
            }
            // A comma-separated `FROM` clause names another table.
            match tokens.get(index) {
                Some(&",") => index += 1,
                _ => break,
            }
        }
    }
    bindings
}

/// Record `alias -> table`, unless `alias` already names a different table.
///
/// A statement whose subquery reuses an outer alias for a different table —
/// the shadowing shape SQL scoping allows — would otherwise get one binding,
/// the last one read, and misattribute every row spelled with that alias to
/// the wrong table. Poisoning it instead drops both bindings for good, so
/// [`QueryPlan::table_of`] falls back to the alias itself.
fn bind(
    bindings: &mut BTreeMap<String, String>,
    poisoned: &mut BTreeSet<String>,
    alias: &str,
    table: &str,
) {
    if poisoned.contains(alias) {
        return;
    }
    match bindings.get(alias) {
        Some(existing) if !existing.eq_ignore_ascii_case(table) => {
            bindings.remove(alias);
            poisoned.insert(alias.to_string());
        }
        Some(_) => {}
        None => {
            bindings.insert(alias.to_string(), table.to_string());
        }
    }
}

/// The statement split into words, with `(`, `)` and `,` standing alone. A
/// single-quoted string literal is read whole, quotes included, so a word
/// inside one — `' from links d '`, say — is never mistaken for SQL.
fn tokenize(sql: &str) -> Vec<&str> {
    let mut tokens = Vec::new();
    let mut rest = sql;
    loop {
        rest = rest.trim_start();
        if rest.is_empty() {
            break;
        }
        if let Some(literal) = string_literal(rest) {
            tokens.push(literal);
            rest = &rest[literal.len()..];
            continue;
        }
        let end = rest
            .find(|c: char| c.is_whitespace() || c == '(' || c == ')' || c == ',' || c == '\'')
            .unwrap_or(rest.len());
        let end = if end == 0 { 1 } else { end };
        tokens.push(&rest[..end]);
        rest = &rest[end..];
    }
    tokens
}

/// The single-quoted SQL string literal starting at `s`, quotes included, if
/// `s` starts with one. `''` inside it is the escaped quote, not the end.
fn string_literal(s: &str) -> Option<&str> {
    if !s.starts_with('\'') {
        return None;
    }
    let bytes = s.as_bytes();
    let mut end = 1;
    while end < bytes.len() {
        if bytes[end] == b'\'' {
            if bytes.get(end + 1) == Some(&b'\'') {
                end += 2;
                continue;
            }
            return Some(&s[..=end]);
        }
        end += 1;
    }
    // An unterminated literal: take the rest rather than looping forever.
    Some(s)
}

/// Whether `s` is a bare SQL identifier: a letter or underscore, then any
/// number of letters, digits or underscores. A token the reader would
/// otherwise treat as an alias but that fails this shape is punctuation the
/// surrounding syntax left behind — a stray `)`, say — not a name the
/// statement gave anything.
fn is_identifier(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// A name with its schema qualifier and its quoting stripped.
fn bare_name(token: &str) -> &str {
    let token = token.rsplit('.').next().unwrap_or(token);
    token.trim_matches(|c| c == '"' || c == '`' || c == '[' || c == ']')
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Canned rows in the shapes SQLite reports them.
    fn plan(details: &[&str]) -> QueryPlan {
        QueryPlan::new("SELECT path FROM documents WHERE stem = ?", rows(details))
    }

    fn rows(details: &[&str]) -> Vec<PlanRow> {
        details
            .iter()
            .enumerate()
            .map(|(i, detail)| PlanRow::new(i as i64 + 2, 0, *detail))
            .collect()
    }

    #[test]
    fn a_plan_carries_the_statement_it_was_taken_of() {
        let plan = plan(&["SEARCH documents USING INDEX documents_stem (stem=?)"]);
        assert_eq!(plan.sql(), "SELECT path FROM documents WHERE stem = ?");
        assert_eq!(plan.rows().len(), 1);
    }

    #[test]
    fn a_scan_is_read_from_either_spelling() {
        assert_eq!(
            PlanRow::new(2, 0, "SCAN documents").scans(),
            Some("documents")
        );
        assert_eq!(
            PlanRow::new(2, 0, "SCAN TABLE documents").scans(),
            Some("documents")
        );
        assert_eq!(PlanRow::new(2, 0, "SEARCH documents").scans(), None);
    }

    /// `SELECT stem FROM documents` against an index on `stem`. It never
    /// touches the table, and it still visits one index entry per row.
    #[test]
    fn a_covering_index_scan_is_unbounded_and_is_not_a_table_scan() {
        let row = PlanRow::new(2, 0, "SCAN documents USING COVERING INDEX documents_stem");
        assert_eq!(row.index(), Some("documents_stem"));
        assert!(row.is_unbounded_scan());
        assert!(!row.is_table_scan());

        let plan = plan(&["SCAN documents USING COVERING INDEX documents_stem"]);
        plan.assert_no_table_scan();
        let failure = std::panic::catch_unwind({
            let plan = plan.clone();
            move || plan.assert_no_full_scan()
        })
        .expect_err("a covering-index scan is unbounded");
        let message = failure
            .downcast_ref::<String>()
            .expect("a formatted assertion message");
        assert!(message.contains("reads a relation end to end"), "{message}");
    }

    #[test]
    fn a_plain_scan_is_both_unbounded_and_a_table_scan() {
        let row = PlanRow::new(2, 0, "SCAN documents");
        assert!(row.is_unbounded_scan());
        assert!(row.is_table_scan());
    }

    /// `SELECT d.path FROM documents AS d WHERE d.stem = ?`: SQLite reports
    /// the alias and nothing else, so only the statement says what `d` is.
    #[test]
    fn an_alias_resolves_to_the_table_the_statement_named() {
        let scan = QueryPlan::new(
            "SELECT d.path FROM documents AS d",
            rows(&["SCAN d USING COVERING INDEX documents_path"]),
        );
        assert_eq!(scan.table_of("d"), "documents");
        let failure = std::panic::catch_unwind({
            let scan = scan.clone();
            move || scan.assert_no_full_scan_of("documents")
        })
        .expect_err("an aliased scan of documents");
        assert!(
            failure
                .downcast_ref::<String>()
                .expect("a formatted assertion message")
                .contains("reads `documents` end to end")
        );

        let search = QueryPlan::new(
            "SELECT d.path FROM documents d WHERE d.stem = ?",
            rows(&["SEARCH d USING INDEX documents_stem (stem=?)"]),
        );
        search.assert_searches("documents");
        search.assert_no_full_scan();
    }

    /// SQLite echoes a schema qualifier back into the plan row, not just the
    /// alias, so a plan-row name has to be stripped of it too, the same as
    /// the SQL side is.
    #[test]
    fn a_schema_qualified_scan_row_still_resolves_to_the_bare_table() {
        let plan = QueryPlan::new(
            "SELECT path FROM main.documents WHERE stem = ?",
            rows(&["SCAN main.documents"]),
        );
        let failure = std::panic::catch_unwind({
            let plan = plan.clone();
            move || plan.assert_no_full_scan_of("documents")
        })
        .expect_err("a schema-qualified scan of documents");
        assert!(
            failure
                .downcast_ref::<String>()
                .expect("a formatted assertion message")
                .contains("reads `documents` end to end")
        );

        QueryPlan::new(
            "SELECT path FROM main.documents WHERE stem = ?",
            rows(&["SEARCH main.documents USING INDEX documents_stem (stem=?)"]),
        )
        .assert_searches("documents");
    }

    /// A subquery that reuses an outer alias for a different table shadows
    /// it, and SQLite reports both scans under the same name. Resolving to
    /// whichever table was bound last would attribute an unbounded scan of
    /// one table to the other; poisoning the alias instead leaves it
    /// standing for itself.
    #[test]
    fn a_shadowed_alias_is_left_unresolved_rather_than_misattributed() {
        let plan = QueryPlan::new(
            "SELECT * FROM documents d WHERE EXISTS (SELECT 1 FROM links d WHERE d.source = 1)",
            rows(&["SCAN d", "SCAN d"]),
        );
        // Not "documents" (the first binding) and not "links" (the last) --
        // the alias stands for itself once it is ambiguous.
        assert_eq!(plan.table_of("d"), "d");

        // The scoped bar cannot vouch for `documents` once its alias is
        // ambiguous: it neither wrongly greens it nor blames `links` for it.
        plan.assert_no_full_scan_of("documents");

        // The unscoped bar does not need to resolve the alias at all, and
        // still catches the scan the scoped bar could not attribute.
        let failure = std::panic::catch_unwind({
            let plan = plan.clone();
            move || plan.assert_no_full_scan()
        })
        .expect_err("an unbounded scan under a shadowed alias");
        assert!(
            failure
                .downcast_ref::<String>()
                .expect("a formatted assertion message")
                .contains("reads a relation end to end")
        );
    }

    /// A CTE's closing paren sits where an alias would, and is not one.
    #[test]
    fn a_closing_paren_after_a_cte_is_not_read_as_an_alias() {
        let plan = QueryPlan::new("WITH x AS (SELECT 1 FROM t) SELECT * FROM x", Vec::new());
        assert_eq!(plan.table_of("t"), "t");
        assert_eq!(plan.table_of(")"), ")");
    }

    /// A string literal is not SQL, even when its contents look like some:
    /// the words inside `' from links d '` name nothing.
    #[test]
    fn a_string_literal_is_not_read_as_sql() {
        for sql in [
            "SELECT * FROM documents WHERE note = ' from links d '",
            "SELECT * FROM documents WHERE note=' from links d '",
            "SELECT * FROM documents WHERE note='it''s from links d '",
        ] {
            let plan = QueryPlan::new(sql, Vec::new());
            assert_eq!(plan.table_of("d"), "d", "for: {sql}");
            assert_eq!(plan.table_of("links"), "links", "for: {sql}");
        }
    }

    #[test]
    fn a_literal_hugging_an_operator_still_ends_the_preceding_word() {
        let plan = QueryPlan::new(
            "SELECT * FROM documents d WHERE note=' from links d '",
            Vec::new(),
        );
        assert_eq!(plan.table_of("d"), "documents");
    }

    #[test]
    fn aliases_are_read_past_joins_commas_and_schema_qualifiers() {
        let joined = QueryPlan::new(
            "SELECT * FROM main.documents AS d JOIN links l ON l.source = d.id",
            Vec::new(),
        );
        assert_eq!(joined.table_of("d"), "documents");
        assert_eq!(joined.table_of("l"), "links");
        assert_eq!(joined.table_of("documents"), "documents");

        let listed = QueryPlan::new("SELECT * FROM documents d, links l, tags", Vec::new());
        assert_eq!(listed.table_of("d"), "documents");
        assert_eq!(listed.table_of("l"), "links");
        assert_eq!(listed.table_of("tags"), "tags");

        // An unaliased table binds nothing, and a subquery is not read.
        let bare = QueryPlan::new(
            "SELECT * FROM documents WHERE id IN (SELECT source FROM links)",
            Vec::new(),
        );
        assert_eq!(bare.table_of("documents"), "documents");
        assert_eq!(bare.table_of("links"), "links");
    }

    /// An FTS5 `MATCH` is planned as a virtual-table index scan. It is a
    /// lookup, not a table read.
    #[test]
    fn a_constrained_virtual_table_scan_is_not_unbounded() {
        let row = PlanRow::new(2, 0, "SCAN doc_fts VIRTUAL TABLE INDEX 0:M1");
        assert_eq!(row.scans(), Some("doc_fts"));
        assert!(!row.is_unbounded_scan());
        QueryPlan::new(
            "SELECT rowid FROM doc_fts WHERE doc_fts MATCH ?",
            rows(&["SCAN doc_fts VIRTUAL TABLE INDEX 0:M1"]),
        )
        .assert_no_full_scan();
    }

    /// The same form with an empty specification is the virtual table asked
    /// for everything, and that is unbounded.
    #[test]
    fn an_unconstrained_virtual_table_scan_is_unbounded() {
        let row = PlanRow::new(2, 0, "SCAN doc_fts VIRTUAL TABLE INDEX 0:");
        assert_eq!(row.scans(), Some("doc_fts"));
        assert!(row.is_unbounded_scan());
    }

    /// A table-valued function is a virtual table whose module can pick a
    /// plan by number alone, with no `idxStr` at all. A nonzero `idxNum`
    /// bounds the scan on its own.
    #[test]
    fn a_virtual_table_bound_by_index_number_alone_is_not_unbounded() {
        let row = PlanRow::new(2, 0, "SCAN json_each VIRTUAL TABLE INDEX 1:");
        assert!(!row.is_unbounded_scan());
        QueryPlan::new(
            "SELECT value FROM json_each(?)",
            rows(&["SCAN json_each VIRTUAL TABLE INDEX 1:"]),
        )
        .assert_no_full_scan();
    }

    /// Only the exact pair `0:` — no index number and no specification —
    /// constrains nothing.
    #[test]
    fn a_virtual_table_scan_with_neither_index_number_nor_specification_is_unbounded() {
        let row = PlanRow::new(2, 0, "SCAN documents_fts VIRTUAL TABLE INDEX 0:");
        assert!(row.is_unbounded_scan());
        let failure = std::panic::catch_unwind(|| {
            QueryPlan::new(
                "SELECT rowid FROM documents_fts",
                rows(&["SCAN documents_fts VIRTUAL TABLE INDEX 0:"]),
            )
            .assert_no_full_scan()
        })
        .expect_err("an unconstrained virtual-table scan");
        assert!(
            failure
                .downcast_ref::<String>()
                .expect("a formatted assertion message")
                .contains("reads a relation end to end")
        );
    }

    #[test]
    fn a_values_clause_names_no_table_and_is_not_unbounded() {
        let row = PlanRow::new(2, 0, "SCAN 2-ROW VALUES CLAUSE");
        assert_eq!(row.scans(), None);
        assert_eq!(row.scan_target(), Some(ScanTarget::ValuesClause));
        assert!(!row.is_unbounded_scan());
        assert!(!row.is_table_scan());
        QueryPlan::new(
            "SELECT * FROM (VALUES (1),(2))",
            rows(&["SCAN 2-ROW VALUES CLAUSE"]),
        )
        .assert_no_full_scan();
    }

    /// A co-routine's own steps are separate rows and are judged there, so
    /// counting the scan of the co-routine as well would fail one plan twice.
    #[test]
    fn a_scan_of_a_co_routine_is_judged_through_its_own_steps() {
        let plan = QueryPlan::new(
            "SELECT * FROM (SELECT stem FROM documents GROUP BY stem) x",
            rows(&[
                "CO-ROUTINE x",
                "SEARCH documents USING INDEX documents_stem (stem=?)",
                "SCAN x",
            ]),
        );
        plan.assert_no_full_scan();

        let scanning = QueryPlan::new(
            "SELECT * FROM (SELECT stem FROM documents GROUP BY stem) x",
            rows(&[
                "CO-ROUTINE x",
                "SCAN documents USING COVERING INDEX documents_stem",
                "SCAN x",
            ]),
        );
        assert_eq!(scanning.unbounded_steps().len(), 1);
    }

    #[test]
    fn a_search_names_its_table_and_its_index() {
        let row = PlanRow::new(3, 0, "SEARCH links USING INDEX links_target (target=?)");
        assert_eq!(row.searches(), Some("links"));
        assert_eq!(row.index(), Some("links_target"));
        assert_eq!(row.constraint(), Some("(target=?)"));
        assert!(!row.uses_automatic_index());
    }

    #[test]
    fn a_search_by_rowid_names_no_index() {
        let row = PlanRow::new(3, 0, "SEARCH documents USING INTEGER PRIMARY KEY (rowid=?)");
        assert_eq!(row.searches(), Some("documents"));
        assert_eq!(row.index(), None);
        assert!(!row.uses_automatic_index());
    }

    /// An index SQLite builds for one statement has no name, and saying it
    /// uses none would be a different claim than the truth.
    #[test]
    fn an_automatic_index_is_reported_as_automatic_rather_than_as_none() {
        let row = PlanRow::new(3, 0, "SEARCH b USING AUTOMATIC COVERING INDEX (x=?)");
        assert_eq!(row.searches(), Some("b"));
        assert_eq!(row.index(), None);
        assert_eq!(row.constraint(), Some("(x=?)"));
        assert!(row.uses_automatic_index());
    }

    #[test]
    fn an_indexed_plan_passes_the_bars() {
        let plan = plan(&[
            "SEARCH documents USING INDEX documents_stem (stem=?)",
            "SEARCH links USING INDEX links_source (source=?)",
        ]);
        plan.assert_no_full_scan();
        plan.assert_no_table_scan();
        plan.assert_no_full_scan_of("documents");
        plan.assert_searches("documents");
        plan.assert_uses_index("documents_stem");
        plan.assert_no_temp_btree();
    }

    #[test]
    #[should_panic(expected = "reads `documents` end to end")]
    fn a_table_scan_fails_the_scan_bar() {
        plan(&["SCAN documents"]).assert_no_full_scan_of("documents");
    }

    #[test]
    #[should_panic(expected = "reads a table without an index")]
    fn a_table_scan_fails_the_weaker_bar_too() {
        plan(&["SCAN documents"]).assert_no_table_scan();
    }

    #[test]
    #[should_panic(expected = "does not search `documents`")]
    fn a_plan_that_scans_fails_the_search_bar() {
        plan(&["SCAN documents"]).assert_searches("documents");
    }

    #[test]
    #[should_panic(expected = "does not use index `documents_stem`")]
    fn a_plan_reading_another_index_fails_the_index_bar() {
        plan(&["SEARCH documents USING INDEX documents_path (path=?)"])
            .assert_uses_index("documents_stem");
    }

    /// The index bar cannot tell a point read from a range: both report
    /// `SEARCH … USING INDEX documents_stem`, and only the constraint says
    /// which one ran.
    #[test]
    fn the_constraint_bar_separates_an_equality_seek_from_a_range_over_the_same_index() {
        let equality = plan(&["SEARCH documents USING INDEX documents_stem (stem=?)"]);
        equality.assert_uses_index("documents_stem");
        equality.assert_search_constraint("documents", "(stem=?)");

        let range = plan(&["SEARCH documents USING INDEX documents_stem (stem>?)"]);
        range.assert_uses_index("documents_stem");
        let failure = std::panic::catch_unwind(move || {
            range.assert_search_constraint("documents", "(stem=?)")
        })
        .expect_err("a range is not an equality seek");
        let message = failure
            .downcast_ref::<String>()
            .expect("a formatted assertion message");
        assert!(
            message.contains("does not carry the constraint"),
            "{message}"
        );
    }

    /// The primary key of a `WITHOUT ROWID` table has no index name, so the
    /// constraint is the only thing that says the seek was a point read.
    #[test]
    fn a_primary_key_seek_is_judged_by_its_constraint() {
        plan(&["SEARCH meta USING PRIMARY KEY (key=?)"])
            .assert_search_constraint("meta", "(key=?)");
    }

    /// The constraint is read off the step that searches the named table, so
    /// another table's equality seek in the same plan cannot answer for it.
    #[test]
    #[should_panic(expected = "does not carry the constraint `(path=?)`")]
    fn another_tables_equality_seek_does_not_answer_the_constraint_bar() {
        plan(&[
            "SEARCH documents USING INDEX documents_path (path>?)",
            "SEARCH links USING INDEX links_source (path=?)",
        ])
        .assert_search_constraint("documents", "(path=?)");
    }

    #[test]
    #[should_panic(expected = "does not search `documents`")]
    fn a_plan_that_scans_fails_the_constraint_bar_too() {
        plan(&["SCAN documents"]).assert_search_constraint("documents", "(path=?)");
    }

    #[test]
    #[should_panic(expected = "builds a temporary B-tree")]
    fn a_sort_into_a_temporary_btree_fails_its_bar() {
        plan(&[
            "SEARCH documents USING INDEX documents_stem (stem=?)",
            "USE TEMP B-TREE FOR ORDER BY",
        ])
        .assert_no_temp_btree();
    }

    /// A failing assertion prints the statement, because the statement is
    /// what the assertion is about.
    #[test]
    fn a_failure_names_the_emitted_statement() {
        let plan = plan(&["SCAN documents"]);
        let failure = std::panic::catch_unwind(move || plan.assert_no_full_scan())
            .expect_err("a scanning plan");
        let message = failure
            .downcast_ref::<String>()
            .expect("a formatted assertion message");
        assert!(
            message.contains("SELECT path FROM documents WHERE stem = ?"),
            "{message}"
        );
    }
}
