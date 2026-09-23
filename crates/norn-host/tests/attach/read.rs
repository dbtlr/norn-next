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

/// The pages of one find read on one snapshot.
pub struct Pages {
    pub pages: Vec<Found>,
}

impl Pages {
    /// What the pages read, summed name by name under the names
    /// [`FindWork::readings`](norn_store::FindWork::readings) gives them.
    pub fn readings(&self) -> CounterSnapshot {
        let mut readings = CounterSnapshot::new();
        for page in &self.pages {
            for (name, value) in page.work.readings() {
                readings.set(name, readings.get(name) + value);
            }
        }
        readings
    }
}

/// Read up to `count` pages of `params` on `snapshot` and keep every one.
pub fn pages(
    snapshot: &Snapshot,
    params: &FindParams,
    declared: &DeclaredFields,
    count: u64,
) -> Pages {
    let mut pages = Vec::new();
    each_page(snapshot, params, declared, count, |page| pages.push(page));
    Pages { pages }
}

/// Read up to `count` pages of `params` on `snapshot`, each continuing the
/// cursor the one before it minted, stopping early at a page that mints none,
/// and hand each page to `visit` as it is read. A page `visit` does not keep
/// is gone before the next one is read.
///
/// **Every statement a page ran is one the snapshot counted.** The snapshot's
/// own statement count across the pages equals the sum of the statements the
/// pages report, and a read that ran one on the snapshot beside the find, or
/// a find whose report missed one, is refused here. It is a consistency check
/// on the report rather than a second reading of it.
pub fn each_page(
    snapshot: &Snapshot,
    params: &FindParams,
    declared: &DeclaredFields,
    count: u64,
    mut visit: impl FnMut(Found),
) {
    let started = snapshot.counters().statements_executed();
    let mut reported = 0;
    let mut after = None;
    for _ in 0..count {
        let request = match after.take() {
            None => params.clone(),
            Some(cursor) => params.clone().with_after(cursor),
        };
        let page = snapshot
            .find(&request, declared)
            .unwrap_or_else(|refusal| panic!("the find was refused: {refusal}"));
        reported += page.work.statements;
        after = page.next.clone();
        visit(page);
        if after.is_none() {
            break;
        }
    }
    assert_eq!(
        snapshot.counters().statements_executed() - started,
        reported,
        "the snapshot counted other statements than the pages report running"
    );
}
