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
//! declaration — and every derived row each store holds is digested: the document rows with their sub-fingerprints, their raw and
//! folded suffix keys and their admitting counts, the links and the keys the link index holds them
//! under, the headings, blocks and tags, the field rows
//! with their typed halves and the offset spelling beside a typed date, every finding with its candidates, classes and path keys, the
//! rules it cites and the head of the value it names, the
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
//! conversion reaches it; no two of its names differ only by case, and every
//! link that raises a link-health finding is addressed in lower case, so a root
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
use norn_store::{
    DerivationVersion, FieldContainer, FieldRow, LinkAnchor, LinkFact, LinkFamily, OffsetSpelling,
    OpenOutcome, TagSource,
};
use norn_testkit::equivalence::{DerivedRows, assert_operationally_valid};
use norn_testkit::process::Sandbox;
use norn_wire::{FindingKind, LinkAddressKind};

/// The digest the corpus derives to, and the derivation version it was taken
/// under.
const PINNED: (DerivationVersion, &str) = (
    DerivationVersion::new(9),
    "3f00b6205dee2a475777fa74923e03c4ea7050b7d8d45b7004044a06570690d3",
);

/// The vault schema the main corpus is derived under: a field of every
/// declared type and of each declared shape, a tag facet that reports what it
/// does not declare, and schema rules that select the documents under
/// `rules/` by their `kind` — two selecting each document, so a finding cites
/// a set of rules, and one pair whose allowed paths share none.
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
  status:
    type: text
    shape: list
  kind:
    type: text
    shape: single
tags:
  declared: [project, area/norn, solo]
  undeclared: report
paths:
  ambiguity_ignore: [\"archive/**\"]
rules:
  task:
    description: A tracked task
    severity: error
    match:
      frontmatter: { kind: task }
    required: { owner: }
    forbidden: { scratch: remove }
    one_of:
      status: { values: [todo, doing, done] }
    max_length: { summary: 8 }
    allowed_paths:
      paths: [\"rules/tasks/**\"]
  filed:
    match:
      frontmatter: { kind: task }
      path: \"rules/**\"
    one_of:
      status: { values: [todo, done] }
    allowed_paths:
      paths: [\"rules/tasks/**\", \"rules/filed/**\"]
  chore:
    match:
      frontmatter: { kind: chore }
    forbidden: { owner: }
    allowed_paths:
      paths: [\"rules/chores/**\"]
  shelved:
    match:
      frontmatter: { kind: chore }
      path: \"rules/**\"
    required: { owner: }
    allowed_paths:
      paths: [\"rules/shelf/**\"]
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
            b"---\r\ntags: solo\r\ntitle: Deep\r\n---\r\n# Deep Note\r\n\r\nBody #deep and [up](../Notes.md).\r\n"
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
        // A link of each link-health kind: naming no document, naming the
        // two twins, and naming a document that holds neither the heading
        // nor the block its anchor names.
        ("health/Links.md", LINK_HEALTH.as_bytes().to_vec()),
        ("twins/one/twin.md", b"# One twin\n".to_vec()),
        ("twins/two/twin.md", b"# The other twin\n".to_vec()),
        // A third twin under the ignored `archive/**`, which the class
        // `[[twin]]` opens keeps out: a target reaches it only by naming
        // `archive/old/twin`, and its row stores that count.
        ("archive/old/twin.md", b"# The archived twin\n".to_vec()),
        ("health/anchored.md", b"# Anchored\n\nA held paragraph. ^held\n".to_vec()),
        // A task both `task` and `filed` select: a missing owner, a
        // forbidden list, a list holding two values outside their closed
        // sets, one of them twice, and a summary past its limit and past the
        // value head.
        ("rules/tasks/a.md", rule_task()),
        // A task standing where `filed` allows it and `task` does not, holding
        // a status `task` allows and `filed` does not.
        (
            "rules/filed/b.md",
            b"---\nkind: task\nowner: me\nstatus: [doing]\n---\n# Filed\n".to_vec(),
        ),
        // A chore `chore` forbids an owner to and `shelved` requires one of,
        // whose two rules allow no path in common.
        (
            "rules/chores/c.md",
            b"---\nkind: chore\nowner: me\n---\n# A chore\n".to_vec(),
        ),
        // A list under a single-shaped key, a scalar under a list-shaped one,
        // and a map under a text field.
        (
            "rules/shapes.md",
            b"---\nkind: [task, chore]\nstatus: todo\ntitle: { a: 1, b: [x] }\n---\n# Shapes\n"
                .to_vec(),
        ),
    ]
}

/// The task under `rules/tasks/`, whose summary runs past both its rule's
/// limit and the value head a finding keeps.
fn rule_task() -> Vec<u8> {
    format!(
        "---\nkind: task\nstatus: [todo, bogus, stalled, bogus]\nscratch: [left, over]\nsummary: {}\n---\n# A task\n",
        "a summary that runs on ".repeat(14)
    )
    .into_bytes()
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
/// embed, a percent-encoded Markdown target, one climbing out of the vault,
/// one naming an attachment, a rooted one, one opening with a URI scheme, one
/// carrying a query and one an encoded separator, a dotted wikilink, a
/// same-document anchor, a wikilink no suffix address reads, `vault://` links
/// of both families, a percent-encoded anchor, a heading chain, an ATX-shaped
/// anchor and empty anchors, body and frontmatter tags, declared and not, block ids, a
/// frontmatter wikilink carrying an alias and an anchor, a tag whose name
/// carries a combining mark, and tags written in another case than an earlier
/// spelling of them, one folding outside ASCII.
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
Tags in another case: #Project, #UNDECLARED-BODY and #Über.

See [[Notes]] and [[notes/Deep Note|shown title]] and [[Glossary#Repeated]] and [[Notes#^para-block]].
Embed ![[picture.png]] and ![[Notes#Setext|embedded]].
Markdown [shown](notes/Deep%20Note.md) and [anchor](Glossary.md#use-norn-bold \"a title\") and [web](https://example.com/page) and ![image](assets/pic.png) and [block](Notes.md#^para-block) and [vault](vault://notes).
A wikilink with a protocol: [[https://example.com/wiki|external]].
A Markdown link climbing out of the vault: [outside](../outside.md), and one to an attachment: [the picture](assets/pic.png).
A dotted wikilink [[v1.2]], a same-document anchor [[#Repeated]], a wikilink no suffix address reads [[../relative]] and vault wikilinks [[vault://notes/Deep Note.md]] and [[vault://v1.2]].
A rooted [rooted](/Notes.md), a scheme [mail](mailto:hi@example.com), a query [query](Notes.md?view=raw) and an encoded separator [slash](notes%2FDeep%20Note.md?x=1).
Anchors read three ways: an encoded fragment [encoded](Glossary.md#Sub%20Setext), a heading chain [[Glossary#Glossary#Repeated]], an ATX-shaped anchor [[Notes### Setext]], and empty anchors [[Notes#]] and [[Notes#^]].

A paragraph closing on a block id. ^glossary-block

```
# not a heading, #not-a-tag, [[not a link]]
```
^after-fence
";

/// A link the store judges broken, one it judges ambiguous, and two whose
/// anchors name a heading and a block `health/anchored.md` does not hold.
///
/// A link-health finding is keyed by the link's address as the root compares
/// it, so an address with an upper-case letter keys it one way on a root that
/// folds case and another on one that does not. Every address here, and every
/// document it names, is written in lower case so the rows are the same on both.
const LINK_HEALTH: &str = "# Link health

A broken [[nowhere at all]], an ambiguous [[twin]], a missing heading [[anchored#No Such Heading]] \
and a missing block [[anchored#^no-such-block]].
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

/// **A list value is judged element by element against the combined
/// constraint.** Two rules selecting one document close `status` over sets
/// whose intersection is `todo` and `done`; the document's list holds both,
/// two values outside them, and one of those twice. Derivation files one
/// `field/not-one-of` per distinct offending element — two, not three, and
/// not one per rule — each naming its element as its value, citing both
/// rules, at the higher of their severities, in the document's own
/// changeset beside its row.
#[test]
fn a_list_value_is_judged_element_by_element_against_the_combined_constraint() {
    let sandbox = Sandbox::new(Path::new(env!("CARGO_TARGET_TMPDIR")), "element-judgment")
        .expect("a sandbox");
    let rows = derive(
        &sandbox.work_dir().join("vault"),
        vec![(
            "tasks/a.md",
            b"---\ntype: task\nstatus: [todo, bogus, done, bogus, stalled]\n---\n# A task\n"
                .to_vec(),
        )],
        "\
version: 1
fields:
  status: { type: text, shape: list }
rules:
  wide: { one_of: { status: { values: [todo, doing, done] } } }
  task: { severity: error, match: { frontmatter: { type: task } }, one_of: { status: { values: [todo, done, open] } } }
",
    );
    let projection = rows.projection();
    assert!(
        projection.document("tasks/a.md").is_some(),
        "the judged document derived no row"
    );
    let findings: Vec<_> = projection
        .findings()
        .iter()
        .map(|finding| {
            (
                finding.kind.as_str(),
                finding.target.as_deref(),
                finding.value.as_ref().map(|(head, _, _)| head.as_str()),
                finding.rules.iter().map(String::as_str).collect(),
                finding.severity.as_str(),
            )
        })
        .collect();
    let not_one_of = FindingKind::NotOneOf.as_str();
    assert_eq!(
        findings,
        [
            (
                not_one_of,
                Some("status"),
                Some("bogus"),
                vec!["task", "wide"],
                "error"
            ),
            (
                not_one_of,
                Some("status"),
                Some("stalled"),
                vec!["task", "wide"],
                "error"
            ),
        ]
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
            .filter(|link| link.fact.family == family)
            .collect();
        assert!(
            links.iter().any(|link| link.fact.protocol.is_some()),
            "{family:?}: no protocol"
        );
        assert!(
            links.iter().any(|link| link.fact.anchor.is_some()),
            "{family:?}: no anchor"
        );
        assert!(
            links
                .iter()
                .any(|link| matches!(link.fact.anchor, Some(LinkAnchor::Block { .. }))),
            "{family:?}: no block reference"
        );
    }
    assert!(
        glossary.links.iter().any(|link| link.fact.title.is_some()),
        "no link title is exercised"
    );
    // The link index: the keys every read of what a link names seeks,
    // derived from each link and the path of the document holding it.
    let keys_of = |document: &norn_testkit::equivalence::ProjectedDocument,
                   protocol: Option<&str>,
                   target: &str| {
        let at = document
            .links
            .iter()
            .position(|link| {
                link.fact.protocol.as_deref() == protocol && link.fact.target == target
            })
            .unwrap_or_else(|| {
                panic!(
                    "`{}` holds no link to {protocol:?} `{target}`",
                    document.path
                )
            }) as u64;
        document
            .link_keys
            .iter()
            .filter(|key| key.link == at)
            .map(|key| key.key.clone())
            .collect::<Vec<String>>()
    };
    let deep = document("notes/Deep Note.md");
    for (holder, protocol, target, keys, shape) in [
        (
            glossary,
            None,
            "notes/Deep%20Note.md",
            &["notes/Deep Note.md"][..],
            "a percent-encoded Markdown link is keyed by the path it decodes to",
        ),
        (
            deep,
            None,
            "../Notes.md",
            &["Notes.md"],
            "a Markdown link climbing a directory is keyed by the path it joins to",
        ),
        (
            glossary,
            None,
            "notes/Deep Note",
            &["Deep Note/notes/"],
            "a wikilink is keyed by its suffix address",
        ),
        (
            glossary,
            None,
            "v1.2",
            &["v1.2/", "v1/"],
            "a dotted wikilink is keyed by both reductions",
        ),
        (
            glossary,
            None,
            "picture.png",
            &["picture.png/", "picture/"],
            "a wikilink naming an attachment is keyed by both reductions",
        ),
        (
            glossary,
            None,
            "assets/pic.png",
            &["assets/pic.png"],
            "a Markdown link naming an attachment is keyed by its path",
        ),
        (
            glossary,
            None,
            "",
            &["Glossary.md"],
            "a same-document anchor is keyed by its own document's path",
        ),
        (
            glossary,
            None,
            "/Notes.md",
            &["Notes.md"],
            "a rooted Markdown link is keyed by the path from the vault root",
        ),
        (
            glossary,
            Some("vault"),
            "notes",
            &["notes"],
            "a vault Markdown link is keyed by the one path from the vault root it spells, \
             never reduced",
        ),
        (
            glossary,
            Some("vault"),
            "notes/Deep Note.md",
            &["notes/Deep Note.md", "notes/Deep Note.md.md"],
            "a vault wikilink is keyed by the root path each of its reductions spells",
        ),
        (
            glossary,
            Some("vault"),
            "v1.2",
            &["v1.2.md", "v1.md"],
            "a vault wikilink whose leaf carries a non-Markdown extension is keyed by the stem \
             as written and the stem without its extension, each a Markdown path",
        ),
        (
            glossary,
            None,
            "Notes.md?view=raw",
            &["Notes.md"],
            "a Markdown link's query is not part of its key",
        ),
        (
            glossary,
            None,
            "notes%2FDeep%20Note.md?x=1",
            &[],
            "an encoded separator is a character inside its segment, which no path holds",
        ),
        (
            glossary,
            None,
            "../outside.md",
            &[],
            "a Markdown link climbing out of the vault names no path",
        ),
        (
            glossary,
            None,
            "../relative",
            &[],
            "a wikilink no suffix address reads is keyed by nothing",
        ),
        (
            glossary,
            None,
            "mailto:hi@example.com",
            &[],
            "a Markdown link opening with a URI scheme is addressed elsewhere",
        ),
        (
            glossary,
            Some("https"),
            "example.com/page",
            &[],
            "a link written with a protocol is addressed elsewhere",
        ),
    ] {
        assert_eq!(
            keys_of(holder, protocol, target),
            keys,
            "the corpus does not hold that {shape}"
        );
    }
    // A wikilink embed is the one embed the text layer records; the Markdown
    // image beside it is not a link, and the digest pins that it is not.
    assert!(
        glossary.links.iter().any(|link| link.fact.embed),
        "no embed is exercised"
    );
    // A frontmatter string value written as a wikilink, alias and anchor
    // included: what opts a plain property into the link graph.
    assert!(
        glossary
            .links
            .iter()
            .any(|link| link.fact.family == LinkFamily::Wikilink
                && link.fact.title.as_deref() == Some("see here")
                && written_anchor(&link.fact) == Some("Setext")),
        "no frontmatter wikilink carrying an alias and an anchor is exercised"
    );
    // The readings a heading and a heading anchor are matched by, and the
    // address kind a link is judged by.
    assert!(
        glossary
            .headings
            .iter()
            .any(|heading| heading.reading != heading.text),
        "no heading whose reading folds its text is exercised"
    );
    let readings = |anchor: &str| {
        glossary
            .links
            .iter()
            .find_map(|link| match &link.fact.anchor {
                Some(LinkAnchor::Heading { written, readings }) if written == anchor => {
                    Some(readings.clone())
                }
                _ => None,
            })
            .unwrap_or_else(|| panic!("the glossary holds no link anchored `{anchor}`"))
    };
    assert_eq!(
        readings("Sub Setext").text,
        "sub setext",
        "an encoded fragment is not recorded decoded"
    );
    for (anchor, marked) in [("Glossary#Repeated", "repeated"), ("## Setext", "setext")] {
        assert_eq!(
            readings(anchor).marked.as_deref(),
            Some(marked),
            "`{anchor}` is not read past its markers"
        );
    }
    assert_eq!(
        glossary
            .links
            .iter()
            .filter(|link| link.fact.family == LinkFamily::Wikilink
                && link.fact.target == "Notes"
                && link.fact.anchor.is_none())
            .count(),
        3,
        "`[[Notes]]`, `[[Notes#]]` and `[[Notes#^]]` are not all stored with no anchor"
    );
    let kinds: BTreeSet<&str> = glossary
        .links
        .iter()
        .map(|link| link.address.as_str())
        .collect();
    assert_eq!(
        kinds,
        [
            LinkAddressKind::Attachment,
            LinkAddressKind::Document,
            LinkAddressKind::Elsewhere
        ]
        .map(LinkAddressKind::as_str)
        .into(),
        "the glossary's links do not exercise every address kind"
    );
    assert!(
        glossary.blocks.len() >= 2,
        "the glossary's block ids derived {:?}",
        glossary.blocks
    );
    for source in [TagSource::Body, TagSource::Frontmatter] {
        assert!(
            glossary.tags.iter().any(|tag| tag.fact.source == source),
            "no {source:?} tag is exercised"
        );
    }
    assert!(
        glossary
            .tags
            .iter()
            .any(|tag| tag.fact.name.contains('\u{301}')),
        "no tag whose name carries a combining mark is exercised"
    );
    let spellings: Vec<(&str, &str)> = glossary
        .tags
        .iter()
        .map(|tag| (tag.fact.name.as_str(), tag.folded_name.as_str()))
        .collect();
    assert!(
        spellings.iter().any(|(name, folded)| name != folded
            && spellings
                .iter()
                .any(|(other, again)| other != name && again == folded)),
        "no tag written in two spellings of one fold is exercised: {spellings:?}"
    );
    assert!(
        spellings
            .iter()
            .any(|(name, folded)| name != folded && !name.is_ascii()),
        "no tag whose fold moves a letter outside ASCII is exercised: {spellings:?}"
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
    assert_eq!(
        rows.fields()
            .get("document[archive/old/twin.md].admitting_segments")
            .map(String::as_str),
        Some("3"),
        "no document the ambiguity-ignore set keeps out of a class is exercised"
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

    // A typed date records the spelling of its offset, and the corpus writes
    // both: a calendar day states none, and an instant states one.
    for spelling in [OffsetSpelling::Unstated, OffsetSpelling::Stated] {
        assert!(
            rows_under("created").iter().any(|row| matches!(
                row,
                FieldRow::Value { offset: Some(held), .. } if *held == spelling
            )),
            "no typed date spelled {spelling:?} is exercised"
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
        FindingKind::Broken,
        FindingKind::Ambiguous,
        FindingKind::MissingAnchor,
        FindingKind::Misplaced,
        FindingKind::DocumentRulesConflict,
        FindingKind::RequiredMissing,
        FindingKind::Forbidden,
        FindingKind::NotOneOf,
        FindingKind::TooLong,
        FindingKind::TypeMismatch,
        FindingKind::ShapeMismatch,
        FindingKind::FieldRulesConflict,
    ] {
        assert!(
            kinds.contains(kind.as_str()),
            "no `{kind}` finding is exercised; the corpus derived {kinds:?}"
        );
    }
    // A missing anchor of each kind a link carries: a heading, and a block.
    let missing: BTreeSet<&str> = projection
        .findings()
        .iter()
        .filter(|finding| {
            finding.path == "health/Links.md" && finding.kind == FindingKind::MissingAnchor.as_str()
        })
        .filter_map(|finding| finding.target.as_deref())
        .collect();
    assert_eq!(
        missing,
        BTreeSet::from(["anchored#No Such Heading", "anchored#^no-such-block"]),
        "a missing heading and a missing block are not both exercised"
    );
    // What a rule finding carries: a set of rules cited, a value head cut
    // short of its value, and a value spelled as canonical JSON — a list's
    // and a map's.
    let findings = projection.findings();
    assert!(
        findings.iter().any(|finding| finding.rules.len() > 1),
        "no finding citing a set of rules is exercised"
    );
    assert!(
        findings.iter().any(|finding| finding
            .value
            .as_ref()
            .is_some_and(|(head, bytes, _)| (head.len() as u64) < *bytes)),
        "no value past its head is exercised"
    );
    for spelled in [r#"["left","over"]"#, r#"{"a":1,"b":["x"]}"#] {
        assert!(
            findings.iter().any(|finding| finding
                .value
                .as_ref()
                .is_some_and(|(head, _, _)| head == spelled)),
            "no value spelled `{spelled}` is exercised"
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
                .any(|tag| tag.fact.name == name && tag.fact.source == source),
            "no undeclared {source:?} tag `{name}` is exercised"
        );
    }
}

/// The heading anchor `link` was written with, where it carries one.
fn written_anchor(link: &LinkFact) -> Option<&str> {
    match &link.anchor {
        Some(LinkAnchor::Heading { written, .. }) => Some(written),
        _ => None,
    }
}
