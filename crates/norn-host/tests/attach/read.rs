//! A find read through a live hold: the request the measurement suites ask,
//! and the pages it reads.
//!
//! The counter lane counts what the find costs and the memory lane holds the
//! process that ran it to a ceiling, so both read the same request, spelled
//! here once.

use norn_store::{DeclaredFields, Found, Snapshot, Store};
use norn_testkit::counters::CounterSnapshot;
use norn_wire::{Direction, FindParams, Sort, SortKey, VaultAddress, VaultName};

/// The rows every find these suites read is bounded at.
pub const FIND_LIMIT: u32 = 25;

/// The declaration the store pins, which a find is compiled under.
///
/// The suites' vault schema declares no field, so the declaration names the
/// pinned fingerprint and nothing else, and every field orders by its raw
/// text.
pub fn the_pinned_declaration(store: &mut Store) -> DeclaredFields {
    let pin = store
        .begin_request()
        .vault_schema_pin()
        .expect("reading the pin")
        .expect("an attachment pins the vault schema");
    DeclaredFields::under(pin.fingerprint)
}

/// A find over `vault`, newest `created` first, bounded at [`FIND_LIMIT`].
///
/// Every generated document carries `created` as an RFC 3339 instant in UTC
/// at one width, so its raw order is its chronological order, and the
/// descending page reads the valued section first: a seek of the key's marker
/// rows that stops at the page's bound.
pub fn bounded_find(vault: &VaultName) -> FindParams {
    FindParams::new(VaultAddress::name(vault.clone()))
        .with_sort(Sort::new(SortKey::field("created"), Direction::Descending))
        .with_limit(FIND_LIMIT)
}

/// The pages of one find read on one snapshot, and the statements they ran
/// on it.
pub struct Pages {
    pub pages: Vec<Found>,
    pub statements_executed: u64,
}

impl Pages {
    /// What the pages read, summed name by name, beside the statements the
    /// snapshot counted running them.
    pub fn readings(&self) -> CounterSnapshot {
        let mut readings = CounterSnapshot::new();
        for page in &self.pages {
            for (name, value) in page.work.readings() {
                readings.set(name, readings.get(name) + value);
            }
        }
        readings.set("snapshot_statements_executed", self.statements_executed);
        readings
    }
}

/// Read up to `count` pages of `params` on `snapshot`, each continuing the
/// cursor the one before it minted, stopping early at a page that mints none.
pub fn pages(
    snapshot: &mut Snapshot,
    params: &FindParams,
    declared: &DeclaredFields,
    count: usize,
) -> Pages {
    let started = snapshot.counters().statements_executed();
    let mut pages: Vec<Found> = Vec::with_capacity(count);
    while pages.len() < count {
        let request = match pages.last() {
            None => params.clone(),
            Some(page) => match &page.next {
                Some(next) => params.clone().with_after(next.clone()),
                None => break,
            },
        };
        pages.push(
            snapshot
                .find(&request, declared)
                .unwrap_or_else(|refusal| panic!("the find was refused: {refusal}")),
        );
    }
    Pages {
        pages,
        statements_executed: snapshot.counters().statements_executed() - started,
    }
}
