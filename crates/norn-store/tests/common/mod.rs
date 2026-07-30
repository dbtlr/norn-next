//! Scratch stores, and the facts the suite writes into them.
//!
//! Every suite in this crate is black box: it reaches the store through its
//! public API and never opens a connection, which is the same rule every other
//! crate in the workspace is held to. What this module holds is the temporary
//! directory a store's file lives in for the length of one test, and builders
//! for the fact shapes the suite writes.
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use norn_store::{
    BlockFact, CandidateFact, DocumentFacts, DocumentPath, FindingFacts, FrontmatterValue,
    HeadingFact, LinkFact, LinkFamily, Span, Store, TagFact, TagSource, VectorFacts,
};

/// Distinguishes two scratch directories taken in the same process.
static SERIAL: AtomicU64 = AtomicU64::new(0);

/// A directory that exists for one test and is removed with it.
pub struct Scratch {
    root: PathBuf,
}

impl Scratch {
    #[allow(clippy::disallowed_methods)] // Harness scaffolding: the directory a test's store lives in.
    pub fn new(label: &str) -> Self {
        let serial = SERIAL.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "norn-store-{label}-{}-{serial}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("a scratch directory");
        Scratch { root }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The path a store's database file sits at. Its parent does not exist, so
    /// that opening a store is what prepares it.
    pub fn database(&self) -> PathBuf {
        self.root.join("derived").join("store.sqlite3")
    }

    pub fn open(&self) -> Store {
        Store::open(self.database()).expect("opening a store")
    }

    /// Write bytes at a path under the scratch root.
    #[allow(clippy::disallowed_methods)] // Harness scaffolding: arranging a file a store has to judge.
    pub fn write(&self, relative: &str, bytes: &[u8]) -> PathBuf {
        let path = self.root.join(relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("a scratch directory");
        }
        std::fs::write(&path, bytes).expect("writing a scratch file");
        path
    }

    #[allow(clippy::disallowed_methods)] // Harness scaffolding: the bytes a store wrote, to damage them.
    pub fn read(&self, path: &Path) -> Vec<u8> {
        std::fs::read(path).expect("reading a scratch file")
    }

    #[allow(clippy::disallowed_methods)] // Harness scaffolding: whether a store's file survived.
    pub fn exists(&self, path: &Path) -> bool {
        std::fs::metadata(path).is_ok()
    }
}

impl Drop for Scratch {
    #[allow(clippy::disallowed_methods)] // Harness scaffolding: removing the directory this test made.
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// A document path, or a panic naming what was wrong with it.
pub fn path(text: &str) -> DocumentPath {
    DocumentPath::new(text)
        .unwrap_or_else(|problem| panic!("`{text}` is a document path: {problem}"))
}

pub fn span(line: u64, column: u64, byte_offset: u64) -> Span {
    Span {
        line,
        column,
        byte_offset,
    }
}

/// A document with a body and nothing derived from it.
pub fn document(text: &str, hash: &str, body: &str) -> DocumentFacts {
    DocumentFacts::new(path(text), hash, body)
}

/// A document carrying one of every fact shape the store holds, including the
/// ones whose optional fields are absent.
pub fn document_with_every_fact(text: &str, hash: &str) -> DocumentFacts {
    let mut facts = document(
        text,
        hash,
        "the body of the document\n\nwith two paragraphs\n",
    );
    facts.body_offset = 42;
    facts.diagnostic_count = 1;
    facts.frontmatter = Some(FrontmatterValue::Map(vec![
        (
            "title".to_string(),
            FrontmatterValue::String("Norn".to_string()),
        ),
        ("draft".to_string(), FrontmatterValue::Bool(false)),
    ]));
    facts.links = vec![
        LinkFact {
            family: LinkFamily::Wikilink,
            embed: true,
            protocol: None,
            target: "glossary".to_string(),
            title: Some("The glossary".to_string()),
            anchor: Some("Terms".to_string()),
            block_ref: None,
            span: span(1, 1, 0),
        },
        LinkFact {
            family: LinkFamily::Markdown,
            embed: false,
            protocol: Some("https".to_string()),
            target: "example.com/docs".to_string(),
            title: Some(String::new()),
            anchor: None,
            block_ref: Some("blk-1".to_string()),
            span: span(3, 5, 30),
        },
    ];
    facts.headings = vec![
        HeadingFact {
            level: 1,
            text: "Use norn".to_string(),
            slug: "use-norn".to_string(),
            span: span(1, 1, 0),
            body_offset: 12,
            inside_container: false,
        },
        HeadingFact {
            level: 3,
            text: "Use norn".to_string(),
            slug: "use-norn-1".to_string(),
            span: span(9, 3, 120),
            body_offset: 134,
            inside_container: true,
        },
    ];
    facts.blocks = vec![
        BlockFact {
            block_id: "blk-1".to_string(),
            span: Some(span(4, 20, 60)),
        },
        BlockFact {
            block_id: "blk-2".to_string(),
            span: None,
        },
    ];
    facts.tags = vec![
        TagFact {
            name: "area/Project".to_string(),
            source: TagSource::Body,
            span: Some(span(6, 4, 80)),
        },
        TagFact {
            name: "reference".to_string(),
            source: TagSource::Frontmatter,
            span: None,
        },
    ];
    facts
}

/// An ambiguity finding over `target`, with `candidates` as its bounded head out
/// of `total`.
pub fn ambiguity(
    at: &str,
    target: &str,
    class_key: &str,
    candidates: &[&str],
    total: u64,
) -> FindingFacts {
    FindingFacts {
        kind: "resolution/ambiguous-target".to_string(),
        severity: "warning".to_string(),
        path: path(at),
        class_key: Some(class_key.to_string()),
        target: Some(target.to_string()),
        span: Some(span(2, 1, 10)),
        candidates: candidates
            .iter()
            .map(|candidate| CandidateFact {
                path: path(candidate),
                suffix: candidate.to_string(),
            })
            .collect(),
        candidates_total: total,
        message: format!("`{target}` addresses {total} documents"),
        detail: None,
    }
}

/// A finding with no ambiguity class — the shape a vault-schema violation takes.
pub fn violation(at: &str) -> FindingFacts {
    FindingFacts {
        kind: "schema/field-missing".to_string(),
        severity: "error".to_string(),
        path: path(at),
        class_key: None,
        target: None,
        span: None,
        candidates: Vec::new(),
        candidates_total: 0,
        message: "the `status` field is required".to_string(),
        detail: Some(r#"{"field":"status"}"#.to_string()),
    }
}

pub fn vector(at: &str, hash: &str) -> VectorFacts {
    VectorFacts {
        path: path(at),
        model_id: "stub".to_string(),
        model_version: "1".to_string(),
        content_hash: hash.to_string(),
        dimensions: 4,
        embedding: vec![0, 1, 2, 3, 4, 5, 6, 7],
    }
}
