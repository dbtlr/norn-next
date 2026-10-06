//! The frontmatter a document holds, read on a snapshot without its body: the
//! content hash the store derived the document's facts from and the value
//! tree its canonical projection holds — and the plan bar that read is judged
//! by.

use std::sync::Arc;

use norn_store::{
    ContentModel, FrontmatterValue, HELD_FRONTMATTER_STATEMENTS, HeldBlock, HeldFrontmatter,
    HeldFrontmatterPlan, HeldFrontmatterStatement, ReadStatement, Snapshot, SnapshotReader, Store,
    StoredPathOrder, induced_failure,
};
use norn_testkit::explain::{Access, PlanRow, QueryPlan};

use crate::common::{Scratch, document, path, write_documents};
use crate::find::{failure_of, rows_of};

/// The frontmatter every shape of value: an integer, a float, a string of
/// digits, a boolean, a null, and nested containers.
fn every_shape() -> FrontmatterValue {
    FrontmatterValue::Map(vec![
        (
            "title".to_string(),
            FrontmatterValue::String("A".to_string()),
        ),
        ("rank".to_string(), FrontmatterValue::Int(3)),
        ("ratio".to_string(), FrontmatterValue::Float(2.0)),
        (
            "code".to_string(),
            FrontmatterValue::String("7".to_string()),
        ),
        ("done".to_string(), FrontmatterValue::Bool(false)),
        ("gone".to_string(), FrontmatterValue::Null),
        (
            "tags".to_string(),
            FrontmatterValue::Sequence(vec![
                FrontmatterValue::String("x".to_string()),
                FrontmatterValue::Map(vec![("k".to_string(), FrontmatterValue::Int(1))]),
            ]),
        ),
    ])
}

/// A store holding a document with frontmatter, one with none, one whose
/// block did not read, and a bulk of others, with a read handle over it.
struct Held {
    _scratch: Scratch,
    store: Store,
    reader: Arc<SnapshotReader>,
}

impl Held {
    fn new(label: &str) -> Held {
        let scratch = Scratch::new(label);
        let mut store = scratch.open_under(StoredPathOrder::Sensitive);
        let fronted = document("notes/fronted.md", "hash-fronted", "body\n")
            .with_frontmatter(Some(every_shape()), &ContentModel::none());
        let mut unread = document("notes/unread.md", "hash-unread", "body\n");
        unread.frontmatter_diagnostic_count = 1;
        let mut documents = vec![
            fronted,
            document("notes/bare.md", "hash-bare", "no block here\n"),
            unread,
        ];
        documents.extend((0..40).map(|at| {
            document(&format!("bulk/{at:02}.md"), "hash-bulk", "bulk\n")
                .with_frontmatter(Some(every_shape()), &ContentModel::none())
        }));
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

    fn held(&self, at: &str) -> Option<HeldFrontmatter> {
        self.snapshot()
            .held_frontmatter(&path(at))
            .unwrap_or_else(|problem| panic!("the frontmatter {at} holds: {problem}"))
    }

    fn plans(&self, at: &str) -> Vec<HeldFrontmatterPlan> {
        self.snapshot()
            .held_frontmatter_plans(&path(at))
            .unwrap_or_else(|problem| panic!("the plans of the frontmatter {at} holds: {problem}"))
    }
}

/// **A document's held frontmatter is the value its projection holds, typed
/// as it was written, beside the hash it was derived from**: a string of
/// digits stays a string, an integral float a float; a document with no block
/// holds none, one whose block did not read holds an unread block, and a path
/// holding no document answers nothing.
#[test]
fn a_documents_held_frontmatter_is_its_projection_typed_beside_its_hash() {
    let held = Held::new("held-frontmatter");
    let fronted = held.held("notes/fronted.md").expect("a document");
    assert_eq!(fronted.content_hash, "hash-fronted");
    let HeldBlock::Read(FrontmatterValue::Map(entries)) = &fronted.block else {
        panic!("a read block: {fronted:?}");
    };
    let shapes = every_shape();
    let FrontmatterValue::Map(written) = &shapes else {
        unreachable!("a map")
    };
    let mut written = written.clone();
    written.sort_by(|left, right| left.0.cmp(&right.0));
    assert_eq!(
        *entries, written,
        "the keys in byte order, each value typed"
    );
    assert_eq!(
        held.held("notes/bare.md"),
        Some(HeldFrontmatter {
            content_hash: "hash-bare".to_string(),
            block: HeldBlock::None,
        })
    );
    assert_eq!(
        held.held("notes/unread.md").map(|held| held.block),
        Some(HeldBlock::Unread)
    );
    assert_eq!(held.held("notes/absent.md"), None);
    assert_eq!(
        held.held("Notes/Fronted.md"),
        None,
        "a path is its own spelling"
    );
}

fn plan(emitted: &HeldFrontmatterPlan) -> QueryPlan {
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

/// **The bar on reading a document's frontmatter without its body.** The
/// holder is one equality seek of `documents_path` at the path, naming no
/// body, and the read runs that one statement and no other.
///
/// Control: the index dropped, the statement reads something else and fails.
#[test]
fn held_frontmatter_is_one_seek_that_never_names_the_body() {
    let mut held = Held::new("held-frontmatter-plan");
    let judge_holder = |plan: &QueryPlan| {
        plan.assert_no_full_scan();
        let rows = rows_of(plan, "d");
        rows.assert_searches_through("documents", Access::Index("documents_path"));
        rows.assert_search_constraint("documents", "(path=?)");
    };
    let plans = held.plans("notes/fronted.md");
    assert_eq!(
        plans.iter().map(|plan| plan.statement).collect::<Vec<_>>(),
        HeldFrontmatterStatement::all()
            .map(ReadStatement::HeldFrontmatter)
            .to_vec(),
    );
    for emitted in &plans {
        assert!(
            !emitted.plan.sql.contains("body"),
            "a statement names the body: {}",
            emitted.plan.sql
        );
    }
    judge_holder(&plan(&plans[0]));

    induced_failure::execute_out_of_band(&mut held.store, "DROP INDEX documents_path")
        .expect("dropping documents_path");
    let plans = held.plans("notes/fronted.md");
    failure_of("documents_path dropped", || judge_holder(&plan(&plans[0])));
}

/// Which test bars each statement the held-frontmatter read names.
/// Exhaustive, so a statement added to [`HeldFrontmatterStatement`] does not
/// compile until its author names the bar.
fn statement_barred_by(statement: HeldFrontmatterStatement) -> &'static str {
    match statement {
        HeldFrontmatterStatement::Holder => {
            "held_frontmatter_is_one_seek_that_never_names_the_body"
        }
    }
}

/// **Every statement the held-frontmatter read names carries a bar.**
#[test]
fn the_held_frontmatter_bars_cover_every_statement() {
    let mut slots: Vec<usize> = HeldFrontmatterStatement::all()
        .into_iter()
        .map(HeldFrontmatterStatement::slot)
        .collect();
    slots.sort_unstable();
    assert_eq!(
        slots,
        (0..HELD_FRONTMATTER_STATEMENTS).collect::<Vec<usize>>()
    );
    for statement in HeldFrontmatterStatement::all() {
        assert!(!statement_barred_by(statement).is_empty());
    }
}
