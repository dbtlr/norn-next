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
    ContentModel, FindStatement, LexicalQuery, PageRefusal, ReadFilter, ReadStatement,
    SEARCH_STATEMENTS, SearchPlan, SearchStatement, Searched, Snapshot, SnapshotReader, Store,
    TagFact, TagSource,
};
use norn_testkit::explain::{Access, PlanRow, QueryPlan, ScanTarget};
use norn_wire::{
    Column, Cursor, CursorKey, CursorOrderChanged, Direction, FieldValue, FindParams, FindingKind,
    LadderDeclaration, Moved, PagedRows, Predicate, ResolutionTarget, Rung, RungSet, Score, Sort,
    SortKey, Unsatisfied, VaultAddress, VaultName,
};

// ---- fixtures ----

/// The fingerprint of the schema the fixture pins.
const SEARCH_SCHEMA: &str = "search-schema";

/// `status` declared as text.
fn declared() -> ContentModel {
    ContentModel::under(SEARCH_SCHEMA).declare("status")
}

/// The body every character FTS5 reads as query syntax stands in, as words.
const SYNTAX_BODY: &str = "foo-bar body:foo NEAR NOT AND OR said \"quoted\" ^caret star* (paren)\n";

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
    request.finish();
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
            .expect("a snapshot")
    }

    fn search(&self, params: &LexicalQuery) -> Searched {
        self.snapshot()
            .search(params, &declared())
            .unwrap_or_else(|refusal| panic!("a search of {params:?}: {refusal}"))
    }

    fn refusal(&self, params: &LexicalQuery) -> PageRefusal {
        self.snapshot()
            .search(params, &declared())
            .expect_err("a refused search")
    }

    fn plans(&self, params: &LexicalQuery) -> Vec<SearchPlan> {
        self.snapshot()
            .search_plans(params, &declared())
            .expect("the plans of a search")
    }
}

fn searching(query: &str) -> LexicalQuery {
    LexicalQuery::new(query)
}

/// The paths a page's hits stand at, in its order.
fn hit_paths(searched: &Searched) -> Vec<&str> {
    searched.hits.iter().map(|hit| hit.path.as_str()).collect()
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
/// whitespace does. So does every character Unicode reads as whitespace: a
/// no-break space parts `lantern` from `harbor` into two terms, which every
/// document holding both answers, rather than one term holding the two words
/// adjacent, which only `notes/lantern.md` holds.
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
    for query in [
        "bar-foo",
        "sta*",
        "foo OR gardens",
        "foo NOT bar-foo",
        "NEAR/2",
    ] {
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
    for spelled in [
        "lantern -harbor",
        "lantern\0harbor",
        "  lantern\t\nharbor  ",
        "lantern\u{00A0}harbor",
    ] {
        assert_eq!(
            hit_paths(&searching_store.search(&searching(spelled))),
            plain,
            "`{spelled:?}` is not the query `lantern harbor`"
        );
    }
}

/// **A quote inside a term is text, and parts the words either side of it.**
/// `foo"bar` holds the words `foo` and `bar`, and matches them adjacent and in
/// order: `notes/syntax.md` holds them so in `foo-bar`, and `notes/joined.md`
/// holds the one word `foobar`, which is not the term. `a"b` holds `a` and `b`,
/// which no document holds adjacent, and `notes/joined.md`'s one word `ab` is
/// not it either.
#[test]
fn a_quote_inside_a_term_is_text_that_parts_its_words() {
    let searching_store = Searching::with_documents(
        "search-inner-quote",
        vec![document("notes/joined.md", "hash-joined", "foobar ab\n")],
    );
    assert_eq!(
        hit_paths(&searching_store.search(&searching("foo\"bar"))),
        ["notes/syntax.md"]
    );
    assert_eq!(
        hit_paths(&searching_store.search(&searching("a\"b"))),
        Vec::<&str>::new()
    );
    assert_eq!(
        hit_paths(&searching_store.search(&searching("foobar ab"))),
        ["notes/joined.md"],
        "the fixture does not hold the joined words"
    );
}

/// **A query holding no word answers no hit, runs no lexical page, and is
/// reported.** An empty query, whitespace, NUL, punctuation, a no-break space,
/// and an emoji the full-text index reads no word in each hold no word, so
/// the page is empty and last, no lexical page runs, and the query is the
/// request's first unsatisfied part, before the conjunction's.
#[test]
fn a_query_holding_no_word_answers_no_hit_and_is_reported() {
    let searching_store = Searching::new("search-no-word");
    for query in [
        "",
        "   \t\n",
        "\0",
        "--",
        "\"",
        "\"\" -- !!",
        "* ^ ( )",
        "\u{00A0}",
        "\u{1F600}",
    ] {
        let searched = searching_store.search(&searching(query));
        assert_eq!(hit_paths(&searched), Vec::<&str>::new(), "`{query:?}`");
        assert_eq!(
            searched.unsatisfied,
            vec![Unsatisfied::query_names_no_word(query)],
            "`{query:?}`"
        );
        assert_eq!(searched.next, None, "`{query:?}`");
        assert!(
            !searching_store
                .plans(&searching(query))
                .iter()
                .any(|plan| plan.statement == ReadStatement::Search(SearchStatement::LexicalPage)),
            "`{query:?}` ran a lexical page"
        );
    }
    let narrowed = searching_store
        .search(&searching("--").with_predicates([Predicate::equal_to("statis", "open")]));
    assert_eq!(
        narrowed.unsatisfied,
        vec![
            Unsatisfied::query_names_no_word("--"),
            Unsatisfied::unknown_predicate_key("statis", vec!["status".to_string()]),
        ]
    );
}

/// **A term holding no word is dropped, and every other term still counts.**
/// `lantern --`, `-- lantern !!` and `lantern` followed by an emoji the index
/// reads no word in each answer what `lantern` answers, and report nothing. A
/// character the index does read a word in is a term like any other: `🙂`
/// answers the document holding it, and `lantern 🙂` answers nothing, since no
/// document holds both.
#[test]
fn a_term_holding_no_word_is_dropped_and_every_other_term_counts() {
    let searching_store = Searching::with_documents(
        "search-wordless-term",
        vec![document("notes/smile.md", "hash-smile", "\u{1F642}\n")],
    );
    let lantern = hit_paths(&searching_store.search(&searching("lantern")))
        .into_iter()
        .map(str::to_string)
        .collect::<Vec<String>>();
    assert_eq!(lantern.len(), 5);
    for query in ["lantern --", "-- lantern !!", "lantern \u{1F600}"] {
        let searched = searching_store.search(&searching(query));
        assert_eq!(hit_paths(&searched), lantern, "`{query:?}`");
        assert!(searched.unsatisfied.is_empty(), "`{query:?}`");
    }
    assert_eq!(
        hit_paths(&searching_store.search(&searching("\u{1F642}"))),
        ["notes/smile.md"]
    );
    assert_eq!(
        hit_paths(&searching_store.search(&searching("lantern \u{1F642}"))),
        Vec::<&str>::new()
    );
}

/// **A word is compared by its first 32768 bytes, in the index and in a query
/// alike.** FTS5 keeps at most 32768 bytes of a token, so a query of `a` 32769
/// times answers the document holding `a` 32768 times and then `z`: to the
/// index both are the same 32768 bytes of `a`. It is no prefix match: `a`
/// 32767 times answers nothing. A `matches` part reads words the same way.
#[test]
fn a_word_is_compared_by_its_first_32768_bytes() {
    let searching_store = Searching::with_documents(
        "search-long-word",
        vec![document(
            "notes/long.md",
            "hash-long",
            &format!("{}z\n", "a".repeat(32_768)),
        )],
    );
    let longer = "a".repeat(32_769);
    assert_eq!(
        hit_paths(&searching_store.search(&searching(&longer))),
        ["notes/long.md"]
    );
    assert_eq!(
        hit_paths(&searching_store.search(&searching(&"a".repeat(32_767)))),
        Vec::<&str>::new()
    );
    let found = searching_store
        .snapshot()
        .find(
            &FindParams::new(VaultAddress::name(
                VaultName::new("notes").expect("a vault name"),
            ))
            .with_predicates([Predicate::matches(longer)]),
            &declared(),
        )
        .expect("a find matching the longer word");
    assert_eq!(
        found
            .rows
            .iter()
            .map(|row| row.path.as_str())
            .collect::<Vec<&str>>(),
        ["notes/long.md"]
    );
}

// ---- paging ----

/// Every hit a drain of `params` answers, `limit` hits a page, each page
/// continuing the cursor the one before it minted, on one snapshot. A drain
/// that has not ended after more pages than the fixture has documents is a
/// cursor that does not advance.
fn drained(snapshot: &Snapshot, params: &LexicalQuery, limit: u32) -> Vec<(String, u64)> {
    let mut hits = Vec::new();
    let mut after: Option<Cursor> = None;
    for _ in 0..=fixture().len() {
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
        assert!(
            searched.moved.is_empty(),
            "one snapshot moved under a drain"
        );
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
    panic!("a drain of {params:?} at {limit} a page did not end: {hits:?}");
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

/// **A cursor that is no hit's is refused, and so is a hit's carrying a
/// schema fingerprint**: a ranking is no schema's order and no page of hits
/// mints one, so such a cursor names no position among hits.
#[test]
fn a_cursor_that_is_no_position_among_hits_is_refused() {
    let searching_store = Searching::new("search-cursor-refused");
    let reading = searching_store.search(&searching("lantern")).snapshot;
    let document = searching("lantern").with_after(Cursor::new(
        reading.clone(),
        CursorKey::document(
            Sort::new(SortKey::path(), Direction::Ascending),
            None,
            "notes/lantern.md",
        ),
    ));
    assert_eq!(
        searching_store.refusal(&document),
        PageRefusal::CursorNotTaken {
            cursor: PagedRows::Document,
            paged: PagedRows::Hit,
        }
    );
    let hit = CursorKey::hit(
        RungSet::lexical(),
        Score::new(1.0).expect("a score"),
        "notes/lantern.md",
    );
    let typed = searching("lantern").with_after(Cursor::new(
        norn_wire::Snapshot::new(
            reading.epoch.clone(),
            reading.generation,
            Some(SEARCH_SCHEMA.to_string()),
            None,
        ),
        hit.clone(),
    ));
    assert_eq!(
        searching_store.refusal(&typed),
        PageRefusal::CursorNotTaken {
            cursor: PagedRows::Hit,
            paged: PagedRows::Hit,
        },
        "a hit cursor carrying a fingerprint was not refused as not taken"
    );
    let fused = CursorKey::hit(
        RungSet::of([Rung::Lexical, Rung::Vector]).expect("a ladder"),
        Score::new(1.0).expect("a score"),
        "notes/lantern.md",
    );
    let typed_and_fused = searching("lantern").with_after(Cursor::new(
        norn_wire::Snapshot::new(
            reading.epoch.clone(),
            reading.generation,
            Some(SEARCH_SCHEMA.to_string()),
            None,
        ),
        fused,
    ));
    assert_eq!(
        searching_store.refusal(&typed_and_fused),
        PageRefusal::CursorNotTaken {
            cursor: PagedRows::Hit,
            paged: PagedRows::Hit,
        },
        "a fingerprinted hit cursor under another ladder was judged by its ladder before its fingerprint"
    );
    let raw = searching("lantern").with_after(Cursor::new(reading.clone(), hit));
    assert!(searching_store.search(&raw).moved.is_empty());
}

/// **A hit cursor ranked by another ladder is refused, naming both**: this
/// rung ranks by the lexical ladder alone, and a score and a path from a fused
/// ranking are a position on another scale.
#[test]
fn a_hit_cursor_ranked_by_another_ladder_is_refused_naming_both() {
    let searching_store = Searching::new("search-cursor-ladder");
    let reading = searching_store.search(&searching("lantern")).snapshot;
    let fused = RungSet::of([Rung::Lexical, Rung::Vector]).expect("a ladder");
    let continued = searching("lantern").with_after(Cursor::new(
        reading,
        CursorKey::hit(
            fused.clone(),
            Score::new(1.0).expect("a score"),
            "notes/lantern.md",
        ),
    ));
    let refusal = searching_store.refusal(&continued);
    assert_eq!(
        refusal,
        PageRefusal::OrderChanged(
            CursorOrderChanged::minted_raw(None).in_ladders(fused, RungSet::lexical())
        )
    );
    assert_eq!(
        refusal.to_string(),
        "the cursor was ranked by the ladder [lexical, vector], and the request ranks by the ladder [lexical]"
    );
}

/// **A lexical page declares the lexical floor as its ladder, and the cursor
/// it mints names the rung set that declaration names**, so the ladder a
/// report declares and the ladder its continuation is judged by are one fact.
#[test]
fn a_page_declares_the_lexical_ladder_its_cursor_names() {
    let searching_store = Searching::new("search-ladder-declared");
    let searched = searching_store.search(&searching("lantern").with_limit(1));
    let next = searched.next.clone().expect("a next page");
    let (_, _, report) = searched.into_report();
    assert_eq!(report.ladder, LadderDeclaration::lexical());
    assert_eq!(report.page.next.as_ref(), Some(&next));
    let CursorKey::Hit { ladder, .. } = next.key() else {
        panic!("a search minted a cursor that is no hit's: {next:?}");
    };
    assert_eq!(ladder, &report.ladder.rung_set());
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
    assert!(
        hit_paths(&searching_store.search(&searching("lantern harbor"))).contains(&"notes/walk.md")
    );

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

// ---- the conjunction ----

/// **A part of the conjunction narrows the hits as it narrows a find**, and the
/// hits it keeps stay in rank order: a tag, a path glob, a field's value, a
/// finding standing over the document, and a `matches` part — which keeps
/// full-text syntax, so `harbor NOT walk` excludes the document holding
/// `walk` — each keep the lantern hits they name, and two parts keep what both
/// do.
#[test]
fn a_part_narrows_the_hits_it_keeps_in_rank_order() {
    let searching_store = Searching::new("search-narrowed");
    let whole = searching_store.search(&searching("lantern"));
    let kept = |predicates: Vec<Predicate>| {
        let searched =
            searching_store.search(&searching("lantern").with_predicates(predicates.clone()));
        assert!(
            searched.unsatisfied.is_empty(),
            "{predicates:?}: {:?}",
            searched.unsatisfied
        );
        hit_paths(&searched)
            .into_iter()
            .map(str::to_string)
            .collect::<Vec<String>>()
    };
    let in_rank_order = |paths: &[&str]| {
        hit_paths(&whole)
            .into_iter()
            .filter(|path| paths.contains(path))
            .map(str::to_string)
            .collect::<Vec<String>>()
    };
    assert_eq!(
        kept(vec![Predicate::tag("draft")]),
        in_rank_order(&["notes/lantern.md"])
    );
    assert_eq!(
        kept(vec![Predicate::path("notes/*")]),
        in_rank_order(&[
            "notes/lantern.md",
            "notes/Twin.md",
            "notes/twin.md",
            "notes/walk.md"
        ])
    );
    assert_eq!(
        kept(vec![Predicate::equal_to("status", "closed")]),
        in_rank_order(&["notes/walk.md"])
    );
    assert_eq!(
        kept(vec![Predicate::has_finding(FindingKind::BodyBytesNotUtf8)]),
        in_rank_order(&["other/finding.md"])
    );
    assert_eq!(
        kept(vec![Predicate::matches("harbor NOT walk")]),
        in_rank_order(&["notes/lantern.md", "notes/Twin.md", "notes/twin.md"])
    );
    assert_eq!(
        kept(vec![
            Predicate::path("notes/*"),
            Predicate::not_equal_to("status", "open"),
        ]),
        in_rank_order(&["notes/Twin.md", "notes/twin.md", "notes/walk.md"])
    );
}

/// **A part a search cannot apply is reported as a find reports it.** A key
/// outside the field universe is reported with the keys near it and filters
/// nothing; a `resolves` part is not applicable to a search and filters
/// nothing; a match part the engine cannot parse and a glob that does not
/// parse are reported, and empty the page. A `links_to` part whose target names
/// no document is reported, and empties the page, as a find reports it.
#[test]
fn a_part_a_search_cannot_apply_is_reported_as_a_find_reports_it() {
    let searching_store = Searching::new("search-unsatisfied");
    let whole = hit_paths(&searching_store.search(&searching("lantern")))
        .into_iter()
        .map(str::to_string)
        .collect::<Vec<String>>();
    let target = ResolutionTarget::new("lantern").expect("a target");

    let unknown = searching_store
        .search(&searching("lantern").with_predicates([Predicate::equal_to("statis", "open")]));
    assert_eq!(hit_paths(&unknown), whole);
    assert_eq!(
        unknown.unsatisfied,
        vec![Unsatisfied::unknown_predicate_key(
            "statis",
            vec!["status".to_string()]
        )]
    );

    let resolves = searching_store
        .search(&searching("lantern").with_predicates([Predicate::resolves(target.clone())]));
    assert_eq!(hit_paths(&resolves), whole);
    assert_eq!(
        resolves.unsatisfied,
        vec![Unsatisfied::resolves_not_applicable(target.clone())]
    );

    let malformed = searching_store
        .search(&searching("lantern").with_predicates([Predicate::matches("\"unterminated")]));
    assert_eq!(hit_paths(&malformed), Vec::<&str>::new());
    assert!(matches!(
        malformed.unsatisfied.as_slice(),
        [Unsatisfied::MalformedQuery { .. }]
    ));

    let glob = searching_store.search(&searching("lantern").with_predicates([Predicate::path("")]));
    assert_eq!(hit_paths(&glob), Vec::<&str>::new());
    assert_eq!(
        glob.unsatisfied,
        vec![Unsatisfied::malformed_glob("", "a pattern cannot be empty")]
    );

    let nowhere = ResolutionTarget::new("nowhere").expect("a target");
    let links_to = searching_store
        .search(&searching("lantern").with_predicates([Predicate::links_to(nowhere.clone())]));
    assert_eq!(hit_paths(&links_to), Vec::<&str>::new());
    assert_eq!(
        links_to.unsatisfied,
        vec![Unsatisfied::links_to_unknown(nowhere)]
    );
}

// ---- the columns ----

/// **A hit carries the row its columns name, and a search naming none
/// hydrates nothing.** With no column, no hit carries a row and the search runs
/// its page and the fingerprint read its declaration is judged by, and no
/// hydration; with the fields and the body, each hit carries its own document's, in
/// rank order, and one hydration reads exactly the hits' rows. A projected key
/// outside the field universe is reported with the keys near it, after the
/// conjunction's parts. A links column is refused, as a find refuses it.
#[test]
fn a_hit_carries_the_row_its_columns_name() {
    let searching_store = Searching::new("search-columns");
    let bare = searching("lantern harbor");
    let plans = searching_store.plans(&bare);
    assert_eq!(
        plans
            .iter()
            .map(|plan| plan.statement)
            .collect::<Vec<ReadStatement>>(),
        [
            ReadStatement::Find(FindStatement::ActiveFingerprint),
            ReadStatement::Search(SearchStatement::LexicalPage),
        ],
        "a search naming no column ran {plans:?}"
    );
    assert_eq!(searching_store.search(&bare).work.documents_hydrated, 0);

    let projected = searching_store.search(
        &bare
            .clone()
            .with_limit(3)
            .with_predicates([Predicate::equal_to("statis", "x")])
            .with_columns([
                Column::field("status"),
                Column::field("stat"),
                Column::body(),
            ]),
    );
    assert_eq!(
        hit_paths(&projected),
        ["notes/lantern.md", "notes/Twin.md", "notes/twin.md"]
    );
    assert_eq!(projected.work.documents_hydrated, 3);
    let row = |at: usize| {
        projected.hits[at]
            .document
            .as_ref()
            .expect("a hit naming columns carries a row")
    };
    for (at, hit) in projected.hits.iter().enumerate() {
        assert_eq!(row(at).path, hit.path);
    }
    assert_eq!(
        row(0)
            .fields
            .as_ref()
            .and_then(|fields| fields.get("status")),
        Some(&FieldValue::scalar("open"))
    );
    assert_eq!(
        row(0).body.as_ref().map(|body| body.text()),
        Some("lantern lantern lantern harbor\n")
    );
    assert_eq!(
        row(1).body.as_ref().map(|body| body.text()),
        Some("harbor lantern twin\n")
    );
    assert_eq!(
        projected.unsatisfied,
        vec![
            Unsatisfied::unknown_predicate_key("statis", vec!["status".to_string()]),
            Unsatisfied::unknown_projection_key("stat", vec!["status".to_string()]),
        ]
    );

    let linked = searching_store.search(&bare.clone().with_columns([Column::links()]));
    assert!(
        !linked.hits.is_empty()
            && linked
                .hits
                .iter()
                .all(|hit| hit.document.as_ref().is_some_and(|row| row.links.is_some())),
        "a hit's row does not carry the links column it names"
    );
}

// ---- what a ranking above the floor draws on ----

/// A find of `predicates` over the fixture's vault, a page of `limit`
/// documents at a time.
fn candidates_of(predicates: Vec<Predicate>, limit: u32) -> FindParams {
    FindParams::new(VaultAddress::name(
        VaultName::new("search").expect("a vault name"),
    ))
    .with_predicates(predicates)
    .with_limit(limit)
}

/// Every candidate `params` admits, drained a page at a time, and the reports
/// of its first page.
fn every_candidate(
    snapshot: &Snapshot,
    params: FindParams,
) -> (Vec<norn_store::Candidate>, Vec<Unsatisfied>) {
    let first = snapshot
        .search_candidates(&params, &declared())
        .unwrap_or_else(|refusal| panic!("the candidates of {params:?}: {refusal}"));
    let unsatisfied = first.unsatisfied.clone();
    let mut every = first.candidates;
    let mut next = first.next;
    while let Some(cursor) = next {
        let page = snapshot
            .search_candidates(&params.clone().with_after(cursor), &declared())
            .expect("a continued page of candidates");
        every.extend(page.candidates);
        next = page.next;
    }
    (every, unsatisfied)
}

/// **A search's candidates are the documents its conjunction admits, in a
/// find's path order, as a search reads the conjunction.** A tag part narrows them to the
/// one document carrying the tag; a `resolves` part is not applicable to a
/// search, so it is reported and filters nothing, where a find of the same
/// part answers the documents its target resolves to; and a page at a time
/// drains every document once.
#[test]
fn a_search_admits_as_candidates_what_its_conjunction_admits() {
    let searching_store = Searching::new("search-candidates");
    let snapshot = searching_store.snapshot();
    let paths = |candidates: &[norn_store::Candidate]| {
        candidates
            .iter()
            .map(|candidate| candidate.path().to_string())
            .collect::<Vec<String>>()
    };

    let (every, unsatisfied) = every_candidate(&snapshot, candidates_of(Vec::new(), 3));
    let (whole, _) = every_candidate(&snapshot, candidates_of(Vec::new(), 1024));
    assert_eq!(
        paths(&every),
        paths(&whole),
        "a drain a page at a time is not the path order one page reads"
    );
    let distinct: std::collections::BTreeSet<String> = paths(&every).into_iter().collect();
    assert_eq!(
        (every.len(), distinct.len()),
        (14, 14),
        "a drain is not every document once"
    );
    assert!(unsatisfied.is_empty());

    let (tagged, _) = every_candidate(&snapshot, candidates_of(vec![Predicate::tag("draft")], 3));
    assert_eq!(paths(&tagged), ["notes/lantern.md"]);

    let target = ResolutionTarget::new("lantern").expect("a target");
    let (resolved, unsatisfied) = every_candidate(
        &snapshot,
        candidates_of(vec![Predicate::resolves(target.clone())], 1024),
    );
    assert_eq!(paths(&resolved), paths(&every));
    assert_eq!(
        unsatisfied,
        vec![Unsatisfied::resolves_not_applicable(target.clone())]
    );
    let found = snapshot
        .find(
            &candidates_of(vec![Predicate::resolves(target)], 1024),
            &declared(),
        )
        .expect("a find of the same part");
    assert_eq!(
        found
            .rows
            .iter()
            .map(|row| row.path.as_str())
            .collect::<Vec<_>>(),
        ["notes/lantern.md"],
        "the find the negative control runs does not answer the part"
    );
}

/// **A ranked hit is hydrated as a find's row is**: in the order it is handed
/// in, at the score it is handed with, carrying the columns it names; a key
/// outside the field universe is reported; a hit naming no column hydrates
/// nothing.
#[test]
fn a_ranked_hit_carries_the_row_its_columns_name() {
    let searching_store = Searching::new("search-hydrate-hits");
    let snapshot = searching_store.snapshot();
    let (every, _) = every_candidate(&snapshot, candidates_of(Vec::new(), 1024));
    let at = |path: &str| {
        every
            .iter()
            .find(|candidate| candidate.path() == path)
            .cloned()
            .expect("a candidate at the path")
    };
    let ranked = vec![
        (at("notes/walk.md"), Score::new(2.0).expect("a score")),
        (at("notes/lantern.md"), Score::new(1.0).expect("a score")),
    ];

    let bare = snapshot
        .hydrate_hits(&ranked, &[], &declared())
        .expect("hits naming no column");
    assert_eq!(bare.work.documents_hydrated, 0);
    assert!(bare.hits.iter().all(|hit| hit.document.is_none()));
    assert_eq!(
        bare.hits
            .iter()
            .map(|hit| (hit.path.as_str(), hit.score.get()))
            .collect::<Vec<_>>(),
        [("notes/walk.md", 2.0), ("notes/lantern.md", 1.0)]
    );

    let projected = snapshot
        .hydrate_hits(
            &ranked,
            &[Column::field("status"), Column::field("stat")],
            &declared(),
        )
        .expect("hits naming columns");
    assert_eq!(projected.work.documents_hydrated, 2);
    let status = |at: usize| {
        projected.hits[at]
            .document
            .as_ref()
            .and_then(|row| row.fields.as_ref())
            .and_then(|fields| fields.get("status"))
            .cloned()
    };
    assert_eq!(status(0), Some(FieldValue::scalar("closed")));
    assert_eq!(status(1), Some(FieldValue::scalar("open")));
    assert_eq!(
        projected.unsatisfied,
        vec![Unsatisfied::unknown_projection_key(
            "stat",
            vec!["status".to_string()]
        )]
    );
}

/// **A fused ranking's cursor is judged as a lexical page's is, beside the
/// sidecar state its answer read**: the same state reports nothing moved,
/// another reports the sidecar moved, and a cursor ranked by another ladder is
/// refused naming both.
#[test]
fn a_hit_cursor_is_judged_beside_the_sidecar_state_its_answer_read() {
    let searching_store = Searching::new("search-judge-sidecar");
    let mut reading = searching_store.search(&searching("lantern")).snapshot;
    let snapshot = searching_store.snapshot();
    let minted = norn_wire::SidecarRevision::new("sidecar", 4);
    reading.sidecar_revision = Some(minted.clone());
    let fused = RungSet::of([Rung::Lexical, Rung::Vector]).expect("a ladder");
    let cursor = Cursor::new(
        reading,
        CursorKey::hit(
            fused.clone(),
            Score::new(0.5).expect("a score"),
            "notes/lantern.md",
        ),
    );

    let same = snapshot
        .judge_hit_cursor(&cursor, &fused, Some(minted))
        .expect("a continuation under the same sidecar state");
    assert_eq!(same.path, "notes/lantern.md");
    assert!(same.moved.is_empty());
    let moved = snapshot
        .judge_hit_cursor(
            &cursor,
            &fused,
            Some(norn_wire::SidecarRevision::new("sidecar", 5)),
        )
        .expect("a continuation under a moved sidecar");
    assert_eq!(moved.moved, vec![Moved::SidecarRevision]);
    assert_eq!(
        snapshot.judge_hit_cursor(&cursor, &RungSet::lexical(), None),
        Err(PageRefusal::OrderChanged(
            CursorOrderChanged::minted_raw(None).in_ladders(fused, RungSet::lexical())
        ))
    );
}

/// The paths `held` answers, in its order.
fn held_paths(held: &norn_store::Held) -> Vec<&str> {
    held.candidates
        .iter()
        .map(|candidate| candidate.path())
        .collect()
}

/// **A snapshot answers which of a set of paths it holds, each once, in path
/// byte order, as candidates its hits are hydrated by.** A path the snapshot
/// holds no document at — one never written, one differing from a held path
/// in ASCII case alone, one a death retracted after the snapshot's writes
/// were read — is left out; a path named twice is answered once; a question
/// naming no path answers nothing and runs no statement. The reading is the
/// snapshot's, naming no fingerprint.
#[test]
fn a_snapshot_answers_which_of_a_set_of_paths_it_holds() {
    let mut searching_store = Searching::new("search-held");
    let snapshot = searching_store.snapshot();
    let held = snapshot
        .held_candidates(&[
            "notes/walk.md",
            "notes/nowhere.md",
            "notes/LANTERN.md",
            "notes/Twin.md",
            "notes/walk.md",
            "notes/lantern.md",
        ])
        .expect("the held paths");
    assert_eq!(
        held_paths(&held),
        ["notes/Twin.md", "notes/lantern.md", "notes/walk.md"]
    );
    assert_eq!(held.snapshot.schema_fingerprint, None);
    assert_eq!(
        held.snapshot.generation,
        u64::try_from(snapshot.reading().write_generation()).expect("a generation")
    );
    let ranked: Vec<(norn_store::Candidate, Score)> = held
        .candidates
        .iter()
        .cloned()
        .map(|candidate| (candidate, Score::new(1.0).expect("a score")))
        .collect();
    let hydrated = snapshot
        .hydrate_hits(&ranked, &[Column::field("status")], &declared())
        .expect("the held documents hydrated");
    assert_eq!(hydrated.work.documents_hydrated, 3);

    let none = snapshot.held_candidates(&[]).expect("no path");
    assert!(none.candidates.is_empty());
    assert!(
        snapshot
            .held_candidates_plans(&[])
            .expect("no plan")
            .is_empty()
    );
    drop(snapshot);

    let mut request = searching_store.store.begin_request();
    crate::common::record_death(
        &mut request,
        &crate::common::path("notes/walk.md"),
        norn_store::Provenance::WatcherRemoval,
    );
    request.finish();
    let after = searching_store.snapshot();
    assert_eq!(
        held_paths(
            &after
                .held_candidates(&["notes/walk.md", "notes/lantern.md"])
                .expect("the held paths")
        ),
        ["notes/lantern.md"]
    );
}

/// **The feed rows past a pair of generations are counted in rows, each feed
/// to the bound.** Past the snapshot's own generation nothing stands; past
/// the generation read before a write that derives three documents and one
/// that records five deaths, the documents feed holds three rows and the
/// death feed five, though the deaths took one generation; a bound below a
/// feed's rows counts that feed to the bound; and each feed is counted past
/// its own generation.
#[test]
fn the_feed_rows_past_a_generation_are_counted_in_rows_to_a_bound() {
    let mut searching_store = Searching::new("search-feed-rows");
    let before = searching_store.snapshot().reading().write_generation();
    assert_eq!(
        searching_store
            .snapshot()
            .feed_rows_after(before, before, 100)
            .expect("the feed rows"),
        norn_store::FeedRows::default()
    );

    write_documents(
        &mut searching_store.store.begin_request(),
        &bulk(3, |at| format!("fresh body {at}\n")),
    );
    let deaths: Vec<norn_store::Change> = (0..5)
        .map(|at| norn_store::Change::Death {
            path: crate::common::path(&format!("filler/{at}.md")),
            provenance: norn_store::Provenance::WatcherRemoval,
        })
        .collect();
    searching_store
        .store
        .begin_request()
        .apply_increment(norn_store::IncrementProvenance::Derived, deaths, &[])
        .expect("five deaths in one write");
    let snapshot = searching_store.snapshot();
    let now = snapshot.reading().write_generation();
    assert_eq!(now, before + 2, "the deaths took more than one generation");

    let counted = |documents_after, tombstones_after, bound| {
        snapshot
            .feed_rows_after(documents_after, tombstones_after, bound)
            .expect("the feed rows")
    };
    assert_eq!(
        counted(before, before, 100),
        norn_store::FeedRows {
            documents: 3,
            tombstones: 5
        }
    );
    assert_eq!(
        counted(before, before, 4),
        norn_store::FeedRows {
            documents: 3,
            tombstones: 4
        }
    );
    assert_eq!(
        counted(now, before, 100),
        norn_store::FeedRows {
            documents: 0,
            tombstones: 5
        }
    );
    assert_eq!(counted(now, now, 100), norn_store::FeedRows::default());
}

// ---- the plan bars ----

/// The plan the store reported for one statement, in the harness's shape.
fn plan(emitted: &SearchPlan) -> QueryPlan {
    QueryPlan::new(
        emitted.plan.sql.clone(),
        emitted
            .plan
            .steps
            .iter()
            .map(|step| PlanRow::new(step.id, step.parent, step.detail.clone()))
            .collect(),
    )
}

/// The one plan `plans` holds for `statement`, with the filters it recorded.
fn plan_of(plans: &[SearchPlan], statement: SearchStatement) -> (QueryPlan, Vec<ReadFilter>) {
    let matching: Vec<&SearchPlan> = plans
        .iter()
        .filter(|plan| plan.statement == ReadStatement::Search(statement))
        .collect();
    assert_eq!(
        matching.len(),
        1,
        "the search runs {statement:?} {} times: {plans:?}",
        matching.len()
    );
    (plan(matching[0]), matching[0].filters.clone())
}

/// `plan` with every row's detail rewritten by `edit`: a plan a control
/// judges in place of the one SQLite reported.
fn rewritten(plan: &QueryPlan, edit: impl Fn(&str) -> String) -> QueryPlan {
    QueryPlan::new(
        plan.sql(),
        plan.rows()
            .iter()
            .map(|row| PlanRow::new(row.id, row.parent, edit(&row.detail)))
            .collect(),
    )
}

/// `plan` with its rows in another order: a plan a control judges in place of
/// the one SQLite reported.
fn reordered(plan: &QueryPlan, order: impl Fn(&[PlanRow]) -> Vec<PlanRow>) -> QueryPlan {
    QueryPlan::new(plan.sql(), order(plan.rows()))
}

/// Which test bars each statement the builder names. Exhaustive, so a
/// statement added to [`SearchStatement`] does not compile until its author
/// names the bar.
fn statement_barred_by(statement: SearchStatement) -> &'static str {
    match statement {
        SearchStatement::LexicalPage => {
            "a_lexical_page_is_driven_by_the_full_text_index_and_sorts_what_it_matched"
        }
        SearchStatement::HeldPaths => "the_held_paths_reach_each_document_by_one_seek_of_its_path",
        SearchStatement::FeedRowsAfter => {
            "the_feed_rows_are_counted_over_a_range_of_each_feeds_index"
        }
    }
}

/// **Every statement the search builder names carries a bar, and one bar
/// judges each**: the statements the bars judge cover the enumeration's slots
/// exactly once.
#[test]
fn the_search_bars_cover_every_statement_once() {
    let judged = [
        SearchStatement::LexicalPage,
        SearchStatement::HeldPaths,
        SearchStatement::FeedRowsAfter,
    ];
    let mut slots: Vec<usize> = judged.iter().map(|statement| statement.slot()).collect();
    slots.sort_unstable();
    assert_eq!(slots, (0..SEARCH_STATEMENTS).collect::<Vec<usize>>());
    for (slot, statement) in SearchStatement::all().into_iter().enumerate() {
        assert_eq!(statement.slot(), slot, "{statement:?} claims another slot");
    }
    let bars: std::collections::BTreeSet<&str> =
        judged.into_iter().map(statement_barred_by).collect();
    assert_eq!(
        bars,
        [
            "a_lexical_page_is_driven_by_the_full_text_index_and_sorts_what_it_matched",
            "the_held_paths_reach_each_document_by_one_seek_of_its_path",
            "the_feed_rows_are_counted_over_a_range_of_each_feeds_index",
        ]
        .into_iter()
        .collect()
    );
}

/// The plan of the one statement `plans` holds, which is `statement`.
fn only_plan(plans: &[SearchPlan], statement: SearchStatement) -> QueryPlan {
    let [emitted] = plans else {
        panic!("the question ran another number of statements than one: {plans:?}");
    };
    assert_eq!(emitted.statement, ReadStatement::Search(statement));
    assert!(emitted.filters.is_empty());
    plan(emitted)
}

/// Judge the held paths' plan: the path list is the outer loop, read as a
/// constrained virtual table, and each path reaches its document by a seek of
/// `documents_path` at the path; nothing is read end to end and nothing is
/// sorted.
fn judge_held(plan: &QueryPlan) {
    plan.assert_no_full_scan();
    plan.assert_no_temp_btree();
    let list: Vec<&PlanRow> = plan
        .rows()
        .iter()
        .filter(|row| row.detail.split_whitespace().nth(1) == Some("j"))
        .collect();
    assert!(
        matches!(
            list.as_slice(),
            [row] if matches!(row.scan_target(), Some(ScanTarget::VirtualTable { .. }))
        ),
        "the path list is not read once as the table function it is: {:?}\nemitted SQL: {}",
        plan.rows(),
        plan.sql()
    );
    let documents = rows_of(plan, "d");
    documents.assert_searches_through("documents", Access::Index("documents_path"));
    documents.assert_search_constraint("documents", "(path=?)");
    let at = |alias: &str| {
        plan.rows()
            .iter()
            .position(|row| row.detail.split_whitespace().nth(1) == Some(alias))
            .unwrap_or_else(|| panic!("no row reads `{alias}`: {:?}", plan.rows()))
    };
    assert!(
        at("j") < at("d"),
        "the path list is not the outer loop: {:?}\nemitted SQL: {}",
        plan.rows(),
        plan.sql()
    );
}

/// **The held paths reach each document by one seek of its path**, so the
/// question costs the paths it names and never the vault, whether it names
/// one path, several, or paths the snapshot does not hold.
///
/// Controls: the documents read end to end, and read before the path list.
/// Each fails.
#[test]
fn the_held_paths_reach_each_document_by_one_seek_of_its_path() {
    let searching_store = Searching::new("search-held-plan");
    let snapshot = searching_store.snapshot();
    let statement = SearchStatement::HeldPaths;
    for paths in [
        vec!["notes/walk.md"],
        vec!["notes/walk.md", "notes/lantern.md", "notes/nowhere.md"],
    ] {
        let plans = snapshot
            .held_candidates_plans(&paths)
            .expect("the held paths' plans");
        judge_held(&only_plan(&plans, statement));
    }
    let plans = snapshot
        .held_candidates_plans(&["notes/walk.md"])
        .expect("the held paths' plans");
    let emitted = only_plan(&plans, statement);
    failure_of("the documents read end to end", || {
        judge_held(&rewritten(&emitted, |detail| {
            if detail.starts_with("SEARCH d ") {
                "SCAN d".to_string()
            } else {
                detail.to_string()
            }
        }))
    });
    failure_of("the documents read before the path list", || {
        judge_held(&reordered(&emitted, |rows| {
            rows.iter().rev().cloned().collect()
        }))
    });
}

/// Judge the feed rows' plan: each feed is read through its change-feed index
/// as a range past the generation, and nothing is read end to end or sorted.
fn judge_feed_rows(plan: &QueryPlan) {
    plan.assert_no_full_scan();
    plan.assert_no_temp_btree();
    for (alias, table, index) in [
        ("d", "documents", "documents_change_feed"),
        ("t", "tombstones", "tombstones_change_feed"),
    ] {
        let reads = rows_of(plan, alias);
        reads.assert_searches_through(table, Access::Index(index));
        reads.assert_search_constraint(table, "(generation>?)");
    }
}

/// **The feed rows are counted over a range of each feed's index**, past the
/// generation it is asked about and stopped at the bound, so the count costs
/// at most twice the bound whatever the feeds hold.
///
/// Control: the death feed read end to end. It fails.
#[test]
fn the_feed_rows_are_counted_over_a_range_of_each_feeds_index() {
    let searching_store = Searching::new("search-feed-rows-plan");
    let snapshot = searching_store.snapshot();
    let generation = snapshot.reading().write_generation();
    let plans = snapshot
        .feed_rows_after_plans(generation, generation - 1, 8)
        .expect("the feed rows' plans");
    let emitted = only_plan(&plans, SearchStatement::FeedRowsAfter);
    judge_feed_rows(&emitted);
    failure_of("the death feed read end to end", || {
        judge_feed_rows(&rewritten(&emitted, |detail| {
            if detail.starts_with("SEARCH t ") {
                "SCAN t".to_string()
            } else {
                detail.to_string()
            }
        }))
    });
}

/// Judge a lexical page's plan.
///
/// The bundled FTS5 plans a read as
/// `SCAN <table> VIRTUAL TABLE INDEX <idxNum>:<idxStr>`: the module's index
/// choice, where `idxStr` spells the constraints SQLite
/// handed it — `M` and a column number for a `MATCH` — and `idxNum` is a bit
/// set whose order bits say the module hands its rows back sorted. So the
/// searching row is the page's read of the index, aliased `ft`: one scan with
/// a `MATCH` selection and `idxNum` zero, sorted by nothing. It is the outer
/// loop — it stands before every other read of the page — and each match
/// reaches its document by row id. The page sorts once: one temporary B-tree
/// for its `ORDER BY`, which every match past the page's position fills. And
/// nothing is read end to end.
fn judge_lexical(plan: &QueryPlan) {
    plan.assert_no_full_scan();
    let searching = rows_of(plan, "ft");
    let [row] = searching.rows() else {
        panic!(
            "the page does not read the full-text index once: {:?}\nemitted SQL: {}",
            plan.rows(),
            plan.sql()
        );
    };
    assert_eq!(plan.table_of("ft"), "documents_fts");
    assert!(
        matches!(
            row.scan_target(),
            Some(ScanTarget::VirtualTable { index_number: "0", specification, .. })
                if specification.starts_with('M')
        ),
        "the page does not read `documents_fts` through its MATCH selection alone: {row}\n\
         the bar reads the bundled FTS5's `INDEX <idxNum>:<idxStr>` spelling of its index \
         choice, so a SQLite version bump that respelled it is a likely cause\n\
         emitted SQL: {}",
        plan.sql()
    );
    let documents = rows_of(plan, "d");
    documents.assert_searches_through("documents", Access::RowId);
    documents.assert_search_constraint("documents", "(rowid=?)");
    let at = |wanted: &PlanRow| {
        plan.rows()
            .iter()
            .position(|row| row == wanted)
            .expect("a row of the plan")
    };
    assert!(
        plan.rows()
            .iter()
            .filter(|other| other.parent == row.parent)
            .all(|other| at(other) >= at(row)),
        "the full-text index is not the page's outer loop: {:?}\nemitted SQL: {}",
        plan.rows(),
        plan.sql()
    );
    let sorts: Vec<&PlanRow> = plan
        .rows()
        .iter()
        .filter(|row| row.detail.contains("TEMP B-TREE"))
        .collect();
    assert!(
        sorts.len() == 1 && sorts[0].detail == "USE TEMP B-TREE FOR ORDER BY",
        "the page does not sort once, for its order: {sorts:?}\nemitted SQL: {}",
        plan.sql()
    );
}

/// One request per filter slot a search applies — every slot but a
/// resolution, which a search reports and never filters by — each with the
/// filter it spells.
fn filtered() -> Vec<(Predicate, ReadFilter)> {
    vec![
        (
            Predicate::equal_to("status", "open"),
            ReadFilter::Equal(norn_store::FieldOrder::Raw),
        ),
        (
            Predicate::not_equal_to("status", "open"),
            ReadFilter::NotEqual(norn_store::FieldOrder::Raw),
        ),
        (
            Predicate::in_any("status", ["open".to_string(), "closed".to_string()]),
            ReadFilter::Member(norn_store::FieldOrder::Raw),
        ),
        (Predicate::has("status"), ReadFilter::Present),
        (Predicate::missing("status"), ReadFilter::Absent),
        (
            Predicate::before("status", "open"),
            ReadFilter::Before(norn_store::FieldOrder::Raw),
        ),
        (
            Predicate::after("status", "closed"),
            ReadFilter::After(norn_store::FieldOrder::Raw),
        ),
        (Predicate::matches("harbor"), ReadFilter::FullText),
        (Predicate::path("notes/*"), ReadFilter::PathGlob),
        (Predicate::tag("draft"), ReadFilter::Tag),
        (
            Predicate::has_finding(FindingKind::BodyBytesNotUtf8),
            ReadFilter::Finding,
        ),
    ]
}

/// **A lexical page is driven by the full-text index, and sorts what it
/// matched.** The page reads `documents_fts` through its `MATCH` selection as
/// its outer loop and reaches each match's document by row id; it sorts once,
/// for its order, since the index hands matches back in row-id order and a
/// ranking is known only once every match is scored. So it holds on a first
/// page, a continuation, a floored page, and a page narrowed by every filter a
/// search applies: a filter is a test of the match's document, and never the
/// loop the page is driven from.
///
/// Controls: the page's read of the index rebuilt with no `MATCH` selection;
/// rebuilt so that the module hands matches back in its rank order and the
/// page sorts nothing — a plan that would order by BM25 alone, with no path to
/// break a tie; the documents reached by a scan rather than by row id; and the
/// documents read before the index, the plan a filter's seek would drive. Each
/// fails.
#[test]
fn a_lexical_page_is_driven_by_the_full_text_index_and_sorts_what_it_matched() {
    let searching_store = Searching::new("search-plan");
    let statement = SearchStatement::LexicalPage;
    let first = searching("lantern harbor");
    let (plan, filters) = plan_of(&searching_store.plans(&first), statement);
    judge_lexical(&plan);
    assert!(filters.is_empty());

    let next = searching_store
        .search(&first.clone().with_limit(1))
        .next
        .expect("a next page");
    let floor = Score::new(0.0).expect("a floor");
    for params in [
        first.clone().with_after(next.clone()),
        first.clone().with_min_score(floor),
        first.clone().with_after(next).with_min_score(floor),
    ] {
        judge_lexical(&plan_of(&searching_store.plans(&params), statement).0);
    }
    for (predicate, shape) in filtered() {
        let (narrowed, recorded) = plan_of(
            &searching_store.plans(&first.clone().with_predicates([predicate.clone()])),
            statement,
        );
        assert_eq!(recorded, vec![shape], "{predicate:?}");
        judge_lexical(&narrowed);
    }
    let two = plan_of(
        &searching_store.plans(
            &first
                .clone()
                .with_predicates([Predicate::tag("draft"), Predicate::matches("walk")]),
        ),
        statement,
    )
    .0;
    judge_lexical(&two);

    let unselected = rewritten(&plan, |detail| {
        detail.replace("VIRTUAL TABLE INDEX 0:M1", "VIRTUAL TABLE INDEX 0:")
    });
    failure_of("a page reading the index with no MATCH selection", || {
        judge_lexical(&unselected)
    });
    let ranked_by_module = QueryPlan::new(
        plan.sql(),
        plan.rows()
            .iter()
            .filter(|row| !row.detail.contains("TEMP B-TREE"))
            .map(|row| {
                PlanRow::new(
                    row.id,
                    row.parent,
                    row.detail.replace("INDEX 0:M1", "INDEX 32:M1"),
                )
            })
            .collect(),
    );
    failure_of("a page the module hands back in rank order", || {
        judge_lexical(&ranked_by_module)
    });
    let scanned = rewritten(&plan, |detail| {
        detail.replace("SEARCH d USING INTEGER PRIMARY KEY (rowid=?)", "SCAN d")
    });
    failure_of("a page scanning the documents", || judge_lexical(&scanned));
    let documents_first = reordered(&plan, |rows| {
        let mut rows = rows.to_vec();
        let ft = rows
            .iter()
            .position(|row| row.detail.starts_with("SCAN ft "))
            .expect("the index's row");
        let d = rows
            .iter()
            .position(|row| row.detail.starts_with("SEARCH d "))
            .expect("the documents' row");
        rows.swap(ft, d);
        rows
    });
    failure_of("a page driven from the documents", || {
        judge_lexical(&documents_first)
    });
}

// ---- the work bars ----

/// `count` more documents under `bulk/`, each tagged `bulk`, each with the
/// body `body` of `at`, its index.
fn bulk(count: usize, body: impl Fn(usize) -> String) -> Vec<norn_store::DocumentFacts> {
    (0..count)
        .map(|at| {
            let mut facts = document(
                &format!("bulk/{at:04}.md"),
                &format!("hash-bulk-{at}"),
                &body(at),
            );
            facts.tags.push(TagFact {
                name: "bulk".to_string(),
                source: TagSource::Body,
                span: None,
            });
            facts
        })
        .collect()
}

/// The page counters of a search's work: what SQLite counted running its
/// lexical page, and the hits the page read.
fn page_work(searched: &Searched) -> (u64, u64, u64, u64) {
    let work = searched.work;
    (
        work.hits_read,
        work.page_full_scan_steps,
        work.page_sorts,
        work.page_vm_steps,
    )
}

/// The requests the vault-size bar reads: a first page, a page narrowed by a
/// tag, a path, a field and a `matches` part, a floored page, and a
/// continuation of a one-term query, whose ranking the vault's size does not
/// reorder.
fn vault_size_requests(store: &Searching) -> Vec<LexicalQuery> {
    let lantern = searching("lantern").with_limit(2);
    let next = store.search(&lantern).next.expect("a next page of lantern");
    vec![
        searching("lantern harbor").with_limit(3),
        searching("lantern harbor"),
        searching("lantern").with_predicates([Predicate::tag("draft")]),
        searching("lantern").with_predicates([Predicate::path("notes/*")]),
        searching("lantern").with_predicates([Predicate::equal_to("status", "open")]),
        searching("lantern").with_predicates([Predicate::matches("harbor")]),
        searching("lantern").with_min_score(Score::new(0.0).expect("a floor")),
        lantern.with_after(next),
    ]
}

/// **A search costs the documents its query matches, and never the vault.**
/// Beside 50 documents of 64-byte bodies and beside 500 of 16 KiB bodies, none
/// holding a term the requests name, every request the bar reads answers the
/// same hits and counts the same work: the same statements, the same hits
/// read, the same VM steps, one sort and no step through a full scan. A
/// document the query does not match is never read, and neither is the body
/// of one it does: a score is computed off the index.
///
/// Control: the larger vault's documents rewritten to hold `lantern`, the
/// work of a `lantern` page grows with them, and the bar fails.
#[test]
fn a_search_costs_the_documents_its_query_matches_and_never_the_vault() {
    let small = Searching::with_documents(
        "search-work-small",
        bulk(50, |at| format!("bulk{at} {}\n", "b".repeat(56))),
    );
    let large = Searching::with_documents(
        "search-work-large",
        bulk(500, |at| format!("bulk{at} {}\n", "b".repeat(16 * 1024))),
    );
    let judge = |small: &Searching, large: &Searching| {
        for (at_small, at_large) in vault_size_requests(small)
            .into_iter()
            .zip(vault_size_requests(large))
        {
            let (on_small, on_large) = (small.search(&at_small), large.search(&at_large));
            assert_eq!(hit_paths(&on_small), hit_paths(&on_large), "{at_small:?}");
            assert_eq!(
                on_small.work.statements, on_large.work.statements,
                "{at_small:?}"
            );
            assert_eq!(
                page_work(&on_small),
                page_work(&on_large),
                "a search's work grew with the vault: {at_small:?}"
            );
            let (_, full_scan_steps, sorts, _) = page_work(&on_large);
            assert_eq!((full_scan_steps, sorts), (0, 1), "{at_small:?}");
        }
    };
    judge(&small, &large);

    let matched = Searching::with_documents(
        "search-work-matched",
        bulk(500, |at| format!("bulk{at} lantern\n")),
    );
    failure_of("a larger vault whose documents the query matches", || {
        judge(&small, &matched)
    });
}

/// **Ranking costs every match, a continuation included, and a narrowing part
/// narrows what is scored, not what is matched.** Beside 50 and then 500
/// documents holding both `lantern` and `harbor`:
///
/// - A first page of three hits reads four hits and sorts once at both sizes,
///   and its VM steps grow with the 450 matches added — every match is scored
///   and sorted before the page's first hit is known. A continuation of it
///   grows the same way: a keyset position bounds what is sorted, and every
///   match is scored again to find what stands past it.
/// - Narrowed by a tag none of the added documents carries, the page answers
///   the same hit at both sizes and still grows with the matches — the index
///   hands every match back, and each is tested against the tag — but by less
///   than half as much per match as the unfiltered page: a match the tag
///   rejects is neither scored, nor reached in `documents`, nor sorted.
#[test]
fn ranking_costs_every_match_and_a_narrowing_part_narrows_what_is_scored() {
    let matches = |at: usize| format!("a bulk lantern harbor {at}\n");
    let small = Searching::with_documents("search-ranking-small", bulk(50, matches));
    let large = Searching::with_documents("search-ranking-large", bulk(500, matches));
    let added = 450;
    let first = searching("lantern harbor").with_limit(3);
    let growth = |params: &dyn Fn(&Searching) -> LexicalQuery| {
        let (on_small, on_large) = (small.search(&params(&small)), large.search(&params(&large)));
        let (small_read, small_scans, small_sorts, small_steps) = page_work(&on_small);
        let (large_read, large_scans, large_sorts, large_steps) = page_work(&on_large);
        assert_eq!(small_read, large_read);
        assert_eq!((small_scans, small_sorts), (0, 1));
        assert_eq!((large_scans, large_sorts), (0, 1));
        (large_steps - small_steps, on_small, on_large)
    };

    let (unfiltered, ..) = growth(&|_| first.clone());
    let (continued, ..) = growth(&|store: &Searching| {
        let next = store.search(&first).next.expect("a next page");
        first.clone().with_after(next)
    });
    for (reading, grown) in [("a first page", unfiltered), ("a continuation", continued)] {
        assert!(
            grown >= added * 8,
            "{reading}'s VM steps grew by {grown} over {added} more matches: ranking did not \
             score every match"
        );
    }

    let (narrowed, on_small, on_large) =
        growth(&|_| first.clone().with_predicates([Predicate::tag("draft")]));
    assert_eq!(hit_paths(&on_small), ["notes/lantern.md"]);
    assert_eq!(hit_paths(&on_large), ["notes/lantern.md"]);
    assert!(
        narrowed >= added,
        "a narrowed page's VM steps grew by {narrowed} over {added} more matches: it did not \
         test each match"
    );
    assert!(
        narrowed * 2 < unfiltered,
        "a narrowed page grew by {narrowed} and an unfiltered one by {unfiltered}: a match the \
         tag rejects was scored"
    );
}

// ---- the payload bar ----

/// **No statement a search runs reads a document's payload, and its page
/// reads the full-text index rather than the text it indexes.** Every
/// statement a search emits — the lexical page, and each probe its
/// conjunction's compilation runs — for every request the vault-size bar reads,
/// under every filter a search applies, reads none of
/// [`crate::common::DOCUMENT_PAYLOAD`] as SQLite's authorizer reports the
/// columns it reads. A `MATCH` and `bm25()` take the column FTS5 names after
/// its table, `documents_fts.documents_fts`, which is the handle to the index —
/// its postings and each document's token counts — and the page reads it;
/// `documents_fts.body` is the text the index is over, read through
/// `documents.body` by row id, and it stays payload, so no search statement
/// reads it. What a search costs never includes the bytes of the documents it
/// ranks.
///
/// Controls: a search projecting the body and the fields hydrates rows reading
/// the body and the frontmatter; and the page's own reads with the full-text
/// body column beside them — the read a snippet or a highlight makes. Each
/// fails the bar.
#[test]
fn no_statement_a_search_runs_reads_a_documents_payload() {
    let searching_store = Searching::new("search-payload");
    let mut shapes = vault_size_requests(&searching_store);
    shapes.extend(
        filtered()
            .into_iter()
            .map(|(predicate, _)| searching("lantern").with_predicates([predicate])),
    );
    shapes.push(searching("lantern").with_predicates([Predicate::equal_to("statis", "x")]));
    shapes.push(searching("lantern").with_predicates([Predicate::path("notes")]));
    let mut reached: Vec<ReadStatement> = Vec::new();
    for params in &shapes {
        for emitted in searching_store.plans(params) {
            reached.push(emitted.statement);
            let reads = reads_of(&emitted.plan);
            reads.assert_reads_none_of(DOCUMENT_PAYLOAD);
            if emitted.statement == ReadStatement::Search(SearchStatement::LexicalPage) {
                assert!(
                    reads.reads("documents_fts", "documents_fts"),
                    "a lexical page did not read the full-text index: {reads:?}"
                );
            }
        }
    }
    let snapshot = searching_store.snapshot();
    let generation = snapshot.reading().write_generation();
    for emitted in snapshot
        .held_candidates_plans(&["notes/walk.md", "notes/nowhere.md"])
        .expect("the held paths' plans")
        .into_iter()
        .chain(
            snapshot
                .feed_rows_after_plans(generation, generation, 8)
                .expect("the feed rows' plans"),
        )
    {
        reached.push(emitted.statement);
        reads_of(&emitted.plan).assert_reads_none_of(DOCUMENT_PAYLOAD);
    }
    drop(snapshot);
    for statement in SearchStatement::all() {
        assert!(
            reached.contains(&ReadStatement::Search(statement)),
            "the payload bar never reached {statement:?}: {reached:?}"
        );
    }
    for probe in [
        FindStatement::ActiveFingerprint,
        FindStatement::KnownKey,
        FindStatement::FieldUniverse,
        FindStatement::BareDirectory,
        FindStatement::MatchProbe,
    ] {
        assert!(
            reached.contains(&ReadStatement::Find(probe)),
            "the payload bar never reached {probe:?}: {reached:?}"
        );
    }

    let hydrated = searching_store
        .plans(&searching("lantern").with_columns([Column::fields(), Column::body()]));
    let rows = hydrated
        .iter()
        .find(|emitted| emitted.statement == ReadStatement::Find(FindStatement::HydrateDocuments))
        .expect("a search naming columns hydrates its hits' rows");
    let rows = reads_of(&rows.plan);
    assert!(
        rows.reads("documents", "body") && rows.reads("documents", "frontmatter"),
        "a hydration projecting the body and the fields read neither: {rows:?}"
    );
    failure_of("a hydration of the body and the fields", || {
        rows.assert_reads_none_of(DOCUMENT_PAYLOAD)
    });
    let page = hydrated
        .iter()
        .find(|emitted| emitted.statement == ReadStatement::Search(SearchStatement::LexicalPage))
        .expect("a lexical page");
    let snippeted = norn_testkit::explain::StatementReads::new(
        page.plan.sql.clone(),
        page.plan
            .reads
            .iter()
            .map(|read| (read.table.clone(), read.column.clone()))
            .chain([("documents_fts".to_string(), "body".to_string())]),
    );
    failure_of("a page reading the full-text body column", || {
        snippeted.assert_reads_none_of(DOCUMENT_PAYLOAD)
    });
}

/// **A search's work reads out whole, each count under its own name**, for
/// the reason a find's does, the nested rows one name per table.
#[test]
fn a_searchs_work_reads_out_every_count_by_name() {
    let work = norn_store::SearchWork {
        statements: 1,
        hits_read: 2,
        page_full_scan_steps: 3,
        page_sorts: 4,
        page_vm_steps: 5,
        documents_hydrated: 6,
        nested_rows: norn_store::NestedRows {
            tags: 7,
            headings: 8,
            blocks: 9,
            links: 10,
        },
        finding_rows: 11,
    };
    assert_eq!(
        work.readings().collect::<Vec<_>>(),
        vec![
            ("search_statements", 1),
            ("search_hits_read", 2),
            ("search_page_full_scan_steps", 3),
            ("search_page_sorts", 4),
            ("search_page_vm_steps", 5),
            ("search_documents_hydrated", 6),
            ("search_tag_rows", 7),
            ("search_heading_rows", 8),
            ("search_block_rows", 9),
            ("search_link_rows", 10),
            ("search_finding_rows", 11),
        ]
    );
}
