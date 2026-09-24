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
//! through the heal every attach runs — the main vault, and a second one for
//! the tag stance the main vault does not take, since a stance is a vault's own
//! declaration — and every derived row each store holds is digested: the document rows with their sub-fingerprints and their raw and
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

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use norn_host::DERIVATION_VERSION;
use norn_store::{DerivationVersion, FieldContainer, FieldRow, LinkFamily, OpenOutcome, TagSource};
use norn_testkit::equivalence::{DerivedRows, assert_operationally_valid};
use norn_testkit::process::Sandbox;
use norn_wire::FindingKind;

/// The digest the corpus derives to, and the derivation version it was taken
/// under.
const PINNED: (DerivationVersion, &str) = (
    DerivationVersion::new(1),
    "8ad4d8cdceed99c97941d797d048ccf0122b1efc3e643241744616d6eca47ef9",
);

/// The vault schema the main corpus is derived under: a field of every
/// declared type, and a tag facet that reports what it does not declare.
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

/// The schema of the second vault: the other stance a tag facet can take on
/// what it does not declare. A stance is a vault's own declaration, so the
/// other one needs a vault of its own.
const ALLOWING_SCHEMA: &str = "\
version: 1
tags:
  declared: [kept]
  undeclared: allow
";

/// The second vault's corpus: tags the facet does not declare, in both homes,
/// under a stance that allows them.
fn allowing_corpus() -> Vec<(&'static str, Vec<u8>)> {
    vec![(
        "Allowed.md",
        b"---\ntags: [kept, free-form]\n---\n# Allowed\n\nA #free-body tag.\n".to_vec(),
    )]
}

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
        // Values whose typed keys only a sign or an offset decides.
        (
            "values/signed.md",
            b"---\nrating: -3\nweight: -0.5\ncreated: 2026-09-24T10:00:00+02:00\n---\n# Signed\n"
                .to_vec(),
        ),
        // A datetime whose zone suffix is `Z` rather than a numeric offset.
        (
            "values/zulu.md",
            b"---\ncreated: 2026-09-24T10:00:00Z\n---\n# Zulu\n".to_vec(),
        ),
        // A datetime carrying fractional seconds, which the clock reading
        // drops before it parses the whole-second field.
        (
            "values/fractional.md",
            b"---\ncreated: 2026-09-24T10:00:00.250+02:00\n---\n# Fractional\n".to_vec(),
        ),
        ("Markup.md", MARKUP.as_bytes().to_vec()),
        // Lone CR line endings, around a fence whose content reads as a tag
        // wherever the fence is not recognized.
        (
            "cr-only.md",
            b"# CR only\r\rBefore #crtag\r\r```\r#fenced\r```\r\rAfter\r".to_vec(),
        ),
        // A document under a hidden directory.
        (
            ".hidden/Hidden Note.md",
            b"# Hidden\n\nA #hidden-tag.\n".to_vec(),
        ),
    ]
}

/// Constructs the text layer masks, skips or records nothing for: HTML
/// comments holding tag markers, an autolink, a reference-style link, an empty
/// heading, a block id after a table, and a setext heading over two lines.
const MARKUP: &str = "# Markup

<!-- #commented -->

Inline <!-- #inline-commented --> comment.

An autolink <https://example.com/auto> and a reference [shown][notes-ref] link.

[notes-ref]: Notes.md

#

| a | b |
| - | - |
| 1 | 2 |

^table-block

Multi line
setext title
============
";

/// Every frontmatter value shape, a field of every declared type and one
/// undeclared, both setext levels, every ATX level, containers, repeated and
/// marked-up headings, a heading ending in a non-breaking space, both link
/// families with and without protocol, title, anchor, block reference and
/// embed, body and frontmatter tags, declared and not, block ids, a
/// frontmatter wikilink carrying an alias and an anchor, and a tag whose name
/// carries a combining mark.
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
related: \"[[Notes#Setext|see here]]\"
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
A tag whose name carries a combining mark: #cafe\u{301}.

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
    let reporting = derive(&root.join("reporting"), corpus(), SCHEMA);
    let allowing = derive(&root.join("allowing"), allowing_corpus(), ALLOWING_SCHEMA);
    assert_the_corpus_exercises_every_fact(&reporting);
    assert_the_allowing_vault_exercises_its_stance(&allowing);

    let digest = DerivedRows::digest(&BTreeMap::from([
        ("reporting", reporting),
        ("allowing", allowing),
    ]));
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

/// Write `files` and `schema` into a vault under `root`, attach a real host to
/// derive it from zero, and read every derived row the store holds.
fn derive(root: &Path, files: Vec<(&'static str, Vec<u8>)>, schema: &str) -> DerivedRows {
    let vault = root.join("vault");
    for (at, bytes) in files {
        let path = vault.join(at);
        std::fs::create_dir_all(path.parent().expect("a corpus path has a parent"))
            .expect("creating a corpus directory");
        std::fs::write(&path, bytes).unwrap_or_else(|e| panic!("writing `{at}`: {e}"));
    }
    std::fs::create_dir_all(vault.join(".norn")).expect("creating the schema directory");
    std::fs::write(vault.join(".norn/schema.yaml"), schema).expect("writing the vault schema");
    let vault = attach::Vault::adopt(root);

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
    DerivedRows::read(&mut store).expect("reading the derived rows")
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
    let levels: BTreeSet<u8> = glossary
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
    // A frontmatter string value written as a wikilink, alias and anchor
    // included: what opts a plain property into the link graph.
    assert!(
        glossary
            .links
            .iter()
            .any(|link| link.family == LinkFamily::Wikilink
                && link.title.as_deref() == Some("see here")
                && link.anchor.as_deref() == Some("Setext")),
        "no frontmatter wikilink carrying an alias and an anchor is exercised"
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
        glossary.tags.iter().any(|tag| tag.name.contains('\u{301}')),
        "no tag whose name carries a combining mark is exercised"
    );
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

    let field_rows: Vec<&FieldRow> = projection
        .documents()
        .iter()
        .flat_map(|document| document.fields.rows())
        .collect();
    let rows_under = |key: &str| -> Vec<&FieldRow> {
        field_rows
            .iter()
            .copied()
            .filter(|row| row.key() == key)
            .collect()
    };
    let containers: BTreeSet<&str> = field_rows
        .iter()
        .filter_map(|row| match row {
            FieldRow::Presence { container, .. } => Some(container.as_str()),
            FieldRow::Value { .. } => None,
        })
        .collect();
    assert_eq!(
        containers,
        FieldContainer::ALL
            .iter()
            .map(|container| container.as_str())
            .collect(),
        "a field container is not exercised"
    );
    let values: Vec<(&Option<String>, &Option<String>, bool, bool)> = field_rows
        .iter()
        .filter_map(|row| match row {
            FieldRow::Value {
                raw,
                typed,
                least_raw,
                least_typed,
                ..
            } => Some((raw, typed, *least_raw, *least_typed)),
            FieldRow::Presence { .. } => None,
        })
        .collect();
    assert!(
        values.iter().any(|(_, typed, _, _)| typed.is_some()),
        "no typed value is exercised"
    );
    assert!(
        rows_under("rating").iter().any(|row| matches!(
            row,
            FieldRow::Value {
                raw: Some(_),
                typed: None,
                ..
            }
        )),
        "no value that does not read as its declared type is exercised"
    );
    assert!(
        values.iter().any(|(_, _, least_raw, _)| *least_raw),
        "no least-raw marker is exercised"
    );
    assert!(
        values.iter().any(|(_, _, _, least_typed)| *least_typed),
        "no least-typed marker is exercised"
    );
    // A typed key a sign or an offset decides: a negative integer, a negative
    // float, an instant stated at an offset from UTC, one stated as `Z`, and
    // one carrying fractional seconds the clock reading drops.
    for (key, raw) in [
        ("rating", "-3"),
        ("weight", "-0.5"),
        ("created", "2026-09-24T10:00:00+02:00"),
        ("created", "2026-09-24T10:00:00Z"),
        ("created", "2026-09-24T10:00:00.250+02:00"),
    ] {
        assert!(
            rows_under(key).iter().any(|row| matches!(
                row,
                FieldRow::Value { raw: Some(held), typed: Some(_), .. } if held == raw
            )),
            "no typed `{key}: {raw}` is exercised"
        );
    }

    let markup = document("Markup.md");
    for written in [
        "<!-- #commented -->",
        "<!-- #inline-commented -->",
        "<https://example.com/auto>",
        "[shown][notes-ref]",
        "[notes-ref]: Notes.md",
        "\n#\n",
        "| 1 | 2 |\n\n^table-block",
    ] {
        assert!(
            markup.body.contains(written),
            "the markup document does not carry `{written}`"
        );
    }
    assert!(
        markup
            .headings
            .iter()
            .any(|heading| heading.text.is_empty()),
        "no empty heading is exercised"
    );
    assert!(
        markup
            .headings
            .iter()
            .any(|heading| heading.text.starts_with("Multi line")
                && heading.text.ends_with("setext title")),
        "no setext heading over two lines is exercised"
    );
    let lone_cr = document("cr-only.md");
    assert!(
        !lone_cr.body.contains('\n') && lone_cr.body.contains("```\r#fenced\r```"),
        "no lone-CR document with a fenced tag marker is exercised"
    );
    // The walk derives a document under a hidden directory like any other,
    // and the digest pins that it does.
    document(".hidden/Hidden Note.md");

    let kinds: BTreeSet<&str> = projection
        .findings()
        .iter()
        .map(|finding| finding.kind.as_str())
        .collect();
    // `path/bytes-not-utf8` is the one kind no corpus here can carry: APFS,
    // where these lanes run on Darwin, refuses a name that is not UTF-8, so
    // the file cannot be written. A corpus that wrote it on Linux alone would
    // derive to a different digest on each host.
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

/// **The other stance, and what it derives.** Tags the facet does not declare,
/// written in the body and in the frontmatter, under a schema that allows them.
fn assert_the_allowing_vault_exercises_its_stance(rows: &DerivedRows) {
    let projection = rows.projection();
    let schema = projection
        .vault_schema()
        .expect("the allowing vault pinned its schema");
    assert!(
        String::from_utf8_lossy(&schema.bytes).contains("undeclared: allow"),
        "the allowing vault's schema does not allow an undeclared tag"
    );
    let allowed = projection
        .document("Allowed.md")
        .expect("the allowing vault derived no row at `Allowed.md`");
    for (name, source) in [
        ("free-body", TagSource::Body),
        ("free-form", TagSource::Frontmatter),
    ] {
        assert!(
            allowed
                .tags
                .iter()
                .any(|tag| tag.name == name && tag.source == source),
            "no undeclared {source:?} tag `{name}` is exercised"
        );
    }
}
