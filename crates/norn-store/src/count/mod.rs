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
//! Every other key, and a tag, groups and orders by its raw text.
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
//! that schema's fingerprint and is refused under any other.
//!
//! # A grouped page is two sections
//!
//! The tallies whose leading member is `null` stand first: a walk of the
//! documents — the matched ones where a filter keeps what it seeks — probing
//! each for a value under the leading key. Then the tallies whose leading
//! member holds a value: a seek of the leading member's value index from the
//! page's position where no filter drives the page, and the matched
//! documents' own leading values where one does. [`CountStatement`] names both
//! and what each reads.
//!
//! **What an unfiltered grouped count costs is linear in the vault's
//! documents.** Its valued section reads the leading key's rows from the page's position on —
//! and, grouped by one key, stops at the page's bound — and its `null` section
//! walks every document, since a document holding no value is found by no seek
//! of the rows that hold one. **A filter that keeps what it seeks drives
//! both**, so a narrowing part narrows the count's cost to the documents it
//! matches.
//!
//! # A conjunction means what it means to a find
//!
//! The conjunction is compiled by the find builder's own compilation, so a
//! part narrows a count exactly as it narrows a find, and the parts that cannot
//! be applied are reported the same way: a part with no meaning empties the
//! match — a grouped count answers no tally, and one grouped by nothing its
//! one tally, of zero, exactly as a filter matching no document does — and a
//! predicate key outside the field universe is reported and filters nothing. **A `resolves` part is not applicable**: it answers which
//! documents a target names, which is a find, so a count reports it
//! ([`Unsatisfied::ResolvesNotApplicable`]) and filters nothing by it.

mod statement;

use norn_db::EmittedPlan;
use norn_wire::{
    CountParams, CountReport, Cursor, CursorKey, GroupKey, Moved, Page, Tally, Unsatisfied,
};

use crate::error::{self, StoreError};
use crate::fields::DeclaredFields;
use crate::find::{
    Conjunction, FieldOrder, FindFilter, FindRefusal, KeyPlace, Lookups, Ran, ReadStatement,
    Report, Resolution, Stepped, page_limit,
};
use crate::store::Snapshot;

pub use statement::{COUNT_STATEMENTS, CountStatement, GroupMember};
use statement::{Member, Tallies, compose_tallies};

/// What [`Snapshot::count`] answers: a page of tallies, where the next begins,
/// and what the request could not apply.
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
    /// The reading the page was answered from, as a cursor carries it: the
    /// schema fingerprint where some member groups under a typed order, and
    /// `None` otherwise.
    pub snapshot: norn_wire::Snapshot,
    /// What the count read.
    pub work: CountWork,
}

impl Counted {
    /// The unsatisfied parts and the report a handler wraps in a
    /// [`norn_wire::VaultAnswer`].
    pub fn into_report(self) -> (Vec<Unsatisfied>, CountReport) {
        (
            self.unsatisfied,
            Page::new(self.tallies, self.next, self.moved),
        )
    }
}

/// What one count read, by kind.
///
/// **The tally counters are the tally statements' cost as SQLite ran them**,
/// read off each statement's own status once its rows are read and summed over
/// the tally statements the count ran — never the probes its conjunction's
/// compilation ran. They are counters rather than a plan's words, so a pair of
/// counts over two vault sizes reads whether a count's work grows with the
/// vault by comparing them.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CountWork {
    /// The statements the count ran, each counted on the snapshot as it ran.
    pub statements: u64,
    /// The tallies the tally statements handed back: one past the bound where
    /// a next page exists.
    pub tallies_read: u64,
    /// Steps the tally statements took through a loop no constraint bounds —
    /// a table or an index read end to end.
    pub full_scan_steps: u64,
    /// Sorts the tally statements ran: a temporary B-tree an order filled
    /// because no index handed its rows back in that order.
    pub sorts: u64,
    /// Virtual-machine operations the tally statements ran: the whole of what
    /// SQLite did for the tallies, whatever it did it on.
    pub vm_steps: u64,
}

impl CountWork {
    /// Add what SQLite counted stepping one tally statement.
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
    /// The statement, named by the builder that names it: a tally statement,
    /// or a probe the conjunction's compilation ran.
    pub statement: ReadStatement,
    /// The filters the statement narrows by, in the request's order.
    pub filters: Vec<FindFilter>,
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
    /// tallies ([`FindRefusal::NotATallyCursor`]), or one minted under
    /// another schema fingerprint than the grouping reads under
    /// ([`FindRefusal::OrderChanged`]).
    pub fn count(
        &self,
        params: &CountParams,
        declared: &DeclaredFields,
    ) -> Result<Counted, FindRefusal> {
        self.run_count(params, declared, &mut Lookups::default())
    }

    /// Every statement [`Snapshot::count`] runs for `params`, in the order it
    /// runs them, each with the plan SQLite reported for it.
    ///
    /// This is the count itself, run on this snapshot, and each plan is taken
    /// of the very text and values its statement ran with, as
    /// [`Snapshot::find_plans`] takes a find's. A statement the count did not
    /// run is not listed.
    pub fn count_plans(
        &self,
        params: &CountParams,
        declared: &DeclaredFields,
    ) -> Result<Vec<CountPlan>, FindRefusal> {
        let mut lookups = Lookups::default();
        self.run_count(params, declared, &mut lookups)?;
        let mut plans = Vec::with_capacity(lookups.ran.len());
        for ran in lookups.ran {
            let (statement, filters) = (ran.statement, ran.filters.clone());
            plans.push(CountPlan {
                statement,
                filters,
                plan: self.explain(ran)?,
            });
        }
        Ok(plans)
    }

    /// The count [`Snapshot::count`] answers and [`Snapshot::count_plans`]
    /// explains, recording every statement it runs in `lookups`.
    fn run_count(
        &self,
        params: &CountParams,
        declared: &DeclaredFields,
        lookups: &mut Lookups,
    ) -> Result<Counted, FindRefusal> {
        let started = self.counters().statements_executed();
        let limit = page_limit(params.limit)?;
        self.declaration_pinned(declared, lookups)?;
        let (members, mut reports) = self.members(&params.by, declared, lookups)?;
        let conjunction = self.compile_conjunction(
            &params.predicates,
            Resolution::NotApplicable,
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

        let mut work = CountWork::default();
        let (tallies, next) = self.page_tallies(
            &members,
            &conjunction,
            limit,
            resume.as_deref(),
            lookups,
            &mut work,
        )?;
        let snapshot = self.reading_facts(order, lookups)?;
        let next = next.map(|last| Cursor::new(snapshot.clone(), last.cursor_key()));
        reports.extend(conjunction.reports);
        let unsatisfied = self.resolve(reports, declared, lookups)?;
        work.statements = self.counters().statements_executed() - started;
        Ok(Counted {
            tallies,
            next,
            moved,
            unsatisfied,
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
        declared: &DeclaredFields,
        lookups: &mut Lookups,
    ) -> Result<(Vec<Option<String>>, Vec<Moved>), FindRefusal> {
        let CursorKey::Tally { group, .. } = cursor.key() else {
            return Err(FindRefusal::NotATallyCursor);
        };
        if group.len() != members.len() {
            return Err(FindRefusal::NotATallyCursor);
        }
        let moved = self.judge_reading(cursor, order, false, lookups)?;
        let at = members
            .iter()
            .zip(group)
            .map(|(member, label)| match label {
                None => Ok(None),
                Some(label) => sort_key(member, label, declared)
                    .map(Some)
                    .ok_or(FindRefusal::NotATallyCursor),
            })
            .collect::<Result<Vec<Option<String>>, FindRefusal>>()?;
        Ok((at, moved))
    }

    /// One page of tallies: at most `limit`, and the tally the next page
    /// continues after.
    fn page_tallies(
        &self,
        members: &[Member<'_>],
        conjunction: &Conjunction,
        limit: usize,
        at: Option<&[Option<String>]>,
        lookups: &mut Lookups,
        work: &mut CountWork,
    ) -> Result<(Vec<Tally>, Option<Tally>), StoreError> {
        let mut tallies: Vec<Tally> = Vec::new();
        if conjunction.matches_nothing {
            // A match a part emptied is still one tally where the count groups
            // by nothing: no document is in it. A grouped count has no group.
            if sections(members, at).contains(&(CountStatement::Total, None)) {
                tallies.push(Tally::new(Vec::new(), 0));
            }
        } else {
            let shapes: Vec<FindFilter> = conjunction
                .filters
                .iter()
                .map(|filter| filter.shape)
                .collect();
            for (statement, after) in sections(members, at) {
                let rows = limit + 1 - tallies.len();
                if rows == 0 {
                    break;
                }
                let composed = compose_tallies(&Tallies {
                    statement,
                    members,
                    after,
                    filters: &conjunction.filters,
                    rows,
                });
                let section = Ran::new(statement, composed).narrowed_by(shapes.clone());
                tallies.extend(self.read_tallies(&mut lookups.ran, section, members.len())?);
                let ran = lookups.ran.last().expect("the section was just recorded");
                work.stepped(ran.stepped);
            }
        }
        work.tallies_read = tallies.len() as u64;
        let next = if tallies.len() > limit {
            tallies.truncate(limit);
            tallies.last().cloned()
        } else {
            None
        };
        Ok((tallies, next))
    }

    /// Run one tally statement over a grouping `width` members wide.
    fn read_tallies(
        &self,
        record: &mut Vec<Ran>,
        section: Ran,
        width: usize,
    ) -> Result<Vec<Tally>, StoreError> {
        // A `null`-lead section selects no leading member: the tuple's first
        // member is `null` on every row it answers.
        let (lead_null, selected) = match section.statement {
            ReadStatement::Count(CountStatement::NullLead(_)) => (true, width - 1),
            ReadStatement::Count(CountStatement::ValuedLead(_)) => (false, width),
            ReadStatement::Count(CountStatement::Total) | ReadStatement::Find(_) => (false, 0),
        };
        self.run_statement(record, section, |row| {
            let mut group: Vec<Option<String>> = Vec::with_capacity(width);
            if lead_null {
                group.push(None);
            }
            for column in 0..selected {
                group.push(row.get(column)?);
            }
            let count: i64 = row.get(selected)?;
            Ok(Tally::new(group, u64::try_from(count).unwrap_or_default()))
        })
        .map_err(|problem| error::sql("reading a page of tallies", problem))
    }

    /// The members `by` groups by, each read under the order the declaration
    /// gives its key, with a field key outside the field universe reported —
    /// its member still groups, and every document's member for it is `null`,
    /// exactly as an unknown predicate key filters nothing and is reported. A
    /// tag member is never unknown.
    fn members<'a>(
        &self,
        by: &'a [GroupKey],
        declared: &DeclaredFields,
        lookups: &mut Lookups,
    ) -> Result<(Vec<Member<'a>>, Vec<Report>), FindRefusal> {
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
                _ => Err(FindRefusal::UnknownPart {
                    part: "a group key",
                }),
            })
            .collect::<Result<Vec<Member<'a>>, FindRefusal>>()?;
        Ok((members, reports))
    }
}

/// The sort key a group labelled `label` stands at under `member`'s order: the
/// label's typed sort key under a typed order, which is `None` where the label
/// does not read as the type, and the label itself otherwise.
fn sort_key(member: &Member<'_>, label: &str, declared: &DeclaredFields) -> Option<String> {
    match (member.shape, member.key) {
        (GroupMember::Field(FieldOrder::Typed), Some(key)) => declared
            .typed_order(key)
            .and_then(|order| order.sort_key(label)),
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
