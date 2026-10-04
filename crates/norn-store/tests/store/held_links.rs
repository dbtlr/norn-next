//! The links a document holds, read on a snapshot without its body: the
//! content hash the store derived the document's facts from, and the links it
//! derived, in document order — and the plan bars that read is judged by.

use std::sync::Arc;

use norn_store::{
    HELD_LINKS_STATEMENTS, HeldLinks, HeldLinksPlan, HeldLinksStatement, LinkAnchor, LinkFact,
    LinkFamily, ReadStatement, Snapshot, SnapshotReader, Store, StoredPathOrder, induced_failure,
};
use norn_testkit::explain::{Access, PlanRow, QueryPlan};

use crate::common::{
    Scratch, document, document_with_every_fact, heading_anchor, path, span, write_documents,
    written_links,
};
use crate::find::{failure_of, rows_of};

/// A store holding a document with every link shape, one with none, and a
/// bulk of others beside them, with a read handle over it.
struct Held {
    _scratch: Scratch,
    store: Store,
    reader: Arc<SnapshotReader>,
}

impl Held {
    fn new(label: &str) -> Held {
        let scratch = Scratch::new(label);
        let mut store = scratch.open_under(StoredPathOrder::Sensitive);
        let mut linked = document_with_every_fact("notes/linked.md", "hash-linked");
        linked.links.push(LinkFact {
            family: LinkFamily::Markdown,
            embed: false,
            protocol: None,
            target: "../other/target.md".to_string(),
            title: None,
            anchor: None,
            span: span(5, 1, 70),
        });
        linked.links.push(LinkFact {
            family: LinkFamily::Wikilink,
            embed: false,
            protocol: None,
            target: "target".to_string(),
            title: None,
            anchor: Some(LinkAnchor::Block {
                id: "b1".to_string(),
            }),
            span: span(6, 1, 90),
        });
        linked.links.push(LinkFact {
            family: LinkFamily::Wikilink,
            embed: false,
            protocol: None,
            target: "target".to_string(),
            title: None,
            anchor: heading_anchor("Heading"),
            span: span(7, 1, 110),
        });
        let mut documents = vec![
            linked,
            document("notes/bare.md", "hash-bare", "no links here\n"),
        ];
        documents.extend(
            (0..40).map(|at| document_with_every_fact(&format!("bulk/{at:02}.md"), "hash-bulk")),
        );
        write_documents(&mut store.begin_request(), &documents);
        let reader = Arc::new(store.open_reader().reader.expect("a reader"));
        Held {
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

    fn held(&self, at: &str) -> Option<HeldLinks> {
        self.snapshot()
            .held_links(&path(at))
            .unwrap_or_else(|problem| panic!("the links {at} holds: {problem}"))
    }

    fn plans(&self, at: &str) -> Vec<HeldLinksPlan> {
        self.snapshot()
            .held_links_plans(&path(at))
            .unwrap_or_else(|problem| panic!("the plans of the links {at} holds: {problem}"))
    }
}

/// **A document's held links are the links the store derived for it, in its
/// order, beside the hash it derived them from.** For a document with every
/// link shape — a heading anchor and a block anchor, an embed, a protocol, a
/// relative path — and for one with none, the read answers exactly the links
/// the writer's own reading of the document's facts holds, in document order,
/// and the content hash its row records. A path holding no document answers
/// nothing.
#[test]
fn a_documents_held_links_are_its_stored_links_in_order_beside_its_hash() {
    let mut held = Held::new("held-links");
    for (at, hash) in [
        ("notes/linked.md", "hash-linked"),
        ("notes/bare.md", "hash-bare"),
    ] {
        let stored = held
            .store
            .begin_request()
            .stored_facts(&path(at))
            .expect("the stored facts")
            .expect("a document the store holds");
        assert_eq!(
            held.held(at),
            Some(HeldLinks {
                content_hash: hash.to_string(),
                links: written_links(&stored),
            }),
            "{at}"
        );
    }
    assert_eq!(held.held("notes/linked.md").unwrap().links.len(), 5);
    assert!(held.held("notes/bare.md").unwrap().links.is_empty());
    assert_eq!(held.held("notes/absent.md"), None);
    assert_eq!(
        held.held("Notes/Linked.md"),
        None,
        "a path is its own spelling"
    );
}

/// **The order is the statement's, not the index's.** With
/// `links_document_ordinal` rebuilt out of band to hand a document's links
/// back in descending ordinal order, the read still answers them in document
/// order: the order is one the statement asks for, which a walk of any index
/// over `(document, ordinal)` satisfies, and never the order an index happens
/// to keep.
///
/// The forbidden shape is a links statement that orders nothing and answers
/// document order only because the index it seeks keeps that order.
#[test]
fn held_links_are_in_document_order_whatever_order_the_index_keeps() {
    let mut held = Held::new("held-links-order");
    let stored = held
        .store
        .begin_request()
        .stored_facts(&path("notes/linked.md"))
        .expect("the stored facts")
        .expect("a document the store holds");
    let in_order = written_links(&stored);
    assert!(in_order.len() >= 3, "{in_order:?}");

    for statement in [
        "DROP INDEX links_document_ordinal",
        "CREATE UNIQUE INDEX links_document_ordinal ON links(document, ordinal DESC)",
    ] {
        induced_failure::execute_out_of_band(&mut held.store, statement)
            .unwrap_or_else(|problem| panic!("{statement}: {problem}"));
    }
    // The rebuilt index is the one the statement seeks, and no sort stands
    // behind it: what orders the rows is the walk of an index kept the other
    // way round.
    let plans = held.plans("notes/linked.md");
    let links = plan_of(&plans, HeldLinksStatement::Links);
    links.assert_no_temp_btree();
    rows_of(&links, "n").assert_searches_through("links", Access::Index("links_document_ordinal"));
    assert_eq!(
        held.held("notes/linked.md").map(|held| held.links),
        Some(in_order),
        "the links came back in the order the index keeps"
    );
}

/// The plan the store reported for one statement, in the harness's shape.
fn plan(emitted: &HeldLinksPlan) -> QueryPlan {
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

/// The one plan `plans` holds for `statement`.
fn plan_of(plans: &[HeldLinksPlan], statement: HeldLinksStatement) -> QueryPlan {
    let mut matching: Vec<QueryPlan> = plans
        .iter()
        .filter(|plan| plan.statement == ReadStatement::HeldLinks(statement))
        .map(plan)
        .collect();
    assert_eq!(matching.len(), 1, "{statement:?} in {plans:?}");
    matching.remove(0)
}

/// **The bar on reading a document's links without its body.** The holder is
/// one equality seek of `documents_path` at the path, and its links one range
/// seek of `links_document_ordinal` at the document that hands them back in
/// order with no sort; neither statement names the body, and the read runs
/// those two statements and no others. Where a path holds no document, the
/// links are not read at all.
///
/// Controls: either index dropped, its statement reads something else and
/// fails.
#[test]
fn held_links_are_two_seeks_that_never_name_the_body() {
    let mut held = Held::new("held-links-plan");
    let judge_holder = |plan: &QueryPlan| {
        plan.assert_no_full_scan();
        let rows = rows_of(plan, "d");
        rows.assert_searches_through("documents", Access::Index("documents_path"));
        rows.assert_search_constraint("documents", "(path=?)");
    };
    let judge_links = |plan: &QueryPlan| {
        plan.assert_no_full_scan();
        plan.assert_no_temp_btree();
        let rows = rows_of(plan, "n");
        rows.assert_searches_through("links", Access::Index("links_document_ordinal"));
        rows.assert_search_constraint("links", "(document=?)");
    };
    let plans = held.plans("notes/linked.md");
    assert_eq!(
        plans.iter().map(|plan| plan.statement).collect::<Vec<_>>(),
        HeldLinksStatement::all()
            .map(ReadStatement::HeldLinks)
            .to_vec(),
    );
    for emitted in &plans {
        assert!(
            !emitted.plan.sql.contains("body"),
            "a statement names the body: {}",
            emitted.plan.sql
        );
    }
    judge_holder(&plan_of(&plans, HeldLinksStatement::Holder));
    judge_links(&plan_of(&plans, HeldLinksStatement::Links));
    let absent = held.plans("notes/absent.md");
    assert_eq!(
        absent.iter().map(|plan| plan.statement).collect::<Vec<_>>(),
        vec![ReadStatement::HeldLinks(HeldLinksStatement::Holder)],
    );

    induced_failure::execute_out_of_band(&mut held.store, "DROP INDEX links_document_ordinal")
        .expect("dropping links_document_ordinal");
    let plans = held.plans("notes/linked.md");
    failure_of("links_document_ordinal dropped", || {
        judge_links(&plan_of(&plans, HeldLinksStatement::Links))
    });
    induced_failure::execute_out_of_band(&mut held.store, "DROP INDEX documents_path")
        .expect("dropping documents_path");
    let plans = held.plans("notes/linked.md");
    failure_of("documents_path dropped", || {
        judge_holder(&plan_of(&plans, HeldLinksStatement::Holder))
    });
}

/// Which test bars each statement the held-links read names. Exhaustive, so
/// a statement added to [`HeldLinksStatement`] does not compile until its
/// author names the bar.
fn statement_barred_by(statement: HeldLinksStatement) -> &'static str {
    match statement {
        HeldLinksStatement::Holder | HeldLinksStatement::Links => {
            "held_links_are_two_seeks_that_never_name_the_body"
        }
    }
}

/// **Every statement the held-links read names carries a bar**: the
/// enumeration's slots are each claimed once, and every statement names the
/// bar that judges it.
#[test]
fn the_held_links_bars_cover_every_statement() {
    let mut slots: Vec<usize> = HeldLinksStatement::all()
        .into_iter()
        .map(HeldLinksStatement::slot)
        .collect();
    slots.sort_unstable();
    assert_eq!(slots, (0..HELD_LINKS_STATEMENTS).collect::<Vec<usize>>());
    for (slot, statement) in HeldLinksStatement::all().into_iter().enumerate() {
        assert_eq!(statement.slot(), slot, "{statement:?} claims another slot");
        assert!(!statement_barred_by(statement).is_empty());
    }
}
