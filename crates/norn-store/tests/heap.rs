//! The store's heap bar for the link-health re-decision: what a write that
//! re-decides every link to a hub stem holds on the Rust heap, measured
//! rather than asserted.
//!
//! **This binary is its own measurement.** Its global allocator counts the
//! live heap ([`norn_testkit::heap`]), which is process-wide, so the binary
//! runs its cases one after another, in one case, and nothing allocates
//! beside them. What the count sees is
//! the Rust heap: the rows a statement hands back once they are read, the
//! links a chunk holds, the findings it files. SQLite's own page cache is
//! taken from `malloc` directly and is not in it; that is the store's, bounded
//! by its cache size, and the same at any vault size.
//!
//! The full-regression memory job runs this case alone through `lane-suite.sh`, which
//! rejects an empty run. The workspace suite leaves the measurement to that lane.

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

/// The links a hub write re-decides, each of which it files a finding about.
#[derive(Clone, Copy)]
struct Shape {
    label: &'static str,
    /// The protocol every link is written with.
    protocol: Option<&'static str>,
    /// The `n`th link's target.
    target: fn(usize) -> String,
    /// How many documents the stem `hub` names before the write.
    hubs: usize,
}

/// A document at `at` holding `links` links written with `protocol`, the
/// `n`th of them to the target `target(n)`.
fn holder(
    at: &str,
    links: usize,
    protocol: Option<&str>,
    target: &impl Fn(usize) -> String,
) -> DocumentFacts {
    let body: String = (0..links).map(|n| format!("[[{}]]\n", target(n))).collect();
    let mut facts = DocumentFacts::new(
        DocumentPath::new(at).expect("a path"),
        format!("hash-{at}"),
        &body,
        body.len() as u64,
    );
    facts.links = (0..links)
        .map(|at| LinkFact {
            field: None,
            family: LinkFamily::Wikilink,
            embed: false,
            protocol: protocol.map(str::to_string),
            target: target(at),
            title: None,
            anchor: None,
            span: Some(Span {
                line: at as u64 + 1,
                column: 1,
                byte_offset: (at * 8) as u64,
            }),
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

/// The heap peak of judging `links` of `shape`: once after a deferred build,
/// or inside the changeset that adds a second hub.
fn hub_peak(links: usize, shape: Shape, judgment: bool) -> usize {
    let Shape {
        label,
        protocol,
        target,
        hubs,
    } = shape;
    let scratch = Scratch::new(&format!("norn-store-heap-{label}-{links}"));
    let mut store = Store::open_throwaway(
        scratch.join("store.sqlite3"),
        StoredPathOrder::Sensitive,
        DerivationVersion::new(1),
    )
    .expect("opening a store");
    let declared = ContentModel::none();
    if judgment {
        store.defer_link_health().expect("deferring the build");
    }
    let holders = links / LINKS_PER_HOLDER;
    for chunk in (0..holders).collect::<Vec<_>>().chunks(100) {
        let mut changes: Vec<Change> = chunk
            .iter()
            .map(|at| {
                let offset = at * LINKS_PER_HOLDER;
                Change::Upsert(holder(
                    &format!("h/{at:04}.md"),
                    LINKS_PER_HOLDER,
                    protocol,
                    &|n| target(offset + n),
                ))
            })
            .collect();
        if chunk[0] == 0 {
            changes.extend((0..hubs).map(|at| Change::Upsert(hub(&format!("m/{at}/hub.md")))));
        }
        store
            .begin_request()
            .apply_increment(IncrementProvenance::Derived, changes, &[], &declared)
            .expect("writing the holders");
    }

    let mark = heap::Mark::set();
    let mut request = store.begin_request();
    if judgment {
        request
            .judge_link_health_once(&declared, &mut || {})
            .expect("judging the complete build");
    } else {
        request
            .apply_increment(
                IncrementProvenance::Derived,
                [Change::Upsert(hub("new/hub.md"))],
                &[],
                &declared,
            )
            .expect("writing a second hub");
    }
    let peak = mark.peak_above();
    let counters = request.finish();
    assert_eq!(
        counters.get(if judgment {
            "links_judged"
        } else {
            "links_redecided"
        }),
        Some(links as u64),
        "{label}"
    );
    assert_eq!(
        counters.get("findings_written"),
        Some(links as u64),
        "{label}"
    );
    peak
}

/// **A hub write holds a chunk on the heap, never the neighborhood.** Writing
/// a second document the stem `hub` names re-decides every link into its
/// class and every link to its path, five thousand links and then fifty
/// thousand, five ways:
///
/// - every link is `[[hub]]`, one key, and each is filed ambiguous, once
///   where the stem names two documents and once where it names nine, which
///   fills the key's head so its total is counted;
/// - every link is `[[dNNNNN/hub]]`, a key of its own in the class, and each
///   is filed broken;
/// - every link is `[[dNNNNN/hub.md]]`, held under one key in `hub/`
///   and another in `hub.md/`, and each is filed broken;
/// - every link is `[[vault://new/hub.md]]`, held under the paths
///   `new/hub.md` and `new/hub.md.md`, and each names the one document, so
///   each is filed a vault-scheme finding.
///
/// The re-decision reads, judges and files one chunk at a time, and keeps
/// what a key names only while the chunk just filed holds it, so the heap the
/// larger write peaks at stands within [`FLAT`] of the smaller one's, where
/// holding the links, their findings, a set of the links judged or what each
/// distinct key names would grow it by the forty-five thousand links between
/// them.
#[test]
#[ignore = "memory-lane case: isolated link-health heap measurement"]
fn a_hub_write_holds_heap_within_a_chunk() {
    let shapes = [
        Shape {
            label: "one-key",
            protocol: None,
            target: |_| "hub".to_string(),
            hubs: 1,
        },
        Shape {
            label: "filled-key",
            protocol: None,
            target: |_| "hub".to_string(),
            hubs: 8,
        },
        Shape {
            label: "distinct-keys",
            protocol: None,
            target: |n| format!("d{n:05}/hub"),
            hubs: 1,
        },
        Shape {
            label: "cross-class-keys",
            protocol: None,
            target: |n| format!("d{n:05}/hub.md"),
            hubs: 1,
        },
        Shape {
            label: "cross-path-keys",
            protocol: Some("vault"),
            target: |_| "new/hub.md".to_string(),
            hubs: 1,
        },
    ];
    for shape in shapes {
        let small = hub_peak(5_000, shape, false);
        let large = hub_peak(50_000, shape, false);
        let label = shape.label;
        eprintln!("{label} hub-write heap peaks: 5k links {small} bytes, 50k links {large} bytes");
        assert!(
            large <= small + FLAT,
            "{label}: re-deciding fifty thousand links peaked {large} bytes above the mark, \
             and five thousand {small}: the re-decision holds more than a chunk"
        );
    }
}

#[test]
#[ignore = "memory-lane case: isolated link-health heap measurement"]
fn a_complete_judgment_holds_heap_within_a_chunk() {
    for shape in [
        Shape {
            label: "judgment-hub",
            protocol: None,
            target: |_| "hub".into(),
            hubs: 8,
        },
        Shape {
            label: "judgment-distinct",
            protocol: None,
            target: |n| format!("d{n:05}/hub"),
            hubs: 1,
        },
    ] {
        let small = hub_peak(5_000, shape, true);
        let large = hub_peak(50_000, shape, true);
        eprintln!(
            "{} judgment heap peaks: 5k links {small} bytes, 50k links {large} bytes",
            shape.label
        );
        assert!(large <= small + FLAT, "{}: {small} -> {large}", shape.label);
    }
}

/// The progress queue stays in TEMP: increasing the vault tenfold cannot
/// retain the content plans or structure rows of previous host chunks.
#[test]
#[ignore = "memory-lane case: isolated construction heap measurement"]
fn construction_steps_hold_heap_within_one_chunk() {
    let small = construction_peak(16);
    let large = construction_peak(160);
    eprintln!("construction steps: 16 documents {small:?} bytes; 160 documents {large:?} bytes");
    assert!(
        large.0 <= small.0 + FLAT && large.1 <= small.1 + FLAT,
        "steps retained a vault's work: {small:?} -> {large:?}"
    );
    // Eight documents, each with 700 frontmatter links and 300 tags;
    // structure adds 100 links and 1,000 tags. Measured floors are 1,386,808
    // and 882,504 bytes. Headroom below 2 KiB rejects eight bytes per fact.
    assert!(
        large.0 <= 1_388_000 && large.1 <= 884_000,
        "dense chunk floors: {large:?}"
    );
}

/// Measure both document steps after names exist, using the same chunk width
/// and fact density at each vault size.
fn construction_peak(documents: usize) -> (usize, usize) {
    use norn_store::{ContentWrite, Step};
    let scratch = Scratch::new("construction-heap");
    let mut store = Store::open_throwaway(
        scratch.join("store.sqlite3"),
        StoredPathOrder::Sensitive,
        DerivationVersion::new(1),
    )
    .unwrap();
    store.defer_link_health().unwrap();
    store.begin_steps().unwrap();
    store
        .begin_request()
        .apply_increment(
            IncrementProvenance::Derived,
            (0..documents).map(|n| Change::Named {
                path: DocumentPath::new(&format!("doc-{n:04}.md")).unwrap(),
            }),
            &[],
            &ContentModel::none(),
        )
        .unwrap();
    store.finish_step(Step::Names).unwrap();
    let mark = heap::Mark::set();
    loop {
        let mut group = store.step_group().unwrap();
        let paths = group.begin_request().content_page(8).unwrap();
        if paths.is_empty() {
            group.commit().unwrap();
            break;
        }
        let rows: Vec<_> = paths
            .into_iter()
            .map(|path| ContentWrite {
                facts: {
                    let mut facts = DocumentFacts::new(path, "hash", "body\n", 5);
                    facts.links =
                        holder(facts.path.as_str(), 700, Some("https"), &|_| "x".into()).links;
                    for link in &mut facts.links {
                        link.field = Some("refs".into());
                        link.span = None;
                    }
                    facts.tags = (0..300)
                        .map(|_| norn_store::TagFact {
                            name: "tag".into(),
                            source: norn_store::TagSource::Frontmatter,
                            span: None,
                        })
                        .collect();
                    facts
                },
                origin: Span {
                    line: 1,
                    column: 1,
                    byte_offset: 0,
                },
            })
            .collect();
        group
            .begin_request()
            .write_content(&rows, &[], &ContentModel::none())
            .unwrap();
        group.commit().unwrap();
    }
    let content_peak = mark.peak_above();
    store.finish_step(Step::Content).unwrap();
    let mark = heap::Mark::set();
    loop {
        let mut group = store.step_group().unwrap();
        let mut rows = group.begin_request().structure_page(8).unwrap();
        if rows.is_empty() {
            group.commit().unwrap();
            break;
        }
        for input in &mut rows {
            group.begin_request().load_structure_tags(input).unwrap();
            input.facts.links = holder(input.facts.path.as_str(), 100, Some("https"), &|_| {
                "example.test".into()
            })
            .links;
            input
                .facts
                .tags
                .extend((0..1000).map(|_| norn_store::TagFact {
                    name: "tag".into(),
                    source: norn_store::TagSource::Body,
                    span: Some(Span {
                        line: 1,
                        column: 1,
                        byte_offset: 0,
                    }),
                }));
        }
        group
            .begin_request()
            .write_structure(&rows, &[], &ContentModel::none())
            .unwrap();
        group.commit().unwrap();
    }
    store.finish_step(Step::Structure).unwrap();
    (content_peak, mark.peak_above())
}
