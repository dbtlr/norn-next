//! The store's heap bar for the link-health re-decision: what a write that
//! re-decides every link to a hub stem holds on the Rust heap, measured
//! rather than asserted.
//!
//! **This binary is its own measurement.** Its global allocator counts the
//! live heap ([`norn_testkit::heap`]), which is process-wide, so the binary
//! holds this one case and nothing allocates beside it. What the count sees is
//! the Rust heap: the rows a statement hands back once they are read, the
//! links a chunk holds, the findings it files. SQLite's own page cache is
//! taken from `malloc` directly and is not in it; that is the store's, bounded
//! by its cache size, and the same at any vault size.

use norn_store::{
    Change, ContentModel, DerivationVersion, DocumentFacts, DocumentPath, IncrementProvenance,
    LinkFact, LinkFamily, Span, Store, StoredPathOrder,
};
use norn_testkit::heap;
use norn_testkit::scratch::Scratch;

/// Every allocation this binary makes goes through the counting allocator.
#[global_allocator]
static HEAP: heap::Counting = heap::Counting;

/// How many `[[hub]]` links each holding document carries.
const LINKS_PER_HOLDER: usize = 100;

/// How far the heap a write re-deciding ten times the links may peak above
/// the smaller write's. The two peaks repeat to within a few hundred bytes;
/// a byte kept for each of the forty-five thousand links between them is
/// past this bound, and a chunk's links held for every chunk is far past it.
const FLAT: usize = 16 * 1024;

/// A document at `at` holding `links` links `[[hub]]`.
fn holder(at: &str, links: usize) -> DocumentFacts {
    let body = "[[hub]]\n".repeat(links);
    let mut facts = DocumentFacts::new(
        DocumentPath::new(at).expect("a path"),
        format!("hash-{at}"),
        &body,
        body.len() as u64,
    );
    facts.links = (0..links)
        .map(|at| LinkFact {
            family: LinkFamily::Wikilink,
            embed: false,
            protocol: None,
            target: "hub".to_string(),
            title: None,
            anchor: None,
            span: Span {
                line: at as u64 + 1,
                column: 1,
                byte_offset: (at * 8) as u64,
            },
        })
        .collect();
    facts
}

/// A document at `at` the stem `hub` names.
fn hub(at: &str) -> DocumentFacts {
    DocumentFacts::new(
        DocumentPath::new(at).expect("a path"),
        "hash-hub",
        "hub\n",
        4,
    )
}

/// The most the heap stood above where it stood before a write that makes
/// `links` links to `hub` ambiguous, each held by a document nobody wrote.
fn hub_write_peak(links: usize) -> usize {
    let scratch = Scratch::new(&format!("norn-store-heap-{links}"));
    let mut store = Store::open_throwaway(
        scratch.join("store.sqlite3"),
        StoredPathOrder::Sensitive,
        DerivationVersion::new(1),
    )
    .expect("opening a store");
    let declared = ContentModel::none();
    let holders = links / LINKS_PER_HOLDER;
    for chunk in (0..holders).collect::<Vec<_>>().chunks(100) {
        let mut changes: Vec<Change> = chunk
            .iter()
            .map(|at| Change::Upsert(holder(&format!("h/{at:04}.md"), LINKS_PER_HOLDER)))
            .collect();
        if chunk[0] == 0 {
            changes.push(Change::Upsert(hub("m/hub.md")));
        }
        store
            .begin_request()
            .apply_increment(IncrementProvenance::Derived, changes, &[], &declared)
            .expect("writing the holders");
    }

    let mark = heap::Mark::set();
    let mut request = store.begin_request();
    request
        .apply_increment(
            IncrementProvenance::Derived,
            [Change::Upsert(hub("new/hub.md"))],
            &[],
            &declared,
        )
        .expect("writing a second hub");
    let peak = mark.peak_above();
    let counters = request.finish();
    assert_eq!(counters.get("links_redecided"), Some(links as u64));
    assert_eq!(counters.get("findings_written"), Some(links as u64));
    peak
}

/// **A hub write holds a chunk on the heap, never the neighborhood.** Writing
/// a second document the stem `hub` names re-decides every link to it and
/// files an ambiguous finding about each: five thousand links, and then fifty
/// thousand. The re-decision reads, judges and files one chunk at a time, so
/// the heap the larger write peaks at stands within [`FLAT`] of the smaller
/// one's, where holding the links, their findings or a set of the links judged
/// would grow it by the forty-five thousand links between them.
#[test]
fn a_hub_write_holds_heap_within_a_chunk() {
    let small = hub_write_peak(5_000);
    let large = hub_write_peak(50_000);
    eprintln!("hub-write heap peaks: 5k links {small} bytes, 50k links {large} bytes");
    assert!(
        large <= small + FLAT,
        "re-deciding fifty thousand links peaked {large} bytes above the mark, and five \
         thousand {small}: the re-decision holds more than a chunk"
    );
}
