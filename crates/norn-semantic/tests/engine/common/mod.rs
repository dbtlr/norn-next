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

/// Distinguishes two scratch directories taken in the same process.
static SERIAL: AtomicU64 = AtomicU64::new(0);

/// A directory that exists for one test and is removed with it, holding a
/// store's database and a sidecar's beside each other.
pub struct Scratch {
    root: PathBuf,
}

impl Scratch {
    #[allow(clippy::disallowed_methods)] // Harness scaffolding: the directory a test's databases live in.
    pub fn new(label: &str) -> Self {
        let serial = SERIAL.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "norn-semantic-{label}-{}-{serial}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("a scratch directory");
        Scratch { root }
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

impl Drop for Scratch {
    #[allow(clippy::disallowed_methods)] // Harness scaffolding: removing the directory this test made.
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
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
