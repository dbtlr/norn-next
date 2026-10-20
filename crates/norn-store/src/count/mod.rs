//! The count builder: a request's conjunction and grouping compiled into
//! statements a read snapshot runs, answering how many documents match in one
//! tally per group.
//!
//! The builder is inherent methods on [`Snapshot`], as the find builder is, so
//! every statement it runs reads the one instant the snapshot was established
//! at and is counted on the snapshot's own statement counter. **A count
//! hydrates no document row**: it answers tallies, and a request that wants
//! the documents is a find.
//!
//! # What a group is
//!
//! A tally's group is one member per key the request groups by, in the
//! request's order. **A key holding a set groups a document once per
//! element**: a field whose value is a sequence counts the document in the
//! group of each distinct scalar it holds, and a tag grouping counts it under
//! each distinct tag. Grouping by several keys groups by their cross product.
//! So **a group counts documents, and one document may stand in several
//! groups**: the tallies of a page can sum past the documents the conjunction
//! matches.
//!
//! **`null` is a document holding no scalar value under the key**: the key
//! missing, a map, an empty sequence, a sequence of nulls, and — under a key
//! the declaration gives a typed order — no value that reads as the declared
//! type. A document holding any value under the key is in that value's group
//! and not in `null`'s.
//!
//! **A key with a typed order groups by typed equality.** Where the
//! declaration gives the key a typed order, a group is the values sharing one
//! typed sort key, ordered by it: `9` and `9.0` under a number are one group.
//! Every other key groups and orders by its raw text.
//!
//! **A tag groups by its fold** (`norn_wire::fold_tag`): `#Work` and `#work`
//! are one group, ordered by the folded name. **A tag member's label is the
//! spelling the tally's own documents write first in the answer order** —
//! the document path with its ASCII case folded, then the path's bytes as the
//! tie-break, then the tag's position in the file — so two tallies, or two
//! conjunctions, may label one tag with two spellings, and a continuation
//! names a tag's group by any spelling of it.
//!
//! **A typed member's label is the byte-least raw spelling among the tally's
//! own documents**: the documents the conjunction matched that stand in that
//! tally, and no others. So one typed value may carry different labels in two
//! tallies of one grouping — `(09, draft)` beside `(9, idea)` — and under two
//! conjunctions. Every label reads back as its group's typed sort key, so
//! paging, and a tally's agreement with a find of its labels, hold tally by
//! tally.
//!
//! **Tallies are ordered by the tuple, ascending, with `null` first in each
//! member.** A page holds at most the request's bound of them; the cursor the
//! next page continues is the last tally's labels, and a continuation reads a
//! typed member's position back as its label's typed sort key — which is the
//! group's, because the typed key is a function of the raw value under the
//! schema the snapshot pins, and a cursor minted in a typed grouping names
//! that schema's fingerprint. A source continuation refuses a different
//! fingerprint; a retained continuation keeps its original order.
//!
//! # Source aggregation and retained results
//!
//! Tallies with a null leading member come from a document walk, probing for
//! absence under that key. Valued tallies use the leading value index or the
//! matched documents. The shared adaptive page drive bounds single-key valued
//! windows and chooses a keeping filter at doubling budgets. A null section
//! costs the smallest keeping filter's seek, or a document walk without one.
//!
//! Multi-key aggregation cannot stream arbitrary tuple order from a per-key
//! index. It runs once and streams complete tallies into an anonymous SQLite
//! result relation with a normalized tuple primary key. Subsequent pages seek
//! that relation; neither its source snapshot nor a demand lease is retained.
//! Native memory stays bounded while result bytes spill to disk. Active result
//! count and absolute lifetime are bounded by [`CountRetentionPolicy`]; disk
//! quotas are optional. Fresh first pages fall back to the source page path
//! on capacity, quota or coordination contention. Continuations refuse those
//! conditions explicitly rather than silently recomputing an expired result.
//!
//! A retained continuation keeps the original counts, typed normalizers,
//! reports and advisories across document and schema changes. Its source is
//! [`Counted::snapshot`] (`counted_at` on the wire); the host still acquires a
//! current trusted reading for each request. Changed grouping or predicates
//! detach the retained identity and use ordinary tuple-position judgment.
//!
//! # A conjunction means what it means to a find
//!
//! The conjunction is compiled by the one compilation every read builder
//! shares, so a part narrows a count exactly as it narrows a find, and the
//! parts that cannot be applied are reported the same way: a part with no
//! meaning empties the match — a grouped count answers no tally, and one
//! grouped by nothing its one tally, of zero, exactly as a filter matching no
//! document does — and a predicate key outside the field universe is such a
//! part, reported with the keys near it. **A `resolves` part is not
//! applicable**: it answers which documents a target names, which is a find,
//! so a count reports it ([`Unsatisfied::ResolvesNotApplicable`]) and filters
//! nothing by it.
//!
//! **A count is advised of a mixed-offset comparison as a find is**, in
//! [`Counted::advisories`]: a grouping by a key with a dated order compares
//! its dates — it makes one group of the dates reading as one instant and
//! orders the groups — and is advised as a grouping where the key holds both
//! offset spellings, before the conjunction's parts are advised.

mod results;
mod statement;
pub(crate) use results::CountResults;
pub use results::{COUNT_RESULT_STATEMENTS, CountResultStatement, CountRetentionPolicy};
use results::{CountOrigin, CountSource, CountTime, RetainPage, TallyPage};

use norn_db::EmittedPlan;
use norn_wire::{
    AnswerAdvisory, CountParams, CountReport, Cursor, CursorKey, GroupKey, Moved, Page, PagedRows,
    RequestPart, Tally, Unsatisfied, fold_tag,
};

use crate::error::{self, StoreError};
use crate::fields::ContentModel;
use crate::read::{
    Compared, Conjunction, DateComparison, Driver, FieldOrder, Filter, KeyPlace, Lookups,
    PageRefusal, Paged, Ran, ReadFilter, ReadStatement, Report, ResolvesPart, Settled, Stepped,
    page_limit,
};
use crate::store::Snapshot;

pub use statement::{COUNT_STATEMENTS, CountStatement, GroupMember};
use statement::{Member, Tallies, compose_tallies};

/// What [`Snapshot::count`] answers: a page of tallies, where the next begins,
/// what the request could not apply, and what the parts it applied assumed.
#[derive(Clone, Debug, PartialEq)]
pub struct Counted {
    /// The tallies, at most the page bound of them, in tuple order.
    pub tallies: Vec<Tally>,
    /// Where the next page begins, and `None` where this page is the last.
    pub next: Option<Cursor>,
    /// What moved between the cursor this page continued and the snapshot it
    /// was answered from. Empty on a first page.
    pub moved: Vec<Moved>,
    /// The parts of the request that could not be applied as asked, in the
    /// order the request names them: the grouping's keys, then the
    /// conjunction's parts.
    pub unsatisfied: Vec<Unsatisfied>,
    /// What the parts that were applied assumed: a mixed-offset comparison,
    /// once per key and place, the grouping before the predicates.
    pub advisories: Vec<AnswerAdvisory>,
    /// The reading the page was answered from, as a cursor carries it: the
    /// schema fingerprint where some member groups under a typed order, and
    /// `None` otherwise.
    pub snapshot: norn_wire::Snapshot,
    /// What the count read.
    pub work: CountWork,
}

impl Counted {
    /// The unsatisfied parts, the advisories and the report a handler wraps
    /// in a [`norn_wire::VaultAnswer`].
    pub fn into_report(self) -> (Vec<Unsatisfied>, Vec<AnswerAdvisory>, CountReport) {
        (
            self.unsatisfied,
            self.advisories,
            CountReport::new(
                Page::new(self.tallies, self.next, self.moved),
                self.snapshot,
            ),
        )
    }
}

/// What one count read, by kind.
///
/// **The tally counters are the count's cost as SQLite ran it**,
/// read off each statement's own status once its rows are read and summed over
/// the tally statements, walk windows, driver probes, result setup, inserts
/// and result pages the count ran — never
/// the probes its conjunction's compilation ran. They are counters rather
/// than a plan's words, so a pair of counts over two vault sizes reads whether
/// a count's work grows with the vault by comparing them.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CountWork {
    /// Source and disposable-result statement executions. The snapshot
    /// counter names source statements only.
    pub statements: u64,
    /// Tallies streamed during population plus rows read for this page,
    /// including one past the bound where a next page exists.
    pub tallies_read: u64,
    /// Steps the page drive took through a loop no constraint bounds —
    /// a table or an index read end to end.
    pub full_scan_steps: u64,
    /// Sorts the page drive ran: a temporary B-tree an order filled
    /// because no index handed its rows back in that order.
    pub sorts: u64,
    /// Virtual-machine operations the page drive ran: the whole of what
    /// SQLite did for the tallies, whatever it did it on.
    pub vm_steps: u64,
}

impl CountWork {
    /// The whole reading, name by name, every count present: the shape a
    /// harness compares, for the reason
    /// [`FindWork::readings`](crate::FindWork::readings) gives.
    pub fn readings(&self) -> impl Iterator<Item = (&'static str, u64)> + '_ {
        [
            ("count_statements", self.statements),
            ("count_tallies_read", self.tallies_read),
            ("count_full_scan_steps", self.full_scan_steps),
            ("count_sorts", self.sorts),
            ("count_vm_steps", self.vm_steps),
        ]
        .into_iter()
    }

    /// Add what SQLite counted stepping the page drive.
    fn stepped(&mut self, stepped: Stepped) {
        self.full_scan_steps += stepped.full_scan_steps;
        self.sorts += stepped.sorts;
        self.vm_steps += stepped.vm_steps;
    }
}

/// A statement a count ran, with the plan SQLite reported for the text and the
/// values it ran with.
#[derive(Clone, Debug)]
pub struct CountPlan {
    /// Executions of this prepared SQL; bulk insertion keeps one bounded record.
    pub executions: u64,
    /// The statement, named by the builder that names it: a tally statement,
    /// or a probe the conjunction's compilation ran.
    pub statement: ReadStatement,
    /// The filters the statement narrows by, in the request's order.
    pub filters: Vec<ReadFilter>,
    pub plan: EmittedPlan,
}

impl Snapshot {
    /// One page of the tallies `params` asks for, in tuple order, continuing
    /// its cursor.
    ///
    /// `declared` is the vault's declaration, read from the schema the
    /// snapshot pins: it decides which grouped keys group by typed equality,
    /// how a conjunction's part compares, and — beside the keys documents
    /// carry — which predicate keys are known. The page holds `params.limit`
    /// tallies, [`crate::DEFAULT_PAGE`] where it names none. A request that
    /// groups by nothing answers one tally over the whole match.
    ///
    /// Refused as a find is refused, through the same compilation: a page
    /// bound outside `1..=`[`crate::MAX_PAGE`], a membership part naming no
    /// value or more than [`crate::IN_VALUES_CEILING`], a declaration read
    /// from another schema than the snapshot pins, a part the store keeps no
    /// index of, and a bound that does not read as its key's declared type.
    /// And refused as a cursor that names no position among the request's
    /// tallies ([`PageRefusal::CursorNotTaken`]), or one minted under
    /// another schema fingerprint than the grouping reads under
    /// ([`PageRefusal::OrderChanged`]).
    pub fn count(
        &self,
        params: &CountParams,
        declared: &ContentModel,
    ) -> Result<Counted, PageRefusal> {
        self.count_with_time(params, declared, std::time::Instant::now())
    }

    /// Negative control: repeat the source aggregation on every page.
    #[cfg(feature = "induced-failure")]
    #[doc(hidden)]
    pub fn count_without_retention(
        &self,
        params: &CountParams,
        declared: &ContentModel,
    ) -> Result<Counted, PageRefusal> {
        let mut lookups = Lookups::default();
        lookups.count_source_only = true;
        self.run_count(
            params,
            declared,
            &mut lookups,
            CountTime::new(std::time::Instant::now()),
        )
    }

    /// Count using an explicit monotonic request time for result retention.
    /// Elapsed work within the call advances this time normally.
    pub fn count_with_time(
        &self,
        params: &CountParams,
        declared: &ContentModel,
        now: std::time::Instant,
    ) -> Result<Counted, PageRefusal> {
        self.run_count(
            params,
            declared,
            &mut Lookups::default(),
            CountTime::new(now),
        )
    }

    /// Every statement [`Snapshot::count`] runs for `params`, in the order it
    /// runs them, each with the plan SQLite reported for it. Repeated result
    /// inserts share one record with their execution count and first bindings;
    /// the recorder never retains a record per output group.
    ///
    /// This is the count itself, run on this snapshot, and each plan is taken
    /// of the very text and values its statement ran with, as
    /// [`Snapshot::find_plans`] takes a find's. A statement the count did not
    /// run is not listed.
    pub fn count_plans(
        &self,
        params: &CountParams,
        declared: &ContentModel,
    ) -> Result<Vec<CountPlan>, PageRefusal> {
        let mut lookups = Lookups::default();
        lookups.capture_results = true;
        self.run_count(
            params,
            declared,
            &mut lookups,
            CountTime::new(std::time::Instant::now()),
        )?;
        lookups
            .ran
            .into_iter()
            .map(|ran| {
                let plan = self.explain_run(&ran)?;
                Ok(CountPlan {
                    executions: ran.executions,
                    statement: ran.statement,
                    filters: ran.filters,
                    plan,
                })
            })
            .collect()
    }

    /// The count [`Snapshot::count`] answers and [`Snapshot::count_plans`]
    /// explains, recording every statement it runs in `lookups`.
    fn run_count(
        &self,
        params: &CountParams,
        declared: &ContentModel,
        lookups: &mut Lookups,
        time: CountTime,
    ) -> Result<Counted, PageRefusal> {
        let started = self.counters().statements_executed();
        let limit = page_limit(params.limit)?;
        let detached;
        let params = if params
            .after
            .as_ref()
            .is_some_and(|cursor| cursor.retained_count_id().is_some())
        {
            if let Some(mut answer) = self.resume_retained_count(params, limit, lookups, time)? {
                answer.work.statements += self.counters().statements_executed() - started;
                return Ok(answer);
            }
            detached = {
                let mut copy = params.clone();
                copy.after = copy.after.as_ref().map(Cursor::without_retained_count);
                copy
            };
            &detached
        } else {
            params
        };
        self.declaration_pinned(declared, lookups)?;
        let (members, mut reports) = self.members(&params.by, declared, lookups)?;
        let mut conjunction = self.compile_conjunction(
            &params.predicates,
            ResolvesPart::NotApplicable,
            declared,
            lookups,
        )?;
        let order = members
            .iter()
            .any(|member| member.shape == GroupMember::Field(FieldOrder::Typed))
            .then_some(FieldOrder::Typed);
        let (resume, moved) = match &params.after {
            None => (None, Vec::new()),
            Some(cursor) => {
                let (at, moved) = self.judge_tally(cursor, &members, order, declared, lookups)?;
                (Some(at), moved)
            }
        };

        let snapshot = self.reading_facts(order, lookups)?;
        let grouped_dates = members
            .iter()
            .filter_map(|member| match (member.shape, member.key) {
                (GroupMember::Field(_), Some(key)) => {
                    DateComparison::of_dated(key, Compared::Group, declared)
                }
                _ => None,
            });
        let advisories =
            self.offset_advisories(&conjunction.date_comparisons(grouped_dates), lookups)?;
        reports.extend(std::mem::take(&mut conjunction.reports));
        let unsatisfied = self.resolve(reports, declared, lookups)?;
        let origin = CountOrigin {
            snapshot: snapshot.clone(),
            unsatisfied: unsatisfied.clone(),
            advisories: advisories.clone(),
        };
        let mut work = CountWork::default();
        let retain = members.len() > 1;
        #[cfg(feature = "induced-failure")]
        let retain = retain && !lookups.count_source_only;
        let retained = if retain {
            self.retained_tallies(
                RetainPage {
                    params,
                    origin,
                    limit,
                    after: resume.as_deref(),
                    time,
                },
                CountSource {
                    members: &members,
                    conjunction: &conjunction,
                    declared,
                },
                lookups,
                &mut work,
            )
        } else {
            self.page_tallies(
                &members,
                &conjunction,
                limit,
                resume.as_deref(),
                lookups,
                &mut work,
            )
            .map(|(rows, next)| TallyPage {
                rows,
                next,
                result_id: None,
            })
            .map_err(PageRefusal::from)
        };
        let TallyPage {
            rows: tallies,
            next,
            result_id,
        } = match retained {
            Ok(page) => page,
            Err(
                PageRefusal::CountResultCapacity { .. }
                | PageRefusal::CountResultTooLarge { .. }
                | PageRefusal::CountResultBusy
                | PageRefusal::CountResultStorageFull
                | PageRefusal::CountCursorExpired
                | PageRefusal::CountResultFailed { .. },
            ) if params.after.is_none() => {
                let (rows, next) =
                    self.page_tallies(&members, &conjunction, limit, None, lookups, &mut work)?;
                TallyPage {
                    rows,
                    next,
                    result_id: None,
                }
            }
            Err(problem) => return Err(problem),
        };
        let next = next.map(|last| match &result_id {
            Some(id) => Cursor::retained_count(
                snapshot.clone(),
                last.group,
                id,
                results::selection_digest(params),
            ),
            None => Cursor::new(snapshot.clone(), last.cursor_key()),
        });
        work.statements += self.counters().statements_executed() - started;
        Ok(Counted {
            tallies,
            next,
            moved,
            unsatisfied,
            advisories,
            snapshot,
            work,
        })
    }

    /// Judge the cursor a count continues: where it resumes — each member's
    /// sort key, `None` for `null` — and what moved since.
    ///
    /// The cursor's reading is judged as a find's is, under the grouping's
    /// order: typed where some member groups under a typed order. Its tuple is
    /// read back member by member, a typed member's label as its typed sort
    /// key, and a tuple of another width, or a label that is no place in its
    /// member's order, names no position here.
    fn judge_tally(
        &self,
        cursor: &Cursor,
        members: &[Member<'_>],
        order: Option<FieldOrder>,
        declared: &ContentModel,
        lookups: &mut Lookups,
    ) -> Result<(Vec<Option<String>>, Vec<Moved>), PageRefusal> {
        let not_taken = || PageRefusal::cursor_not_taken(cursor, PagedRows::Tally);
        let CursorKey::Tally { group, .. } = cursor.key() else {
            return Err(not_taken());
        };
        if group.len() != members.len() {
            return Err(not_taken());
        }
        let moved = self.judge_reading(cursor, order, false, lookups)?;
        let at = members
            .iter()
            .zip(group)
            .map(|(member, label)| match label {
                None => Ok(None),
                Some(label) => sort_key(member, label, declared)
                    .map(Some)
                    .ok_or_else(not_taken),
            })
            .collect::<Result<Vec<Option<String>>, PageRefusal>>()?;
        Ok((at, moved))
    }

    /// One page of tallies: at most `limit`, and the tally the next page
    /// continues after.
    ///
    /// `work` takes the tallies read — one past the bound where a next page
    /// exists — and what SQLite counted stepping the page drive, windows and
    /// probes included.
    fn page_tallies(
        &self,
        members: &[Member<'_>],
        conjunction: &Conjunction,
        limit: usize,
        at: Option<&[Option<String>]>,
        lookups: &mut Lookups,
        work: &mut CountWork,
    ) -> Result<(Vec<Tally>, Option<Tally>), StoreError> {
        let sections = sections(members, at);
        if conjunction.matches_nothing {
            // A match a part emptied is still one tally where the count groups
            // by nothing: no document is in it, and no statement is run to say
            // so. A grouped count has no group.
            let tallies = if sections.contains(&(CountStatement::Total, None)) {
                vec![Tally::new(Vec::new(), 0)]
            } else {
                Vec::new()
            };
            return Ok((tallies, None));
        }
        let paged = CountPages {
            members,
            filters: &conjunction.filters,
        };
        let sections: Vec<_> = sections
            .into_iter()
            .map(|(statement, after)| CountSection {
                statement,
                after: after.map(<[Option<String>]>::to_vec),
                past: None,
            })
            .collect();
        let page = if members.is_empty() {
            self.read_page(
                sections,
                limit,
                &mut lookups.ran,
                |record, section, rows| {
                    paged.read(self, record, &section, Driver::Walked, None, rows)
                },
            )?
        } else {
            let parts: Vec<&Filter> = conjunction.filters.iter().collect();
            self.drive_page(
                &paged,
                &parts,
                None,
                limit + 1,
                sections,
                limit + 1,
                Settled::Unasked,
                lookups,
            )?
            .into_page(limit)
        };
        work.tallies_read += page.read;
        work.stepped(page.stepped);
        Ok((page.rows, page.next))
    }

    /// Run one tally statement over a grouping `width` members wide.
    fn read_tallies(
        &self,
        record: &mut Vec<Ran>,
        section: Ran,
        members: &[Member<'_>],
    ) -> Result<Vec<Tally>, StoreError> {
        let width = members.len();
        // A `null`-lead section selects no leading member: the tuple's first
        // member is `null` on every row it answers.
        let (lead_null, selected) = match section.statement {
            ReadStatement::Count(CountStatement::NullLead(_)) => (true, width - 1),
            ReadStatement::Count(CountStatement::ValuedLead(_)) => (false, width),
            ReadStatement::Count(CountStatement::Total)
            | ReadStatement::Count(CountStatement::ValuedWindow(_))
            | ReadStatement::Find(_)
            | ReadStatement::Validate(_)
            | ReadStatement::Describe(_)
            | ReadStatement::Search(_)
            | ReadStatement::Get(_)
            | ReadStatement::Resolution(_)
            | ReadStatement::HeldLinks(_)
            | ReadStatement::StemRarity
            | ReadStatement::StemPostings
            | ReadStatement::CandidateDocument
            | ReadStatement::Moved(_)
            | ReadStatement::HeldFrontmatter(_)
            | ReadStatement::RepairAdvisory(_)
            | ReadStatement::CountResult(_) => (false, 0),
        };
        self.run_statement(record, section, |row| {
            let mut group: Vec<Option<String>> = Vec::with_capacity(width);
            if lead_null {
                group.push(None);
            }
            // The selected columns are the members after the ones the
            // section leaves `null`.
            for (column, member) in members[width - selected..].iter().enumerate() {
                group.push(member.shape.read_label(row.get(column)?));
            }
            let count: i64 = row.get(selected)?;
            Ok(Tally::new(group, u64::try_from(count).unwrap_or_default()))
        })
        .map_err(|problem| error::sql("reading a page of tallies", problem))
    }

    /// The members `by` groups by, each read under the order the declaration
    /// gives its key, with a field key outside the field universe reported —
    /// its member still groups, and every document's member for it is `null`.
    /// A tag member is never unknown.
    fn members<'a>(
        &self,
        by: &'a [GroupKey],
        declared: &ContentModel,
        lookups: &mut Lookups,
    ) -> Result<(Vec<Member<'a>>, Vec<Report>), PageRefusal> {
        let mut reports = Vec::new();
        let members = by
            .iter()
            .map(|key| match key {
                GroupKey::Field { key, .. } => {
                    if !self.is_known(key, declared, lookups)? {
                        reports.push(Report::Unknown(KeyPlace::Group, key.clone()));
                    }
                    Ok(Member {
                        shape: GroupMember::Field(match declared.typed_order(key) {
                            Some(_) => FieldOrder::Typed,
                            None => FieldOrder::Raw,
                        }),
                        key: Some(key),
                    })
                }
                GroupKey::Tag { .. } => Ok(Member {
                    shape: GroupMember::Tag,
                    key: None,
                }),
                _ => Err(PageRefusal::UnknownPart {
                    part: RequestPart::unknown("a group key"),
                }),
            })
            .collect::<Result<Vec<Member<'a>>, PageRefusal>>()?;
        Ok((members, reports))
    }
}

/// The sort key a group labelled `label` stands at under `member`'s order: the
/// label's typed sort key under a typed order, which is `None` where the label
/// does not read as the type; a tag label's fold, so every spelling of a tag
/// names its one group; and the label itself otherwise.
fn sort_key(member: &Member<'_>, label: &str, declared: &ContentModel) -> Option<String> {
    match (member.shape, member.key) {
        (GroupMember::Field(FieldOrder::Typed), Some(key)) => declared
            .typed_order(key)
            .and_then(|order| order.sort_key(label)),
        (GroupMember::Tag, _) => Some(fold_tag(label)),
        _ => Some(label.to_string()),
    }
}

/// The sections a page reads from `at` on, in order, each with the position
/// it resumes after.
///
/// A count grouped by nothing is one tally, which a continuation has already
/// passed. A grouped count reads its `null`-lead section, then its valued
/// section. A position whose leading member is `null` stands in the `null`
/// section, resuming at its trailing members — where there are none, the
/// section's one tally is the position itself, and the section is passed. A
/// position whose leading member holds a value stands in the valued section.
fn sections<'a>(
    members: &[Member<'_>],
    at: Option<&'a [Option<String>]>,
) -> Vec<(CountStatement, Option<&'a [Option<String>]>)> {
    let Some(lead) = members.first() else {
        return match at {
            None => vec![(CountStatement::Total, None)],
            Some(_) => Vec::new(),
        };
    };
    let null = CountStatement::NullLead(lead.shape);
    let valued = CountStatement::ValuedLead(lead.shape);
    match at {
        None => vec![(null, None), (valued, None)],
        Some(at) if at.first().is_some_and(Option::is_none) => {
            let mut sections = Vec::new();
            if members.len() > 1 {
                sections.push((null, Some(&at[1..])));
            }
            sections.push((valued, None));
            sections
        }
        Some(at) => vec![(valued, Some(at))],
    }
}

/// A tally section's tuple position and the leading group a round passed.
#[derive(Clone)]
struct CountSection {
    statement: CountStatement,
    after: Option<Vec<Option<String>>>,
    past: Option<String>,
}

/// A count's rows are complete tallies in tuple order, not documents in path
/// order. Its walk budgets the leading index's value rows, before any part
/// tests them: a window ends at one leading sort key, reads every row at it,
/// and resumes past that whole group, so no tally or label is split between
/// rounds. Trailing members stay within the leading group's window. A null
/// section has no leading value to seek: it asks for a small keeping part
/// to drive its document probes, even where the valued section walks.
struct CountPages<'a> {
    members: &'a [Member<'a>],
    filters: &'a [Filter],
}

impl Paged for CountPages<'_> {
    type Section = CountSection;
    type Row = Tally;
    type Bound = String;

    fn read(
        &self,
        snapshot: &Snapshot,
        record: &mut Vec<Ran>,
        section: &CountSection,
        driver: Driver,
        until: Option<&String>,
        rows: usize,
    ) -> Result<Vec<Tally>, StoreError> {
        let driver = if matches!(section.statement, CountStatement::NullLead(_))
            && driver == Driver::Walked
        {
            let parts: Vec<&Filter> = self.filters.iter().collect();
            snapshot.matched_driver(&parts, rows, record)?
        } else {
            driver
        };
        let composed = compose_tallies(&Tallies {
            statement: section.statement,
            members: self.members,
            after: section.after.as_deref(),
            filters: self.filters,
            driver,
            past: section.past.as_deref(),
            until: until.map(String::as_str),
            rows: Some(rows),
            offset: 0,
        });
        let read = Ran::new(section.statement, composed)
            .narrowed_by(self.filters.iter().map(Filter::shape).collect());
        snapshot.read_tallies(record, read, self.members)
    }

    fn window(
        &self,
        snapshot: &Snapshot,
        record: &mut Vec<Ran>,
        section: &CountSection,
        offset: usize,
    ) -> Result<Option<String>, StoreError> {
        let CountStatement::ValuedLead(member) = section.statement else {
            return Ok(None);
        };
        let statement = CountStatement::ValuedWindow(member);
        let composed = compose_tallies(&Tallies {
            statement,
            members: self.members,
            after: section.after.as_deref(),
            filters: &[],
            driver: Driver::Walked,
            past: section.past.as_deref(),
            until: None,
            rows: Some(1),
            offset,
        });
        snapshot
            .run_statement(record, Ran::new(statement, composed), |row| row.get(0))
            .map(|rows| rows.into_iter().next())
            .map_err(|problem| error::sql("reading a tally walk's window", problem))
    }

    fn resumed(&self, section: &CountSection, bound: &String) -> CountSection {
        CountSection {
            past: Some(bound.clone()),
            ..section.clone()
        }
    }
}
