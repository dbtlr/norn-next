//! **The derivation digest.** A pinned corpus derived from zero digests to a
//! pinned number, and the number moves only with the derivation version.
//!
//! A store records the derivation version its rows were written by and an
//! open under another one rebuilds from zero. Nothing but this suite makes the
//! version move when it has to: a change to what derivation writes for
//! unchanged input — a parser that trims a heading differently, one more field
//! row, a class filed under another key — leaves the DDL fingerprint and the
//! path order where they were, and a store an earlier build derived would keep
//! its rows until each file was edited.
//!
//! So the corpus below is attached by a real host, which derives it from zero
//! through the heal every attach runs, and every derived row the store holds is
//! digested: the document rows with their sub-fingerprints and their raw and
//! folded suffix keys, the links, headings, blocks and tags, the field rows
//! with their typed halves, every finding with its candidates and classes, the
//! terms the full-text index holds, and the pinned vault schema. Row
//! identifiers, write generations and timestamps are left out, because none of
//! them is a function of the vault: they say where a row landed, how many
//! writes one store took, and when.
//!
//! **The digest is pinned beside the version it was taken under.** Where the
//! digest moves and the version does not, derivation writes different rows for
//! the same bytes and every existing store needs the rebuild only a new version
//! forces: the failure prints the new digest and says to move
//! `DERIVATION_VERSION`. Where the version moved, the pair is taken again.
//!
//! **The digest is the same on every host.** The corpus is written from this
//! file rather than read off a checkout, so no line-ending or attribute
//! conversion reaches it; no two of its names differ only by case, so a root
//! that folds case derives the same rows as one that does not; every name is
//! valid UTF-8 in precomposed form, which every volume a lane runs on keeps as
//! written; and the rows are vault-relative and sorted before they are hashed.
//!
//! **The corpus has to exercise what derivation writes**, or a change to an
//! unexercised fact slips past the digest. The case asserts the shapes it
//! stands on, so a corpus edit that stops exercising one fails here rather
//! than weakening the digest in silence. A derivation change touching a fact
//! no document below carries grows the corpus with it.
//!
//! Each tree sits in a testkit sandbox, which is a unix-only harness.
#![cfg(unix)]
#![allow(clippy::disallowed_methods)] // Harness scaffolding: this suite's own corpus.

mod attach;

use std::path::Path;

use norn_host::DERIVATION_VERSION;
use norn_store::{DerivationVersion, LinkFamily, OpenOutcome, TagSource};
use norn_testkit::equivalence::{DerivedRows, assert_operationally_valid};
use norn_testkit::process::Sandbox;
use norn_wire::FindingKind;

/// The digest the corpus derives to, and the derivation version it was taken
/// under.
const PINNED: (DerivationVersion, &str) = (
    DerivationVersion::new(1),
    "196a01a52f97df8c8e9ad37ea0affa74bb88a79dd19f06dc42541aac2c220992",
);

/// The vault schema the corpus is derived under: a field of every declared
/// type, and a tag facet that reports what it does not declare.
const SCHEMA: &str = "\
version: 1
fields:
  title:
    type: text
  rating:
    type: number
  weight:
    type: number
  published:
    type: boolean
  created:
    type: date
  topics:
    type: tags
tags:
  declared: [project, area/norn, solo]
  undeclared: report
";

/// The corpus: every document and every non-document file, by the path it is
/// written at.
fn corpus() -> Vec<(&'static str, Vec<u8>)> {
    // Distinct keys past the text layer's frontmatter bound, so the block is
    // refused for its size and never parsed.
    let mut too_large = String::from("---\n");
    for line in 0..400 {
        too_large.push_str(&format!(
            "padding{line}: a line past the frontmatter bound\n"
        ));
    }
    too_large.push_str("---\n# Too large\n");
    vec![
        ("Glossary.md", GLOSSARY.as_bytes().to_vec()),
        ("Notes.md", NOTES.as_bytes().to_vec()),
        // CRLF line endings: every span and offset is counted in the bytes
        // the file holds.
        (
            "notes/Deep Note.md",
            b"---\r\ntags: solo\r\ntitle: Deep\r\n---\r\n# Deep Note\r\n\r\nBody #deep\r\n"
                .to_vec(),
        ),
        (
            "Ünïcode Straße.md",
            "## 日本語\n\nA #café tag and a [[Glossary|glossary]] link.\n"
                .as_bytes()
                .to_vec(),
        ),
        ("empty.md", Vec::new()),
        (
            "only-frontmatter.md",
            b"---\nrating: high\npublished: maybe\ncreated: not a date\n---\n".to_vec(),
        ),
        (
            "broken/unclosed.md",
            b"---\ntitle: never closes\n# A heading\n".to_vec(),
        ),
        (
            "broken/unreadable.md",
            b"---\ntitle: [unclosed\n---\n# Unreadable\n".to_vec(),
        ),
        ("broken/too-large.md", too_large.into_bytes()),
        (
            "broken/not-utf8.md",
            b"# a title\n\nthese bytes are not text: \xff\xfe\n".to_vec(),
        ),
        (
            "broken/back\\slash.md",
            b"# a name no document has\n".to_vec(),
        ),
        ("assets/pic.png", b"\x89PNG not a document".to_vec()),
        ("clutter.txt", b"# not a document either\n".to_vec()),
    ]
}

/// Every frontmatter value shape, a field of every declared type and one
/// undeclared, both setext levels, every ATX level, containers, repeated and
/// marked-up headings, a heading ending in a non-breaking space, both link
/// families with and without protocol, title, anchor, block reference and
/// embed, body and frontmatter tags, declared and not, and block ids.
const GLOSSARY: &str = "---
title: The glossary
aliases: [gloss, \"Glossary Term\"]
rating: 4
weight: 2.5
published: true
nothing: null
empty_list: []
created: 2026-09-24
topics: [project, stray]
tags: [project, \"#area/norn\", undeclared-in-frontmatter]
nested:
  depth: 1
  inner:
    - a
    - key: value
---
# Glossary

Setext Heading
==============

Sub setext
----------

## A heading ending in a non-breaking space\u{a0}

### Use `norn` **bold**

#### Repeated

#### Repeated

> ## Quoted heading

- ## Listed heading

##### Fifth level

###### Deepest ######

A paragraph with a #project tag, a (#area/norn) tag, an #undeclared-body tag, not a#tag, and #123.

See [[Notes]] and [[notes/Deep Note|shown title]] and [[Glossary#Repeated]] and [[Notes#^para-block]].
Embed ![[picture.png]] and ![[Notes#Setext|embedded]].
Markdown [shown](notes/Deep%20Note.md) and [anchor](Glossary.md#use-norn-bold \"a title\") and [web](https://example.com/page) and ![image](assets/pic.png) and [block](Notes.md#^para-block) and [vault](vault://Notes).
A wikilink with a protocol: [[https://example.com/wiki|external]].

A paragraph closing on a block id. ^glossary-block

```
# not a heading, #not-a-tag, [[not a link]]
```
^after-fence
";

/// The target of the corpus's block references, and a document with no
/// frontmatter at all.
const NOTES: &str = "# Notes

Setext
------

A paragraph the glossary references. ^para-block

- a list item with a block id ^list-block
";

#[test]
fn the_derivation_digest_moves_only_with_the_derivation_version() {
    let sandbox =
        Sandbox::new(Path::new(env!("CARGO_TARGET_TMPDIR")), "derivation").expect("a sandbox");
    let root = sandbox.work_dir();
    let vault = root.join("vault");
    for (at, bytes) in corpus() {
        let path = vault.join(at);
        std::fs::create_dir_all(path.parent().expect("a corpus path has a parent"))
            .expect("creating a corpus directory");
        std::fs::write(&path, bytes).unwrap_or_else(|e| panic!("writing `{at}`: {e}"));
    }
    std::fs::create_dir_all(vault.join(".norn")).expect("creating the schema directory");
    std::fs::write(vault.join(".norn/schema.yaml"), SCHEMA).expect("writing the vault schema");
    let vault = attach::Vault::adopt(&root);

    {
        let host = vault.host();
        drop(attach::attach_and_wait(&host, vault.name()));
    }

    let mut store = vault.store();
    assert_eq!(
        *store.open_outcome(),
        OpenOutcome::Reused,
        "the store the attach derived did not reopen under this build's derivation version"
    );
    assert_operationally_valid(&mut store, "the corpus derived from zero");
    let rows = DerivedRows::read(&mut store).expect("reading the derived rows");
    assert_the_corpus_exercises_every_fact(&rows);

    let digest = rows.digest();
    let (pinned_version, pinned_digest) = PINNED;
    assert!(
        DERIVATION_VERSION == pinned_version && digest == pinned_digest,
        "{}",
        if DERIVATION_VERSION == pinned_version {
            format!(
                "the corpus now derives to {digest}, and derivation version {pinned_version} \
                 derived it to {pinned_digest}. Where derivation writes different rows for the \
                 same bytes, every store an earlier build derived needs the rebuild only a new \
                 version forces: move DERIVATION_VERSION in crates/norn-host/src/derivation.rs \
                 and pin ({}, \"{digest}\") here. Where this corpus, its schema or the way the \
                 rows are digested is all that changed, pin ({pinned_version}, \"{digest}\"); a corpus edit that lands with a \
                 derivation change still owes the new version.",
                DERIVATION_VERSION.get() + 1
            )
        } else {
            format!(
                "DERIVATION_VERSION is {DERIVATION_VERSION} and the digest here was taken under \
                 {pinned_version}: pin ({DERIVATION_VERSION}, \"{digest}\") here."
            )
        }
    );
}

/// **The shapes the digest stands on.** Each is a fact a derivation change
/// could move; the corpus is edited with this list, so it keeps carrying all
/// of them.
fn assert_the_corpus_exercises_every_fact(rows: &DerivedRows) {
    let projection = rows.projection();
    let document = |path: &str| {
        projection
            .document(path)
            .unwrap_or_else(|| panic!("the corpus derived no row at `{path}`"))
    };

    let glossary = document("Glossary.md");
    let levels: std::collections::BTreeSet<u8> = glossary
        .headings
        .iter()
        .map(|heading| heading.level)
        .collect();
    assert_eq!(
        levels,
        (1..=6).collect(),
        "a heading level is not exercised"
    );
    assert!(
        glossary
            .headings
            .iter()
            .any(|heading| heading.inside_container),
        "no heading inside a container is exercised"
    );
    assert!(
        glossary.headings.iter().any(|heading| heading
            .text
            .starts_with("A heading ending in a non-breaking space")),
        "the heading ending in a non-breaking space derived no heading"
    );
    assert!(
        glossary
            .headings
            .iter()
            .any(|heading| heading.slug.ends_with("-1")),
        "no repeated heading's slug is exercised"
    );
    for family in [LinkFamily::Wikilink, LinkFamily::Markdown] {
        let links: Vec<_> = glossary
            .links
            .iter()
            .filter(|link| link.family == family)
            .collect();
        assert!(
            links.iter().any(|link| link.protocol.is_some()),
            "{family:?}: no protocol"
        );
        assert!(
            links.iter().any(|link| link.anchor.is_some()),
            "{family:?}: no anchor"
        );
        assert!(
            links.iter().any(|link| link.block_ref.is_some()),
            "{family:?}: no block reference"
        );
    }
    assert!(
        glossary.links.iter().any(|link| link.title.is_some()),
        "no link title is exercised"
    );
    // A wikilink embed is the one embed the text layer records; the Markdown
    // image beside it is not a link, and the digest pins that it is not.
    assert!(
        glossary.links.iter().any(|link| link.embed),
        "no embed is exercised"
    );
    assert!(
        glossary.blocks.len() >= 2,
        "the glossary's block ids derived {:?}",
        glossary.blocks
    );
    for source in [TagSource::Body, TagSource::Frontmatter] {
        assert!(
            glossary.tags.iter().any(|tag| tag.source == source),
            "no {source:?} tag is exercised"
        );
    }
    assert!(
        glossary.frontmatter.is_some() && !glossary.fields.rows().is_empty(),
        "the glossary's frontmatter derived no field rows"
    );
    assert!(
        document("notes/Deep Note.md").body.contains("\r\n"),
        "no CRLF document is exercised"
    );
    assert!(
        rows.fields()
            .iter()
            .any(|(field, value)| field.ends_with(".folded_suffix_key")
                && rows
                    .fields()
                    .get(&field.replace(".folded_suffix_key", ".suffix_key"))
                    .is_some_and(|raw| raw != value)),
        "no suffix key that folds to another spelling is exercised"
    );

    let kinds: std::collections::BTreeSet<&str> = projection
        .findings()
        .iter()
        .map(|finding| finding.kind.as_str())
        .collect();
    for kind in [
        FindingKind::PathNamesNoDocument,
        FindingKind::BodyBytesNotUtf8,
        FindingKind::FrontmatterTooLarge,
        FindingKind::FrontmatterUnclosed,
        FindingKind::FrontmatterUnreadable,
        FindingKind::UndeclaredTag,
    ] {
        assert!(
            kinds.contains(kind.as_str()),
            "no `{kind}` finding is exercised; the corpus derived {kinds:?}"
        );
    }
    assert!(
        !projection.indexed_terms().is_empty(),
        "the full-text index holds no term"
    );
}
