//! The stem trigram index's maintenance, bounded retrieval, and query plans.
//! The plan bars also hold the document death cascade to its trigram index.

use super::common::*;
use norn_store::similarity::trigrams;

/// A newly created path writes its distinct stem trigrams once. Changing the
/// body at that same path writes no trigram rows.
#[test]
fn a_path_unchanged_edit_writes_zero_trigram_rows() {
    let scratch = Scratch::new("stem-edit");
    let mut store = scratch.open();
    let mut first = store.begin_request();
    write_document(&mut first, &document("alpha.md", "one", "before"));
    assert_eq!(
        first.counters().get("trigram_rows_written"),
        Some(trigrams("alpha").len() as u64)
    );
    let mut second = store.begin_request();
    write_document(&mut second, &document("alpha.md", "two", "after"));
    assert_eq!(second.counters().get("trigram_rows_written"), Some(0));
}

/// A rename removes the old path's indexed trigrams and writes exactly the new
/// stem's set. Deleting the renamed document leaves no indexed trigram rows.
#[test]
fn a_rename_keeps_only_the_new_stems_trigrams_and_a_delete_keeps_none() {
    let scratch = Scratch::new("stem-rename");
    let mut store = scratch.open();
    let mut request = store.begin_request();
    write_document(&mut request, &document("old.md", "one", "body"));
    record_death(
        &mut request,
        &path("old.md"),
        norn_store::Provenance::PlanDelete,
    );
    write_document(&mut request, &document("new.md", "one", "body"));
    let rows = request
        .indexed_trigrams_after(None, norn_store::MAX_PAGE)
        .unwrap();
    assert_eq!(
        rows,
        trigrams("new")
            .into_iter()
            .map(|trigram| (path("new.md"), trigram))
            .collect::<Vec<_>>()
    );
    record_death(
        &mut request,
        &path("new.md"),
        norn_store::Provenance::PlanDelete,
    );
    assert!(
        request
            .indexed_trigrams_after(None, norn_store::MAX_PAGE)
            .unwrap()
            .is_empty()
    );
}

/// Writing the same new paths costs the sum of their distinct stem trigrams at
/// both store sizes. The documents already stored add no trigram writes to the
/// changeset.
#[test]
fn trigram_maintenance_cost_follows_inserted_stems_at_both_store_sizes() {
    let inserted = ["x.md", "distinctive.md", "long-stem-for-a-bounded-write.md"]
        .map(|at| document(at, "new", "body"));
    let expected: u64 = inserted
        .iter()
        .map(|facts| trigrams(facts.path.stem()).len() as u64)
        .sum();
    for size in [3, 300] {
        let scratch = Scratch::new("stem-write-size");
        let mut store = scratch.open();
        let existing: Vec<_> = (0..size)
            .map(|at| document(&format!("existing/{at}.md"), "old", "body"))
            .collect();
        write_documents(&mut store.begin_request(), &existing);
        let mut request = store.begin_request();
        write_documents(&mut request, &inserted);
        assert_eq!(
            request.counters().get("trigram_rows_written"),
            Some(expected)
        );
    }
}

/// The retrieval prefix misses no candidate reaching the Jaccard cutoff.
/// Returned candidates carry the shared and total trigram counts computed over
/// their stems, in path order.
#[test]
fn similar_stems_candidates_are_complete_and_shared_counts_match_brute_force() {
    let scratch = Scratch::new("stem-candidates");
    let mut store = scratch.open();
    let documents = [
        "alpha.md",
        "alphas.md",
        "alpine.md",
        "beta.md",
        "nested/ALPHA.md",
    ]
    .map(|at| document(at, "hash", "body"));
    write_documents(&mut store.begin_request(), &documents);
    let reader = std::sync::Arc::new(store.open_reader().reader.unwrap());
    let snapshot = reader.try_take().unwrap().establish().unwrap();
    for query in ["alpha", "alphas", "alpine", "beta", "ÁLPHA"] {
        let answer = snapshot.similar_stems(query).unwrap();
        let norn_store::StemMatches::Candidates(candidates) = answer.matches else {
            panic!("small postings")
        };
        let q = trigrams(query);
        assert_eq!(answer.query_trigrams, q.len());
        assert!(
            candidates
                .windows(2)
                .all(|pair| pair[0].path < pair[1].path)
        );
        for facts in &documents {
            let c = trigrams(facts.path.stem());
            let candidate = candidates
                .iter()
                .find(|candidate| candidate.path == facts.path);
            if norn_store::similarity::jaccard(&q, &c) >= norn_store::similarity::CUTOFF {
                assert!(
                    candidate.is_some(),
                    "missed {} for {query}",
                    facts.path.as_str()
                );
            }
            if let Some(candidate) = candidate {
                assert_eq!(candidate.shared, q.intersection(&c).count());
                assert_eq!(candidate.candidate_trigrams, c.len());
            }
        }
    }
}

/// A selected posting list past the cap answers TooCommon, naming the rarest
/// selected over-cap trigram. That answer runs no postings statement.
#[test]
fn selected_postings_over_the_cap_answer_too_common() {
    let scratch = Scratch::new("stem-cap");
    let mut store = scratch.open();
    let documents: Vec<_> = (0..=norn_store::similarity::POSTINGS_CAP)
        .map(|at| document(&format!("{at}/alpha.md"), "hash", "body"))
        .collect();
    write_documents(&mut store.begin_request(), &documents);
    let reader = std::sync::Arc::new(store.open_reader().reader.unwrap());
    let snapshot = reader.try_take().unwrap().establish().unwrap();
    let answer = snapshot.similar_stems("alpha").unwrap();
    assert_eq!(
        answer.matches,
        norn_store::StemMatches::TooCommon {
            trigram: "  a".to_string()
        }
    );
    assert_eq!(answer.work.postings_rows_read, 0);
}

/// Common query trigrams may exceed the cap when every selected list fits.
/// Selected lists of exactly the cap remain admitted, and both row readings
/// stay within their declared bounds.
#[test]
fn capped_common_trigrams_leave_rare_prefix_candidates_and_bounded_work() {
    let scratch = Scratch::new("stem-rare-prefix");
    let mut store = scratch.open();
    let mut documents: Vec<_> = (0..=norn_store::similarity::POSTINGS_CAP)
        .map(|at| document(&format!("{at}/abc.md"), "hash", "body"))
        .collect();
    documents.extend(
        (0..norn_store::similarity::POSTINGS_CAP)
            .map(|at| document(&format!("rare/{at}/abcdef.md"), "hash", "body")),
    );
    write_documents(&mut store.begin_request(), &documents);
    let reader = std::sync::Arc::new(store.open_reader().reader.unwrap());
    let snapshot = reader.try_take().unwrap().establish().unwrap();
    let answer = snapshot.similar_stems("abcdef").unwrap();
    let norn_store::StemMatches::Candidates(candidates) = answer.matches else {
        panic!("rare prefix fits")
    };
    assert!(
        candidates
            .iter()
            .any(|candidate| candidate.path == path("rare/0/abcdef.md"))
    );
    let t = answer.query_trigrams;
    let cap = norn_store::similarity::POSTINGS_CAP as u64;
    assert_eq!(candidates.len(), cap as usize);
    assert_eq!(answer.work.rarity_rows_read, 3 * (cap + 1) + 4 * cap);
    assert_eq!(answer.work.postings_rows_read, 4 * cap);
    assert!(answer.work.rarity_rows_read <= t as u64 * (cap + 1));
    assert!(
        answer.work.postings_rows_read
            <= (t - norn_store::similarity::required_shared(t) + 1) as u64 * cap
    );
}

/// Every stem read seeks its index and builds no temporary B-tree. The drain
/// seeks the path and document-trigram indexes, and a document's death cascades
/// through the document-trigram index.
#[test]
fn stem_statements_seek_without_scans_or_sorts() {
    use norn_testkit::explain::{Access, QueryPlan};
    fn plan(emitted: norn_store::EmittedPlan) -> QueryPlan {
        QueryPlan::new(
            emitted.sql,
            emitted
                .steps
                .into_iter()
                .map(|step| norn_testkit::explain::PlanRow::new(step.id, step.parent, step.detail))
                .collect(),
        )
    }
    let scratch = Scratch::new("stem-plans");
    let mut store = scratch.open();
    write_document(
        &mut store.begin_request(),
        &document("alpha.md", "hash", "body"),
    );
    let drain = plan(
        store
            .begin_request()
            .emitted_plan(norn_store::ExplainedStatement::IndexedTrigramPage)
            .unwrap(),
    );
    let reader = std::sync::Arc::new(store.open_reader().reader.unwrap());
    let snapshot = reader.try_take().unwrap().establish().unwrap();
    let plans = snapshot.similar_stems_plans("alpha").unwrap();
    assert!(
        plans
            .iter()
            .any(|p| p.statement == norn_store::ReadStatement::StemRarity)
    );
    assert!(
        plans
            .iter()
            .any(|p| p.statement == norn_store::ReadStatement::StemPostings)
    );
    for p in plans {
        let read = plan(p.plan);
        read.assert_no_temp_btree();
        assert!(read.unbounded_steps().is_empty(), "{:?}", read.rows());
        read.assert_searches_through("document_trigrams", Access::PrimaryKey);
        if p.statement == norn_store::ReadStatement::StemPostings {
            read.assert_searches_through("documents", Access::RowId);
        }
    }
    drain.assert_no_temp_btree();
    assert!(drain.unbounded_steps().is_empty(), "{:?}", drain.rows());
    assert!(
        drain
            .searches_of("documents")
            .iter()
            .any(|row| row.access() == Some(Access::Index("documents_path")))
    );
    assert!(
        drain
            .searches_of("document_trigrams")
            .iter()
            .any(|row| row.access() == Some(Access::Index("document_trigrams_document_trigram")))
    );
    let delete: QueryPlan = plan(
        store
            .begin_request()
            .emitted_plan(norn_store::ExplainedStatement::Write(
                norn_store::WriteStatement::DeleteDocument,
            ))
            .unwrap(),
    );
    assert!(
        delete
            .searches_of("document_trigrams")
            .iter()
            .any(|row| row.access() == Some(Access::Index("document_trigrams_document_trigram")))
    );
}
