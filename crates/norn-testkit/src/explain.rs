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
//! Nothing here opens a database. The rows are handed in by whoever ran the
//! `EXPLAIN`, which is the store's own API.

use std::fmt;

/// One row of `EXPLAIN QUERY PLAN`, in SQLite's shape.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlanRow {
    pub id: i64,
    pub parent: i64,
    pub detail: String,
}

impl PlanRow {
    pub fn new(id: i64, parent: i64, detail: impl Into<String>) -> Self {
        PlanRow {
            id,
            parent,
            detail: detail.into(),
        }
    }

    /// The table this row scans, if it scans one.
    ///
    /// A scan reads every row it is given. SQLite spells the older form
    /// `SCAN TABLE t` and the current one `SCAN t`, and an alias follows as
    /// `AS x`; both spellings and either form of alias name the same table.
    pub fn scans(&self) -> Option<&str> {
        target(&self.detail, "SCAN ")
    }

    /// The table this row searches, if it searches one. A search uses an
    /// index or a rowid to reach the rows it wants.
    pub fn searches(&self) -> Option<&str> {
        target(&self.detail, "SEARCH ")
    }

    /// The index this row reads through, if it names one.
    pub fn index(&self) -> Option<&str> {
        let (_, rest) = self.detail.split_once(" USING ")?;
        let rest = rest.strip_prefix("COVERING ").unwrap_or(rest);
        let rest = rest.strip_prefix("INDEX ")?;
        Some(rest.split_whitespace().next().unwrap_or(rest))
    }

    /// Whether this row reads a whole table without an index.
    pub fn is_full_scan(&self) -> bool {
        self.scans().is_some() && self.index().is_none()
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
}

impl QueryPlan {
    /// Pair an emitted statement with the plan SQLite reported for it.
    pub fn new(sql: impl Into<String>, rows: Vec<PlanRow>) -> Self {
        QueryPlan {
            sql: sql.into(),
            rows,
        }
    }

    /// The statement the assertions are about.
    pub fn sql(&self) -> &str {
        &self.sql
    }

    pub fn rows(&self) -> &[PlanRow] {
        &self.rows
    }

    /// Every row that reads a whole table without an index.
    pub fn full_scans(&self) -> Vec<&PlanRow> {
        self.rows.iter().filter(|row| row.is_full_scan()).collect()
    }

    /// **The scan bar.** No step of this plan reads `table` end to end.
    pub fn assert_no_full_scan_of(&self, table: &str) {
        let scans: Vec<&PlanRow> = self
            .full_scans()
            .into_iter()
            .filter(|row| row.scans() == Some(table))
            .collect();
        assert!(
            scans.is_empty(),
            "the plan scans `{table}` end to end: {}\nemitted SQL: {}",
            rows_display(&scans),
            self.sql
        );
    }

    /// No step of this plan reads any table end to end.
    pub fn assert_no_full_scan(&self) {
        let scans = self.full_scans();
        assert!(
            scans.is_empty(),
            "the plan scans a table end to end: {}\nemitted SQL: {}",
            rows_display(&scans),
            self.sql
        );
    }

    /// **The index bar.** The plan reaches `table` by a search rather than a
    /// scan, which is the difference an index makes.
    pub fn assert_searches(&self, table: &str) {
        assert!(
            self.rows.iter().any(|row| row.searches() == Some(table)),
            "the plan does not search `{table}`: {}\nemitted SQL: {}",
            rows_display(&self.rows.iter().collect::<Vec<_>>()),
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

fn target<'a>(detail: &'a str, verb: &str) -> Option<&'a str> {
    let rest = detail.strip_prefix(verb)?;
    let rest = rest.strip_prefix("TABLE ").unwrap_or(rest);
    let name = rest.split_whitespace().next()?;
    Some(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Canned rows in the shapes SQLite reports them.
    fn plan(details: &[&str]) -> QueryPlan {
        QueryPlan::new(
            "SELECT path FROM documents WHERE stem = ?",
            details
                .iter()
                .enumerate()
                .map(|(i, detail)| PlanRow::new(i as i64 + 2, 0, *detail))
                .collect(),
        )
    }

    #[test]
    fn a_plan_carries_the_statement_it_was_taken_of() {
        let plan = plan(&["SEARCH documents USING INDEX documents_stem (stem=?)"]);
        assert_eq!(plan.sql(), "SELECT path FROM documents WHERE stem = ?");
        assert_eq!(plan.rows().len(), 1);
    }

    #[test]
    fn a_scan_is_read_from_either_spelling_and_past_an_alias() {
        assert_eq!(
            PlanRow::new(2, 0, "SCAN documents").scans(),
            Some("documents")
        );
        assert_eq!(
            PlanRow::new(2, 0, "SCAN TABLE documents").scans(),
            Some("documents")
        );
        assert_eq!(
            PlanRow::new(2, 0, "SCAN documents AS d").scans(),
            Some("documents")
        );
        assert_eq!(PlanRow::new(2, 0, "SEARCH documents").scans(), None);
    }

    #[test]
    fn a_scan_through_an_index_is_not_a_full_scan() {
        let row = PlanRow::new(2, 0, "SCAN documents USING COVERING INDEX documents_stem");
        assert_eq!(row.index(), Some("documents_stem"));
        assert!(!row.is_full_scan());
        assert!(PlanRow::new(2, 0, "SCAN documents").is_full_scan());
    }

    #[test]
    fn a_search_names_its_table_and_its_index() {
        let row = PlanRow::new(3, 0, "SEARCH links USING INDEX links_target (target=?)");
        assert_eq!(row.searches(), Some("links"));
        assert_eq!(row.index(), Some("links_target"));
    }

    #[test]
    fn a_search_by_rowid_names_no_index() {
        let row = PlanRow::new(3, 0, "SEARCH documents USING INTEGER PRIMARY KEY (rowid=?)");
        assert_eq!(row.searches(), Some("documents"));
        assert_eq!(row.index(), None);
    }

    #[test]
    fn an_indexed_plan_passes_the_bars() {
        let plan = plan(&[
            "SEARCH documents USING INDEX documents_stem (stem=?)",
            "SEARCH links USING INDEX links_source (source=?)",
        ]);
        plan.assert_no_full_scan();
        plan.assert_no_full_scan_of("documents");
        plan.assert_searches("documents");
        plan.assert_uses_index("documents_stem");
        plan.assert_no_temp_btree();
    }

    #[test]
    #[should_panic(expected = "scans `documents` end to end")]
    fn a_table_scan_fails_the_scan_bar() {
        plan(&["SCAN documents"]).assert_no_full_scan_of("documents");
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
