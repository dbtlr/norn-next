//! The statement the search builder emits, named, the one composer that spells
//! it with its parameters, and the reading of a plain-text query as the
//! full-text expression it binds.
//!
//! **One function writes the statement's text and binds its parameters.**
//! [`compose_lexical_page`] numbers each parameter as it writes the placeholder
//! for it, through the binder every read builder's composer numbers with, and
//! spells a conjunction's filters through the filter fragments every read
//! builder shares, so a part narrows a search exactly as it narrows a find.

use norn_db::rusqlite::types::Value;

use crate::read::{Binder, Filter};

/// Every statement shape the search builder runs, named.
///
/// The same discipline as [`crate::FindStatement`]: [`SearchStatement::all`]
/// holds each shape once, [`SearchStatement::slot`] is exhaustive over the
/// enum, and [`SEARCH_STATEMENTS`] is the count a census is checked against. A
/// search compiles its conjunction through the compilation every read builder
/// shares and hydrates a hit's document row through the hydration a find's
/// rows are read through, so the probes and the hydration it runs are
/// [`crate::FindStatement`]s and are named there.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SearchStatement {
    /// A page of the lexical rung's hits: the documents whose body the
    /// full-text index matches every term of the query, each scored by BM25,
    /// in score order from the page's position.
    ///
    /// **The full-text index drives it**: `documents_fts` is read through its
    /// `MATCH` selection, which hands back each matching document's row id,
    /// and each match reaches its document by that row id. **Ranking costs the
    /// matched set**: a score is computed for every match, and the matches
    /// past the page's position and at or above the floor are sorted before
    /// the page's first hit is known — one temporary B-tree per page, whether
    /// the page is a first page or a continuation. A filter narrows what is
    /// scored and sorted, and never the matches the index hands back.
    LexicalPage,
}

/// How many statement shapes [`SearchStatement::all`] holds.
pub const SEARCH_STATEMENTS: usize = 1;

impl SearchStatement {
    /// Every statement shape, in slot order.
    pub fn all() -> [Self; SEARCH_STATEMENTS] {
        [Self::LexicalPage]
    }

    /// Where this statement stands in [`Self::all`]. Exhaustive, so a shape
    /// added to the enum has to take a slot.
    pub fn slot(self) -> usize {
        let slot = match self {
            Self::LexicalPage => 0,
        };
        assert!(
            slot < SEARCH_STATEMENTS,
            "slot {slot} is outside the enumeration: grow `all` and `SEARCH_STATEMENTS` with the \
             statement that took it"
        );
        slot
    }
}

/// The full-text expression a plain-text query reads as, or `None` where it
/// names no term.
///
/// **Any string is a query, and none of its characters is syntax.** A term is
/// a run of characters between whitespace or NUL; each is quoted — an embedded
/// `"` doubled — so FTS5 reads it as a string whatever it holds, and the terms
/// are joined by spaces, which FTS5 reads as a conjunction: a document matches
/// where its body holds every term. Inside a string, FTS5's tokenizer reads
/// the term as it reads a body, so a term it splits into several tokens —
/// `foo-bar`, `body:foo` — matches those tokens adjacent and in order, and a
/// term it reads no token in — `--`, `"` — is a phrase of no token, which FTS5
/// drops from the conjunction. NUL separates terms because FTS5 reads a
/// string up to its first NUL and would find it unterminated.
///
/// A query holding no term at all is `None`: FTS5 refuses an empty
/// expression, and a search naming nothing matches nothing.
pub(crate) fn lexical_expression(query: &str) -> Option<String> {
    let terms: Vec<String> = query
        .split(|character: char| character.is_whitespace() || character == '\0')
        .filter(|term| !term.is_empty())
        .map(|term| format!("\"{}\"", term.replace('"', "\"\"")))
        .collect();
    (!terms.is_empty()).then(|| terms.join(" "))
}

/// Where a lexical page resumes: the score and the path of the hit it stopped
/// after.
#[derive(Clone, Copy, Debug)]
pub(crate) struct HitPosition<'a> {
    pub(crate) score: f64,
    pub(crate) path: &'a str,
}

/// What one lexical page reads: the full-text expression, where it resumes,
/// the least score a hit may carry, the filters it narrows by, and how many
/// hits.
pub(crate) struct LexicalPage<'a> {
    pub(crate) expression: &'a str,
    pub(crate) after: Option<HitPosition<'a>>,
    pub(crate) floor: Option<f64>,
    pub(crate) filters: &'a [Filter],
    pub(crate) rows: usize,
}

/// [`SearchStatement::LexicalPage`] and its parameters, in the numbering the
/// text states.
///
/// It selects each hit's document id, its path and its score, `-bm25()` over
/// the match: FTS5's BM25 is lower for the more relevant match, and a score is
/// higher for it. The index is read as `ft`, and FTS5 names the column a
/// `MATCH` and `bm25()` take after its table, so both name it `ft.documents_fts`:
/// the alias is what keeps the page's read of the index apart from a `matches`
/// filter's read of the same table in a plan. The hits are ordered by score descending, then by path in
/// byte order, which makes the order total.
///
/// **Where the page resumes is a bound on `(score, path)`**, compared against
/// the score the statement computes for each match — the same computation over
/// the same index, so the score a cursor carries compares equal to the score
/// its hit is computed at on the snapshot that minted it. An unset position
/// binds positive infinity, which every score is below, and an unset floor
/// negative infinity, which every score is at or above, so the text does not
/// branch on either.
///
/// `CROSS JOIN` keeps the full-text index the outer loop, so every filter is
/// a test of the match's document rather than a seek the planner could put
/// before the index: a filter seeking first would reach the index once per
/// document it kept, and each reach scores its match against the whole index
/// again.
pub(crate) fn compose_lexical_page(page: &LexicalPage<'_>) -> (String, Vec<Value>) {
    let mut binder = Binder::default();
    let expression = binder.bind(Value::Text(page.expression.to_string()));
    let (after_score, after_path) = match page.after {
        None => (f64::INFINITY, String::new()),
        Some(at) => (at.score, at.path.to_string()),
    };
    let after_score = binder.bind(Value::Real(after_score));
    let after_path = binder.bind(Value::Text(after_path));
    let floor = binder.bind(Value::Real(page.floor.unwrap_or(f64::NEG_INFINITY)));
    let filters: String = page
        .filters
        .iter()
        .map(|filter| {
            format!(
                "\n               AND {}",
                filter.spell("d.id", &mut binder)
            )
        })
        .collect();
    let limit = binder.bind(Value::Integer(
        i64::try_from(page.rows).expect("a page's hit count fits i64"),
    ));
    (
        format!(
            "SELECT d.id, d.path, -bm25(ft.documents_fts) AS score
             FROM documents_fts AS ft CROSS JOIN documents AS d ON d.id = ft.rowid
             WHERE ft.documents_fts MATCH {expression}
               AND (score < {after_score} OR (score = {after_score} AND d.path > {after_path}))
               AND score >= {floor}{filters}
             ORDER BY score DESC, d.path
             LIMIT {limit}"
        ),
        binder.into_values(),
    )
}
