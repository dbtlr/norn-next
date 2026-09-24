//! The search builder's lexical floor: what a page of hits answers, how its
//! query is read, how it pages, and the plan, work and payload bars its one
//! statement is judged by.
//!
//! The fixture is fourteen documents under a pinned schema. Five carry the
//! term `lantern`, four of them beside `harbor`, at different frequencies and
//! lengths, so BM25 ranks them apart; two carry the same body at paths that
//! differ only in ASCII case, so they rank level and the path breaks the tie;
//! one carries every character FTS5 reads as syntax; eight carry neither term.

use std::sync::Arc;

use crate::common::{DOCUMENT_PAYLOAD, Scratch, document, reads_of, violation, write_documents};
use crate::find::{failure_of, map, rows_of, string};
use norn_store::{
    ContentModel, FindStatement, PageRefusal, ReadFilter, ReadStatement, SEARCH_STATEMENTS,
    SearchPlan, SearchStatement, Searched, Snapshot, SnapshotReader, Store, TagFact, TagSource,
    induced_failure,
};
use norn_testkit::explain::{Access, PlanRow, QueryPlan, ScanTarget};
use norn_wire::{
    Column, Cursor, CursorKey, FieldValue, FindingKind, Moved, Predicate, ResolutionTarget, Score,
    SearchParams, Unsatisfied, VaultAddress, VaultName,
};

// ---- fixtures ----

/// The fingerprint of the schema the fixture pins.
const SEARCH_SCHEMA: &str = "search-schema";

/// `status` declared as text.
fn declared() -> ContentModel {
    ContentModel::under(SEARCH_SCHEMA).declare("status")
}

/// The body every character FTS5 reads as query syntax stands in, as words.
const SYNTAX_BODY: &str =
    "foo-bar body:foo NEAR NOT AND OR said \"quoted\" ^caret star* (paren)\n";

/// The fixture's documents:
///
/// | path | body | also |
/// |---|---|---|
/// | `notes/lantern.md` | `lantern` three times, `harbor` once | `status: open`, tag `draft` |
/// | `notes/walk.md` | `lantern` and `harbor` in fifteen words | `status: closed` |
/// | `notes/Twin.md` | `harbor lantern twin` | |
/// | `notes/twin.md` | `harbor lantern twin` | |
/// | `other/finding.md` | `lantern`, no `harbor` | a finding |
/// | `notes/syntax.md` | [`SYNTAX_BODY`] | |
/// | `filler/0.md` … `filler/7.md` | neither term | |
fn fixture() -> Vec<norn_store::DocumentFacts> {
    let declared = declared();
    let mut lantern = document(
        "notes/lantern.md",
        "hash-lantern",
        "lantern lantern lantern harbor\n",
    )
    .with_frontmatter(Some(map(vec![("status", string("open"))])), &declared);
    lantern.tags.push(TagFact {
        name: "draft".to_string(),
        source: TagSource::Body,
        span: None,
    });
    let mut documents = vec![
        lantern,
        document(
            "notes/walk.md",
            "hash-walk",
            "a lantern by the harbor on a long walk along the quiet shore at night\n",
        )
        .with_frontmatter(Some(map(vec![("status", string("closed"))])), &declared),
        document("notes/Twin.md", "hash-twin-upper", "harbor lantern twin\n"),
        document("notes/twin.md", "hash-twin-lower", "harbor lantern twin\n"),
        document(
            "other/finding.md",
            "hash-finding",
            "a lantern in the other folder\n",
        ),
        document("notes/syntax.md", "hash-syntax", SYNTAX_BODY),
    ];
    documents.extend((0..8).map(|at| {
        document(
            &format!("filler/{at}.md"),
            &format!("hash-filler-{at}"),
            "an unrelated body about gardens\n",
        )
    }));
    documents
}

/// A store holding the fixture under its schema, and `more` beside it.
fn seed(store: &mut Store, more: Vec<norn_store::DocumentFacts>) {
    store
        .begin_request()
        .pin_vault_schema(SEARCH_SCHEMA.as_bytes(), SEARCH_SCHEMA)
        .expect("pinning the fixture's schema");
    let mut request = store.begin_request();
    write_documents(&mut request, &fixture());
    request
        .record_finding(&violation("other/finding.md"))
        .expect("recording a finding");
    drop(request);
    if !more.is_empty() {
        write_documents(&mut store.begin_request(), &more);
    }
}

/// A seeded store and the read handle its snapshots are established on.
struct Searching {
    _scratch: Scratch,
    store: Store,
    reader: Arc<SnapshotReader>,
}

impl Searching {
    fn new(label: &str) -> Self {
        Self::with_documents(label, Vec::new())
    }

    /// The fixture and `more` documents beside it.
    fn with_documents(label: &str, more: Vec<norn_store::DocumentFacts>) -> Self {
        let scratch = Scratch::new(label);
        let mut store = scratch.open();
        seed(&mut store, more);
        let reader = Arc::new(
            store
                .open_reader()
                .reader
                .expect("a live store mints a reader"),
        );
        Searching {
            _scratch: scratch,
            store,
            reader,
        }
    }

    fn snapshot(&self) -> Snapshot {
        self.reader
            .try_take()
            .expect("a handle nothing is reading holds its connection")
            .establish()
            .snapshot
            .expect("a snapshot")
    }

    fn search(&self, params: &SearchParams) -> Searched {
        self.snapshot()
            .search(params, &declared())
            .unwrap_or_else(|refusal| panic!("a search of {params:?}: {refusal}"))
    }

    fn refusal(&self, params: &SearchParams) -> PageRefusal {
        self.snapshot()
            .search(params, &declared())
            .expect_err("a refused search")
    }

    fn plans(&self, params: &SearchParams) -> Vec<SearchPlan> {
        self.snapshot()
            .search_plans(params, &declared())
            .expect("the plans of a search")
    }
}

fn vault() -> VaultAddress {
    VaultAddress::name(VaultName::new("notes").expect("a vault name"))
}

fn searching(query: &str) -> SearchParams {
    SearchParams::new(vault(), query)
}

/// The paths a page's hits stand at, in its order.
fn hit_paths(searched: &Searched) -> Vec<&str> {
    searched
        .hits
        .iter()
        .map(|hit| hit.path.as_str())
        .collect()
}

fn scores(searched: &Searched) -> Vec<f64> {
    searched.hits.iter().map(|hit| hit.score.get()).collect()
}

// ---- what a hit is ----

/// **A search answers the documents holding every term, most relevant
/// first.** `lantern harbor` answers the four documents carrying both and not
/// `other/finding.md`, which carries `lantern` alone. The document holding
/// `lantern` three times in four words outranks the two holding each term once
/// in three, and they outrank the one holding each once in fifteen. **A score
/// is higher for the more relevant hit**, every one positive: BM25 as FTS5
/// computes it, negated.
#[test]
fn a_search_answers_the_documents_holding_every_term_most_relevant_first() {
    let searching_store = Searching::new("search-every-term");
    let searched = searching_store.search(&searching("lantern harbor"));
    assert_eq!(
        hit_paths(&searched),
        [
            "notes/lantern.md",
            "notes/Twin.md",
            "notes/twin.md",
            "notes/walk.md"
        ]
    );
    let scores = scores(&searched);
    assert!(
        scores[0] > scores[1] && scores[2] > scores[3],
        "the hits are not ranked apart: {scores:?}"
    );
    assert!(
        scores.iter().all(|score| *score > 0.0),
        "a score is not higher-is-better: {scores:?}"
    );
    assert!(searched.unsatisfied.is_empty());
    assert_eq!(searched.next, None);
    assert!(
        searched.hits.iter().all(|hit| hit.document.is_none()),
        "a search projecting no column carried a document row"
    );

    assert_eq!(
        hit_paths(&searching_store.search(&searching("lantern gardens"))),
        Vec::<&str>::new(),
        "a document holding one term of two was a hit"
    );
    assert_eq!(
        hit_paths(&searching_store.search(&searching("lantern"))).len(),
        5
    );
}

/// **Hits of equal relevance are ordered by path, in byte order.** The two
/// twins carry one body, so they score exactly alike, and `notes/Twin.md`
/// stands before `notes/twin.md` because `T` is the lesser byte — the order a
/// case-folding comparison would not tell apart.
#[test]
fn hits_of_equal_relevance_are_ordered_by_path_in_byte_order() {
    let searching_store = Searching::new("search-tie");
    let searched = searching_store.search(&searching("twin"));
    assert_eq!(hit_paths(&searched), ["notes/Twin.md", "notes/twin.md"]);
    let scores = scores(&searched);
    assert_eq!(scores[0].to_bits(), scores[1].to_bits());
}

// ---- a query is plain text ----

/// **Every character of a query is text.** A hyphen, a colon, a quote, a
/// caret, a star, a parenthesis and the words FTS5 reads as operators — each
/// answers the documents whose body holds those words, and none is reported
/// or refused: `NEAR`, `NOT`, `AND` and `OR` are words `notes/syntax.md`
/// holds, and `body:foo` is no column filter. A term the tokenizer reads
/// several words in matches them adjacent and in order, so `foo-bar` and
/// `bar-foo` part, and a star is no prefix: `sta*` does not reach `star`.
/// `OR` is no disjunction: `foo OR gardens` answers no document, since none
/// holds all three words. A
/// hyphen before a term negates nothing, and NUL separates terms as
/// whitespace does.
#[test]
fn a_query_is_plain_text() {
    let searching_store = Searching::new("search-plain-text");
    for query in [
        "foo-bar",
        "body:foo",
        "NEAR",
        "NOT",
        "AND OR",
        "and or not near",
        "\"quoted\"",
        "said \"quoted\"",
        "^caret",
        "star*",
        "(paren)",
        "(paren",
        "body:foo NEAR(NOT)",
        "foo AND bar",
    ] {
        let searched = searching_store.search(&searching(query));
        assert_eq!(
            hit_paths(&searched),
            ["notes/syntax.md"],
            "`{query}` was not read as the words it spells"
        );
        assert!(searched.unsatisfied.is_empty(), "`{query}`");
    }
    // As FTS5 syntax, `OR` would answer every document holding `gardens` and
    // `NOT` every one holding `foo` without `bar`.
    for query in ["bar-foo", "sta*", "foo OR gardens", "foo NOT bar-foo", "NEAR/2"] {
        assert_eq!(
            hit_paths(&searching_store.search(&searching(query))),
            Vec::<&str>::new(),
            "`{query}` answered a hit"
        );
    }
    let plain = hit_paths(&searching_store.search(&searching("lantern harbor")))
        .into_iter()
        .map(str::to_string)
        .collect::<Vec<String>>();
    for spelled in ["lantern -harbor", "lantern\0harbor", "  lantern\t\nharbor  "] {
        assert_eq!(
            hit_paths(&searching_store.search(&searching(spelled))),
            plain,
            "`{spelled:?}` is not the query `lantern harbor`"
        );
    }
}

/// **A query naming no term answers no hit, and reports nothing.** An empty
/// query, whitespace, NUL, and terms of punctuation alone name no word, so no
/// document holds every term; none of them is refused, and each answers a last
/// page.
#[test]
fn a_query_naming_no_term_answers_no_hit() {
    let searching_store = Searching::new("search-no-term");
    for query in ["", "   \t\n", "\0", "--", "\"", "\"\" -- !!", "* ^ ( )"] {
        let searched = searching_store.search(&searching(query));
        assert_eq!(hit_paths(&searched), Vec::<&str>::new(), "`{query:?}`");
        assert!(searched.unsatisfied.is_empty(), "`{query:?}`");
        assert_eq!(searched.next, None, "`{query:?}`");
    }
}

// ---- paging ----

/// Every hit a drain of `params` answers, `limit` hits a page, each page
/// continuing the cursor the one before it minted, on one snapshot.
fn drained(snapshot: &Snapshot, params: &SearchParams, limit: u32) -> Vec<(String, u64)> {
    let mut hits = Vec::new();
    let mut after: Option<Cursor> = None;
    loop {
        let mut page = params.clone().with_limit(limit);
        if let Some(cursor) = after.take() {
            page = page.with_after(cursor);
        }
        let searched = snapshot
            .search(&page, &declared())
            .unwrap_or_else(|refusal| panic!("a page of {page:?}: {refusal}"));
        assert!(
            searched.hits.len() <= limit as usize,
            "a page held more than its bound"
        );
        assert!(searched.moved.is_empty(), "one snapshot moved under a drain");
        hits.extend(
            searched
                .hits
                .iter()
                .map(|hit| (hit.path.as_str().to_string(), hit.score.get().to_bits())),
        );
        match searched.next {
            Some(cursor) => after = Some(cursor),
            None => return hits,
        }
    }
}

/// **A drain a page at a time answers the ranking one page does.** At a bound
/// of one, two and three hits a page, each page continuing the cursor the page
/// before minted on the same snapshot, the hits are the whole page's — every
/// score carried bit for bit — including where a page stops between the two
/// twins that score alike, and under a floor.
#[test]
fn a_drain_a_page_at_a_time_answers_the_ranking_one_page_does() {
    let searching_store = Searching::new("search-drain");
    let snapshot = searching_store.snapshot();
    for params in [
        searching("lantern"),
        searching("harbor lantern"),
        searching("twin"),
    ] {
        let whole = drained(&snapshot, &params, 100);
        assert!(whole.len() >= 2, "{params:?} answered {whole:?}");
        for limit in [1, 2, 3] {
            assert_eq!(
                drained(&snapshot, &params, limit),
                whole,
                "a drain of {params:?} at {limit} a page"
            );
        }
    }
    let lantern = drained(&snapshot, &searching("lantern"), 100);
    let floor = f64::from_bits(lantern[2].1);
    let floored = searching("lantern").with_min_score(Score::new(floor).expect("a score"));
    for limit in [1, 2, 3] {
        assert_eq!(
            drained(&snapshot, &floored, limit),
            drained(&snapshot, &floored, 100),
            "a floored drain at {limit} a page"
        );
    }
}

/// **A floor admits the hits scored at or above it, on the score's own
/// scale.** Floored at the third hit's score, `lantern` answers the hits down
/// to it and every hit tied with it; floored one unit in the last place above
/// the best score it answers nothing, and floored below zero, everything. A
/// floor bounds the answer and is never a refusal or a report.
#[test]
fn a_floor_admits_the_hits_scored_at_or_above_it() {
    let searching_store = Searching::new("search-floor");
    let whole = searching_store.search(&searching("lantern"));
    let scores = scores(&whole);
    let at = |floor: f64| {
        searching_store.search(
            &searching("lantern").with_min_score(Score::new(floor).expect("a finite floor")),
        )
    };
    let third = at(scores[2]);
    let expected: Vec<&str> = whole
        .hits
        .iter()
        .filter(|hit| hit.score.get() >= scores[2])
        .map(|hit| hit.path.as_str())
        .collect();
    assert_eq!(hit_paths(&third), expected);
    assert_eq!(hit_paths(&third)[..3], hit_paths(&whole)[..3]);
    assert!(third.unsatisfied.is_empty());
    assert_eq!(
        hit_paths(&at(f64::from_bits(scores[0].to_bits() + 1))),
        Vec::<&str>::new()
    );
    assert_eq!(hit_paths(&at(-1.0)), hit_paths(&whole));
}

/// **A cursor that is no hit's is refused, and so is one minted under a schema
/// fingerprint**: a ranking is no schema's order, so a cursor naming one names
/// a position in another order.
#[test]
fn a_cursor_that_is_no_position_among_hits_is_refused() {
    let searching_store = Searching::new("search-cursor-refused");
    let reading = searching_store.search(&searching("lantern")).snapshot;
    let document = searching("lantern").with_after(Cursor::new(
        reading.clone(),
        CursorKey::document(None, "notes/lantern.md"),
    ));
    assert_eq!(
        searching_store.refusal(&document),
        PageRefusal::NotAHitCursor
    );
    let hit = CursorKey::hit(Score::new(1.0).expect("a score"), "notes/lantern.md");
    let typed = searching("lantern").with_after(Cursor::new(
        norn_wire::Snapshot::new(
            reading.epoch.clone(),
            reading.generation,
            Some(SEARCH_SCHEMA.to_string()),
            None,
        ),
        hit.clone(),
    ));
    assert!(
        matches!(
            searching_store.refusal(&typed),
            PageRefusal::OrderChanged(_)
        ),
        "a hit cursor minted under a fingerprint was continued"
    );
    let raw = searching("lantern").with_after(Cursor::new(reading, hit));
    assert!(searching_store.search(&raw).moved.is_empty());
}

/// **A document edited is searched as it now reads, on a snapshot established
/// after the edit.** `notes/walk.md` rewritten to hold `beacon` and not
/// `lantern` is a hit for `beacon` and no longer one for `lantern harbor`; a
/// continuation of a cursor minted before the edit is answered from the new
/// snapshot and reports that the store's writes moved under it.
#[test]
fn a_document_edited_is_searched_as_it_now_reads() {
    let mut searching_store = Searching::new("search-edited");
    let before = searching_store.search(&searching("lantern harbor").with_limit(1));
    let cursor = before.next.clone().expect("a next page");
    assert!(hit_paths(&searching_store.search(&searching("lantern harbor")))
        .contains(&"notes/walk.md"));

    write_documents(
        &mut searching_store.store.begin_request(),
        &[document(
            "notes/walk.md",
            "hash-walk-2",
            "a beacon by the harbor\n",
        )],
    );
    assert_eq!(
        hit_paths(&searching_store.search(&searching("beacon"))),
        ["notes/walk.md"]
    );
    let after = searching_store.search(&searching("lantern harbor"));
    assert_eq!(
        hit_paths(&after),
        ["notes/lantern.md", "notes/Twin.md", "notes/twin.md"]
    );
    let continued = searching_store.search(&searching("lantern harbor").with_after(cursor));
    assert_eq!(continued.moved, vec![Moved::Generation]);
}
