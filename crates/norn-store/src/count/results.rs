//! Immutable, admitted tuple relations. Source aggregation stays in the count composer.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::time::{Duration, Instant};

use norn_db::TemporaryDatabase;
use norn_db::rusqlite::{self, Row, params_from_iter, types::Value};
use norn_wire::{
    AnswerAdvisory, CountParams, Cursor, CursorKey, Moved, PagedRows, Snapshot as Reading, Tally,
    Unsatisfied, fold_tag,
};

use super::{CountStatement, CountWork, Counted, Member, Tallies, compose_tallies};
use crate::fields::{ContentModel, TypedOrder};
use crate::read::{
    Conjunction, FieldOrder, KeysetPage, Lookups, PageRefusal, Ran, Stepped, StreamFailure,
};
use crate::store::Snapshot;

/// Retention limits for a store's immutable grouped results. Source query
/// sorter spill files are outside this retained-table quota.
#[derive(Clone, Copy, Debug)]
pub struct CountRetentionPolicy {
    pub lifetime: Duration,
    pub max_results: usize,
    pub max_result_bytes: Option<u64>,
    pub total_bytes: Option<u64>,
}

impl Default for CountRetentionPolicy {
    fn default() -> Self {
        Self {
            lifetime: Duration::from_secs(600),
            max_results: 32,
            max_result_bytes: None,
            total_bytes: None,
        }
    }
}

/// Statements executed on disposable result storage, never on lane-1.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CountResultStatement {
    Configure,
    Create,
    Begin,
    Insert,
    Commit,
    Page,
    Size,
    Identity,
}
pub const COUNT_RESULT_STATEMENTS: usize = 8;
impl CountResultStatement {
    pub const fn all() -> [Self; COUNT_RESULT_STATEMENTS] {
        [
            Self::Configure,
            Self::Create,
            Self::Begin,
            Self::Insert,
            Self::Commit,
            Self::Page,
            Self::Size,
            Self::Identity,
        ]
    }
    pub const fn slot(self) -> usize {
        match self {
            Self::Configure => 0,
            Self::Create => 1,
            Self::Begin => 2,
            Self::Insert => 3,
            Self::Commit => 4,
            Self::Page => 5,
            Self::Size => 6,
            Self::Identity => 7,
        }
    }
}

#[derive(Clone)]
pub(super) struct CountOrigin {
    pub snapshot: Reading,
    pub unsatisfied: Vec<Unsatisfied>,
    pub advisories: Vec<AnswerAdvisory>,
}

pub(super) struct CountSource<'a, 'm> {
    pub members: &'a [Member<'m>],
    pub conjunction: &'a Conjunction,
    pub declared: &'a ContentModel,
}

pub(super) struct RetainPage<'a> {
    pub params: &'a CountParams,
    pub origin: CountOrigin,
    pub limit: usize,
    pub after: Option<&'a [Option<String>]>,
    pub time: CountTime,
}

pub(super) struct TallyPage {
    pub rows: Vec<Tally>,
    pub next: Option<Tally>,
    pub result_id: Option<String>,
}

pub(crate) struct CountResults {
    state: Mutex<Registry>,
    policy: CountRetentionPolicy,
}
#[derive(Default)]
struct Registry {
    entries: BTreeMap<String, Arc<Retained>>,
    occupied: u64,
    slots: usize,
}
impl Default for CountResults {
    fn default() -> Self {
        Self::new(CountRetentionPolicy::default())
    }
}
impl std::fmt::Debug for CountResults {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CountResults").finish_non_exhaustive()
    }
}
impl CountResults {
    pub(crate) fn new(policy: CountRetentionPolicy) -> Self {
        Self {
            state: Mutex::new(Registry::default()),
            policy,
        }
    }
    fn lock(&self) -> Result<MutexGuard<'_, Registry>, PageRefusal> {
        self.state
            .try_lock()
            .map_err(|_| PageRefusal::CountResultBusy)
    }
    fn prune(&self, now: Instant) -> Result<(), PageRefusal> {
        let retired = {
            let mut state = self.lock()?;
            let expired = state
                .entries
                .iter()
                .filter(|(_, entry)| {
                    now >= entry.deadline || !entry.healthy.load(Ordering::Acquire)
                })
                .map(|(id, _)| id.clone())
                .collect::<Vec<_>>();
            expired
                .into_iter()
                .filter_map(|id| state.entries.remove(&id))
                .collect::<Vec<_>>()
        };
        // Closing disposable files can perform I/O; never do it under the registry lock.
        drop(retired);
        Ok(())
    }
    fn reserve(self: &Arc<Self>, time: CountTime) -> Result<Reservation, PageRefusal> {
        let mut state = self.lock()?;
        let remaining = self
            .policy
            .total_bytes
            .map(|total| total.saturating_sub(state.occupied));
        let limit = match (self.policy.max_result_bytes, remaining) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        let capacity = || PageRefusal::CountResultCapacity {
            retry_after_millis: state
                .entries
                .values()
                .map(|entry| {
                    u64::try_from(
                        entry
                            .deadline
                            .saturating_duration_since(time.now())
                            .as_millis(),
                    )
                    .unwrap_or(u64::MAX)
                })
                .min(),
        };
        let maximum = match (self.policy.max_result_bytes, self.policy.total_bytes) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        if state.slots >= self.policy.max_results {
            return Err(capacity());
        }
        let on_full = limit.map(|bytes| {
            if Some(bytes) == maximum {
                PageRefusal::CountResultTooLarge { max_bytes: bytes }
            } else {
                capacity()
            }
        });
        if limit.is_some_and(|bytes| bytes < 8192) {
            return Err(on_full.expect("quota"));
        }
        let bytes = limit.unwrap_or(0);
        state.occupied = state.occupied.checked_add(bytes).ok_or_else(capacity)?;
        state.slots += 1;
        Ok(Reservation {
            owner: Arc::clone(self),
            bytes,
            limit,
            active: true,
        })
    }
}

struct Reservation {
    owner: Arc<CountResults>,
    bytes: u64,
    limit: Option<u64>,
    active: bool,
}
impl Reservation {
    fn commit(
        mut self,
        built: BuiltResult,
        request: CountParams,
        origin: CountOrigin,
        orders: Vec<Order>,
        schema: Option<String>,
        deadline: Instant,
    ) -> Arc<Retained> {
        {
            let mut state = self
                .owner
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.occupied = state.occupied - self.bytes + built.bytes;
        }
        self.active = false;
        Arc::new(Retained {
            id: built.id,
            digest: selection_digest(&request),
            request,
            origin,
            orders,
            source_schema: schema,
            deadline,
            bytes: built.bytes,
            healthy: AtomicBool::new(true),
            database: Mutex::new(Some(built.database)),
            owner: Arc::downgrade(&self.owner),
        })
    }
}
impl Drop for Reservation {
    fn drop(&mut self) {
        if self.active {
            let mut state = self
                .owner
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.occupied -= self.bytes;
            state.slots -= 1;
        }
    }
}
struct BuiltResult {
    database: TemporaryDatabase,
    bytes: u64,
    id: String,
}
struct Retained {
    id: String,
    digest: String,
    request: CountParams,
    origin: CountOrigin,
    orders: Vec<Order>,
    source_schema: Option<String>,
    deadline: Instant,
    bytes: u64,
    healthy: AtomicBool,
    database: Mutex<Option<TemporaryDatabase>>,
    owner: Weak<CountResults>,
}
impl Drop for Retained {
    fn drop(&mut self) {
        let database = self
            .database
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        drop(database);
        if let Some(owner) = self.owner.upgrade() {
            let mut state = owner
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.occupied -= self.bytes;
            state.slots -= 1;
        }
    }
}

enum Order {
    Raw,
    Typed(TypedOrder),
    Tag,
}
impl Order {
    fn of(member: &Member<'_>, declared: &ContentModel) -> Self {
        match member.shape {
            super::GroupMember::Field(FieldOrder::Raw) => Self::Raw,
            super::GroupMember::Field(FieldOrder::Typed) => Self::Typed(
                declared
                    .typed_order(member.key.expect("field key"))
                    .expect("typed order")
                    .clone(),
            ),
            super::GroupMember::Tag => Self::Tag,
        }
    }
    fn read(&self, label: &str) -> Option<String> {
        match self {
            Self::Raw => Some(label.to_string()),
            Self::Typed(order) => order.sort_key(label),
            Self::Tag => Some(fold_tag(label)),
        }
    }
}

/// Injected request time advances by real elapsed work within the call.
#[derive(Clone, Copy)]
pub(super) struct CountTime {
    at: Instant,
    started: Instant,
}
impl CountTime {
    pub fn new(at: Instant) -> Self {
        Self {
            at,
            started: Instant::now(),
        }
    }
    fn now(self) -> Instant {
        self.at + self.started.elapsed()
    }
}

fn tuple_key(members: impl IntoIterator<Item = Option<String>>) -> Vec<u8> {
    let mut encoded = Vec::new();
    for member in members {
        match member {
            None => encoded.push(0),
            Some(text) => {
                encoded.push(1);
                for byte in text.bytes() {
                    if byte == 0 {
                        encoded.extend_from_slice(&[0, 255]);
                    } else {
                        encoded.push(byte);
                    }
                }
                encoded.extend_from_slice(&[0, 0]);
            }
        }
    }
    encoded
}
pub(super) fn selection_digest(params: &CountParams) -> String {
    let selection = serde_json::to_string(&(&params.vault, &params.by, &params.predicates))
        .expect("typed count selection serializes");
    crate::hash::sub_fingerprint(&selection)
}
fn binding(params: &CountParams) -> CountParams {
    let mut copy = params.clone();
    copy.after = None;
    copy.limit = None;
    copy
}
fn failed(problem: impl std::fmt::Display) -> PageRefusal {
    PageRefusal::CountResultFailed {
        message: problem.to_string(),
    }
}
fn scratch_error(problem: rusqlite::Error) -> PageRefusal {
    failed(norn_db::sql("using disposable count results", problem))
}
fn record_run(
    db: &TemporaryDatabase,
    name: CountResultStatement,
    sql: String,
    values: Vec<Value>,
    capture: bool,
) -> Result<Ran, PageRefusal> {
    let mut ran = Ran::new(crate::read::ReadStatement::CountResult(name), (sql, values));
    if capture {
        ran.external_plan = Some(
            db.emitted_plan(ran.sql(), params_from_iter(ran.values().iter()))
                .map_err(failed)?,
        );
    }
    Ok(ran)
}
fn result_rows<T>(
    db: &TemporaryDatabase,
    name: CountResultStatement,
    sql: String,
    values: Vec<Value>,
    lookups: &mut Lookups,
    work: &mut CountWork,
    read: impl FnMut(&Row<'_>) -> rusqlite::Result<T>,
) -> Result<Vec<T>, PageRefusal> {
    let mut ran = record_run(db, name, sql, values, lookups.capture_results)?;
    let mut statement = db.connection().prepare(ran.sql()).map_err(scratch_error)?;
    let rows = statement
        .query_map(params_from_iter(ran.values().iter()), read)
        .map_err(scratch_error)?
        .collect::<rusqlite::Result<Vec<T>>>();
    ran.stepped = Stepped::of(&statement);
    work.statements += 1;
    work.stepped(ran.stepped);
    lookups.ran.push(ran);
    rows.map_err(scratch_error)
}
fn control(
    db: &TemporaryDatabase,
    name: CountResultStatement,
    sql: &str,
    lookups: &mut Lookups,
    work: &mut CountWork,
) -> Result<(), PageRefusal> {
    result_rows(db, name, sql.to_string(), Vec::new(), lookups, work, |_| {
        Ok(())
    })?;
    Ok(())
}

impl Snapshot {
    /// A retained result is looked up before compiling against a newer schema.
    /// A changed request returns None and follows ordinary tuple-position judgment.
    pub(super) fn resume_retained_count(
        &self,
        params: &CountParams,
        limit: usize,
        lookups: &mut Lookups,
        time: CountTime,
    ) -> Result<Option<Counted>, PageRefusal> {
        let Some(cursor) = &params.after else {
            return Ok(None);
        };
        let Some(id) = cursor.retained_count_id() else {
            return Ok(None);
        };
        if cursor.retained_count_binding() != Some(selection_digest(params).as_str()) {
            return Ok(None);
        }
        let owner = self.count_results();
        owner.prune(time.now())?;
        let retained = owner
            .lock()?
            .entries
            .get(id)
            .cloned()
            .ok_or(PageRefusal::CountCursorExpired)?;
        if retained.request != binding(params) {
            return Ok(None);
        }
        if cursor.snapshot() != &retained.origin.snapshot {
            return Err(PageRefusal::cursor_not_taken(cursor, PagedRows::Tally));
        }
        let CursorKey::Tally { group, .. } = cursor.key() else {
            return Err(PageRefusal::cursor_not_taken(cursor, PagedRows::Tally));
        };
        if group.len() != retained.orders.len() {
            return Err(PageRefusal::cursor_not_taken(cursor, PagedRows::Tally));
        }
        let normalized = group
            .iter()
            .zip(&retained.orders)
            .map(|(label, order)| match label {
                None => Ok(None),
                Some(label) => order
                    .read(label)
                    .map(Some)
                    .ok_or_else(|| PageRefusal::cursor_not_taken(cursor, PagedRows::Tally)),
            })
            .collect::<Result<Vec<_>, _>>()?;
        let mut work = CountWork::default();
        let page = self.result_page(
            &retained,
            limit,
            Some(&normalized),
            lookups,
            &mut work,
            time,
        )?;
        Ok(Some(retained.answer(page, Vec::new(), work)))
    }

    pub(super) fn retained_tallies(
        &self,
        request: RetainPage<'_>,
        source: CountSource<'_, '_>,
        lookups: &mut Lookups,
        work: &mut CountWork,
    ) -> Result<TallyPage, PageRefusal> {
        let RetainPage {
            params,
            origin,
            limit,
            after,
            time,
        } = request;
        let CountSource {
            members,
            conjunction,
            declared,
        } = source;
        let owner = self.count_results();
        owner.prune(time.now())?;
        let request = binding(params);
        let schema = declared.schema().map(str::to_string);
        let reused = {
            let state = owner.lock()?;
            state
                .entries
                .values()
                .find(|entry| {
                    entry.request == request
                        && entry.origin.snapshot == origin.snapshot
                        && entry.source_schema == schema
                        && time.now() + owner.policy.lifetime / 2 <= entry.deadline
                })
                .cloned()
        };
        let retained = match reused {
            Some(retained) => retained,
            None => {
                let reservation = owner.reserve(time)?;
                let built = self.populate_count_result(
                    CountSource {
                        members,
                        conjunction,
                        declared,
                    },
                    &reservation,
                    lookups,
                    work,
                )?;
                let orders = members
                    .iter()
                    .map(|member| Order::of(member, declared))
                    .collect();
                reservation.commit(
                    built,
                    request,
                    origin,
                    orders,
                    schema,
                    time.now() + owner.policy.lifetime,
                )
            }
        };
        let page = self.result_page(&retained, limit, after, lookups, work, time)?;
        if page.next.is_some() && !lookups.capture_results {
            let mut state = owner.lock()?;
            state
                .entries
                .entry(retained.id.clone())
                .or_insert_with(|| Arc::clone(&retained));
        }
        let id = page.next.as_ref().map(|_| retained.id.clone());
        Ok(TallyPage {
            rows: page.rows,
            next: page.next,
            result_id: id,
        })
    }

    fn result_page(
        &self,
        retained: &Retained,
        limit: usize,
        after: Option<&[Option<String>]>,
        lookups: &mut Lookups,
        work: &mut CountWork,
        time: CountTime,
    ) -> Result<KeysetPage<Tally>, PageRefusal> {
        if time.now() >= retained.deadline || !retained.healthy.load(Ordering::Acquire) {
            return Err(PageRefusal::CountCursorExpired);
        }
        let guard = retained
            .database
            .try_lock()
            .map_err(|_| PageRefusal::CountResultBusy)?;
        let db = guard.as_ref().expect("live result");
        let labels = (0..retained.orders.len())
            .map(|at| format!("label{at}"))
            .collect::<Vec<_>>()
            .join(", ");
        let key = after
            .map(|tuple| tuple_key(tuple.iter().cloned()))
            .unwrap_or_default();
        let rows = result_rows(
            db,
            CountResultStatement::Page,
            format!("SELECT {labels}, count FROM result WHERE tuple > ?1 ORDER BY tuple LIMIT ?2"),
            vec![Value::Blob(key), Value::Integer((limit + 1) as i64)],
            lookups,
            work,
            |row| {
                let group = (0..retained.orders.len())
                    .map(|at| row.get(at))
                    .collect::<rusqlite::Result<Vec<Option<String>>>>()?;
                Ok(Tally::new(group, row.get::<_, u64>(retained.orders.len())?))
            },
        )
        .inspect_err(|_| {
            // Disposable corruption cannot condemn lane-1. Retire this identity
            // so later requests restart rather than replaying a broken relation.
            retained.healthy.store(false, Ordering::Release);
        })?;
        work.tallies_read += rows.len() as u64;
        if time.now() >= retained.deadline {
            return Err(PageRefusal::CountCursorExpired);
        }
        Ok(KeysetPage::cut(rows, limit, Stepped::default()))
    }

    fn populate_count_result(
        &self,
        source: CountSource<'_, '_>,
        reservation: &Reservation,
        lookups: &mut Lookups,
        work: &mut CountWork,
    ) -> Result<BuiltResult, PageRefusal> {
        let CountSource {
            members,
            conjunction,
            declared,
        } = source;
        let budget = reservation.limit;
        let db = TemporaryDatabase::open(64, budget).map_err(failed)?;
        for setup in db.opening() {
            let mut ran = record_run(
                &db,
                CountResultStatement::Configure,
                setup.sql.clone(),
                Vec::new(),
                lookups.capture_results,
            )?;
            ran.stepped = Stepped {
                full_scan_steps: setup.full_scan_steps,
                sorts: setup.sorts,
                vm_steps: setup.vm_steps,
            };
            work.statements += 1;
            work.stepped(ran.stepped);
            lookups.ran.push(ran);
        }
        let labels = (0..members.len())
            .map(|at| format!("label{at} TEXT"))
            .collect::<Vec<_>>()
            .join(", ");
        control(
            &db,
            CountResultStatement::Create,
            &format!(
                "CREATE TABLE result (tuple BLOB PRIMARY KEY NOT NULL, count INTEGER NOT NULL, {labels}) WITHOUT ROWID"
            ),
            lookups,
            work,
        )?;
        control(&db, CountResultStatement::Begin, "BEGIN", lookups, work)?;
        let placeholders = (1..=members.len() + 2)
            .map(|at| format!("?{at}"))
            .collect::<Vec<_>>()
            .join(", ");
        let insert_sql = format!("INSERT INTO result VALUES ({placeholders})");
        let mut insertion = db
            .connection()
            .prepare(&insert_sql)
            .map_err(scratch_error)?;
        let mut inserted: Option<Ran> = None;
        let mut insertion_at = None;
        let orders = members
            .iter()
            .map(|member| Order::of(member, declared))
            .collect::<Vec<_>>();
        let mut stream_failure = None;
        if !conjunction.matches_nothing {
            let parts = conjunction.filters.iter().collect::<Vec<_>>();
            let probes_start = lookups.ran.len();
            let driver = self.matched_driver(&parts, 1, &mut lookups.ran)?;
            for ran in &lookups.ran[probes_start..] {
                work.stepped(ran.stepped);
            }
            for statement in [
                CountStatement::NullLead(members[0].shape),
                CountStatement::ValuedLead(members[0].shape),
            ] {
                let source = Ran::new(
                    statement,
                    compose_tallies(&Tallies {
                        statement,
                        members,
                        after: None,
                        filters: &conjunction.filters,
                        driver,
                        past: None,
                        until: None,
                        rows: None,
                        offset: 0,
                    }),
                )
                .narrowed_by(
                    conjunction
                        .filters
                        .iter()
                        .map(|filter| filter.shape())
                        .collect(),
                );
                let source_start = lookups.ran.len();
                let stream = self.stream_statement(&mut lookups.ran, source, |row| {
                    let first = usize::from(matches!(statement, CountStatement::NullLead(_)));
                    let mut labels = Vec::with_capacity(members.len());
                    if first == 1 {
                        labels.push(None);
                    }
                    for (column, member) in members[first..].iter().enumerate() {
                        labels.push(member.shape.read_label(row.get(column).map_err(|error| {
                            PageRefusal::from(crate::error::sql("reading complete tallies", error))
                        })?));
                    }
                    let count: i64 = row.get(members.len() - first).map_err(|error| {
                        PageRefusal::from(crate::error::sql("reading complete tallies", error))
                    })?;
                    let key =
                        tuple_key(orders.iter().zip(&labels).map(|(order, label)| {
                            label.as_ref().and_then(|label| order.read(label))
                        }));
                    let mut values = vec![Value::Blob(key), Value::Integer(count)];
                    values.extend(
                        labels
                            .into_iter()
                            .map(|label| label.map_or(Value::Null, Value::Text)),
                    );
                    if inserted.is_none() {
                        let mut ran = record_run(
                            &db,
                            CountResultStatement::Insert,
                            insert_sql.clone(),
                            values.clone(),
                            lookups.capture_results,
                        )?;
                        ran.executions = 0;
                        inserted = Some(ran);
                    }
                    inserted.as_mut().expect("insert recorded").executions += 1;
                    work.tallies_read += 1;
                    insertion
                        .execute(params_from_iter(values))
                        .map_err(|error| {
                            if error.sqlite_error_code() == Some(rusqlite::ErrorCode::DiskFull) {
                                PageRefusal::CountResultStorageFull
                            } else {
                                scratch_error(error)
                            }
                        })?;
                    Ok::<_, PageRefusal>(())
                });
                for ran in &lookups.ran[source_start..] {
                    work.stepped(ran.stepped);
                }
                if insertion_at.is_none() && inserted.is_some() {
                    insertion_at = Some(source_start + 1);
                }
                match stream {
                    Ok(()) => {}
                    Err(StreamFailure::Source(error)) => {
                        stream_failure = Some(PageRefusal::from(crate::error::sql(
                            "aggregating a retained count",
                            error.into_inner(),
                        )));
                        break;
                    }
                    Err(StreamFailure::Consumer(error)) => {
                        stream_failure = Some(error);
                        break;
                    }
                }
            }
        }
        if let Some(mut ran) = inserted {
            ran.stepped = Stepped::of(&insertion);
            work.statements += ran.executions;
            work.stepped(ran.stepped);
            lookups
                .ran
                .insert(insertion_at.expect("first insertion position"), ran);
        }
        drop(insertion);
        if let Some(error) = stream_failure {
            return Err(error);
        }
        control(&db, CountResultStatement::Commit, "COMMIT", lookups, work)?;
        let pages = result_rows(
            &db,
            CountResultStatement::Size,
            "PRAGMA page_count".to_string(),
            Vec::new(),
            lookups,
            work,
            |row| row.get::<_, u64>(0),
        )?
        .remove(0);
        let id = result_rows(
            &db,
            CountResultStatement::Identity,
            "SELECT lower(hex(randomblob(16)))".to_string(),
            Vec::new(),
            lookups,
            work,
            |row| row.get::<_, String>(0),
        )?
        .remove(0);
        Ok(BuiltResult {
            database: db,
            bytes: pages * 4096,
            id,
        })
    }
}
impl Retained {
    fn answer(&self, page: KeysetPage<Tally>, moved: Vec<Moved>, work: CountWork) -> Counted {
        let next = page.next.map(|last| {
            Cursor::retained_count(
                self.origin.snapshot.clone(),
                last.group,
                &self.id,
                &self.digest,
            )
        });
        Counted {
            tallies: page.rows,
            next,
            moved,
            unsatisfied: self.origin.unsatisfied.clone(),
            advisories: self.origin.advisories.clone(),
            snapshot: self.origin.snapshot.clone(),
            work,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DerivationVersion, Store, StoredPathOrder};
    use norn_testkit::scratch::Scratch;
    use norn_wire::{GroupKey, VaultAddress, VaultName};

    #[test]
    fn reservations_price_inflight_slots_and_bytes_and_release_on_failure() {
        let owner = Arc::new(CountResults::new(CountRetentionPolicy {
            max_results: 2,
            max_result_bytes: Some(16384),
            total_bytes: Some(24576),
            ..CountRetentionPolicy::default()
        }));
        let time = CountTime::new(Instant::now());
        let first = owner.reserve(time).unwrap();
        let second = owner.reserve(time).unwrap();
        assert_eq!(first.limit, Some(16384));
        assert_eq!(second.limit, Some(8192));
        assert!(matches!(
            owner.reserve(time),
            Err(PageRefusal::CountResultCapacity { .. })
        ));
        drop(first);
        let replacement = owner.reserve(time).unwrap();
        assert_eq!(replacement.limit, Some(16384));
        drop((second, replacement));
        assert_eq!(owner.lock().unwrap().occupied, 0);
        assert_eq!(owner.lock().unwrap().slots, 0);
        let too_small = Arc::new(CountResults::new(CountRetentionPolicy {
            max_result_bytes: Some(4096),
            ..CountRetentionPolicy::default()
        }));
        assert!(matches!(
            too_small.reserve(time),
            Err(PageRefusal::CountResultTooLarge { max_bytes: 4096 })
        ));
    }

    #[test]
    fn a_broken_result_is_discarded_without_damaging_the_source() {
        let scratch = Scratch::new("count-broken-result");
        let store = Store::open(
            scratch.join("store.sqlite3"),
            StoredPathOrder::Sensitive,
            DerivationVersion::new(1),
        )
        .unwrap();
        let reader = Arc::new(store.open_reader().reader.unwrap());
        let snapshot = reader.try_take().unwrap().establish().unwrap();
        let params = CountParams::new(VaultAddress::name(VaultName::new("test").unwrap()))
            .with_by([GroupKey::field("one"), GroupKey::field("two")])
            .with_limit(1);
        let owner = snapshot.count_results();
        let db = TemporaryDatabase::open(64, None).unwrap();
        let origin = CountOrigin {
            snapshot: Reading::new("epoch", 0, None, None),
            unsatisfied: Vec::new(),
            advisories: Vec::new(),
        };
        let result = owner
            .reserve(CountTime::new(Instant::now()))
            .unwrap()
            .commit(
                BuiltResult {
                    database: db,
                    bytes: 0,
                    id: "broken".to_string(),
                },
                binding(&params),
                origin,
                vec![Order::Raw, Order::Raw],
                None,
                Instant::now() + Duration::from_secs(600),
            );
        let cursor = Cursor::retained_count(
            result.origin.snapshot.clone(),
            vec![None, None],
            &result.id,
            &result.digest,
        );
        owner
            .lock()
            .unwrap()
            .entries
            .insert(result.id.clone(), Arc::clone(&result));
        let params = params.with_after(cursor);
        assert!(matches!(
            snapshot.count(&params, &ContentModel::none()),
            Err(PageRefusal::CountResultFailed { .. })
        ));
        assert!(matches!(
            snapshot.count(&params, &ContentModel::none()),
            Err(PageRefusal::CountCursorExpired)
        ));
        assert!(
            snapshot
                .count(&CountParams::new(params.vault), &ContentModel::none())
                .is_ok()
        );
        drop(result);
        assert_eq!(owner.lock().unwrap().slots, 0);
    }

    #[test]
    fn a_busy_registry_keeps_a_first_page_answerable() {
        let scratch = Scratch::new("count-busy-registry");
        let store = Store::open(
            scratch.join("store.sqlite3"),
            StoredPathOrder::Sensitive,
            DerivationVersion::new(1),
        )
        .unwrap();
        let reader = Arc::new(store.open_reader().reader.unwrap());
        let snapshot = reader.try_take().unwrap().establish().unwrap();
        let params = CountParams::new(VaultAddress::name(VaultName::new("test").unwrap()))
            .with_by([GroupKey::field("one"), GroupKey::field("two")]);
        let guard = snapshot.count_results().state.lock().unwrap();
        assert!(snapshot.count(&params, &ContentModel::none()).is_ok());
        let cursor = Cursor::new(
            Reading::new("epoch", 0, None, None),
            CursorKey::tally([None, None]),
        );
        assert!(matches!(
            snapshot.count(&params.with_after(cursor), &ContentModel::none()),
            Err(PageRefusal::CountResultBusy)
        ));
        drop(guard);
    }
}
