//! Links: what each link a document holds names at the read, the documents
//! holding a link to one document, and the plan and work bars those reads are
//! judged by.
//!
//! A link row carries the documents its target resolves to now and the health
//! that gives it, on a find's links column and a get's links page alike. A
//! wikilink's target is a suffix address read through the one resolver; a
//! Markdown link's is a path joined to its document's directory; a link is
//! judged by resolving it first, and one addressed elsewhere is not judged. A
//! `links_to` part matches the documents holding a link that resolves to
//! exactly the one document its own target names.

use std::sync::Arc;

use norn_store::{
    CANDIDATE_HEAD, ContentModel, DocumentFacts, FindStatement, LinkFact, LinkFamily, ReadFilter,
    Snapshot, SnapshotReader, Store, StoredPathOrder, SuffixKey, Validation, induced_failure,
};
use norn_testkit::explain::{Access, PlanRow, QueryPlan};
use norn_wire::{
    CollectionPage, CollectionSelector, Column, CountParams, Cursor, FindParams, GetParams,
    GetReport, LinkHealth, LinkRow, Pattern, Predicate, ResolutionTarget, Unsatisfied,
    ValidateParams, VaultAddress, VaultName,
};

use crate::common::{
    DOCUMENT_PAYLOAD, Scratch, document, reads_of, span, violation, write_documents,
};
use crate::find::{failure_of, rows_of};

use StoredPathOrder::{AsciiCaseInsensitive as Folding, Sensitive};

// ---- fixtures ----

/// The fingerprint the suite's schema is pinned under.
const SCHEMA: &str = "links-schema";

/// The statements a read of what targets name runs, which the find census
/// holds this suite's resolution bar to.
pub(crate) const LINK_READS: [FindStatement; 4] = [
    FindStatement::ClassHead,
    FindStatement::ClassTotal,
    FindStatement::CandidateSuffixes,
    FindStatement::LinkTargets,
];

/// The declaration, keeping nothing out of any class.
fn bare() -> ContentModel {
    ContentModel::under(SCHEMA)
}

/// The declaration the fixture is read under: `archive/**` kept out of every
/// class a target that does not name it opens.
fn declared() -> ContentModel {
    bare().declare_ambiguity_ignore(Pattern::parse("archive/**").expect("a glob"))
}

fn link(
    family: LinkFamily,
    protocol: Option<&str>,
    target: &str,
    anchor: Option<&str>,
) -> LinkFact {
    LinkFact {
        family,
        embed: false,
        protocol: protocol.map(str::to_string),
        target: target.to_string(),
        title: None,
        anchor: anchor.map(str::to_string),
        block_ref: None,
        span: span(1, 1, 0),
    }
}

fn wikilink(target: &str) -> LinkFact {
    link(LinkFamily::Wikilink, None, target, None)
}

fn markdown(target: &str) -> LinkFact {
    link(LinkFamily::Markdown, None, target, None)
}

/// A document at `at` holding `links`.
fn holding(at: &str, links: Vec<LinkFact>) -> DocumentFacts {
    let mut facts = document(at, &format!("hash-{at}"), "a body\n");
    facts.links = links;
    facts
}

/// The links `src/a.md` holds, in order, beside what each names on a root
/// that tells spellings apart, under [`declared`].
fn source_links() -> Vec<LinkFact> {
    vec![
        // 0: one document, `archive/glossary.md` being an ignored place.
        wikilink("glossary"),
        // 1: two documents.
        wikilink("dup"),
        // 2: none.
        wikilink("missing"),
        // 3: one document, the anchor split off.
        link(LinkFamily::Wikilink, None, "glossary", Some("Heading")),
        // 4: one document.
        wikilink("notes/dup"),
        // 5: the path it joins to.
        markdown("../x/y.md"),
        // 6: percent-decoded.
        markdown("../x/my%20note.md"),
        // 7: above the vault root.
        markdown("../../escape.md"),
        // 8: never reduced.
        markdown("../x/y"),
        // 9: a protocol.
        link(LinkFamily::Markdown, Some("https"), "example.com", None),
        // 10: an attachment.
        wikilink("picture.png"),
        // 11: its own document.
        markdown("./a.md"),
        // 12: both reductions of a dotted leaf.
        wikilink("report.md"),
        // 13: a same-document anchor, which names its own document.
        link(LinkFamily::Wikilink, None, "", Some("Heading")),
        // 14: the same, in the Markdown family.
        link(LinkFamily::Markdown, None, "", Some("frag")),
    ]
}

/// The fixture's documents.
fn documents() -> Vec<DocumentFacts> {
    vec![
        holding("notes/glossary.md", Vec::new()),
        holding("archive/glossary.md", Vec::new()),
        holding("notes/dup.md", Vec::new()),
        holding("other/dup.md", Vec::new()),
        holding("x/y.md", Vec::new()),
        holding("x/my note.md", Vec::new()),
        holding("r/report.md", Vec::new()),
        holding("r/report.md.md", Vec::new()),
        holding("src/a.md", source_links()),
        holding(
            "src/b.md",
            vec![wikilink("glossary"), wikilink("notes/dup")],
        ),
        holding("src/c.md", vec![markdown("../notes/glossary.md")]),
        holding(
            "src/d.md",
            vec![wikilink("GLOSSARY"), markdown("../NOTES/Glossary.md")],
        ),
    ]
}

/// A store over a root proven to have one case behaviour, its schema pinned,
/// holding `documents`, and a read handle over it.
struct Linked {
    _scratch: Scratch,
    store: Store,
    reader: Arc<SnapshotReader>,
}

impl Linked {
    fn holding(label: &str, order: StoredPathOrder, documents: &[DocumentFacts]) -> Self {
        let scratch = Scratch::new(label);
        let mut store = Store::open(scratch.database(), order, crate::common::DERIVATION)
            .expect("opening a store");
        store
            .begin_request()
            .pin_vault_schema(SCHEMA.as_bytes(), SCHEMA)
            .expect("pinning the suite's schema");
        write_documents(&mut store.begin_request(), documents);
        let reader = Arc::new(store.open_reader().reader.expect("a reader"));
        Linked {
            _scratch: scratch,
            store,
            reader,
        }
    }

    /// The fixture, and `bulk` more documents under `bulk/` that link
    /// nowhere the fixture's documents are.
    fn new(label: &str, order: StoredPathOrder, bulk: usize) -> Self {
        let mut all = documents();
        all.extend((0..bulk).map(|at| {
            holding(
                &format!("bulk/{at:04}.md"),
                vec![wikilink(&format!("bulk/{:04}", (at + 1) % bulk.max(1)))],
            )
        }));
        Self::holding(label, order, &all)
    }

    fn snapshot(&self) -> Snapshot {
        self.reader
            .try_take()
            .expect("a handle nothing is reading holds its connection")
            .establish()
            .expect("a snapshot")
    }

    fn find_under(&self, params: &FindParams, declared: &ContentModel) -> norn_store::Found {
        self.snapshot()
            .find(params, declared)
            .unwrap_or_else(|refusal| panic!("a find of {params:?}: {refusal}"))
    }

    fn find(&self, params: &FindParams) -> norn_store::Found {
        self.find_under(params, &declared())
    }

    fn plans(&self, params: &FindParams) -> Vec<norn_store::FindPlan> {
        self.snapshot()
            .find_plans(params, &declared())
            .unwrap_or_else(|refusal| panic!("the plans of {params:?}: {refusal}"))
    }

    /// The links the document at `at` holds, as a find's links column carries
    /// them, under `declared`.
    fn links_under(&self, at: &str, declared: &ContentModel) -> Vec<LinkRow> {
        let found = self.find_under(
            &request()
                .with_predicates([Predicate::path(at)])
                .with_columns([Column::links()]),
            declared,
        );
        let [row] = &found.rows[..] else {
            panic!("`{at}` is not one row: {:?}", found.rows);
        };
        let links = row.links.clone().expect("the links column");
        assert_eq!(links.total, links.items.len() as u64);
        links.items
    }

    fn links(&self, at: &str) -> Vec<LinkRow> {
        self.links_under(at, &declared())
    }

    /// The paths of the documents holding a link to `target`, and the parts
    /// the find reported.
    fn linking_to(&self, target: &str) -> (Vec<String>, Vec<Unsatisfied>) {
        let found =
            self.find(&request().with_predicates([Predicate::links_to(resolution(target))]));
        (paths(&found), found.unsatisfied)
    }

    fn backlinks(&self, target: &str) -> Vec<String> {
        let (paths, unsatisfied) = self.linking_to(target);
        assert_eq!(unsatisfied, Vec::new(), "`{target}`");
        paths
    }

    fn drop_index(&mut self, index: &str) {
        induced_failure::execute_out_of_band(&mut self.store, &format!("DROP INDEX {index}"))
            .unwrap_or_else(|problem| panic!("dropping {index}: {problem}"));
    }
}

fn address() -> VaultAddress {
    VaultAddress::name(VaultName::new("notes").expect("a vault name"))
}

fn request() -> FindParams {
    FindParams::new(address())
}

fn resolution(text: &str) -> ResolutionTarget {
    ResolutionTarget::new(text).expect("a target")
}

fn paths(found: &norn_store::Found) -> Vec<String> {
    found
        .rows
        .iter()
        .map(|row| row.path.as_str().to_string())
        .collect()
}

/// A link row's health and the paths its head names.
fn reading(row: &LinkRow) -> (LinkHealth, Vec<String>, u64) {
    (
        row.health(),
        row.targets
            .candidates()
            .iter()
            .map(|candidate| candidate.path.as_str().to_string())
            .collect(),
        row.targets.total(),
    )
}

fn names(health: LinkHealth, paths: &[&str]) -> (LinkHealth, Vec<String>, u64) {
    (
        health,
        paths.iter().map(|path| path.to_string()).collect(),
        paths.len() as u64,
    )
}

// ---- what a link names ----

/// **A wikilink is healthy, broken or ambiguous by what its target names**,
/// read through the one resolver on either root: one document is healthy,
/// none is broken, two are ambiguous and the head names both, a dotted leaf
/// reduces both ways, and an anchor is split off before resolution and carried
/// as written, never checked. The head names each document by its minimal
/// disambiguating suffix.
#[test]
fn a_wikilink_is_healthy_broken_or_ambiguous_by_what_its_target_names() {
    for order in [Sensitive, Folding] {
        let linked = Linked::new(&format!("links-wikilinks-{order:?}"), order, 0);
        let links = linked.links("src/a.md");
        assert_eq!(links.len(), source_links().len());
        assert_eq!(
            reading(&links[0]),
            names(LinkHealth::Healthy, &["notes/glossary.md"]),
            "{order:?}"
        );
        assert_eq!(
            reading(&links[1]),
            names(LinkHealth::Ambiguous, &["notes/dup.md", "other/dup.md"]),
            "{order:?}"
        );
        assert_eq!(
            links[1]
                .targets
                .candidates()
                .iter()
                .map(|candidate| candidate.suffix.as_str())
                .collect::<Vec<_>>(),
            ["notes/dup", "other/dup"]
        );
        assert_eq!(
            reading(&links[2]),
            names(LinkHealth::Broken, &[]),
            "{order:?}"
        );
        assert_eq!(
            reading(&links[3]),
            names(LinkHealth::Healthy, &["notes/glossary.md"]),
            "{order:?}"
        );
        assert_eq!(links[3].anchor, Some(norn_wire::Anchor::heading("Heading")));
        assert_eq!(
            reading(&links[4]),
            names(LinkHealth::Healthy, &["notes/dup.md"]),
            "{order:?}"
        );
        assert_eq!(
            reading(&links[12]),
            names(LinkHealth::Ambiguous, &["r/report.md.md", "r/report.md"]),
            "{order:?}: `report.md` reduces to `report` and to itself, in the ladder's order"
        );
    }
}

/// **A Markdown link names the one path it joins to**, percent-decoded and
/// never reduced: `../x/y.md` names `x/y.md`, a percent-encoded space names the
/// document whose name holds the space, a path climbing above the vault root
/// names nothing, and `../x/y` names the path `x/y`, which no document stands
/// at, and never `x/y.md`. A link to its own document names it.
#[test]
fn a_markdown_link_names_the_one_path_it_joins_to() {
    for order in [Sensitive, Folding] {
        let linked = Linked::new(&format!("links-markdown-{order:?}"), order, 0);
        let links = linked.links("src/a.md");
        assert_eq!(reading(&links[5]), names(LinkHealth::Healthy, &["x/y.md"]));
        assert_eq!(
            reading(&links[6]),
            names(LinkHealth::Healthy, &["x/my note.md"])
        );
        assert_eq!(reading(&links[7]), names(LinkHealth::Broken, &[]));
        assert_eq!(reading(&links[8]), names(LinkHealth::Broken, &[]));
        for at in [11, 13, 14] {
            assert_eq!(
                reading(&links[at]),
                names(LinkHealth::Healthy, &["src/a.md"]),
                "{order:?} link {at}"
            );
        }
        let linked_c = linked.links("src/c.md");
        assert_eq!(
            reading(&linked_c[0]),
            names(LinkHealth::Healthy, &["notes/glossary.md"])
        );
    }
}

/// A vault whose `x/src.md` holds a link of every address shape, beside the
/// documents they may name.
fn addressed() -> Vec<DocumentFacts> {
    vec![
        holding("v1.2.md", Vec::new()),
        holding("diagram.png.md", Vec::new()),
        holding("notes/glossary.md", Vec::new()),
        holding("x/y.md", Vec::new()),
        holding("x/a/b.md", Vec::new()),
        holding("x/q.md", Vec::new()),
        holding(
            "x/src.md",
            vec![
                // 0: a dotted leaf reduced to the document it names.
                wikilink("v1.2"),
                // 1: an attachment no document is.
                wikilink("photo.png"),
                // 2: an attachment's name a document carries.
                wikilink("diagram.png"),
                // 3: a `vault://` wikilink, a path from the vault root.
                link(
                    LinkFamily::Wikilink,
                    Some("vault"),
                    "notes/glossary.md",
                    None,
                ),
                // 4: a `vault://` Markdown link.
                link(LinkFamily::Markdown, Some("vault"), "x/y.md", None),
                // 5: a URI scheme with no `//`.
                markdown("mailto:someone@example.com"),
                // 6: another.
                markdown("tel:+1-555-0100"),
                // 7: an encoded separator inside one segment.
                markdown("a%2Fb.md"),
                // 8: the separator itself.
                markdown("a/b.md"),
                // 9: a query, which is not part of the path.
                markdown("q.md?x=1"),
                // 10: a rooted path.
                markdown("/x/y.md"),
            ],
        ),
    ]
}

/// **A link is judged by resolving it first, and its address is read
/// protocol first and family second**, on either root: `[[v1.2]]` names
/// `v1.2.md`; `[[photo.png]]` naming no document is an attachment and not
/// judged, while `[[diagram.png]]` names `diagram.png.md` and is healthy; a
/// `vault://` link of either family is a path from the vault root; a Markdown
/// target opening with a URI scheme is not judged; a Markdown target is split
/// into segments before it is decoded, so `%2F` is a character inside a
/// segment and names no document; and its query is not part of its path.
#[test]
fn a_link_is_judged_by_resolving_it_first() {
    for order in [Sensitive, Folding] {
        let linked = Linked::holding(&format!("links-addressed-{order:?}"), order, &addressed());
        let links = linked.links("x/src.md");
        for (at, expected) in [
            (0, names(LinkHealth::Healthy, &["v1.2.md"])),
            (1, names(LinkHealth::NotJudged, &[])),
            (2, names(LinkHealth::Healthy, &["diagram.png.md"])),
            (3, names(LinkHealth::Healthy, &["notes/glossary.md"])),
            (4, names(LinkHealth::Healthy, &["x/y.md"])),
            (5, names(LinkHealth::NotJudged, &[])),
            (6, names(LinkHealth::NotJudged, &[])),
            (7, names(LinkHealth::Broken, &[])),
            (8, names(LinkHealth::Healthy, &["x/a/b.md"])),
            (9, names(LinkHealth::Healthy, &["x/q.md"])),
            (10, names(LinkHealth::Healthy, &["x/y.md"])),
        ] {
            assert_eq!(
                reading(&links[at]),
                expected,
                "{order:?} link {at}, `{}`",
                links[at].target
            );
        }
        for target in ["v1.2", "diagram.png", "notes/glossary", "x/a/b", "x/q"] {
            assert_eq!(
                linked.backlinks(target),
                ["x/src.md"],
                "{order:?} `{target}`"
            );
        }
        assert_eq!(linked.backlinks("x/y"), ["x/src.md"], "{order:?}");
    }
}

/// A `vault://` link of `family` to `target`.
fn vault(family: LinkFamily, target: &str) -> LinkFact {
    link(family, Some("vault"), target, None)
}

/// A vault whose `h/` documents each hold one `vault://` link, beside the
/// documents they may name: `sub/Deep.md` a suffix of no root path, and
/// `my note.md` the path a percent-encoded space decodes to.
fn rooted() -> Vec<DocumentFacts> {
    use LinkFamily::{Markdown, Wikilink};
    let mut all = vec![
        holding("Notes.md", Vec::new()),
        holding("v1.md", Vec::new()),
        holding("sub/Deep.md", Vec::new()),
        holding("my note.md", Vec::new()),
    ];
    all.extend(
        [
            ("h/w-notes.md", Wikilink, "Notes"),
            ("h/w-notes-md.md", Wikilink, "Notes.md"),
            ("h/w-v12.md", Wikilink, "v1.2"),
            ("h/w-deep.md", Wikilink, "Deep"),
            ("h/w-encoded.md", Wikilink, "my%20note"),
            ("h/w-query.md", Wikilink, "Notes?x"),
            ("h/w-lower.md", Wikilink, "notes"),
            ("h/m-notes.md", Markdown, "Notes"),
            ("h/m-notes-md.md", Markdown, "Notes.md"),
            ("h/m-encoded.md", Markdown, "my%20note.md"),
            ("h/m-query.md", Markdown, "Notes.md?x=1"),
            ("h/m-lower.md", Markdown, "notes.md"),
        ]
        .into_iter()
        .map(|(at, family, target)| holding(at, vec![vault(family, target)])),
    );
    all
}

/// **A `vault://` link is rooted at the vault root, and its family keeps its
/// own rules**, on either root. A wikilink names the root path each of its
/// reductions spells: `[[vault://Notes]]` and `[[vault://Notes.md]]` name
/// `Notes.md`, and `[[vault://v1.2]]` names `v1.md` — its dotted leaf's
/// stem, stripped of its extension, exactly as a suffix wikilink's own
/// second reduction reaches it; it is never a suffix address, so
/// `[[vault://Deep]]` does not reach `sub/Deep.md`; and nothing in
/// it is decoded or cut off, so `[[vault://my%20note]]` and
/// `[[vault://Notes?x]]` name nothing. A Markdown link names the one path it
/// spells, percent-decoded and its query cut off and never reduced:
/// `[t](vault://Notes)` names the path `Notes`, where no document stands. A
/// path is matched under the root's order, so a lowercase spelling names
/// `Notes.md` only where the root folds case. A links-to part reaches each
/// link that names its document.
#[test]
fn a_vault_link_is_rooted_and_its_family_keeps_its_rules() {
    for order in [Sensitive, Folding] {
        let linked = Linked::holding(&format!("links-rooted-{order:?}"), order, &rooted());
        let folded = if order == Folding {
            names(LinkHealth::Healthy, &["Notes.md"])
        } else {
            names(LinkHealth::Broken, &[])
        };
        for (at, expected) in [
            ("h/w-notes.md", names(LinkHealth::Healthy, &["Notes.md"])),
            ("h/w-notes-md.md", names(LinkHealth::Healthy, &["Notes.md"])),
            ("h/w-v12.md", names(LinkHealth::Healthy, &["v1.md"])),
            ("h/w-deep.md", names(LinkHealth::Broken, &[])),
            ("h/w-encoded.md", names(LinkHealth::Broken, &[])),
            ("h/w-query.md", names(LinkHealth::Broken, &[])),
            ("h/w-lower.md", folded.clone()),
            ("h/m-notes.md", names(LinkHealth::Broken, &[])),
            ("h/m-notes-md.md", names(LinkHealth::Healthy, &["Notes.md"])),
            (
                "h/m-encoded.md",
                names(LinkHealth::Healthy, &["my note.md"]),
            ),
            ("h/m-query.md", names(LinkHealth::Healthy, &["Notes.md"])),
            ("h/m-lower.md", folded.clone()),
        ] {
            assert_eq!(reading(&linked.links(at)[0]), expected, "{order:?} `{at}`");
        }
        let mut notes = vec![
            "h/m-notes-md.md",
            "h/m-query.md",
            "h/w-notes-md.md",
            "h/w-notes.md",
        ];
        if order == Folding {
            notes.extend(["h/m-lower.md", "h/w-lower.md"]);
            notes.sort_unstable();
        }
        assert_eq!(linked.backlinks("Notes"), notes, "{order:?}");
        assert_eq!(linked.backlinks("v1"), ["h/w-v12.md"], "{order:?}");
        assert_eq!(linked.backlinks("my note"), ["h/m-encoded.md"], "{order:?}");
        assert_eq!(
            linked.backlinks("sub/Deep"),
            Vec::<String>::new(),
            "{order:?}"
        );
    }
}

/// **A `vault://` wikilink's dotted-leaf reduction reaches the same document
/// a suffix wikilink's does.** With only `v1.md` standing, `[[v1.2]]` and
/// `[[vault://v1.2]]` both strip `v1.2`'s leaf to its stem `v1` for their
/// second reduction, so both name `v1.md` and are healthy, on either root.
#[test]
fn a_vault_wikilinks_dotted_leaf_reduction_matches_a_suffix_wikilinks() {
    for order in [Sensitive, Folding] {
        let linked = Linked::holding(
            &format!("links-vault-dotted-leaf-{order:?}"),
            order,
            &[
                holding("v1.md", Vec::new()),
                holding("h/s.md", vec![wikilink("v1.2")]),
                holding("h/r.md", vec![vault(LinkFamily::Wikilink, "v1.2")]),
            ],
        );
        for at in ["h/s.md", "h/r.md"] {
            assert_eq!(
                reading(&linked.links(at)[0]),
                names(LinkHealth::Healthy, &["v1.md"]),
                "{order:?} `{at}`"
            );
        }
    }
}

/// **A rooted wikilink naming two documents is a backlink of neither**: with
/// `Notes.md` and `Notes.md.md` both standing, `[[vault://Notes.md]]` names
/// each by one of its reductions, so it is ambiguous, and a links-to part
/// naming either document alone confirms the link's other path and drops it.
/// The Markdown link beside it names `Notes.md` alone and is its backlink.
#[test]
fn a_rooted_wikilink_naming_two_documents_is_a_backlink_of_neither() {
    for order in [Sensitive, Folding] {
        let linked = Linked::holding(
            &format!("links-rooted-ambiguous-{order:?}"),
            order,
            &[
                holding("Notes.md", Vec::new()),
                holding("Notes.md.md", Vec::new()),
                holding("h/a.md", vec![vault(LinkFamily::Wikilink, "Notes.md")]),
                holding("h/b.md", vec![vault(LinkFamily::Markdown, "Notes.md")]),
            ],
        );
        assert_eq!(
            reading(&linked.links("h/a.md")[0]),
            names(LinkHealth::Ambiguous, &["Notes.md", "Notes.md.md"]),
            "{order:?}"
        );
        assert_eq!(linked.backlinks("Notes"), ["h/b.md"], "{order:?}");
        assert_eq!(
            linked.backlinks("Notes.md.md"),
            Vec::<String>::new(),
            "{order:?}"
        );
    }
}

/// **A `%` spells a byte only before two hexadecimal digits**: `%+1` is no
/// escape, so `[t](100%+1.md)` names the document whose name holds those
/// three characters as written.
#[test]
fn a_percent_not_before_two_hexadecimal_digits_is_itself() {
    let linked = Linked::holding(
        "links-percent-sign",
        Sensitive,
        &[
            holding("a/100%+1.md", Vec::new()),
            holding("a/src.md", vec![markdown("100%+1.md")]),
        ],
    );
    assert_eq!(
        reading(&linked.links("a/src.md")[0]),
        names(LinkHealth::Healthy, &["a/100%+1.md"])
    );
}

/// **A link addressed elsewhere, or naming an attachment no document is, is
/// not judged**: one written with a protocol and one naming an attachment
/// that resolves to no document each carry no document and the fourth
/// health, on either root.
#[test]
fn a_link_addressed_elsewhere_or_to_an_absent_attachment_is_not_judged() {
    for order in [Sensitive, Folding] {
        let linked = Linked::new(&format!("links-unjudged-{order:?}"), order, 0);
        let links = linked.links("src/a.md");
        for at in [9, 10] {
            assert_eq!(
                reading(&links[at]),
                names(LinkHealth::NotJudged, &[]),
                "{order:?} {:?}",
                links[at].target
            );
        }
    }
}

/// **The root's case behaviour is the resolution's**: where the root tells
/// spellings apart, `[[GLOSSARY]]` and `../NOTES/Glossary.md` name nothing;
/// where it folds ASCII case, each names `notes/glossary.md`, and a lowercase
/// path names the document standing at its uppercase spelling.
#[test]
fn a_folding_root_resolves_every_case_spelling_of_either_family() {
    for (order, expected) in [
        (Sensitive, names(LinkHealth::Broken, &[])),
        (Folding, names(LinkHealth::Healthy, &["Notes/Glossary.md"])),
    ] {
        let upper = Linked::holding(
            &format!("links-case-upper-{order:?}"),
            order,
            &[
                holding("Notes/Glossary.md", Vec::new()),
                holding("src/e.md", vec![markdown("../notes/glossary.md")]),
            ],
        );
        assert_eq!(reading(&upper.links("src/e.md")[0]), expected, "{order:?}");
    }
    let sensitive = Linked::new("links-case-sensitive", Sensitive, 0);
    for row in sensitive.links("src/d.md") {
        assert_eq!(reading(&row), names(LinkHealth::Broken, &[]), "{row:?}");
    }
    let folding = Linked::new("links-case-folding", Folding, 0);
    for row in folding.links("src/d.md") {
        assert_eq!(
            reading(&row),
            names(LinkHealth::Healthy, &["notes/glossary.md"]),
            "{row:?}"
        );
    }
}

/// **An ignored place stays out of a link's resolution unless the link names
/// it**: with `archive/**` ignored, `[[glossary]]` names `notes/glossary.md`
/// alone; with nothing ignored it names both and is ambiguous. A Markdown
/// link's exact path is not a class, and no glob narrows it.
#[test]
fn an_ignored_place_stays_out_of_a_links_resolution() {
    for order in [Sensitive, Folding] {
        let linked = Linked::new(&format!("links-ignored-{order:?}"), order, 0);
        assert_eq!(
            reading(&linked.links_under("src/b.md", &declared())[0]),
            names(LinkHealth::Healthy, &["notes/glossary.md"])
        );
        assert_eq!(
            reading(&linked.links_under("src/b.md", &bare())[0]),
            names(
                LinkHealth::Ambiguous,
                &["archive/glossary.md", "notes/glossary.md"]
            )
        );
        let archived = bare().declare_ambiguity_ignore(Pattern::parse("x/**").expect("a glob"));
        assert_eq!(
            reading(&linked.links_under("src/a.md", &archived)[5]),
            names(LinkHealth::Healthy, &["x/y.md"])
        );
    }
}

/// **A get pages a document's links resolved as a find's links column carries
/// them**, and a page drained at one, two and three links is the whole.
#[test]
fn a_get_pages_a_documents_links_as_a_row_carries_them() {
    let linked = Linked::new("links-get", Sensitive, 0);
    let whole = linked.links("src/a.md");
    for limit in 1..=3u32 {
        let mut drained: Vec<LinkRow> = Vec::new();
        let mut after: Option<Cursor> = None;
        loop {
            let mut params = GetParams::new(address(), resolution("src/a"))
                .with_collection(CollectionSelector::Links)
                .with_limit(limit);
            if let Some(cursor) = after.take() {
                params = params.with_after(cursor);
            }
            let gotten = linked
                .snapshot()
                .get(&params, &declared(), &NoText)
                .unwrap_or_else(|refusal| panic!("a links page: {refusal}"));
            let GetReport::Collection {
                page: CollectionPage::Links { page, .. },
                ..
            } = gotten.report
            else {
                panic!("a get answered {:?}, not a links page", gotten.report);
            };
            assert!(page.rows.len() <= limit as usize);
            drained.extend(page.rows);
            match page.next {
                Some(cursor) => after = Some(cursor),
                None => break,
            }
        }
        assert_eq!(drained, whole, "at {limit}");
    }
}

/// A get's document reader, which a links page never reads.
struct NoText;

impl norn_store::DocumentText for NoText {
    fn section(
        &self,
        _: &[norn_store::HeadingFact],
        _: &str,
        _: &str,
    ) -> Option<norn_store::SectionAt> {
        None
    }

    fn block(&self, _: &str, _: usize) -> std::ops::Range<usize> {
        0..0
    }
}

// ---- which documents link to one ----

/// **A links-to part matches the documents holding a link that resolves to
/// exactly the one document its target names**, over each family: a wikilink
/// and a Markdown link to it alike, an anchor on either side ignored, and a
/// document linking to itself among them. A link naming two documents is a
/// backlink of neither, and a broken link of none.
#[test]
fn links_to_matches_a_link_that_resolves_to_exactly_its_document() {
    for order in [Sensitive, Folding] {
        let linked = Linked::new(&format!("links-to-{order:?}"), order, 0);
        let mut glossary = vec!["src/a.md", "src/b.md", "src/c.md"];
        if order == Folding {
            glossary.push("src/d.md");
        }
        assert_eq!(linked.backlinks("glossary"), glossary, "{order:?}");
        assert_eq!(linked.backlinks("notes/glossary#Heading"), glossary);
        assert_eq!(linked.backlinks("notes/dup"), ["src/a.md", "src/b.md"]);
        assert_eq!(linked.backlinks("other/dup"), Vec::<String>::new());
        assert_eq!(linked.backlinks("x/y"), ["src/a.md"]);
        assert_eq!(linked.backlinks("my note"), ["src/a.md"]);
        assert_eq!(linked.backlinks("src/a"), ["src/a.md"]);
        assert_eq!(linked.backlinks("r/report"), Vec::<String>::new());
        assert_eq!(linked.backlinks("report.md.md"), Vec::<String>::new());
        assert_eq!(linked.backlinks("archive/glossary"), Vec::<String>::new());
    }
}

/// **A links-to part keeps a link only where the named document is in the
/// link's class and no other document is**, each bound of that class read as
/// a resolution reads it, on either root:
///
/// - with `archive/**` ignored, `[[glossary]]` does not reach
///   `archive/glossary.md` — the one document the vault holds — so it is no
///   backlink of it, while `[[archive/glossary]]` is;
/// - a class holds the keys its prefix opens and no key past them, so `foo0.md`
///   is outside `[[foo]]`'s class and the link is a backlink of `n/foo.md`;
/// - a class holds the key its prefix is, so `glossary.md` is inside
///   `[[glossary]]`'s class beside `notes/glossary.md`, and the link is a
///   backlink of neither.
#[test]
fn links_to_keeps_a_link_whose_class_holds_the_named_document_alone() {
    for order in [Sensitive, Folding] {
        let archived = Linked::holding(
            &format!("links-to-admitted-{order:?}"),
            order,
            &[
                holding("archive/glossary.md", Vec::new()),
                holding("src/a.md", vec![wikilink("glossary")]),
                holding("src/b.md", vec![wikilink("archive/glossary")]),
            ],
        );
        assert_eq!(
            archived.backlinks("archive/glossary"),
            ["src/b.md"],
            "{order:?}"
        );
        let beside = Linked::holding(
            &format!("links-to-upper-{order:?}"),
            order,
            &[
                holding("n/foo.md", Vec::new()),
                holding("foo0.md", Vec::new()),
                holding("src/a.md", vec![wikilink("foo")]),
            ],
        );
        assert_eq!(beside.backlinks("n/foo"), ["src/a.md"], "{order:?}");
        let ambiguous = Linked::holding(
            &format!("links-to-lower-{order:?}"),
            order,
            &[
                holding("glossary.md", Vec::new()),
                holding("notes/glossary.md", Vec::new()),
                holding("src/a.md", vec![wikilink("glossary")]),
            ],
        );
        assert_eq!(
            ambiguous.backlinks("notes/glossary"),
            Vec::<String>::new(),
            "{order:?}"
        );
    }
}

/// **An ignored place is kept out of a links-to part as it is out of a
/// link's resolution**: with nothing ignored, `[[glossary]]` is ambiguous, so
/// it is a backlink of neither document, and the part's own target
/// `glossary` names two documents and is reported.
#[test]
fn links_to_applies_the_ignored_places_a_resolution_applies() {
    let linked = Linked::new("links-to-ignored", Sensitive, 0);
    let found = linked.find_under(
        &request().with_predicates([Predicate::links_to(resolution("notes/glossary"))]),
        &bare(),
    );
    assert_eq!(paths(&found), ["src/c.md"]);
    let found = linked.find_under(
        &request().with_predicates([Predicate::links_to(resolution("archive/glossary"))]),
        &bare(),
    );
    assert_eq!(paths(&found), Vec::<String>::new());
    assert_eq!(found.unsatisfied, Vec::new());
}

/// **A links-to target naming several documents or none is reported in
/// band**, the head of an ambiguous one beside it, and the part matches no
/// document.
#[test]
fn a_links_to_target_naming_several_or_none_is_reported_in_band() {
    let linked = Linked::new("links-to-reported", Sensitive, 0);
    let (paths, unsatisfied) = linked.linking_to("dup");
    assert_eq!(paths, Vec::<String>::new());
    let [
        Unsatisfied::LinksToAmbiguous {
            target, candidates, ..
        },
    ] = unsatisfied.as_slice()
    else {
        panic!("an ambiguous target reported {unsatisfied:?}");
    };
    assert_eq!(target, &resolution("dup"));
    assert_eq!(candidates.total(), 2);
    assert_eq!(
        candidates
            .candidates()
            .iter()
            .map(|candidate| candidate.suffix.as_str())
            .collect::<Vec<_>>(),
        ["notes/dup", "other/dup"]
    );
    let (paths, unsatisfied) = linked.linking_to("missing");
    assert_eq!(paths, Vec::<String>::new());
    assert_eq!(
        unsatisfied,
        vec![Unsatisfied::links_to_unknown(resolution("missing"))]
    );
    let (_, unsatisfied) = linked.linking_to("../escape");
    assert_eq!(
        unsatisfied,
        vec![Unsatisfied::links_to_unknown(resolution("../escape"))]
    );
}

/// **Backlinks drained at one, two and three rows a page are the whole**, on
/// either root.
#[test]
fn backlinks_drained_at_one_two_and_three_are_the_whole() {
    for order in [Sensitive, Folding] {
        let linked = Linked::new(&format!("links-drain-{order:?}"), order, 0);
        let whole = linked.backlinks("glossary");
        assert!(whole.len() >= 3);
        for limit in 1..=3u32 {
            let mut drained: Vec<String> = Vec::new();
            let mut params = request()
                .with_predicates([Predicate::links_to(resolution("glossary"))])
                .with_limit(limit);
            loop {
                let found = linked.find(&params);
                assert!(found.rows.len() <= limit as usize);
                drained.extend(paths(&found));
                match found.next {
                    Some(cursor) => params = params.with_after(cursor),
                    None => break,
                }
            }
            assert_eq!(drained, whole, "{order:?} at {limit}");
        }
    }
}

/// **A count and a validate narrow by a links-to part as a find does**: the
/// count tallies the documents linking to the target, and the validate
/// answers the findings standing over them and no others.
#[test]
fn a_count_and_a_validate_narrow_by_links_to() {
    let mut linked = Linked::new("links-count-validate", Sensitive, 0);
    let part = Predicate::links_to(resolution("glossary"));
    let counted = linked
        .snapshot()
        .count(
            &CountParams::new(address()).with_predicates([part.clone()]),
            &declared(),
        )
        .expect("a count");
    assert_eq!(counted.tallies.len(), 1);
    assert_eq!(counted.tallies[0].count, 3);
    let reported = linked
        .snapshot()
        .count(
            &CountParams::new(address()).with_predicates([Predicate::links_to(resolution("dup"))]),
            &declared(),
        )
        .expect("a count");
    assert_eq!(reported.tallies[0].count, 0);
    assert!(matches!(
        reported.unsatisfied.as_slice(),
        [Unsatisfied::LinksToAmbiguous { .. }]
    ));

    for at in ["src/b.md", "x/y.md"] {
        linked
            .store
            .begin_request()
            .record_finding(&violation(at))
            .expect("recording a finding");
    }
    let validated = linked
        .snapshot()
        .validate(
            &ValidateParams::new(address()).with_predicates([part]),
            &declared(),
        )
        .expect("a validate");
    let Validation::Findings { rows, .. } = validated.answer else {
        panic!("a validate answered a summary");
    };
    assert_eq!(
        rows.iter().map(|row| row.path.as_str()).collect::<Vec<_>>(),
        ["src/b.md"]
    );
}

// ---- the plan bars ----

/// The plan the store reported for one statement, in the harness's shape.
fn plan(emitted: &norn_store::FindPlan) -> QueryPlan {
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

/// Every plan `plans` holds for `statement`.
fn plans_of(plans: &[norn_store::FindPlan], statement: FindStatement) -> Vec<QueryPlan> {
    let matching: Vec<QueryPlan> = plans
        .iter()
        .filter(|plan| plan.statement == statement)
        .map(plan)
        .collect();
    assert!(
        !matching.is_empty(),
        "the find runs no {statement:?}: {:?}",
        plans.iter().map(|plan| plan.statement).collect::<Vec<_>>()
    );
    matching
}

/// `plan` with every row's detail rewritten by `edit`.
fn rewritten(plan: &QueryPlan, edit: impl Fn(&str) -> String) -> QueryPlan {
    QueryPlan::new(
        plan.sql(),
        plan.rows()
            .iter()
            .map(|row| PlanRow::new(row.id, row.parent, edit(&row.detail)))
            .collect(),
    )
}

/// Judge a read of a class: a search of `documents` through `index`, the key
/// the root probes, bounded on both sides, and nothing read end to end.
fn judge_class(plan: &QueryPlan, index: &str, key: &str) {
    plan.assert_no_full_scan();
    let rows = rows_of(plan, "dr");
    rows.assert_searches_through("documents", Access::Index(index));
    rows.assert_search_constraint("documents", &format!("({key}>? AND {key}<?)"));
}

/// Judge a read of what a page's links name: each link's keys a search of
/// `link_keys_link` at the link, each suffix key's class a search of
/// `documents` through `class_index` bounded on both sides, each path key's
/// documents a search of `documents` through `path_index` at the path, and
/// nothing read end to end.
fn judge_link_targets(plan: &QueryPlan, class_index: &str, key: &str, path_index: &str) {
    plan.assert_no_full_scan();
    for alias in ["lk", "lr"] {
        let keys = rows_of(plan, alias);
        keys.assert_searches_through("link_keys", Access::Index("link_keys_link"));
        keys.assert_search_constraint("link_keys", "(link=?)");
    }
    let class = rows_of(plan, "dl");
    class.assert_searches_through("documents", Access::Index(class_index));
    class.assert_search_constraint("documents", &format!("({key}>? AND {key}<?)"));
    let at = rows_of(plan, "dp");
    at.assert_searches_through("documents", Access::Index(path_index));
    at.assert_search_constraint("documents", "(path=?)");
}

/// A vault whose `src/many.md` links to a target naming seven documents and
/// to a path at which seven case spellings stand, so every head a link's
/// resolution reads fills and is counted.
fn crowded(label: &str, order: StoredPathOrder) -> Linked {
    let mut all: Vec<DocumentFacts> = (1..=7)
        .map(|at| holding(&format!("d{at}/glossary.md"), Vec::new()))
        .collect();
    all.extend(
        [
            "p/Q.md", "p/q.md", "P/q.md", "P/Q.md", "p/Q.MD", "P/q.MD", "p/q.MD",
        ]
        .iter()
        .map(|at| holding(at, Vec::new())),
    );
    all.push(holding(
        "src/many.md",
        vec![wikilink("glossary"), markdown("../p/q.md")],
    ));
    Linked::holding(label, order, &all)
}

/// **A read of what targets name seeks the class or the path each names**: a
/// page's links are read as one set, each link's keys a seek of
/// `link_keys_link`, each suffix key's class a seek of `documents_suffix_key`
/// where the root tells spellings apart and of `documents_folded_suffix_key`
/// where it folds ASCII case, bounded on both sides by the key's range, and
/// each path key's documents a seek of `documents_path` at the path where the
/// root tells spellings apart and of `documents_path_nocase` where it folds;
/// the candidates' suffixes are each a range seek of the same suffix key. A
/// links-to part's target is read by the class statements, a head and a total
/// over the same ranges.
///
/// Controls: the read of what the links name reads a document end to end, and
/// fails; each index dropped, the read reads something else, and fails.
#[test]
fn a_read_of_targets_seeks_the_class_or_the_path_each_names() {
    for (order, class_index, key, path_index) in [
        (
            Sensitive,
            "documents_suffix_key",
            "suffix_key",
            "documents_path",
        ),
        (
            Folding,
            "documents_folded_suffix_key",
            "folded_suffix_key",
            "documents_path_nocase",
        ),
    ] {
        let linked = crowded(&format!("links-resolution-plan-{order:?}"), order);
        let params = request()
            .with_predicates([Predicate::path("src/many.md")])
            .with_columns([Column::links()]);
        let plans = linked.plans(&params);
        let [targets] = &plans_of(&plans, FindStatement::LinkTargets)[..] else {
            panic!("a page's links are not read by one statement: {plans:?}");
        };
        judge_link_targets(targets, class_index, key, path_index);
        let [suffixes] = &plans_of(&plans, FindStatement::CandidateSuffixes)[..] else {
            panic!("a page's candidates are not named by one statement: {plans:?}");
        };
        judge_class(suffixes, class_index, key);

        let walked = rewritten(targets, |detail| {
            if detail.starts_with("SEARCH dl ") {
                "SCAN dl".to_string()
            } else {
                detail.to_string()
            }
        });
        failure_of("a class read end to end", || {
            judge_link_targets(&walked, class_index, key, path_index)
        });

        // A links-to part's target is read by the class statements.
        let linking =
            linked.plans(&request().with_predicates([Predicate::links_to(resolution("glossary"))]));
        for statement in [FindStatement::ClassHead, FindStatement::ClassTotal] {
            for plan in plans_of(&linking, statement) {
                judge_class(&plan, class_index, key);
            }
        }
        for plan in plans_of(&linking, FindStatement::CandidateSuffixes) {
            judge_class(&plan, class_index, key);
        }

        // Each index is dropped on its own fresh fixture: on a store shared
        // across the three, the first drop alone leaves `link_keys_link`
        // missing for the rest, and the later controls would fail on that
        // residual break whether or not they judged their own row at all.
        for index in ["link_keys_link", class_index, path_index] {
            let mut probe = crowded(
                &format!("links-resolution-plan-{order:?}-without-{index}"),
                order,
            );
            probe.drop_index(index);
            let unindexed = probe.plans(&params);
            failure_of(&format!("{index} dropped"), || {
                judge_link_targets(
                    &plans_of(&unindexed, FindStatement::LinkTargets)[0],
                    class_index,
                    key,
                    path_index,
                )
            });
        }
    }
}

/// **A link's head is cut in the statement that names it.** On either root,
/// `[[glossary]]` names seven documents and carries a head of
/// [`CANDIDATE_HEAD`] of them beside a total of seven, and the candidate rows
/// the resolution reads are exactly the candidates the links carry, at most
/// [`CANDIDATE_HEAD`] per link, whether a find's links column or a get's links
/// page resolves them: no candidate is read past a head and dropped after.
#[test]
fn a_links_head_is_cut_in_the_statement_that_names_it() {
    for order in [Sensitive, Folding] {
        let linked = crowded(&format!("links-head-cut-{order:?}"), order);
        let found = linked.find(
            &request()
                .with_predicates([Predicate::path("src/many.md")])
                .with_columns([Column::links()]),
        );
        let [row] = &found.rows[..] else {
            panic!("`src/many.md` is not one row: {:?}", found.rows);
        };
        let links = &row.links.as_ref().expect("the links column").items;
        let glossary = links
            .iter()
            .find(|link| link.target == "glossary")
            .expect("the glossary link");
        assert_eq!(
            (
                glossary.targets.candidates().len(),
                glossary.targets.total()
            ),
            (CANDIDATE_HEAD, 7),
            "{order:?}"
        );
        let carried: u64 = links
            .iter()
            .map(|link| link.targets.candidates().len() as u64)
            .sum();
        let ceiling = (links.len() * CANDIDATE_HEAD) as u64;
        let gotten = linked
            .snapshot()
            .get(
                &GetParams::new(address(), resolution("src/many"))
                    .with_collection(CollectionSelector::Links)
                    .with_limit(10),
                &declared(),
                &NoText,
            )
            .unwrap_or_else(|refusal| panic!("a links page: {refusal}"));
        for (read, reader) in [
            (found.work.link_candidates_read, "a find's links column"),
            (gotten.work.link_candidates_read, "a get's links page"),
        ] {
            assert!(
                read <= ceiling,
                "{reader} under {order:?} read {read} candidate rows for {} links, past \
                 {CANDIDATE_HEAD} per link",
                links.len()
            );
            assert_eq!(
                read, carried,
                "{reader} under {order:?} reads the candidates its links carry and no others"
            );
        }
    }
}

/// Judge a links-to part's rows in a page's plan: the links that could name
/// the document are equality seeks of the link index at its keys, each link
/// reached has its own keys read off `link_keys_link`, and the documents each
/// of those reaches are a range seek of the suffix key the root probes or a
/// seek of the path; nothing in the plan is read end to end.
fn judge_links_to(
    page: &QueryPlan,
    key_index: &str,
    key: &str,
    suffix_index: &str,
    path_index: &str,
) {
    page.assert_no_full_scan();
    let seek = rows_of(page, "lk");
    seek.assert_searches_through("link_keys", Access::Index(key_index));
    seek.assert_search_constraint("link_keys", &format!("({key}=?)"));
    for alias in ["lo", "lp"] {
        let own = rows_of(page, alias);
        own.assert_searches_through("link_keys", Access::Index("link_keys_link"));
        own.assert_search_constraint("link_keys", "(link=?)");
    }
    let suffix_column = if key == "key" {
        "suffix_key"
    } else {
        "folded_suffix_key"
    };
    let reached = rows_of(page, "dl");
    reached.assert_searches_through("documents", Access::Index(suffix_index));
    reached.assert_search_constraint(
        "documents",
        &format!("({suffix_column}>? AND {suffix_column}<?)"),
    );
    let at = rows_of(page, "dp");
    at.assert_searches_through("documents", Access::Index(path_index));
    at.assert_search_constraint("documents", "(path=?)");
}

/// **A links-to part seeks the link index and confirms each link it reaches
/// by seeks**: the page is driven from equality seeks of `link_keys_key` —
/// `link_keys_folded_key` where the root folds ASCII case — at each key the
/// named document is named by, and each link reached is confirmed to name
/// that document alone by a seek of its own keys and of the documents they
/// reach. It never reads `links` or scans the link index.
///
/// Controls: each index the part seeks dropped, the part reads something else
/// and fails.
#[test]
fn a_links_to_part_seeks_the_link_index_and_confirms_each_link_by_seeks() {
    for (order, key_index, key, suffix_index, path_index) in [
        (
            Sensitive,
            "link_keys_key",
            "key",
            "documents_suffix_key",
            "documents_path",
        ),
        (
            Folding,
            "link_keys_folded_key",
            "folded_key",
            "documents_folded_suffix_key",
            "documents_path_nocase",
        ),
    ] {
        let mut linked = Linked::new(&format!("links-to-plan-{order:?}"), order, 0);
        let params = request().with_predicates([Predicate::links_to(resolution("glossary"))]);
        let page_of = |linked: &Linked| {
            let plans = linked.plans(&params);
            let page = plans
                .iter()
                .find(|plan| plan.filters == [ReadFilter::LinksTo(SuffixKey::under(order))])
                .expect("the page statement");
            plan(page)
        };
        let page = page_of(&linked);
        judge_links_to(&page, key_index, key, suffix_index, path_index);
        assert!(
            !page
                .rows()
                .iter()
                .any(|row| row.detail.contains(" links ") || row.detail.ends_with(" links")),
            "a links-to part read `links`: {:?}",
            page.rows()
        );
        for index in [key_index, "link_keys_link", suffix_index, path_index] {
            linked.drop_index(index);
            let unindexed = page_of(&linked);
            failure_of(&format!("{index} dropped"), || {
                judge_links_to(&unindexed, key_index, key, suffix_index, path_index)
            });
        }
    }
}

// ---- the work bars ----

/// **A links-to part's work follows the target's backlinks, not the vault**:
/// the same part over the fixture and over the fixture beside 400 more
/// documents that link among themselves costs the same statements, steps and
/// sorts; and a target with more backlinks costs more.
#[test]
fn a_links_to_parts_work_follows_its_backlinks_not_the_vault() {
    let small = Linked::new("links-to-work-small", Sensitive, 40);
    let large = Linked::new("links-to-work-large", Sensitive, 400);
    let work = |linked: &Linked, target: &str| {
        let found =
            linked.find(&request().with_predicates([Predicate::links_to(resolution(target))]));
        (
            found.work.statements,
            found.work.page_vm_steps,
            found.work.page_sorts,
            found.work.page_full_scan_steps,
        )
    };
    for target in ["glossary", "notes/dup", "x/y", "dup", "missing"] {
        assert_eq!(work(&small, target), work(&large, target), "{target}");
    }
    assert!(work(&small, "glossary").1 > work(&small, "x/y").1);
    assert_eq!(work(&small, "glossary").3, 0);
}

/// A vault of eight target documents `t/t0.md` to `t/t7.md`, a document
/// holding `few` links to them and one holding `many`, and eight `r/` rows each
/// holding one link of every shape: one document, two, none, a path, and one
/// addressed elsewhere.
fn linking(label: &str, few: usize, many: usize) -> Linked {
    let to = |count: usize| {
        (0..count)
            .map(|at| wikilink(&format!("t{}", at % 8)))
            .collect::<Vec<LinkFact>>()
    };
    let mut all: Vec<DocumentFacts> = (0..8)
        .map(|at| holding(&format!("t/t{at}.md"), Vec::new()))
        .collect();
    all.push(holding("dup/one/d.md", Vec::new()));
    all.push(holding("dup/two/d.md", Vec::new()));
    all.push(holding("h/few.md", to(few)));
    all.push(holding("h/many.md", to(many)));
    all.extend((0..8).map(|at| {
        holding(
            &format!("r/{at}.md"),
            vec![
                wikilink(&format!("t{at}")),
                wikilink("d"),
                wikilink("missing"),
                markdown(&format!("../t/t{at}.md")),
                link(LinkFamily::Markdown, Some("https"), "example.com", None),
            ],
        )
    }));
    Linked::holding(label, Sensitive, &all)
}

/// **A page's links cost a fixed number of statements, however many links
/// and rows it carries**: a row holding two links and one holding forty, a
/// page of one row and a page of eight, and a get's links page of one link and
/// of forty each run the same statements, while the link rows they read grow.
/// The fixture beside 40 and beside 400 more documents costs the same work.
#[test]
fn a_pages_links_cost_a_fixed_number_of_statements() {
    let linked = linking("links-column-statements", 2, 40);
    let column = |predicate: Predicate| {
        let params = request()
            .with_predicates([predicate])
            .with_columns([Column::links()]);
        let found = linked.find(&params);
        (found.work.statements, found.work.nested_rows.links)
    };
    let (few, few_rows) = column(Predicate::path("h/few.md"));
    let (many, many_rows) = column(Predicate::path("h/many.md"));
    assert_eq!((few_rows, many_rows), (2, 40));
    assert_eq!(few, many, "statements grew with the links a row holds");
    let (one, one_rows) = column(Predicate::path("r/0*"));
    let (eight, eight_rows) = column(Predicate::path("r/*"));
    assert_eq!((one_rows, eight_rows), (5, 40));
    assert_eq!(one, eight, "statements grew with the rows a page holds");

    let page = |limit: u32| {
        let gotten = linked
            .snapshot()
            .get(
                &GetParams::new(address(), resolution("h/many"))
                    .with_collection(CollectionSelector::Links)
                    .with_limit(limit),
                &declared(),
                &NoText,
            )
            .unwrap_or_else(|refusal| panic!("a links page: {refusal}"));
        let GetReport::Collection {
            page: CollectionPage::Links { page, .. },
            ..
        } = &gotten.report
        else {
            panic!("a get answered {:?}, not a links page", gotten.report);
        };
        (page.rows.len(), gotten.work.statements)
    };
    let (one_link, at_one) = page(1);
    let (forty_links, at_forty) = page(40);
    assert_eq!((one_link, forty_links), (1, 40));
    assert_eq!(at_one, at_forty, "a get's statements grew with its page");

    let small = Linked::new("links-column-work-small", Sensitive, 40);
    let large = Linked::new("links-column-work-large", Sensitive, 400);
    let work = |linked: &Linked, at: &str| {
        let params = request()
            .with_predicates([Predicate::path(at)])
            .with_columns([Column::links()]);
        let found = linked.find(&params);
        (found.work.statements, found.work.nested_rows.links)
    };
    for at in ["src/a.md", "src/b.md", "src/c.md"] {
        assert_eq!(work(&small, at), work(&large, at), "{at}");
    }
}

/// **No statement a link's resolution or a links-to part runs reads a
/// document's body or its frontmatter**: they read keys, paths and link rows.
#[test]
fn no_link_read_reads_a_documents_payload() {
    for order in [Sensitive, Folding] {
        let linked = Linked::new(&format!("links-payload-{order:?}"), order, 0);
        let plans = linked.plans(
            &request()
                .with_predicates([Predicate::links_to(resolution("glossary"))])
                .with_columns([Column::links()]),
        );
        assert!(
            plans
                .iter()
                .any(|plan| plan.statement == FindStatement::LinkTargets)
        );
        for emitted in &plans {
            reads_of(&emitted.plan).assert_reads_none_of(DOCUMENT_PAYLOAD);
        }
        // Control: the reader sees a payload column where one is read.
        let body = linked.plans(&request().with_columns([Column::body()]));
        let hydrate = body
            .iter()
            .find(|plan| plan.statement == FindStatement::HydrateDocuments)
            .expect("a hydration");
        failure_of("a body read", || {
            reads_of(&hydrate.plan).assert_reads_none_of(DOCUMENT_PAYLOAD)
        });
    }
}
