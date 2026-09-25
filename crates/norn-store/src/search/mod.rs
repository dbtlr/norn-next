//! The search builder's lexical floor: a plain-text query ranked by BM25 over
//! the full-text pillar, on a read snapshot.
//!
//! The builder is inherent methods on [`Snapshot`], as the find builder is, so
//! every statement it runs reads the one instant the snapshot was established
//! at and is counted on the snapshot's own statement counter. **It answers the
//! lexical rung and no other**: the full-text index is maintained inside the
//! write that derives a document, so a lexical answer is transactional with
//! derivation and runs no model. The rungs above it are answered by engines
//! over their own state, and whatever else a request's rung set names is the
//! host's to run and fuse. So the builder reads a [`LexicalQuery`], which names
//! no rung set: the host builds one for the lexical rung, and the floor, the
//! bound and the cursor it carries are that rung's own.
//!
//! # A query is plain text
//!
//! The query is split into terms at whitespace — every character Unicode reads
//! as whitespace — and at NUL. A term counts only where it holds a word, as the
//! index's tokenizer reads words ([`words`] states the rule and pins it to the
//! tokenizer): **a term holding no word is dropped**, and each other term is
//! quoted so that no character of it is full-text syntax. A hit is a document
//! whose body holds every term, and a term holding several words matches them
//! adjacent and in order — [`statement`] states the reading whole. So `AND`,
//! `NEAR`, `body:foo`, a quote, a hyphen or a star in a query are words or
//! separators, never syntax, and every string is a query the engine parses.
//! Matching folds case and diacritics as the tokenizer does, and FTS5 compares
//! a word by its first 32768 bytes, in the index and in a query alike.
//!
//! **A query holding no word answers no hit, and is reported**: an empty one,
//! whitespace, or terms of punctuation alone run no lexical page, and the
//! query is the request's first unsatisfied part
//! ([`Unsatisfied::QueryNamesNoWord`]).
//!
//! A `matches` part keeps full-text match syntax, and narrows a search through
//! the conjunction as it narrows a find.
//!
//! # A hit is scored higher-is-better, and paged by `(score, path)`
//!
//! A hit's score is FTS5's BM25 negated, so it is higher for the more relevant
//! hit, and hits are ordered by score descending, then by path in byte order.
//! A floor admits the hits scored at or above it, on the same scale. A page
//! holds at most its bound of hits and reads one past it to learn a next page
//! exists; the cursor it mints names the last hit's score and path, and a
//! continuation resumes after that position, comparing the score it carries
//! with each match's score exactly as it was computed. On the snapshot that
//! minted the cursor that computation is the one the page ran, so a drain a
//! page at a time is the whole ranking; a continuation answered from another
//! snapshot reports what moved, and resumes after the same `(score, path)` in a
//! ranking the corpus may have moved under.
//!
//! **Ranking costs the matched set.** The full-text index hands back every
//! document matching the query; each is tested against the conjunction, every
//! one it keeps is scored, and every one of those past the page's position is
//! sorted before the page holds its first hit. So a page costs the query's
//! matches rather than its bound, and a continuation scores every match again.
//! A part of the conjunction narrows what is scored, sorted and hydrated, never
//! what is matched. What a page does not cost is the vault: a document the
//! query does not match is never read, and a match's score reads the index and
//! never its body.
//!
//! # A conjunction means what it means to a find
//!
//! The conjunction is compiled by the one compilation every read builder
//! shares, so a part narrows a search exactly as it narrows a find and is
//! reported the same way where it cannot be applied: a part with no meaning
//! empties the page, and a predicate key outside the field universe is
//! reported and filters nothing. **A `resolves` part is not applicable**: it
//! answers which documents a target names, which is a find, so a search
//! reports it and filters nothing by it. A part comparing dates of both offset
//! spellings is advised as a find's is, in [`Searched::advisories`], wherever
//! a lexical page ran.
//!
//! # A hit carries the row its columns name
//!
//! A request naming columns hydrates each hit's document row through the one
//! hydration a find's rows are read through, so a hit costs the columns it
//! names; a request naming none carries no row and hydrates nothing.

mod statement;
mod words;

use norn_db::EmittedPlan;
use norn_wire::{
    AnswerAdvisory, Column, Cursor, CursorKey, Hit, Moved, Page, Predicate, Score, SearchReport,
    Unsatisfied,
};

use crate::error::{self, StoreError};
use crate::fields::ContentModel;
use crate::find::{FindWork, FoundKey, NestedRows, Projection};
use crate::read::{Lookups, PageRefusal, Ran, ReadFilter, ReadStatement, ResolvesPart, page_limit};
use crate::store::Snapshot;

use statement::{HitPosition, LexicalPage, compose_lexical_page, lexical_expression};
pub use statement::{SEARCH_STATEMENTS, SearchStatement};

/// A request for one page of the lexical rung's hits: what [`Snapshot::search`]
/// reads.
///
/// **It names the lexical rung and no other.** A `search` on the wire,
/// [`norn_wire::SearchParams`], names a rung set, and its floor, its bound and
/// its cursor are over the answer that set makes: where the set is the lexical
/// floor alone that answer is this rung's page, and where it names a rung above
/// the floor it is the host's fusion of every rung's hits. So the host builds
/// this request for the lexical rung itself, and **its floor, its cursor and its
/// bound are the lexical rung's own**: a floor on the BM25 scale this rung
/// scores on, a cursor this rung minted, and a bound on this rung's page — never
/// the fused answer's.
#[derive(Clone, Debug, PartialEq)]
pub struct LexicalQuery {
    /// The plain-text query to rank against, as written.
    pub query: String,
    /// The conjunction a hit must also satisfy. Empty filters nothing.
    pub predicates: Vec<Predicate>,
    /// The least lexical score a hit may carry, and `None` for every hit.
    pub min_score: Option<Score>,
    /// The columns each hit's document row carries. Empty hydrates no row.
    pub columns: Vec<Column>,
    /// How many hits the page holds at most, and `None` for
    /// [`crate::DEFAULT_PAGE`].
    pub limit: Option<u32>,
    /// The lexical page this one continues, and `None` for the first page.
    pub after: Option<Cursor>,
}

impl LexicalQuery {
    /// A first page of the lexical hits for `query`, unfloored and unfiltered,
    /// hydrating no document row.
    pub fn new(query: impl Into<String>) -> Self {
        LexicalQuery {
            query: query.into(),
            predicates: Vec::new(),
            min_score: None,
            columns: Vec::new(),
            limit: None,
            after: None,
        }
    }

    /// The request filtered by `predicates`.
    #[must_use]
    pub fn with_predicates(mut self, predicates: impl IntoIterator<Item = Predicate>) -> Self {
        self.predicates = predicates.into_iter().collect();
        self
    }

    /// The request floored at `min_score`.
    #[must_use]
    pub const fn with_min_score(mut self, min_score: Score) -> Self {
        self.min_score = Some(min_score);
        self
    }

    /// The request projecting `columns` onto each hit's document row.
    #[must_use]
    pub fn with_columns(mut self, columns: impl IntoIterator<Item = Column>) -> Self {
        self.columns = columns.into_iter().collect();
        self
    }

    /// The request bounded at `limit` hits.
    #[must_use]
    pub const fn with_limit(mut self, limit: u32) -> Self {
        self.limit = Some(limit);
        self
    }

    /// The request continuing the lexical page `after` names.
    #[must_use]
    pub fn with_after(mut self, after: Cursor) -> Self {
        self.after = Some(after);
        self
    }
}

/// What [`Snapshot::search`] answers: a page of ranked hits, where the next
/// begins, what the request could not apply, and what the parts it applied
/// assumed.
#[derive(Clone, Debug, PartialEq)]
pub struct Searched {
    /// The hits, at most the page bound of them, most relevant first.
    pub hits: Vec<Hit>,
    /// Where the next page begins, and `None` where this page is the last.
    pub next: Option<Cursor>,
    /// What moved between the cursor this page continued and the snapshot it
    /// was answered from. Empty on a first page.
    pub moved: Vec<Moved>,
    /// The parts of the request that could not be applied as asked, in the
    /// order the request names them: a query holding no word, the
    /// conjunction's parts, then the projection's keys.
    pub unsatisfied: Vec<Unsatisfied>,
    /// What the parts that were applied assumed: a mixed-offset comparison,
    /// once per key, in the order the request names the parts.
    pub advisories: Vec<AnswerAdvisory>,
    /// The reading the page was answered from, as a cursor carries it. The
    /// ranking is no schema's order, so it names no fingerprint.
    pub snapshot: norn_wire::Snapshot,
    /// What the search read.
    pub work: SearchWork,
}

impl Searched {
    /// The unsatisfied parts, the advisories and the report a handler wraps
    /// in a [`norn_wire::VaultAnswer`].
    pub fn into_report(self) -> (Vec<Unsatisfied>, Vec<AnswerAdvisory>, SearchReport) {
        (
            self.unsatisfied,
            self.advisories,
            Page::new(self.hits, self.next, self.moved),
        )
    }
}

/// What one search read, by kind.
///
/// **The page counters are the lexical page's cost as SQLite ran it**, read
/// off the page statement's own status once its hits are read. They are
/// counters rather than a plan's words, so a pair of searches over two vaults
/// reads whether a page's work grows with the vault, or with the query's
/// matches, by comparing them. The work of the full-text module inside the
/// statement — walking a term's postings — is the module's own and no counter
/// here sees it; what they see is each match the module hands back.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SearchWork {
    /// The statements the search ran, each counted on the snapshot as it ran.
    pub statements: u64,
    /// The hits the page statement handed back: one past the bound where a
    /// next page exists.
    pub hits_read: u64,
    /// Steps the page statement took through a loop no constraint bounds — a
    /// table or an index read end to end.
    pub page_full_scan_steps: u64,
    /// Sorts the page statement ran: the one a ranking fills with the matches
    /// past the page's position.
    pub page_sorts: u64,
    /// Virtual-machine operations the page statement ran: one pass over each
    /// match the full-text index handed back, whatever it did with it.
    pub page_vm_steps: u64,
    /// The document rows hydrated for the hits.
    pub documents_hydrated: u64,
    /// The nested-table rows hydrated for the hits, by table.
    pub nested_rows: NestedRows,
    /// The finding rows hydrated for the hits.
    pub finding_rows: u64,
}

/// A statement a search ran, with the plan SQLite reported for the text and
/// the values it ran with.
#[derive(Clone, Debug)]
pub struct SearchPlan {
    /// The statement, named by the builder that names it: the lexical page, a
    /// probe the conjunction's compilation ran, or a hydration.
    pub statement: ReadStatement,
    /// The filters the statement narrows by, in the request's order.
    pub filters: Vec<ReadFilter>,
    pub plan: EmittedPlan,
}

/// One hit a lexical page found.
#[derive(Clone, Debug)]
struct RankedKey {
    document: i64,
    path: String,
    score: f64,
}

impl Snapshot {
    /// One page of the lexical rung's hits for `request`, most relevant first,
    /// continuing its cursor.
    ///
    /// `declared` is the vault's declaration, read from the schema the
    /// snapshot pins: it decides how a conjunction's part compares and —
    /// beside the keys documents carry — which predicate and projected keys
    /// are known. The page holds `request.limit` hits,
    /// [`crate::DEFAULT_PAGE`] where it names none.
    ///
    /// Refused as a find is refused, through the same compilation: a page
    /// bound outside `1..=`[`crate::MAX_PAGE`], a membership part naming no
    /// value or more than [`crate::IN_VALUES_CEILING`], a declaration read from
    /// another schema than the snapshot pins, a part or a projected column
    /// this build of the store does not know, and a bound that does not read
    /// as its key's declared type. And refused as a cursor that names no position among
    /// hits ([`PageRefusal::NotAHitCursor`]), or one minted under a schema
    /// fingerprint, which no ranking is ([`PageRefusal::OrderChanged`]).
    pub fn search(
        &self,
        request: &LexicalQuery,
        declared: &ContentModel,
    ) -> Result<Searched, PageRefusal> {
        self.run_search(request, declared, &mut Lookups::default())
    }

    /// Every statement [`Snapshot::search`] runs for `request`, in the order it
    /// runs them, each with the plan SQLite reported for it.
    ///
    /// This is the search itself, run on this snapshot, and each plan is taken
    /// of the very text and values its statement ran with, as
    /// [`Snapshot::find_plans`] takes a find's. A statement the search did not
    /// run is not listed.
    pub fn search_plans(
        &self,
        request: &LexicalQuery,
        declared: &ContentModel,
    ) -> Result<Vec<SearchPlan>, PageRefusal> {
        let mut lookups = Lookups::default();
        self.run_search(request, declared, &mut lookups)?;
        Ok(
            self.explained(lookups.ran, |statement, filters, plan| SearchPlan {
                statement,
                filters,
                plan,
            })?,
        )
    }

    /// The search [`Snapshot::search`] answers and [`Snapshot::search_plans`]
    /// explains, recording every statement it runs in `lookups`.
    fn run_search(
        &self,
        request: &LexicalQuery,
        declared: &ContentModel,
        lookups: &mut Lookups,
    ) -> Result<Searched, PageRefusal> {
        let started = self.counters().statements_executed();
        let limit = page_limit(request.limit)?;
        let projection = Projection::of(&request.columns)?;
        self.declaration_pinned(declared, lookups)?;
        let mut conjunction = self.compile_conjunction(
            &request.predicates,
            ResolvesPart::NotApplicable,
            declared,
            lookups,
        )?;
        let (resume, moved) = match &request.after {
            None => (None, Vec::new()),
            Some(cursor) => {
                let (at, moved) = self.judge_hit(cursor, lookups)?;
                (Some(at), moved)
            }
        };
        let fields =
            self.projected_keys(&projection, declared, lookups, &mut conjunction.reports)?;

        let expression = lexical_expression(&request.query);
        let mut work = SearchWork::default();
        let after = resume.as_ref().map(|(score, path)| HitPosition {
            score: *score,
            path: path.as_str(),
        });
        let (ranked, next) = match expression.as_deref() {
            Some(expression) if !conjunction.matches_nothing => self.page_hits(
                &LexicalPage {
                    expression,
                    after,
                    floor: request.min_score.map(Score::get),
                    filters: &conjunction.filters,
                    rows: limit,
                },
                lookups,
                &mut work,
            )?,
            // No document can be a hit, so no lexical page runs.
            _ => (Vec::new(), None),
        };
        let snapshot = self.reading_facts(None, lookups)?;
        let next = next
            .map(|last| Ok::<_, StoreError>(CursorKey::hit(score_of(last.score)?, last.path)))
            .transpose()?
            .map(|key| Cursor::new(snapshot.clone(), key));
        // A query holding no word runs no lexical page, so its conjunction
        // compared no date.
        let compared = match expression {
            Some(_) => conjunction.date_comparisons([]),
            None => Vec::new(),
        };
        let advisories = self.offset_advisories(&compared, lookups)?;
        let mut unsatisfied = Vec::new();
        if expression.is_none() {
            unsatisfied.push(Unsatisfied::query_names_no_word(&request.query));
        }
        unsatisfied.extend(self.resolve(conjunction.reports, declared, lookups)?);
        let hits = self.hits(
            &ranked,
            &request.columns,
            &projection,
            &fields,
            declared,
            lookups,
            &mut work,
        )?;
        work.statements = self.counters().statements_executed() - started;
        Ok(Searched {
            hits,
            next,
            moved,
            unsatisfied,
            advisories,
            snapshot,
            work,
        })
    }

    /// Judge the cursor a search continues: where it resumes — the score and
    /// the path of the hit it stopped after — and what moved since.
    ///
    /// A ranking is no schema's order, so the cursor's reading is judged as a
    /// raw order's is: one minted under a fingerprint is refused.
    fn judge_hit(
        &self,
        cursor: &Cursor,
        lookups: &mut Lookups,
    ) -> Result<((f64, String), Vec<Moved>), PageRefusal> {
        let CursorKey::Hit { score, path, .. } = cursor.key() else {
            return Err(PageRefusal::NotAHitCursor);
        };
        let moved = self.judge_reading(cursor, None, false, lookups)?;
        Ok(((score.get(), path.clone()), moved))
    }

    /// One page of ranked hits: at most `page.rows` of them, and the hit the
    /// next page continues after.
    ///
    /// The statement reads one hit past the page's bound, to learn a next page
    /// exists. `work` takes what the page statement cost.
    fn page_hits(
        &self,
        page: &LexicalPage<'_>,
        lookups: &mut Lookups,
        work: &mut SearchWork,
    ) -> Result<(Vec<RankedKey>, Option<RankedKey>), StoreError> {
        let shapes: Vec<ReadFilter> = page.filters.iter().map(|filter| filter.shape).collect();
        let page = self.read_page(
            [SearchStatement::LexicalPage],
            page.rows,
            &mut lookups.ran,
            |record, statement, rows| {
                let composed = compose_lexical_page(&LexicalPage { rows, ..*page });
                let section = Ran::new(statement, composed).narrowed_by(shapes.clone());
                self.run_statement(record, section, |row| {
                    Ok(RankedKey {
                        document: row.get(0)?,
                        path: row.get(1)?,
                        score: row.get(2)?,
                    })
                })
                .map_err(|problem| error::sql("reading a page of ranked hits", problem))
            },
        )?;
        work.hits_read = page.read;
        work.page_full_scan_steps = page.stepped.full_scan_steps;
        work.page_sorts = page.stepped.sorts;
        work.page_vm_steps = page.stepped.vm_steps;
        Ok((page.rows, page.next))
    }

    /// The hits `ranked` names, in its order, each carrying its document row
    /// where the request names a column.
    #[allow(clippy::too_many_arguments)] // What a hit's row is hydrated under is named by each of these, and none of them groups with another.
    fn hits(
        &self,
        ranked: &[RankedKey],
        columns: &[Column],
        projection: &Projection<'_>,
        fields: &[&str],
        declared: &ContentModel,
        lookups: &mut Lookups,
        work: &mut SearchWork,
    ) -> Result<Vec<Hit>, StoreError> {
        let mut rows = if columns.is_empty() {
            Vec::new()
        } else {
            let keys: Vec<FoundKey> = ranked
                .iter()
                .map(|key| FoundKey::unsorted(key.document, key.path.clone()))
                .collect();
            let mut hydration = FindWork::default();
            let rows =
                self.hydrate_rows(&keys, projection, fields, declared, lookups, &mut hydration)?;
            work.documents_hydrated = hydration.documents_hydrated;
            work.nested_rows = hydration.nested_rows;
            work.finding_rows = hydration.finding_rows;
            rows
        }
        .into_iter();
        ranked
            .iter()
            .map(|key| {
                let path = norn_wire::DocumentPath::new(&key.path).map_err(|problem| {
                    StoreError::Damaged {
                        what: format!("`documents.path` holds no document path: {problem}"),
                    }
                })?;
                let hit = Hit::new(path, score_of(key.score)?);
                Ok(match rows.next() {
                    Some(row) => hit.with_document(row),
                    None => hit,
                })
            })
            .collect()
    }
}

/// A score a page computed, as the wire carries it. BM25 over a match is a
/// finite number, so one that is not is a store this build did not write.
fn score_of(score: f64) -> Result<Score, StoreError> {
    Score::new(score).map_err(|_| StoreError::Damaged {
        what: format!("a lexical page scored a hit {score}, which is no relevance score"),
    })
}
