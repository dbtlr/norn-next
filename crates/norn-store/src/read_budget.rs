//! Atomic connection admission and warm reuse across a host's vaults.
use crate::StoreError;
use norn_db::{Database, TemporaryDatabase};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt;
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex, Weak};
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReadNeed {
    Source,
    SourceAndScratch,
    /// A fresh page can use its complete source-only fallback at its turn.
    SourceAndOptionalScratch,
}
impl ReadNeed {
    fn width(self) -> usize {
        if self == Self::Source { 1 } else { 2 }
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReadBudgetPolicy {
    connections: usize,
    idle_grace: Duration,
}
impl ReadBudgetPolicy {
    pub fn new(connections: usize, idle_grace: Duration) -> Result<Self, StoreError> {
        if connections < 2 {
            return Err(StoreError::Sql {
                operation: "configuring reader capacity",
                condition: None,
                message: "reader capacity must allow at least two connections".into(),
            });
        }
        Ok(Self {
            connections,
            idle_grace,
        })
    }
    pub fn connections(self) -> usize {
        self.connections
    }
    pub fn idle_grace(self) -> Duration {
        self.idle_grace
    }
}
impl Default for ReadBudgetPolicy {
    fn default() -> Self {
        Self {
            connections: 4,
            idle_grace: Duration::from_secs(5),
        }
    }
}
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ReadBudgetCensus {
    pub connections_opened: u64,
    pub live_connections: usize,
    pub idle_connections: usize,
    pub waiting_reads: usize,
    pub peak_connections: usize,
}
pub struct ReadBudget {
    policy: ReadBudgetPolicy,
    state: Mutex<State>,
    changed: Condvar,
}
impl fmt::Debug for ReadBudget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ReadBudget")
            .field("policy", &self.policy)
            .field("census", &self.census())
            .finish()
    }
}
#[derive(Default)]
struct State {
    opened: u64,
    next: u64,
    live: usize,
    peak: usize,
    pools: BTreeMap<u64, PoolState>,
    idle_pools: BTreeSet<u64>,
    turns: VecDeque<PathBuf>,
    waiters: BTreeMap<PathBuf, VecDeque<u64>>,
    waiting: usize,
    garbage: Vec<Reserved>,
}
struct PoolState {
    group: PathBuf,
    kind: Kind,
    idle: VecDeque<(Instant, Reserved)>,
    failure: Option<StoreError>,
}
impl State {
    fn idle_count(&self) -> usize {
        self.idle_pools
            .iter()
            .map(|id| self.pools[id].idle.len())
            .sum()
    }
    fn pop_idle(&mut self, id: u64, newest: bool) -> Option<Reserved> {
        let pool = self.pools.get_mut(&id)?;
        let (_, resource) = if newest {
            pool.idle.pop_back()?
        } else {
            pool.idle.pop_front()?
        };
        if pool.idle.is_empty() {
            self.idle_pools.remove(&id);
        }
        Some(resource)
    }
    fn scratch_candidate(&self, group: &PathBuf) -> Option<u64> {
        self.idle_pools.iter().copied().find(|id| {
            self.pools
                .get(id)
                .is_some_and(|pool| pool.kind == Kind::Scratch && &pool.group == group)
        })
    }
}
#[derive(Clone, Copy, Eq, PartialEq)]
pub(crate) enum Kind {
    Source,
    Scratch,
}
#[derive(Clone)]
pub(crate) struct Pool {
    pub budget: Weak<ReadBudget>,
    pub id: u64,
    pub group: PathBuf,
}
pub(crate) enum Cached {
    Source(Database),
    Scratch {
        database: TemporaryDatabase,
        _owner: Arc<dyn Send + Sync>,
    },
}
/// Connections close before their credit is released. Weak ownership avoids a cache cycle.
pub(crate) struct Reserved {
    pub item: Option<Cached>,
    credit: Credit,
    pub pool: u64,
}
impl Reserved {
    pub(crate) fn budget(&self) -> Option<Arc<ReadBudget>> {
        self.credit.budget.upgrade()
    }
}
struct Credit {
    budget: Weak<ReadBudget>,
}
impl Drop for Credit {
    fn drop(&mut self) {
        if let Some(budget) = self.budget.upgrade() {
            let mut state = budget
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.live -= 1;
            drop(state);
            budget.changed.notify_all();
        }
    }
}
pub(crate) struct Grant {
    pub source: Reserved,
    pub scratch: Option<Reserved>,
    pub group: PathBuf,
}
impl ReadBudget {
    pub fn new(policy: ReadBudgetPolicy) -> Arc<Self> {
        Arc::new(Self {
            policy,
            state: Mutex::new(State::default()),
            changed: Condvar::new(),
        })
    }
    pub fn census(&self) -> ReadBudgetCensus {
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        ReadBudgetCensus {
            connections_opened: state.opened,
            live_connections: state.live,
            idle_connections: state
                .idle_pools
                .iter()
                .map(|id| state.pools[id].idle.len())
                .sum(),
            waiting_reads: state.waiting,
            peak_connections: state.peak,
        }
    }
    pub(crate) fn pool(self: &Arc<Self>, group: PathBuf, kind: Kind) -> Pool {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.next += 1;
        let id = state.next;
        state.pools.insert(
            id,
            PoolState {
                group: group.clone(),
                kind,
                idle: VecDeque::new(),
                failure: None,
            },
        );
        Pool {
            budget: Arc::downgrade(self),
            id,
            group,
        }
    }
    /// Called outside entry gates; closing a cached connection may perform I/O.
    pub fn trim(&self, now: Instant) {
        let retired = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let mut retired = std::mem::take(&mut state.garbage);
            let idle = state.idle_pools.iter().copied().collect::<Vec<_>>();
            for id in idle {
                let pool = state.pools.get_mut(&id).expect("indexed idle pool");
                while pool.idle.front().is_some_and(|(at, _)| {
                    now.saturating_duration_since(*at) >= self.policy.idle_grace
                }) {
                    retired.push(pool.idle.pop_front().expect("idle front").1);
                }
                if pool.idle.is_empty() {
                    state.idle_pools.remove(&id);
                }
            }
            retired
        };
        drop(retired);
    }
    pub(crate) fn drain(&self) {
        let retired = std::mem::take(
            &mut self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .garbage,
        );
        drop(retired);
    }
    fn remove_waiter(&self, group: &PathBuf, id: u64) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut removed = false;
        let empty = if let Some(queue) = state.waiters.get_mut(group) {
            queue.retain(|ticket| {
                if *ticket == id {
                    removed = true;
                    false
                } else {
                    true
                }
            });
            queue.is_empty()
        } else {
            false
        };
        if removed {
            state.waiting -= 1;
        }
        if empty {
            state.waiters.remove(group);
            state.turns.retain(|turn| turn != group);
        }
        drop(state);
        self.changed.notify_all();
    }
    fn attempt(
        self: &Arc<Self>,
        state: &mut State,
        pool: &Pool,
        need: ReadNeed,
        warm_only: bool,
    ) -> Option<Grant> {
        let source = state.pools.get(&pool.id)?;
        let warm = !source.idle.is_empty();
        if warm_only && !warm {
            return None;
        }
        let mut scratch_pool = if need != ReadNeed::Source {
            state.scratch_candidate(&pool.group)
        } else {
            None
        };
        let mut width = need.width();
        let mut cached = usize::from(warm) + usize::from(scratch_pool.is_some());
        if need == ReadNeed::SourceAndOptionalScratch
            && state.live + width - cached > self.policy.connections
        {
            if self.policy.connections - state.live + state.idle_count() + state.garbage.len() >= 2
            {
                return None;
            }
            width = 1;
            scratch_pool = None;
            cached = usize::from(warm);
        }
        if state.live + width - cached > self.policy.connections {
            return None;
        }
        let mut reserve = |id, warm| {
            if warm {
                return state.pop_idle(id, true).expect("warm resource");
            }
            state.live += 1;
            state.peak = state.peak.max(state.live);
            Reserved {
                item: None,
                credit: Credit {
                    budget: Arc::downgrade(self),
                },
                pool: id,
            }
        };
        let source = reserve(pool.id, warm);
        let scratch =
            (width == 2).then(|| reserve(scratch_pool.unwrap_or(pool.id), scratch_pool.is_some()));
        Some(Grant {
            source,
            scratch,
            group: pool.group.clone(),
        })
    }
    pub(crate) fn try_grant(self: &Arc<Self>, pool: &Pool, need: ReadNeed) -> Option<Grant> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.waiting != 0 {
            return None;
        }
        self.attempt(&mut state, pool, need, true)
    }
    pub(crate) fn grant(
        self: &Arc<Self>,
        pool: &Pool,
        need: ReadNeed,
        deadline: Instant,
    ) -> Option<Grant> {
        self.drain();
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !state.pools.contains_key(&pool.id) {
            return None;
        }
        state.next += 1;
        let ticket = state.next;
        if !state.waiters.contains_key(&pool.group) {
            state.turns.push_back(pool.group.clone());
        }
        state
            .waiters
            .entry(pool.group.clone())
            .or_default()
            .push_back(ticket);
        state.waiting += 1;
        let mut waiter = Waiting {
            budget: Arc::clone(self),
            ticket,
            group: pool.group.clone(),
            active: true,
        };
        loop {
            if !state.garbage.is_empty() {
                let closing = std::mem::take(&mut state.garbage);
                drop(state);
                drop(closing);
                state = self
                    .state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                continue;
            }
            if !state.pools.contains_key(&pool.id) || Instant::now() >= deadline {
                drop(state);
                drop(waiter);
                return None;
            }
            #[allow(clippy::manual_pop_if)] // Rotate only after the complete grant succeeds.
            if state.turns.front().is_some_and(|group| {
                state
                    .waiters
                    .get(group)
                    .is_some_and(|queue| queue.front() == Some(&ticket))
            }) {
                if let Some(grant) = self.attempt(&mut state, pool, need, false) {
                    let group = state.turns.pop_front().expect("granted vault turn");
                    let queue = state.waiters.get_mut(&group).expect("granted vault queue");
                    queue.pop_front();
                    if queue.is_empty() {
                        state.waiters.remove(&group);
                    } else {
                        state.turns.push_back(group);
                    }
                    state.waiting -= 1;
                    waiter.active = false;
                    drop(state);
                    drop(waiter);
                    self.changed.notify_all();
                    return Some(grant);
                }
                let keep_pair = need == ReadNeed::SourceAndScratch
                    || (need == ReadNeed::SourceAndOptionalScratch
                        && self.policy.connections - state.live + state.idle_count() >= 2);
                let scratch = keep_pair
                    .then(|| state.scratch_candidate(&pool.group))
                    .flatten();
                let victim = state
                    .idle_pools
                    .iter()
                    .copied()
                    .filter(|id| {
                        (*id != pool.id && Some(*id) != scratch) || state.pools[id].idle.len() > 1
                    })
                    .min_by_key(|id| {
                        state.pools[id]
                            .idle
                            .front()
                            .expect("indexed idle resource")
                            .0
                    });
                if let Some(id) = victim {
                    let closing = state.pop_idle(id, false);
                    drop(state);
                    drop(closing);
                    state = self
                        .state
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    continue;
                }
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            state = self
                .changed
                .wait_timeout(state, remaining)
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .0;
        }
    }
    pub(crate) fn put(&self, resource: Reserved) {
        if resource.item.is_none() {
            drop(resource);
            return;
        }
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(pool) = state.pools.get_mut(&resource.pool) {
            let id = resource.pool;
            pool.idle.push_back((Instant::now(), resource));
            state.idle_pools.insert(id);
        } else {
            state.garbage.push(resource);
        }
        drop(state);
        self.changed.notify_all();
    }
    pub(crate) fn take_idle(&self, pool: u64) -> Option<Reserved> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .pop_idle(pool, true)
    }
    pub(crate) fn opened(&self) {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .opened += 1;
    }
}
struct Waiting {
    budget: Arc<ReadBudget>,
    ticket: u64,
    group: PathBuf,
    active: bool,
}
impl Drop for Waiting {
    fn drop(&mut self) {
        if self.active {
            self.budget.remove_waiter(&self.group, self.ticket);
        }
    }
}
impl Pool {
    pub(crate) fn is_live(&self) -> bool {
        self.budget.upgrade().is_some_and(|budget| {
            budget
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .pools
                .contains_key(&self.id)
        })
    }
    pub(crate) fn retire(&self) {
        if let Some(budget) = self.budget.upgrade() {
            let mut state = budget
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(pool) = state.pools.remove(&self.id) {
                state.idle_pools.remove(&self.id);
                state
                    .garbage
                    .extend(pool.idle.into_iter().map(|(_, resource)| resource));
            }
            drop(state);
            budget.changed.notify_all();
        }
    }
    pub(crate) fn failure(&self, error: Option<StoreError>) {
        let Some(budget) = self.budget.upgrade() else {
            return;
        };
        if let Some(pool) = budget
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .pools
            .get_mut(&self.id)
        {
            pool.failure = error;
        }
    }
    pub(crate) fn last_failure(&self) -> Option<StoreError> {
        let budget = self.budget.upgrade()?;
        let state = budget
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.pools.get(&self.id)?.failure.clone()
    }
}
impl Grant {
    pub(crate) fn finish(mut self, database: Database) {
        self.source.item = Some(Cached::Source(database));
        if let Some(budget) = self.source.credit.budget.upgrade() {
            budget.put(self.source);
            if let Some(scratch) = self.scratch.take() {
                budget.put(scratch);
            }
        }
    }
}
