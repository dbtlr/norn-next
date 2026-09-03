//! Scratch stores and sidecars, the counting embedder, and the from-zero
//! recompute the convergence bar compares against.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use norn_embed::{EmbedError, Embedder, Embedding, Model, StubEmbedder};
use norn_semantic::{Engine, VectorRow};
use norn_store::{
    Change, DocumentFacts, DocumentPath, FeedCursor, IncrementProvenance, Provenance, Store,
};
use norn_testkit::scratch::Scratch as TestkitScratch;

/// A store's database and a sidecar's beside each other, under a directory
/// that lasts one test.
///
/// The naming and the removal are [`norn_testkit::scratch::Scratch`]'s; what
/// this adds is where each database sits inside the tree and how one is
/// opened.
pub struct Scratch {
    root: TestkitScratch,
}

impl Scratch {
    pub fn new(label: &str) -> Self {
        Scratch {
            root: TestkitScratch::new(&format!("norn-semantic-{label}")),
        }
    }

    pub fn store_path(&self) -> PathBuf {
        self.root.join("derived").join("store.sqlite3")
    }

    pub fn sidecar_path(&self) -> PathBuf {
        self.root.join("derived").join("semantic.sqlite3")
    }

    pub fn store(&self) -> Store {
        Store::open(self.store_path()).expect("opening a store")
    }

    pub fn engine(&self, embedder: Arc<dyn Embedder>) -> Engine {
        Engine::open(self.sidecar_path(), embedder).expect("opening an engine")
    }
}

/// The stub embedder behind a call counter, so a case can assert what was
/// recomputed rather than only what resulted.
pub struct CountingEmbedder {
    inner: StubEmbedder,
    calls: AtomicU64,
}

impl CountingEmbedder {
    pub fn new() -> Arc<Self> {
        Arc::new(CountingEmbedder {
            inner: StubEmbedder::new(),
            calls: AtomicU64::new(0),
        })
    }

    /// How many times `embed` was asked, across every handle to this counter.
    pub fn calls(&self) -> u64 {
        self.calls.load(Ordering::Relaxed)
    }
}

impl Embedder for CountingEmbedder {
    fn model(&self) -> &Model {
        self.inner.model()
    }

    fn dimensions(&self) -> std::num::NonZeroUsize {
        self.inner.dimensions()
    }

    fn embed(&self, text: &str) -> Result<Embedding, EmbedError> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        self.inner.embed(text)
    }
}

/// The stub embedder with a one-shot side effect: the first `embed` call
/// opens its own handle on the store and applies the given change, so a
/// drain in progress meets a store that moved between its page read and a
/// later fetch — the readings the deferred arm answers.
pub struct InterferingEmbedder {
    inner: StubEmbedder,
    store_path: PathBuf,
    effect: Box<dyn Fn(&mut Store) + Send + Sync>,
    fired: std::sync::atomic::AtomicBool,
}

impl InterferingEmbedder {
    /// An embedder whose first call rewrites `interference` behind the
    /// drain's back.
    pub fn rewriting(store_path: PathBuf, interference: DocumentFacts) -> Arc<Self> {
        Self::with_effect(
            store_path,
            Box::new(move |store| write_document(store, &interference)),
        )
    }

    /// An embedder whose first call records the death of `at` behind the
    /// drain's back.
    pub fn killing(store_path: PathBuf, at: &str) -> Arc<Self> {
        let at = at.to_string();
        Self::with_effect(store_path, Box::new(move |store| record_death(store, &at)))
    }

    fn with_effect(
        store_path: PathBuf,
        effect: Box<dyn Fn(&mut Store) + Send + Sync>,
    ) -> Arc<Self> {
        Arc::new(InterferingEmbedder {
            inner: StubEmbedder::new(),
            store_path,
            effect,
            fired: std::sync::atomic::AtomicBool::new(false),
        })
    }
}

impl Embedder for InterferingEmbedder {
    fn model(&self) -> &Model {
        self.inner.model()
    }

    fn dimensions(&self) -> std::num::NonZeroUsize {
        self.inner.dimensions()
    }

    fn embed(&self, text: &str) -> Result<Embedding, EmbedError> {
        if !self.fired.swap(true, Ordering::Relaxed) {
            let mut store = Store::open(&self.store_path).expect("a second handle on the store");
            (self.effect)(&mut store);
        }
        self.inner.embed(text)
    }
}

/// An embedder that always refuses, for the refusal path.
pub struct RefusingEmbedder {
    inner: StubEmbedder,
}

impl RefusingEmbedder {
    pub fn new() -> Arc<Self> {
        Arc::new(RefusingEmbedder {
            inner: StubEmbedder::new(),
        })
    }
}

impl Embedder for RefusingEmbedder {
    fn model(&self) -> &Model {
        self.inner.model()
    }

    fn dimensions(&self) -> std::num::NonZeroUsize {
        self.inner.dimensions()
    }

    fn embed(&self, _text: &str) -> Result<Embedding, EmbedError> {
        Err(EmbedError::runtime(
            self.inner.model().clone(),
            "the harness refuses every input",
        ))
    }
}

/// An embedder that answers one value short of its promise, for the width
/// guard.
pub struct NarrowEmbedder {
    inner: StubEmbedder,
}

impl NarrowEmbedder {
    pub fn new() -> Arc<Self> {
        Arc::new(NarrowEmbedder {
            inner: StubEmbedder::new(),
        })
    }
}

impl Embedder for NarrowEmbedder {
    fn model(&self) -> &Model {
        self.inner.model()
    }

    fn dimensions(&self) -> std::num::NonZeroUsize {
        self.inner.dimensions()
    }

    fn embed(&self, text: &str) -> Result<Embedding, EmbedError> {
        let embedding = self.inner.embed(text)?;
        let mut values = embedding.values().to_vec();
        values.pop();
        Ok(Embedding::new(embedding.model().clone(), values))
    }
}

/// A document path, or a panic naming what was wrong with it.
pub fn path(text: &str) -> DocumentPath {
    DocumentPath::new(text)
        .unwrap_or_else(|problem| panic!("`{text}` is a document path: {problem}"))
}

/// A document with a body and nothing else derived from it.
pub fn document(text: &str, hash: &str, body: &str) -> DocumentFacts {
    DocumentFacts::new(path(text), hash, body, body.len() as u64)
}

/// Write one document as a changeset of its own.
pub fn write_document(store: &mut Store, facts: &DocumentFacts) {
    store
        .begin_request()
        .apply_increment(
            IncrementProvenance::Derived,
            [Change::Upsert(facts.clone())],
        )
        .expect("applying a document upsert");
}

/// Record one death as a changeset of its own.
pub fn record_death(store: &mut Store, at: &str) {
    store
        .begin_request()
        .apply_increment(
            IncrementProvenance::Derived,
            [Change::Death {
                path: path(at),
                provenance: Provenance::PlanDelete,
            }],
        )
        .expect("applying a death");
}

/// The from-zero recompute the convergence bar compares against: every
/// current lane-1 document, fetched and embedded independently of anything
/// the engine holds, in path order.
pub fn recompute(store: &mut Store, embedder: &dyn Embedder) -> Vec<VectorRow> {
    const PAGE: usize = 64;
    let mut rows = Vec::new();
    let mut cursor: Option<FeedCursor> = None;
    loop {
        let page = store
            .begin_request()
            .changed_documents_after(cursor.as_ref(), PAGE)
            .expect("a page of the document feed");
        let Some((last, _)) = page.last() else { break };
        cursor = Some(last.clone());
        let full = page.len() == PAGE;
        for (_, fed) in &page {
            let facts = store
                .begin_request()
                .stored_facts(&fed.path)
                .expect("reading a document")
                .expect("a document the feed just named");
            let embedding = embedder.embed(&facts.body).expect("embedding a body");
            rows.push(VectorRow {
                path: fed.path.as_str().to_string(),
                input_hash: fed.body_hash.clone(),
                values: embedding.values().to_vec(),
            });
        }
        if !full {
            break;
        }
    }
    rows.sort_by(|a, b| a.path.cmp(&b.path));
    rows
}
