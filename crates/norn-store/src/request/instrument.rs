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

use std::num::NonZeroUsize;

use norn_db::EmittedPlan;
use norn_db::rusqlite::types::Null;
use norn_db::rusqlite::{params, params_from_iter};
use norn_wire::FindingKind;

use std::collections::BTreeMap;

use crate::ddl;
use crate::facts::CANDIDATE_HEAD;
use crate::health::LINK_HEALTH_CHUNK;
use crate::health::statement::{self as health, Selected};
use crate::path::ClassKey;
use crate::read::SuffixSpellings;
use crate::resolve::AmbiguityIgnore;
use crate::write_path::WriteStatement;

use super::{
    DOCUMENT_BLOCKS_SQL, DOCUMENT_FIELDS_SQL, DOCUMENT_HEADINGS_SQL, DOCUMENT_LINK_KEYS_SQL,
    DOCUMENT_LINKS_SQL, DOCUMENT_TAGS_SQL, DiscardScope, DocumentPath, FINDING_ID_CHUNK,
    FOLDED_VALUE_DISCARD_SQL, FeedCursor, FindingCursor, INDEXED_TERM_PAGE_SQL, MAX_PAGE,
    PATH_DISCARD_SQL, PathKey, Request, RuleSetCursor, STORED_TOMBSTONE_SQL, SUFFIX_KEY_PAGE_SQL,
    StoreError, StoredPathOrder, SubjectScope, SuffixKey, SuffixProbe, TOMBSTONE_PAGE_SQL,
    TYPED_VALUE_DISCARD_SQL, TargetClass, document_feed_sql, document_page_parameters,
    document_page_sql, feed_page_parameters, finding_candidates_sql, finding_classes_sql,
    finding_id_parameters, finding_page_parameters, finding_page_sql, finding_paths_sql,
    finding_rules_sql, finding_subject_parameters, finding_subjects_sql, findings_in_class_sql,
    probe_parameters, rule_set_page_parameters, rule_set_page_sql, stored_document_sql,
    stored_facts_document_sql, stored_findings_sql, subject_discard_parameters,
    subject_discard_sql, suffix_candidates_sql, text_page_parameters, tombstone_feed_sql,
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

/// The first finding id a chunk is explained with. A chunk of `n` ids is
/// explained with the `n` consecutive ids from here: distinct keys inside the
/// table's own range, each bound rather than null, which is what a chunk the
/// readers run holds.
const EXPLAINED_FIRST_FINDING_ID: i64 = 1;

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
        if let ExplainedStatement::FindingCandidates(ids)
        | ExplainedStatement::FindingClasses(ids)
        | ExplainedStatement::FindingPaths(ids)
        | ExplainedStatement::FindingRules(ids) = statement
            && ids.get() > FINDING_ID_CHUNK
        {
            return Err(StoreError::Bound {
                what: "a finding-id chunk",
                limit: FINDING_ID_CHUNK,
                given: ids.get(),
            });
        }
        // A class statement is explained over the key space its reader ranges
        // over, so a class or probe that reader refuses is refused here too.
        match statement {
            ExplainedStatement::SuffixCandidates(resolution) => self.check_class(resolution)?,
            ExplainedStatement::FindingsInClass(probe) => self.check_probe(probe)?,
            ExplainedStatement::PathDiscard(path) => self.check_path_key(path)?,
            _ => {}
        }
        // The judgment's statements are spelled in the key space the store's
        // order selects, as the judgment spells them.
        let order = self.store.path_order();
        let key = SuffixKey::under(order);
        let sql = match statement {
            ExplainedStatement::SuffixCandidates(resolution) => suffix_candidates_sql(resolution),
            ExplainedStatement::FindingsInClass(probe) => {
                findings_in_class_sql(probe.range_count())
            }
            ExplainedStatement::PathDiscard(_) => PATH_DISCARD_SQL.to_string(),
            ExplainedStatement::SubjectDiscard(_, scope) => subject_discard_sql(scope),
            ExplainedStatement::TypedValueDiscard => TYPED_VALUE_DISCARD_SQL.to_string(),
            ExplainedStatement::FoldedValueDiscard => FOLDED_VALUE_DISCARD_SQL.to_string(),
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
            ExplainedStatement::DocumentLinkKeys => DOCUMENT_LINK_KEYS_SQL.to_string(),
            ExplainedStatement::DocumentHeadings => DOCUMENT_HEADINGS_SQL.to_string(),
            ExplainedStatement::DocumentBlocks => DOCUMENT_BLOCKS_SQL.to_string(),
            ExplainedStatement::DocumentTags => DOCUMENT_TAGS_SQL.to_string(),
            ExplainedStatement::DocumentFields => DOCUMENT_FIELDS_SQL.to_string(),
            ExplainedStatement::StoredTombstone(_) => STORED_TOMBSTONE_SQL.to_string(),
            ExplainedStatement::StoredFindings(_) => stored_findings_sql(),
            ExplainedStatement::VaultSchemaPin | ExplainedStatement::WriteGeneration => {
                norn_db::meta::META_READ_SQL.to_string()
            }
            ExplainedStatement::FindingCandidates(ids) => finding_candidates_sql(ids.get()),
            ExplainedStatement::FindingClasses(ids) => finding_classes_sql(ids.get()),
            ExplainedStatement::FindingPaths(ids) => finding_paths_sql(ids.get()),
            ExplainedStatement::FindingRules(ids) => finding_rules_sql(ids.get()),
            ExplainedStatement::StoredRuleSetPage => rule_set_page_sql(),
            ExplainedStatement::LinkHealthLinks => health::links_sql(key, Selected::Documents),
            ExplainedStatement::LinkHealthClassLinks => health::links_sql(key, Selected::Class),
            ExplainedStatement::LinkHealthPathLinks => health::links_sql(key, Selected::Path),
            ExplainedStatement::LinkHealthHeads => health::heads_sql(key),
            ExplainedStatement::LinkHealthTotals => health::totals_sql(key),
            ExplainedStatement::LinkHealthSuffixes => explained_suffixes(order)?.0,
            ExplainedStatement::LinkHealthAnchors => health::anchors_sql(),
            ExplainedStatement::LinkHealthWrittenLinks => health::links_sql(key, Selected::Written),
            ExplainedStatement::LinkHealthClassFindings => health::class_findings_sql(),
            ExplainedStatement::LinkHealthFoundLinks => health::links_sql(key, Selected::Links),
            ExplainedStatement::LinkHealthDiscard => health::discard_sql(),
            ExplainedStatement::LinkHealthOccupied => health::occupied_sql(key),
            ExplainedStatement::Write(write) => write.sql().to_string(),
        };
        let database = &self.store.database;
        Ok(match statement {
            ExplainedStatement::SuffixCandidates(resolution) => {
                database.emitted_plan(&sql, params_from_iter(resolution.parameters()))
            }
            ExplainedStatement::FindingsInClass(probe) => {
                database.emitted_plan(&sql, probe_parameters(probe))
            }
            ExplainedStatement::PathDiscard(path) => {
                database.emitted_plan(&sql, params![path.as_str()])
            }
            ExplainedStatement::SubjectDiscard(path, scope) => database.emitted_plan(
                &sql,
                params_from_iter(subject_discard_parameters(path, scope)),
            ),
            // The pin binds nothing: every typed value is derived under the
            // schema being replaced.
            ExplainedStatement::TypedValueDiscard => database.emitted_plan(&sql, []),
            // Nor does the fold clear: every fold it clears is a declared tag
            // key's, derived under the schema being replaced.
            ExplainedStatement::FoldedValueDiscard => database.emitted_plan(&sql, []),
            ExplainedStatement::FindingSubjectsWithoutRows(scope, kinds, _) => {
                let cursor = explained_page_cursor(scope);
                database.emitted_plan(
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
                database.emitted_plan(
                    &sql,
                    params_from_iter(document_page_parameters(scope, Some(&cursor), MAX_PAGE)),
                )
            }
            // Each of the four enumerations is explained with its cursor
            // bound, for the reason [`explained_page_cursor`] states: the
            // statement text does not branch on the cursor, so the plan is the
            // same either way today, and an edit that ever gave the cursor its
            // own text would otherwise be explained on the first page alone.
            ExplainedStatement::StoredFindingPage => database.emitted_plan(
                &sql,
                params_from_iter(finding_page_parameters(
                    Some(FindingCursor(EXPLAINED_FINDING_CURSOR)),
                    MAX_PAGE,
                )),
            ),
            ExplainedStatement::StoredRuleSetPage => database.emitted_plan(
                &sql,
                params_from_iter(rule_set_page_parameters(
                    Some(RuleSetCursor(EXPLAINED_FINDING_CURSOR)),
                    MAX_PAGE,
                )),
            ),
            ExplainedStatement::StoredTombstonePage | ExplainedStatement::StoredSuffixKeyPage => {
                database.emitted_plan(
                    &sql,
                    params_from_iter(text_page_parameters(
                        Some(EXPLAINED_PAGE_CURSOR_LEAF),
                        MAX_PAGE,
                    )),
                )
            }
            ExplainedStatement::IndexedTermPage => database.emitted_plan(
                &sql,
                params_from_iter(text_page_parameters(Some(EXPLAINED_TERM_CURSOR), MAX_PAGE)),
            ),
            // Both halves of the feed cursor are bound, for the same reason
            // the enumerations bind theirs: the statement text does not
            // branch on the cursor, so the plan is the same in either state
            // today, and a cursor left null would explain the first page
            // alone if an edit ever gave the two states their own text.
            ExplainedStatement::DocumentFeedPage | ExplainedStatement::TombstoneFeedPage => {
                database.emitted_plan(
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
                database.emitted_plan(&sql, params![path.as_str()])
            }
            // The six fact statements are keyed by a row id rather than a
            // path, and the id is bound for the reason a page's cursor is —
            // see [`EXPLAINED_DOCUMENT_ROW`].
            ExplainedStatement::DocumentLinks
            | ExplainedStatement::DocumentLinkKeys
            | ExplainedStatement::DocumentHeadings
            | ExplainedStatement::DocumentBlocks
            | ExplainedStatement::DocumentTags
            | ExplainedStatement::DocumentFields => {
                database.emitted_plan(&sql, params![EXPLAINED_DOCUMENT_ROW])
            }
            // The pin reads three keys through one statement, so the plan is
            // taken under the first of the three.
            ExplainedStatement::VaultSchemaPin => {
                database.emitted_plan(&sql, params![ddl::meta::VAULT_SCHEMA_BYTES])
            }
            ExplainedStatement::WriteGeneration => {
                database.emitted_plan(&sql, params![norn_db::meta::WRITE_GENERATION])
            }
            ExplainedStatement::FindingCandidates(ids)
            | ExplainedStatement::FindingClasses(ids)
            | ExplainedStatement::FindingPaths(ids)
            | ExplainedStatement::FindingRules(ids) => {
                let chunk: Vec<i64> = (EXPLAINED_FIRST_FINDING_ID..).take(ids.get()).collect();
                database.emitted_plan(&sql, finding_id_parameters(&chunk))
            }
            // Each judgment statement is explained over a list of one entry:
            // the statement text does not branch on the list's length, and
            // each entry costs its own seeks.
            ExplainedStatement::LinkHealthLinks => database.emitted_plan(
                &sql,
                params_from_iter(health::documents_parameters(&[EXPLAINED_PAGE_CURSOR_LEAF])?),
            ),
            // A paged selection is explained resuming past a driver row
            // inside its own range, for the reason a page's cursor is bound
            // — see [`explained_page_cursor`] — at the bound a re-decision
            // reads it by.
            ExplainedStatement::LinkHealthClassLinks => {
                let class = ClassKey::new(EXPLAINED_CLASS_KEY)?;
                database.emitted_plan(
                    &sql,
                    params_from_iter(health::class_parameters(
                        &class,
                        &explained_after(class.as_str()),
                        LINK_HEALTH_CHUNK,
                    )),
                )
            }
            ExplainedStatement::LinkHealthPathLinks => {
                let path = PathKey::new(EXPLAINED_PAGE_CURSOR_LEAF)?;
                database.emitted_plan(
                    &sql,
                    params_from_iter(health::path_parameters(
                        &path,
                        &explained_after(path.as_str()),
                        LINK_HEALTH_CHUNK,
                    )),
                )
            }
            ExplainedStatement::LinkHealthWrittenLinks => database.emitted_plan(
                &sql,
                params_from_iter(health::written_parameters(
                    EXPLAINED_FEED_GENERATION,
                    &explained_after(EXPLAINED_PAGE_CURSOR_LEAF),
                    LINK_HEALTH_CHUNK,
                )),
            ),
            ExplainedStatement::LinkHealthHeads => database.emitted_plan(
                &sql,
                params_from_iter(health::heads_parameters(
                    &[(EXPLAINED_CLASS_KEY, 1)],
                    &[EXPLAINED_PAGE_CURSOR_LEAF],
                    CANDIDATE_HEAD,
                )?),
            ),
            ExplainedStatement::LinkHealthTotals => database.emitted_plan(
                &sql,
                params_from_iter(health::totals_parameters(
                    &[(EXPLAINED_CLASS_KEY, 1)],
                    &[EXPLAINED_PAGE_CURSOR_LEAF],
                    &AmbiguityIgnore::none(),
                    order,
                )?),
            ),
            ExplainedStatement::LinkHealthSuffixes => {
                database.emitted_plan(&sql, params_from_iter(explained_suffixes(order)?.1))
            }
            // The link and its target are keyed by row ids no caller above
            // this crate holds, so both are bound as a fact read's id is.
            ExplainedStatement::LinkHealthAnchors => database.emitted_plan(
                &sql,
                params_from_iter(health::anchors_parameters(&[(
                    EXPLAINED_DOCUMENT_ROW,
                    EXPLAINED_DOCUMENT_ROW,
                )])?),
            ),
            // A page of a class's findings resumes past a membership inside
            // the class, for the reason a page's cursor is bound, at the bound
            // a re-decision reads it by.
            ExplainedStatement::LinkHealthClassFindings => {
                let class = ClassKey::new(EXPLAINED_CLASS_KEY)?;
                database.emitted_plan(
                    &sql,
                    params_from_iter(health::class_findings_parameters(
                        &class,
                        &explained_after(class.as_str()),
                        EXPLAINED_FEED_GENERATION,
                        LINK_HEALTH_CHUNK,
                    )),
                )
            }
            // Both lists of row ids are explained over one id, which is bound
            // as a fact read's id is.
            ExplainedStatement::LinkHealthFoundLinks | ExplainedStatement::LinkHealthDiscard => {
                database.emitted_plan(
                    &sql,
                    params_from_iter(health::ids_parameters(&[EXPLAINED_DOCUMENT_ROW])?),
                )
            }
            // Both lists hold one key, so every arm is explained over a key
            // it walks.
            ExplainedStatement::LinkHealthOccupied => {
                let class = ClassKey::new(EXPLAINED_CLASS_KEY)?;
                let path = PathKey::new(EXPLAINED_PAGE_CURSOR_LEAF)?;
                database.emitted_plan(
                    &sql,
                    params_from_iter(health::occupied_parameters(&[&class], &[path.as_str()])?),
                )
            }
            // A write statement is explained, not run: its plan is a report
            // about the statement, so every value it binds is null.
            ExplainedStatement::Write(write) => database.emitted_plan(
                &sql,
                params_from_iter(std::iter::repeat_n(Null, write.parameter_count())),
            ),
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
        self.read_work.steps.get()
    }

    /// How many statements this request's multi-row reads have run, counted
    /// over the same statements [`Request::read_steps`] counts the steps of.
    ///
    /// It is the reading a bar over how a reader splits its work into
    /// statements is taken through, and nothing outside this crate's own tests
    /// reads it.
    #[cfg(test)]
    pub(crate) fn read_statements(&self) -> u64 {
        self.read_work.statements.get()
    }
}

/// Which statement [`Request::emitted_plan`] is asked about, with what that
/// statement is bound to.
///
/// The parameters ride the variant because a plan is taken of a statement as it
/// is executed: the probe readers range over a [`SuffixProbe`], the
/// increment's subject-axis discard is keyed by one path and its path-axis
/// discard by one path key, and a finding-detail read is spelled by how many
/// ids its chunk holds.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExplainedStatement<'a> {
    /// [`Request::suffix_candidates`], over the key the resolution's root
    /// probes.
    SuffixCandidates(&'a TargetClass),
    /// [`Request::findings_in_class`].
    FindingsInClass(&'a SuffixProbe),
    /// The subject-scoped discard: [`Request::apply_increment`] runs it whole
    /// once per path it writes a row to or kills, and narrowed to
    /// [`FindingKind::BodyBytesNotUtf8`] once per [`crate::Change::Vacated`]
    /// place; [`Request::discard_findings_about`] runs it over the kinds a
    /// caller is re-deriving.
    SubjectDiscard(&'a DocumentPath, DiscardScope<'a>),
    /// The clear [`Request::pin_vault_schema`] runs over the field pillar's
    /// typed values, in the pin's transaction.
    TypedValueDiscard,
    /// The clear [`Request::pin_vault_schema`] runs over the folds of the keys
    /// a schema declares `tags`, in the pin's transaction.
    FoldedValueDiscard,
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
    /// statement found. The five below are the same read over the link keys
    /// and the other fact tables, and each is keyed by that document's row id
    /// rather than by a path.
    DocumentLinks,
    /// The link keys [`Request::stored_facts`] reads: the document's link rows
    /// by the row id, each link's keys by the link.
    DocumentLinkKeys,
    /// The heading rows [`Request::stored_facts`] reads.
    DocumentHeadings,
    /// The block-id rows [`Request::stored_facts`] reads.
    DocumentBlocks,
    /// The tag rows [`Request::stored_facts`] reads.
    DocumentTags,
    /// The field rows [`Request::stored_facts`] reads.
    DocumentFields,
    /// [`Request::stored_tombstone`], the death recorded for one path.
    StoredTombstone(&'a DocumentPath),
    /// [`Request::stored_findings`], every finding recorded about one path.
    StoredFindings(&'a DocumentPath),
    /// The pinned-scalar read [`Request::vault_schema_pin`] runs once per key
    /// it reports. One statement covers all three: the key rides the parameter
    /// rather than the text.
    VaultSchemaPin,
    /// The pinned-scalar read [`Request::write_generation`] runs: the store's
    /// last committed write generation, which a lane-2 consumer reads before a
    /// feed page and records beside the completion that page reaches. The
    /// statement text is the pin's; the key it binds is its own.
    WriteGeneration,
    /// The candidate heads of a chunk of this many findings, which
    /// [`Request::stored_findings`], [`Request::findings_in_class`] and
    /// [`Request::stored_findings_after`] read once per chunk of the findings
    /// their first statement found. A chunk holds from one id to
    /// [`FINDING_ID_CHUNK`]: the width is nonzero by its type, and a width
    /// above the chunk bound is refused as [`StoreError::Bound`].
    ///
    /// An empty chunk is not a statement any reader runs, so it has no
    /// spelling here at all:
    ///
    /// ```compile_fail,E0308
    /// norn_store::ExplainedStatement::FindingCandidates(0);
    /// ```
    FindingCandidates(NonZeroUsize),
    /// The class memberships of a chunk of this many findings, read beside
    /// [`ExplainedStatement::FindingCandidates`] by the same three readers,
    /// and spelled by a nonzero width for the same reason.
    FindingClasses(NonZeroUsize),
    /// The path-keyed discard [`Request::apply_increment`] runs once per path
    /// its changeset writes or kills, over the findings keyed by that exact
    /// path. A path key outside the store's key space is refused as
    /// [`StoreError::KeySpace`], as the discard's own filing refuses one.
    PathDiscard(&'a PathKey),
    /// The path keys of a chunk of this many findings, read beside
    /// [`ExplainedStatement::FindingCandidates`] by the same three readers,
    /// and spelled by a nonzero width for the same reason.
    FindingPaths(NonZeroUsize),
    /// The links a link-health judgment judges
    /// ([`Request::judge_selected_links`]) selected by the documents holding
    /// them: every link a list of documents holds, beside the keys the link
    /// index holds each under.
    LinkHealthLinks,
    /// The links a judgment judges selected by a class: a page of the links
    /// held under a key in one class's range, beside every key each is held
    /// under — the second arm of the re-decision
    /// [`Request::apply_increment`] runs.
    LinkHealthClassLinks,
    /// The links a judgment judges selected by a path key: a page of the
    /// links held under exactly that key, beside every key each is held under
    /// — the re-decision's third arm.
    LinkHealthPathLinks,
    /// The head of what each distinct key a judgment's links hold names, at
    /// most [`crate::CANDIDATE_HEAD`] documents of a suffix key's class or at
    /// a path key's path, in the resolution ladder's order.
    LinkHealthHeads,
    /// How many documents each key a judgment reads names, for the keys whose
    /// head filled.
    LinkHealthTotals,
    /// The minimal disambiguating suffixes of the candidates a judgment's
    /// findings carry: the statement a read names its candidates by, run on
    /// the writer.
    LinkHealthSuffixes,
    /// Whether the one document each link naming one document names holds the
    /// place that link's anchor names, for the links carrying an anchor: the
    /// anchor predicate over a list of links and their targets.
    LinkHealthAnchors,
    /// The links the re-decision [`Request::apply_increment`] runs judges
    /// first: a page of the links held by the documents its changeset wrote,
    /// the documents stamped with its generation, beside every key each is
    /// held under.
    LinkHealthWrittenLinks,
    /// The findings standing under one class the re-decision's second arm
    /// discards first: a page of the class's memberships the changeset did
    /// not file, each beside its finding's row id and the row id of the link
    /// the finding is about.
    LinkHealthClassFindings,
    /// The links a page of discarded findings was about, read by their row
    /// ids beside every key each is held under, so the second arm judges the
    /// ones no key of the class reaches.
    LinkHealthFoundLinks,
    /// The discard of a page of findings by their row ids, which the second
    /// arm runs over each page of a class's findings.
    LinkHealthDiscard,
    /// Which of a chunk of the classes and path keys a changeset changed the
    /// re-decision's passes could read anything under: the one statement a
    /// key no link and no finding is held under costs a share of, and no
    /// pass beside it.
    LinkHealthOccupied,
    /// A statement the write path runs, named in [`crate::WriteStatement`]: the
    /// increment's prepared statements, the findings writes, the generation and
    /// pinned-scalar writes and the schema pin's discard of stale findings.
    /// One variant carries the registry rather than one variant per statement,
    /// so the registry's own census is what says every statement has a bar.
    Write(WriteStatement),
    /// [`Request::stored_rule_sets_after`], the page a caller drains every
    /// rule set the store holds through.
    StoredRuleSetPage,
    /// The rules a chunk of this many findings cite, read through the rule
    /// set each finding's row names beside
    /// [`ExplainedStatement::FindingCandidates`] by the same three readers,
    /// and spelled by a nonzero width for the same reason.
    FindingRules(NonZeroUsize),
}

/// How many keyed point reads this seam names.
///
/// It is the length of [`ExplainedStatement::point_reads`], and that is the
/// whole of the guarantee: a point read dropped from the census does not
/// compile, rather than quietly narrowing the bar that iterates it.
pub const POINT_READS: usize = 12;

/// How many statements this seam names in total.
///
/// It is the length of [`ExplainedStatement::all`], which is the enumeration
/// every other census is checked against.
pub const STATEMENTS: usize = 44;

impl<'a> ExplainedStatement<'a> {
    /// Every statement this seam names, in slot order, each bound to a subject
    /// a caller supplies.
    ///
    /// A list of statements is not self-checking: an entry dropped from a list
    /// and a count shrunk beside it compiles, and a bar that iterates that list
    /// then judges one statement fewer without saying so. This is the one
    /// enumeration the narrower censuses are read out of, and
    /// [`Self::slot`] — exhaustive over this enum — is what binds a variant to
    /// its place in it.
    ///
    /// The parameters are the ones a statement cannot be spelled without, and
    /// `ids` is the width of a finding-id chunk. The write registry holds one slot
    /// here, bound to its first statement: its own census is
    /// [`WriteStatement::all`]. The scope, the order and the
    /// discard scope are not parameters: a statement's place in this
    /// enumeration does not depend on which of them it carries, and the bars
    /// that care about those axes range over them themselves.
    pub fn all(
        subject: &'a DocumentPath,
        resolution: &'a TargetClass,
        probe: &'a SuffixProbe,
        path_key: &'a PathKey,
        kinds: &'a [FindingKind],
        ids: NonZeroUsize,
    ) -> [Self; STATEMENTS] {
        [
            Self::SuffixCandidates(resolution),
            Self::FindingsInClass(probe),
            Self::SubjectDiscard(subject, DiscardScope::EveryKind),
            Self::TypedValueDiscard,
            Self::FindingSubjectsWithoutRows(
                SubjectScope::Vault,
                kinds,
                StoredPathOrder::Sensitive,
            ),
            Self::StoredDocumentPage(SubjectScope::Vault, StoredPathOrder::Sensitive),
            Self::StoredFindingPage,
            Self::StoredTombstonePage,
            Self::StoredSuffixKeyPage,
            Self::IndexedTermPage,
            Self::DocumentFeedPage,
            Self::TombstoneFeedPage,
            Self::StoredDocument(subject),
            Self::StoredFactsDocument(subject),
            Self::DocumentLinks,
            Self::DocumentLinkKeys,
            Self::DocumentHeadings,
            Self::DocumentBlocks,
            Self::DocumentTags,
            Self::DocumentFields,
            Self::StoredTombstone(subject),
            Self::StoredFindings(subject),
            Self::VaultSchemaPin,
            Self::WriteGeneration,
            Self::FindingCandidates(ids),
            Self::FindingClasses(ids),
            Self::PathDiscard(path_key),
            Self::FindingPaths(ids),
            Self::LinkHealthLinks,
            Self::LinkHealthClassLinks,
            Self::LinkHealthPathLinks,
            Self::LinkHealthHeads,
            Self::LinkHealthTotals,
            Self::LinkHealthSuffixes,
            Self::LinkHealthAnchors,
            Self::LinkHealthWrittenLinks,
            Self::LinkHealthClassFindings,
            Self::LinkHealthFoundLinks,
            Self::LinkHealthDiscard,
            Self::LinkHealthOccupied,
            Self::Write(WriteStatement::UpsertDocument),
            Self::StoredRuleSetPage,
            Self::FindingRules(ids),
            Self::FoldedValueDiscard,
        ]
    }

    /// Where this statement stands in [`Self::all`].
    ///
    /// The `match` is exhaustive, so a variant added to this enum has to take a
    /// slot, and a slot outside the enumeration is refused here rather than
    /// left to a bar to notice. The suite walks the two together: every member
    /// of [`Self::all`] claims the slot it occupies, which is what says the
    /// enumeration holds each variant once and in the order it declares.
    pub fn slot(self) -> usize {
        let slot = match self {
            Self::SuffixCandidates(_) => 0,
            Self::FindingsInClass(_) => 1,
            Self::SubjectDiscard(..) => 2,
            Self::TypedValueDiscard => 3,
            Self::FindingSubjectsWithoutRows(..) => 4,
            Self::StoredDocumentPage(..) => 5,
            Self::StoredFindingPage => 6,
            Self::StoredTombstonePage => 7,
            Self::StoredSuffixKeyPage => 8,
            Self::IndexedTermPage => 9,
            Self::DocumentFeedPage => 10,
            Self::TombstoneFeedPage => 11,
            Self::StoredDocument(_) => 12,
            Self::StoredFactsDocument(_) => 13,
            Self::DocumentLinks => 14,
            Self::DocumentLinkKeys => 15,
            Self::DocumentHeadings => 16,
            Self::DocumentBlocks => 17,
            Self::DocumentTags => 18,
            Self::DocumentFields => 19,
            Self::StoredTombstone(_) => 20,
            Self::StoredFindings(_) => 21,
            Self::VaultSchemaPin => 22,
            Self::WriteGeneration => 23,
            Self::FindingCandidates(_) => 24,
            Self::FindingClasses(_) => 25,
            Self::PathDiscard(_) => 26,
            Self::FindingPaths(_) => 27,
            Self::LinkHealthLinks => 28,
            Self::LinkHealthClassLinks => 29,
            Self::LinkHealthPathLinks => 30,
            Self::LinkHealthHeads => 31,
            Self::LinkHealthTotals => 32,
            Self::LinkHealthSuffixes => 33,
            Self::LinkHealthAnchors => 34,
            Self::LinkHealthWrittenLinks => 35,
            Self::LinkHealthClassFindings => 36,
            Self::LinkHealthFoundLinks => 37,
            Self::LinkHealthDiscard => 38,
            Self::LinkHealthOccupied => 39,
            Self::Write(_) => 40,
            Self::StoredRuleSetPage => 41,
            Self::FindingRules(_) => 42,
            Self::FoldedValueDiscard => 43,
        };
        assert!(
            slot < STATEMENTS,
            "slot {slot} is outside the enumeration: grow `all` and `STATEMENTS` with the \
             statement that took it"
        );
        slot
    }

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
            Self::DocumentLinkKeys,
            Self::DocumentHeadings,
            Self::DocumentBlocks,
            Self::DocumentTags,
            Self::DocumentFields,
            Self::StoredTombstone(subject),
            Self::StoredFindings(subject),
            Self::VaultSchemaPin,
            Self::WriteGeneration,
        ]
    }

    /// Whether this statement answers about a key its caller already holds,
    /// rather than draining a page, ranging over a probe or answering about a
    /// chunk of keys at once.
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
            | Self::DocumentLinkKeys
            | Self::DocumentHeadings
            | Self::DocumentBlocks
            | Self::DocumentTags
            | Self::DocumentFields
            | Self::StoredTombstone(_)
            | Self::StoredFindings(_)
            | Self::VaultSchemaPin
            | Self::WriteGeneration => true,
            Self::SuffixCandidates(_)
            | Self::FindingsInClass(_)
            | Self::SubjectDiscard(..)
            | Self::TypedValueDiscard
            | Self::FoldedValueDiscard
            | Self::FindingSubjectsWithoutRows(..)
            | Self::StoredDocumentPage(..)
            | Self::StoredFindingPage
            | Self::StoredTombstonePage
            | Self::StoredSuffixKeyPage
            | Self::IndexedTermPage
            | Self::DocumentFeedPage
            | Self::TombstoneFeedPage
            | Self::FindingCandidates(_)
            | Self::FindingClasses(_)
            | Self::PathDiscard(_)
            | Self::FindingPaths(_)
            | Self::LinkHealthLinks
            | Self::LinkHealthClassLinks
            | Self::LinkHealthPathLinks
            | Self::LinkHealthHeads
            | Self::LinkHealthTotals
            | Self::LinkHealthSuffixes
            | Self::LinkHealthAnchors
            | Self::LinkHealthWrittenLinks
            | Self::LinkHealthClassFindings
            | Self::LinkHealthFoundLinks
            | Self::LinkHealthDiscard
            | Self::LinkHealthOccupied
            | Self::Write(_)
            | Self::StoredRuleSetPage
            | Self::FindingRules(_) => false,
        }
    }
}

/// The suffix key a judgment's key statements are explained with: a class
/// key, bound rather than null, for the reason [`explained_page_cursor`]
/// states.
const EXPLAINED_CLASS_KEY: &str = "explained/";

/// The driver row a paged link selection is explained resuming past: `text`,
/// with the row keys bound inside their table's range.
fn explained_after(text: &str) -> health::After {
    health::After {
        text: text.to_string(),
        first: EXPLAINED_DOCUMENT_ROW,
        second: EXPLAINED_DOCUMENT_ROW,
    }
}

/// The candidate-suffix statement a judgment runs, as its text and values,
/// over one candidate: the statement a read names candidates by, spelled
/// through the same [`SuffixSpellings`].
fn explained_suffixes(
    order: StoredPathOrder,
) -> Result<(String, Vec<norn_db::rusqlite::types::Value>), StoreError> {
    let named = BTreeMap::from([(EXPLAINED_DOCUMENT_ROW, EXPLAINED_PAGE_CURSOR_LEAF)]);
    SuffixSpellings::new(&named, &AmbiguityIgnore::none(), order)?.statement()
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
