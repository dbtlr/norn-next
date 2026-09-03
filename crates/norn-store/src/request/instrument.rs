//! The instrument a work reading is taken through: the plan handout and the
//! step-count accessor.
//!
//! **The instrument explains the same SQL the product readers run.** Both
//! halves call the same `*_sql` and `*_parameters` builders — those stay in
//! [`super`], the product side — so a plan bar here is a plan of the statement
//! a reader actually executes rather than of a copy hand-spelled to match it.
//! What lives in this file is the pairing: which statement [`emitted_plan`] is
//! asked about, the cursor it explains a paged statement with, and the counter
//! [`read_steps`] reads back.
//!
//! [`emitted_plan`]: Request::emitted_plan
//! [`read_steps`]: Request::read_steps
//!
//! Carved out of the request behavior a candidate is certified for so that an
//! ordinary product edit to a reader does not also move the instrument a work
//! bar is read through — the two are certified separately and change for
//! different reasons.

use norn_db::EmittedPlan;
use norn_db::rusqlite::{params, params_from_iter};
use norn_wire::FindingKind;

use crate::ddl;

use super::{
    DOCUMENT_BLOCKS_SQL, DOCUMENT_HEADINGS_SQL, DOCUMENT_LINKS_SQL, DOCUMENT_TAGS_SQL,
    DiscardScope, DocumentPath, FeedCursor, FindingCursor, INDEXED_TERM_PAGE_SQL, MAX_PAGE,
    Request, STORED_TOMBSTONE_SQL, SUFFIX_KEY_PAGE_SQL, StoreError, StoredPathOrder, SubjectScope,
    SuffixProbe, TOMBSTONE_PAGE_SQL, class_discard_sql, document_feed_sql,
    document_page_parameters, document_page_sql, feed_page_parameters, finding_page_parameters,
    finding_page_sql, finding_subject_parameters, finding_subjects_sql, findings_in_class_sql,
    probe_parameters, stored_document_sql, stored_facts_document_sql, stored_findings_sql,
    subject_discard_parameters, subject_discard_sql, suffix_candidates_sql, text_page_parameters,
    tombstone_feed_sql,
};

/// The leaf a page's explained cursor is spelled with, under whatever floor the
/// scope being explained has.
const EXPLAINED_PAGE_CURSOR_LEAF: &str = "explained-page-cursor.md";

/// The row key a findings page is explained from. Any key inside the table's
/// own range does; what matters is that the cursor is bound rather than null.
const EXPLAINED_FINDING_CURSOR: i64 = 1;

/// The term an indexed-term page is explained from.
const EXPLAINED_TERM_CURSOR: &str = "explained-page-cursor";

/// The generation a change-feed page is explained from. Any generation inside
/// the sequence's own range does; what matters is that the cursor is bound
/// rather than null.
const EXPLAINED_FEED_GENERATION: i64 = 1;

/// The document row a fact read is explained from.
///
/// A fact statement is keyed by the row id the snapshot's first statement
/// found, which is an id no caller above this crate holds. Any id inside the
/// table's own range does; what matters is that the key is bound rather than
/// null, for the reason [`explained_page_cursor`] states.
const EXPLAINED_DOCUMENT_ROW: i64 = 1;

impl<'a> Request<'a> {
    /// One named statement as this crate emits it, with the query plan SQLite
    /// reported for **that** statement.
    ///
    /// A plan bar is worth something only against the SQL that actually ran, and
    /// no caller above this crate holds a connection to take a plan of its own.
    /// So the store hands the pair out: this crate names the statement and binds
    /// it as its execution site does, the substrate reports the plan for it, and
    /// the caller judges that. It is deliberately not a way to explain arbitrary
    /// SQL — the statements are named, and each of them carries the parameters
    /// its own execution site binds.
    ///
    /// **An explain is a report about a statement rather than a run of it**, so
    /// what taking one steps is not work [`Request::read_steps`] attributes to
    /// this request's readers.
    pub fn emitted_plan(
        &self,
        statement: ExplainedStatement<'_>,
    ) -> Result<EmittedPlan, StoreError> {
        let sql = match statement {
            ExplainedStatement::SuffixCandidates(probe) => {
                suffix_candidates_sql(probe.range_count())
            }
            ExplainedStatement::FindingsInClass(probe) => {
                findings_in_class_sql(probe.range_count())
            }
            ExplainedStatement::ClassDiscard(probe) => class_discard_sql(probe.range_count()),
            ExplainedStatement::SubjectDiscard(_, scope) => subject_discard_sql(scope),
            ExplainedStatement::FindingSubjectsWithoutRows(scope, kinds, order) => {
                finding_subjects_sql(scope, kinds.len(), order)
            }
            ExplainedStatement::StoredDocumentPage(scope, order) => document_page_sql(scope, order),
            ExplainedStatement::StoredFindingPage => finding_page_sql(),
            ExplainedStatement::StoredTombstonePage => TOMBSTONE_PAGE_SQL.to_string(),
            ExplainedStatement::StoredSuffixKeyPage => SUFFIX_KEY_PAGE_SQL.to_string(),
            ExplainedStatement::IndexedTermPage => INDEXED_TERM_PAGE_SQL.to_string(),
            ExplainedStatement::DocumentFeedPage => document_feed_sql(),
            ExplainedStatement::TombstoneFeedPage => tombstone_feed_sql(),
            ExplainedStatement::StoredDocument(_) => stored_document_sql(),
            ExplainedStatement::StoredFactsDocument(_) => stored_facts_document_sql(),
            ExplainedStatement::DocumentLinks => DOCUMENT_LINKS_SQL.to_string(),
            ExplainedStatement::DocumentHeadings => DOCUMENT_HEADINGS_SQL.to_string(),
            ExplainedStatement::DocumentBlocks => DOCUMENT_BLOCKS_SQL.to_string(),
            ExplainedStatement::DocumentTags => DOCUMENT_TAGS_SQL.to_string(),
            ExplainedStatement::StoredTombstone(_) => STORED_TOMBSTONE_SQL.to_string(),
            ExplainedStatement::StoredFindings(_) => stored_findings_sql(),
            ExplainedStatement::VaultSchemaPin => norn_db::meta::META_READ_SQL.to_string(),
        };
        let connection = self.store.connection();
        Ok(match statement {
            ExplainedStatement::SuffixCandidates(probe)
            | ExplainedStatement::FindingsInClass(probe)
            | ExplainedStatement::ClassDiscard(probe) => {
                norn_db::emitted_plan(connection, &sql, probe_parameters(probe))
            }
            ExplainedStatement::SubjectDiscard(path, scope) => norn_db::emitted_plan(
                connection,
                &sql,
                params_from_iter(subject_discard_parameters(path, scope)),
            ),
            ExplainedStatement::FindingSubjectsWithoutRows(scope, kinds, _) => {
                let cursor = explained_page_cursor(scope);
                norn_db::emitted_plan(
                    connection,
                    &sql,
                    params_from_iter(finding_subject_parameters(
                        scope,
                        kinds,
                        Some(&cursor),
                        MAX_PAGE,
                    )),
                )
            }
            ExplainedStatement::StoredDocumentPage(scope, _) => {
                let cursor = explained_page_cursor(scope);
                norn_db::emitted_plan(
                    connection,
                    &sql,
                    params_from_iter(document_page_parameters(scope, Some(&cursor), MAX_PAGE)),
                )
            }
            // Each of the four enumerations is explained with its cursor
            // bound, for the reason [`explained_page_cursor`] states: the
            // statement text does not branch on the cursor, so the plan is the
            // same either way today, and an edit that ever gave the cursor its
            // own text would otherwise be explained on the first page alone.
            ExplainedStatement::StoredFindingPage => norn_db::emitted_plan(
                connection,
                &sql,
                params_from_iter(finding_page_parameters(
                    Some(FindingCursor(EXPLAINED_FINDING_CURSOR)),
                    MAX_PAGE,
                )),
            ),
            ExplainedStatement::StoredTombstonePage | ExplainedStatement::StoredSuffixKeyPage => {
                norn_db::emitted_plan(
                    connection,
                    &sql,
                    params_from_iter(text_page_parameters(
                        Some(EXPLAINED_PAGE_CURSOR_LEAF),
                        MAX_PAGE,
                    )),
                )
            }
            ExplainedStatement::IndexedTermPage => norn_db::emitted_plan(
                connection,
                &sql,
                params_from_iter(text_page_parameters(Some(EXPLAINED_TERM_CURSOR), MAX_PAGE)),
            ),
            // Both halves of the feed cursor are bound, for the same reason
            // the enumerations bind theirs: the statement text does not
            // branch on the cursor, so the plan is the same in either state
            // today, and a cursor left null would explain the first page
            // alone if an edit ever gave the two states their own text.
            ExplainedStatement::DocumentFeedPage | ExplainedStatement::TombstoneFeedPage => {
                norn_db::emitted_plan(
                    connection,
                    &sql,
                    params_from_iter(feed_page_parameters(
                        Some(&explained_feed_cursor()),
                        MAX_PAGE,
                    )),
                )
            }
            // The path-keyed point reads are explained under the path the
            // caller asked about, which is exactly what their execution sites
            // bind.
            ExplainedStatement::StoredDocument(path)
            | ExplainedStatement::StoredFactsDocument(path)
            | ExplainedStatement::StoredTombstone(path)
            | ExplainedStatement::StoredFindings(path) => {
                norn_db::emitted_plan(connection, &sql, params![path.as_str()])
            }
            // The four fact statements are keyed by a row id rather than a
            // path, and the id is bound for the reason a page's cursor is —
            // see [`EXPLAINED_DOCUMENT_ROW`].
            ExplainedStatement::DocumentLinks
            | ExplainedStatement::DocumentHeadings
            | ExplainedStatement::DocumentBlocks
            | ExplainedStatement::DocumentTags => {
                norn_db::emitted_plan(connection, &sql, params![EXPLAINED_DOCUMENT_ROW])
            }
            // The pin reads three keys through one statement, so the plan is
            // taken under the first of the three.
            ExplainedStatement::VaultSchemaPin => {
                norn_db::emitted_plan(connection, &sql, params![ddl::meta::VAULT_SCHEMA_BYTES])
            }
        }?)
    }

    /// **The work bar's reading.** How many SQLite virtual-machine steps this
    /// request's multi-row reads have cost so far.
    ///
    /// # This is harness evidence, not a derivation counter
    ///
    /// A [`DerivationCounters`](crate::DerivationCounters) reading
    /// answers *what changed in the derived state* — a document upserted, a
    /// finding written — and is a closed vocabulary the product itself is
    /// defined in terms of: a warm request finishes at zero because it changed
    /// nothing, and that is a claim about semantic truth. A step count answers
    /// *what the engine spent reaching an answer*. The two never merge:
    ///
    /// - Reading rows back derives nothing, so a step count in the counter
    ///   vocabulary would move on every warm read and break the zero-on-warm bar
    ///   by construction.
    /// - A step count is engine-version-sensitive. The same statement over the
    ///   same rows steps a different number of times under a different SQLite
    ///   build, so nothing may be *specified* in terms of it.
    /// - What it is good for is an authored-bound grammar: a bar of the shape
    ///   `floor + coefficient × rows` distinguishes a reader that seeks from one
    ///   that scans, whatever the engine's constant factor. That is a harness
    ///   judgment about execution cost, and it lives here for the same reason
    ///   [`Request::emitted_plan`] does — the statements are this crate's, and
    ///   no caller above it holds a connection to measure one.
    ///
    /// # What it counts
    ///
    /// Every statement run through the multi-row reader, which is every paged
    /// reader and every enumeration. A keyed single-row read goes through
    /// SQLite's own one-row convenience and exposes no statement handle, so it
    /// contributes nothing; a bar states its subject as a drain, which is what
    /// the paged readers are. Taking an [`Request::emitted_plan`] contributes
    /// nothing either.
    pub fn read_steps(&self) -> u64 {
        self.read_steps.get()
    }
}

/// Which statement [`Request::emitted_plan`] is asked about, with what that
/// statement is bound to.
///
/// The parameters ride the variant because a plan is taken of a statement as it
/// is executed: the probe readers range over a [`SuffixProbe`], and the
/// increment's subject-axis discard is keyed by one path.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExplainedStatement<'a> {
    /// [`Request::suffix_candidates`].
    SuffixCandidates(&'a SuffixProbe),
    /// [`Request::findings_in_class`].
    FindingsInClass(&'a SuffixProbe),
    /// The class-scoped discard: [`Request::discard_findings_in_class`] runs it
    /// over a probe a caller named, and [`Request::apply_increment`] runs it
    /// over each class its changed paths are in.
    ClassDiscard(&'a SuffixProbe),
    /// The subject-scoped discard: [`Request::apply_increment`] runs it whole
    /// once per changed path, and [`Request::discard_findings_about`] runs it
    /// over the kinds a caller is re-deriving.
    SubjectDiscard(&'a DocumentPath, DiscardScope<'a>),
    /// [`Request::finding_subjects_without_rows_after`], which a walk pages its
    /// scope's unaccounted places through.
    FindingSubjectsWithoutRows(SubjectScope<'a>, &'a [FindingKind], StoredPathOrder),
    /// The ordered document page a heal merges its walk against: the whole
    /// vault through [`Request::stored_documents_after_ordered`], a root and
    /// its descendants through
    /// [`Request::stored_documents_in_subtree_after_ordered`], and a
    /// directory's descendants through
    /// [`Request::stored_documents_under_after_ordered`]. The order is the one
    /// the walk proved for the vault root, because the two sides of a merge
    /// advance together.
    StoredDocumentPage(SubjectScope<'a>, StoredPathOrder),
    /// [`Request::stored_findings_after`], the page a caller drains the whole
    /// findings table through.
    StoredFindingPage,
    /// [`Request::stored_tombstones_after`], the page a caller drains the whole
    /// tombstones pillar through.
    StoredTombstonePage,
    /// [`Request::suffix_keys_after`], the page a caller drains every stored
    /// suffix key through to recompute it against its own path.
    StoredSuffixKeyPage,
    /// [`Request::indexed_terms_after`], the page a caller drains the full-text
    /// index's own vocabulary through.
    IndexedTermPage,
    /// [`Request::changed_documents_after`], the generation-ordered walk a
    /// lane-2 consumer reads live document rows through.
    DocumentFeedPage,
    /// [`Request::changed_tombstones_after`], the generation-ordered walk a
    /// lane-2 consumer reads recorded deaths through.
    TombstoneFeedPage,
    /// [`Request::stored_document`], the one row a path stands at.
    StoredDocument(&'a DocumentPath),
    /// The statement [`Request::stored_facts`] opens its snapshot with: the
    /// document's row, its body and its row id, keyed by the caller's path.
    StoredFactsDocument(&'a DocumentPath),
    /// The link rows [`Request::stored_facts`] reads for the document its first
    /// statement found. The three below are the same read over the other fact
    /// tables, and each is keyed by that document's row id rather than by a
    /// path.
    DocumentLinks,
    /// The heading rows [`Request::stored_facts`] reads.
    DocumentHeadings,
    /// The block-id rows [`Request::stored_facts`] reads.
    DocumentBlocks,
    /// The tag rows [`Request::stored_facts`] reads.
    DocumentTags,
    /// [`Request::stored_tombstone`], the death recorded for one path.
    StoredTombstone(&'a DocumentPath),
    /// [`Request::stored_findings`], every finding recorded about one path.
    StoredFindings(&'a DocumentPath),
    /// The pinned-scalar read [`Request::vault_schema_pin`] runs once per key
    /// it reports. One statement covers all three: the key rides the parameter
    /// rather than the text.
    VaultSchemaPin,
}

/// How many keyed point reads this seam names.
///
/// It is the length of [`ExplainedStatement::point_reads`], and that is the
/// whole of the guarantee: a point read dropped from the census does not
/// compile, rather than quietly narrowing the bar that iterates it.
pub const POINT_READS: usize = 9;

impl<'a> ExplainedStatement<'a> {
    /// Every keyed point read, each bound to one subject.
    ///
    /// **The set is named here rather than beside the bar that judges it**, so
    /// there is one list rather than a list and a copy of it. A bar holding its
    /// own copy greens whatever the copy forgot.
    pub fn point_reads(subject: &'a DocumentPath) -> [Self; POINT_READS] {
        [
            Self::StoredDocument(subject),
            Self::StoredFactsDocument(subject),
            Self::DocumentLinks,
            Self::DocumentHeadings,
            Self::DocumentBlocks,
            Self::DocumentTags,
            Self::StoredTombstone(subject),
            Self::StoredFindings(subject),
            Self::VaultSchemaPin,
        ]
    }

    /// Whether this statement answers about a key its caller already holds,
    /// rather than draining a page or ranging over a probe.
    ///
    /// The `match` is exhaustive, so a variant added to this enum has to say
    /// which of the two it is. A statement that says it is a point read belongs
    /// in [`Self::point_reads`], and the bar over that census checks the answer
    /// both ways: every statement it is handed reads `true` here, and the
    /// census is the length this seam declares.
    pub fn is_point_read(self) -> bool {
        match self {
            Self::StoredDocument(_)
            | Self::StoredFactsDocument(_)
            | Self::DocumentLinks
            | Self::DocumentHeadings
            | Self::DocumentBlocks
            | Self::DocumentTags
            | Self::StoredTombstone(_)
            | Self::StoredFindings(_)
            | Self::VaultSchemaPin => true,
            Self::SuffixCandidates(_)
            | Self::FindingsInClass(_)
            | Self::ClassDiscard(_)
            | Self::SubjectDiscard(..)
            | Self::FindingSubjectsWithoutRows(..)
            | Self::StoredDocumentPage(..)
            | Self::StoredFindingPage
            | Self::StoredTombstonePage
            | Self::StoredSuffixKeyPage
            | Self::IndexedTermPage
            | Self::DocumentFeedPage
            | Self::TombstoneFeedPage => false,
        }
    }
}

/// The cursor [`Request::emitted_plan`] explains a paged statement with.
///
/// Both paged statements are explained with a cursor **bound and inside the
/// scope**, and for one reason each.
///
/// Bound rather than `NULL`, because the statement text does not branch on the
/// cursor and so the plan is the same in both states — but an edit that ever
/// gave the cursor its own text would otherwise be explained on the first page
/// alone, and the bar would quietly narrow to the cheaper of the two pages.
///
/// Inside the scope rather than any path at all, because a cursor below a
/// bounded scope's floor is not a page any caller reads: it is the coalesced
/// floor's other argument that would win there, so explaining one would state
/// the bar over a parameterization paging never produces. A descendant of the
/// scope's own lower bound is a page two of that scope.
fn explained_page_cursor(scope: SubjectScope<'_>) -> DocumentPath {
    let spelling = match scope.bounds() {
        None => format!("explained/{EXPLAINED_PAGE_CURSOR_LEAF}"),
        Some((_, (lower, _))) => format!("{lower}{EXPLAINED_PAGE_CURSOR_LEAF}"),
    };
    DocumentPath::new(&spelling).expect("a scope's explained cursor is a document path")
}

/// The cursor [`Request::emitted_plan`] explains a change-feed page with.
///
/// Both halves bound, because both are arguments of the one row-value floor the
/// plan is judged on: a page explained with either half null would state the bar
/// over a parameterization a resumed drain never produces.
fn explained_feed_cursor() -> FeedCursor {
    FeedCursor {
        generation: EXPLAINED_FEED_GENERATION,
        path: DocumentPath::new(EXPLAINED_PAGE_CURSOR_LEAF)
            .expect("the explained feed cursor is a document path"),
    }
}
