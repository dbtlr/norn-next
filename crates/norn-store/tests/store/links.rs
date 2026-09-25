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
    ContentModel, DocumentFacts, FindStatement, LinkFact, LinkFamily, ReadFilter, Snapshot,
    SnapshotReader, Store, StoredPathOrder, SuffixKey, Validation, induced_failure,
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

/// The statements a link's resolution runs, which the find census holds this
/// suite's resolution bar to.
pub(crate) const LINK_READS: [FindStatement; 5] = [
    FindStatement::ClassHead,
    FindStatement::ClassTotal,
    FindStatement::CandidateSuffix,
    FindStatement::PathHead,
    FindStatement::PathTotal,
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
            .snapshot
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
        assert_eq!(
            reading(&links[11]),
            names(LinkHealth::Healthy, &["src/a.md"])
        );
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
/// where it folds ASCII case, each names `notes/glossary.md`.
#[test]
fn a_folding_root_resolves_every_case_spelling_of_either_family() {
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

/// Judge a read of the documents at one path: a search of `documents` through
/// `index` at the path, handed back in path order, and nothing read end to
/// end.
fn judge_path(plan: &QueryPlan, index: &str) {
    plan.assert_no_full_scan();
    plan.assert_no_temp_btree();
    let rows = rows_of(plan, "dr");
    rows.assert_searches_through("documents", Access::Index(index));
    rows.assert_search_constraint("documents", "(path=?)");
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

/// **A link's resolution reads the class or the path its target names**: a
/// wikilink's head, its total and each candidate's suffix probe are each a
/// seek of `documents_suffix_key` where the root tells spellings apart and of
/// `documents_folded_suffix_key` where it folds ASCII case, bounded on both
/// sides by the target's range; a Markdown link's head and total are a seek of
/// `documents_path` at the path where the root tells spellings apart and of
/// `documents_path_nocase` where it folds, the head handed back in path order.
/// A get's target and a links-to part's are read by the same class
/// statements.
///
/// Controls: a head read end to end fails; each index dropped, the head reads
/// something else, and fails.
#[test]
fn a_link_resolution_reads_the_class_or_the_path_its_target_names() {
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
        let mut linked = crowded(&format!("links-resolution-plan-{order:?}"), order);
        let params = request()
            .with_predicates([Predicate::path("src/many.md")])
            .with_columns([Column::links()]);
        let plans = linked.plans(&params);
        for statement in [
            FindStatement::ClassHead,
            FindStatement::ClassTotal,
            FindStatement::CandidateSuffix,
        ] {
            for plan in plans_of(&plans, statement) {
                judge_class(&plan, class_index, key);
            }
        }
        if order == Folding {
            for statement in [FindStatement::PathHead, FindStatement::PathTotal] {
                for plan in plans_of(&plans, statement) {
                    judge_path(&plan, path_index);
                }
            }
        } else {
            judge_path(
                &plans_of(&plans, FindStatement::PathHead).remove(0),
                path_index,
            );
        }
        let head = plans_of(&plans, FindStatement::ClassHead).remove(0);
        let walked = rewritten(&head, |detail| {
            if detail.starts_with("SEARCH dr ") {
                "SCAN dr".to_string()
            } else {
                detail.to_string()
            }
        });
        failure_of("a head read end to end", || {
            judge_class(&walked, class_index, key)
        });
        let path = plans_of(&plans, FindStatement::PathHead).remove(0);
        let walked = rewritten(&path, |detail| {
            if detail.starts_with("SEARCH dr ") {
                "SCAN dr".to_string()
            } else {
                detail.to_string()
            }
        });
        failure_of("a path head read end to end", || {
            judge_path(&walked, path_index)
        });

        // The same class statements answer a links-to part's target.
        let linking = linked
            .plans(&request().with_predicates([Predicate::links_to(resolution("d1/glossary"))]));
        for plan in plans_of(&linking, FindStatement::ClassHead) {
            judge_class(&plan, class_index, key);
        }

        linked.drop_index(class_index);
        linked.drop_index(path_index);
        let unindexed = linked.plans(&params);
        failure_of(&format!("{class_index} dropped"), || {
            judge_class(
                &plans_of(&unindexed, FindStatement::ClassHead)[0],
                class_index,
                key,
            )
        });
        failure_of(&format!("{path_index} dropped"), || {
            judge_path(
                &plans_of(&unindexed, FindStatement::PathHead)[0],
                path_index,
            )
        });
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

/// **A links column's work follows the page's links, not the vault**: the
/// same row over the fixture and over it beside 400 more documents costs the
/// same, and a row holding more links costs more.
#[test]
fn a_links_columns_work_follows_the_pages_links_not_the_vault() {
    let small = Linked::new("links-column-work-small", Sensitive, 40);
    let large = Linked::new("links-column-work-large", Sensitive, 400);
    let work = |linked: &Linked, at: &str| {
        let params = request()
            .with_predicates([Predicate::path(at)])
            .with_columns([Column::links()]);
        let plans = linked.plans(&params);
        let found = linked.find(&params);
        (
            plans.len(),
            found.work.statements,
            found.work.nested_rows.links,
        )
    };
    for at in ["src/a.md", "src/b.md", "src/c.md"] {
        assert_eq!(work(&small, at), work(&large, at), "{at}");
    }
    assert!(work(&small, "src/a.md").1 > work(&small, "src/b.md").1);
    assert_eq!(work(&small, "src/a.md").2, source_links().len() as u64);
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
                .any(|plan| plan.statement == FindStatement::PathHead)
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
