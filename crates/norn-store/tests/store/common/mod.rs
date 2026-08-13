//! Scratch stores, and the facts the suite writes into them.
//!
//! What this module holds is the temporary directory a store's file lives in for
//! the length of one test, and builders for the fact shapes the suite writes.
//! Reading and writing files beside a store belongs to the lifecycle suite, which
//! is the only one that judges the file, so those helpers live there.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use norn_store::{
    BlockFact, CandidateFact, Change, ClassKey, DerivationCounters, DocumentFacts, DocumentPath,
    FindingFacts, FrontmatterValue, HeadingFact, IncrementOutcome, IncrementProvenance, LinkFact,
    LinkFamily, Provenance, Request, Span, Store, TagFact, TagSource, VectorFacts, suffix_probe,
};
use norn_testkit::counters::CounterSnapshot;
use norn_wire::{FindingKind, Severity};

/// A snapshot of a request's reading, in the shape the harness compares.
pub fn snapshot(counters: &DerivationCounters) -> CounterSnapshot {
    counters.readings().collect()
}

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

    /// The path a store's database file sits at. Its parent does not exist, so
    /// that opening a store is what prepares it.
    pub fn database(&self) -> PathBuf {
        self.root.join("derived").join("store.sqlite3")
    }

    pub fn open(&self) -> Store {
        Store::open(self.database()).expect("opening a store")
    }
}

impl Drop for Scratch {
    #[allow(clippy::disallowed_methods)] // Harness scaffolding: removing the directory this test made.
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// Write one document as a changeset of its own.
///
/// The store's one way in for document facts is a changeset, so a case that is
/// not *about* changesets still applies one — this is that changeset, spelled
/// once. The mark is `Derived`, which is what a case about anything else is: a
/// host read the file and derived these facts from the bytes it read.
pub fn write_document(request: &mut Request<'_>, facts: &DocumentFacts) -> IncrementOutcome {
    request
        .apply_increment(
            IncrementProvenance::Derived,
            [Change::Upsert(facts.clone())],
        )
        .expect("applying a document upsert")
}

/// Record one death as a changeset of its own.
pub fn record_death(
    request: &mut Request<'_>,
    at: &DocumentPath,
    provenance: Provenance,
) -> IncrementOutcome {
    request
        .apply_increment(
            IncrementProvenance::Derived,
            [Change::Death {
                path: at.clone(),
                provenance,
            }],
        )
        .expect("applying a death")
}

/// Write several documents in one changeset, which is what a case that needs a
/// populated store rather than a sequence of writes wants.
pub fn write_documents(request: &mut Request<'_>, facts: &[DocumentFacts]) -> IncrementOutcome {
    request
        .apply_increment(
            IncrementProvenance::Derived,
            facts
                .iter()
                .cloned()
                .map(Change::Upsert)
                .collect::<Vec<_>>(),
        )
        .expect("applying a changeset")
}

/// A document path, or a panic naming what was wrong with it.
pub fn path(text: &str) -> DocumentPath {
    DocumentPath::new(text)
        .unwrap_or_else(|problem| panic!("`{text}` is a document path: {problem}"))
}

/// A class key, or a panic naming what was wrong with it.
pub fn class(text: &str) -> ClassKey {
    ClassKey::new(text).unwrap_or_else(|problem| panic!("`{text}` is a class key: {problem}"))
}

/// A set of class keys, which is what a finding's membership is.
pub fn classes(texts: &[&str]) -> BTreeSet<ClassKey> {
    texts.iter().copied().map(class).collect()
}

pub fn span(line: u64, column: u64, byte_offset: u64) -> Span {
    Span {
        line,
        column,
        byte_offset,
    }
}

/// A document with a body and nothing derived from it. Its body is the whole
/// document, so its size is the body's length.
pub fn document(text: &str, hash: &str, body: &str) -> DocumentFacts {
    DocumentFacts::new(path(text), hash, body, body.len() as u64)
}

/// A document carrying one of every fact shape the store holds, including the
/// ones whose optional fields are absent.
pub fn document_with_every_fact(text: &str, hash: &str) -> DocumentFacts {
    let mut facts = document(
        text,
        hash,
        "the body of the document\n\nwith two paragraphs\n",
    );
    // A frontmatter block sits ahead of the body, so the document is longer than
    // its body by exactly the frame.
    facts.body_offset = 42;
    facts.byte_length = facts.body_offset + facts.body.len() as u64;
    facts.frontmatter_diagnostic_count = 1;
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

/// An ambiguity finding over `target`, in the one class `class_key` names, with
/// `candidates` as its bounded head out of `total`.
pub fn ambiguity(
    at: &str,
    target: &str,
    class_key: &str,
    candidates: &[&str],
    total: u64,
) -> FindingFacts {
    let mut finding = ambiguity_for_target(at, target, candidates, total);
    finding.class_keys = classes(&[class_key]);
    finding
}

/// The same finding with its classes taken from the probe `target` opens, which
/// is how a resolution reading mints them: one class per reduction.
pub fn ambiguity_for_target(
    at: &str,
    target: &str,
    candidates: &[&str],
    total: u64,
) -> FindingFacts {
    FindingFacts {
        kind: FindingKind::PathNamesNoDocument,
        severity: Severity::Warning,
        path: path(at),
        class_keys: suffix_probe(target)
            .unwrap_or_else(|problem| panic!("`{target}` is a suffix target: {problem}"))
            .class_keys(),
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

/// A finding about the document derived at its subject: no ambiguity class, and
/// a kind that stands beside a document row rather than in place of one.
pub fn unread_block(at: &str) -> FindingFacts {
    FindingFacts {
        kind: FindingKind::FrontmatterUnreadable,
        severity: Severity::Error,
        path: path(at),
        class_keys: BTreeSet::new(),
        target: None,
        span: None,
        candidates: Vec::new(),
        candidates_total: 0,
        message: format!("`{at}` derives without its frontmatter"),
        detail: Some("the block is not well-formed".to_string()),
    }
}

/// A finding with no ambiguity class — the shape a vault-schema violation takes.
pub fn violation(at: &str) -> FindingFacts {
    FindingFacts {
        kind: FindingKind::BodyBytesNotUtf8,
        severity: Severity::Error,
        path: path(at),
        class_keys: BTreeSet::new(),
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
