//! Link health: the findings the store judges about each link a set of
//! documents holds — broken, ambiguous, or missing the place its anchor names —
//! and the work and plan bars that judgment is held to.
//!
//! The documents here are derived from Markdown through the text layer, as the
//! host derives them, so a link, a heading and a block reach the store with the
//! readings a vault gives them.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use norn_store::{
    AnchorReadings, BlockFact, CANDIDATE_HEAD, Change, ClassKey, ContentModel, DocumentFacts,
    ExplainedStatement, FindingFacts, HeadingFact, IncrementProvenance, KeySummaries, LinkAnchor,
    LinkFact, LinkFamily, LinkSelection, PathKey, Request, Snapshot, SnapshotReader, Span, Store,
    StoredPathOrder, SuffixKey, Validation, induced_failure,
};
use norn_testkit::explain::{Access, PlanRow, QueryPlan};
use norn_wire::{
    Column, FindParams, FindingKind, FindingRow, Hint, LinkHealth, LinkRow, Pattern, Predicate,
    ResolutionTarget, Severity, ValidateParams, VaultAddress, VaultName,
};

use crate::common::{Scratch, path};
use crate::find::failure_of;

use StoredPathOrder::{AsciiCaseInsensitive as Folding, Sensitive};

// ---- fixtures ----

/// The fingerprint the suite's schema is pinned under.
const SCHEMA: &str = "health-schema";

/// The declaration every judgment and read here is taken under: `archive/**`
/// kept out of every class a target that does not name it opens.
fn declared() -> ContentModel {
    ContentModel::under(SCHEMA)
        .declare_ambiguity_ignore(Pattern::parse("archive/**").expect("a glob"))
}

/// The document at `at` whose body is `body`, with the links, headings and
/// blocks the text layer reads out of it, mapped onto the store's facts as the
/// host maps them.
pub(crate) fn derived(at: &str, body: &str) -> DocumentFacts {
    let scan = norn_text::BodyScan::new(body);
    let mut facts = DocumentFacts::new(path(at), format!("hash-{at}"), body, body.len() as u64);
    let span = |value: norn_text::SourceSpan| Span {
        line: value.line as u64,
        column: value.column as u64,
        byte_offset: value.byte_offset as u64,
    };
    facts.links = scan
        .links()
        .into_iter()
        .map(|link| LinkFact {
            field: None,
            family: match link.family {
                norn_text::LinkFamily::Wikilink => LinkFamily::Wikilink,
                norn_text::LinkFamily::Markdown => LinkFamily::Markdown,
            },
            embed: link.embed,
            protocol: link.protocol,
            target: link.target,
            title: link.title,
            anchor: match (link.anchor, link.block_ref) {
                (Some(written), _) => {
                    norn_text::anchor_readings(&written).map(|readings| LinkAnchor::Heading {
                        written,
                        readings: AnchorReadings {
                            text: readings.text,
                            marked: readings.marked,
                        },
                    })
                }
                (None, Some(id)) => (!id.is_empty()).then_some(LinkAnchor::Block { id }),
                (None, None) => None,
            },
            span: link.span.map(span),
        })
        .collect();
    facts.headings = scan
        .headings()
        .iter()
        .map(|heading| HeadingFact {
            level: heading.level,
            text: heading.text.clone(),
            reading: norn_text::heading_reading(&heading.text),
            slug: heading.slug.clone(),
            span: span(heading.span),
            body_offset: heading.body_offset as u64,
            inside_container: heading.inside_container,
        })
        .collect();
    facts.blocks = scan
        .block_ids()
        .into_iter()
        .map(|block| BlockFact {
            block_id: block.id,
            span: None,
        })
        .collect();
    facts
}

/// A body holding each of `lines` as a paragraph of its own.
pub(crate) fn body_of(lines: &[&str]) -> String {
    lines.iter().map(|line| format!("{line}\n\n")).collect()
}

/// A store over a root proven to have one case behaviour, its schema pinned,
/// holding a set of documents, and a read handle over it.
struct Judging {
    _scratch: Scratch,
    store: Store,
    reader: Arc<SnapshotReader>,
}

impl Judging {
    fn new(label: &str, order: StoredPathOrder, documents: &[DocumentFacts]) -> Self {
        let scratch = Scratch::new(label);
        let mut store = scratch.open_under(order);
        store
            .begin_request()
            .pin_vault_schema(SCHEMA.as_bytes(), SCHEMA)
            .expect("pinning the suite's schema");
        store
            .begin_request()
            .apply_increment(
                IncrementProvenance::Derived,
                documents.iter().cloned().map(Change::Upsert),
                &[],
                &declared(),
            )
            .expect("writing the documents");
        let reader = Arc::new(store.open_reader().reader.expect("a reader"));
        Judging {
            _scratch: scratch,
            store,
            reader,
        }
    }

    /// The findings judged about the links `holders` hold, and the steps the
    /// judgment took.
    fn judged(&mut self, holders: &[&str]) -> (Vec<FindingFacts>, u64) {
        let request = self.store.begin_request();
        let holders: Vec<_> = holders.iter().map(|at| path(at)).collect();
        let findings = request
            .judge_link_health(&holders, &declared())
            .expect("judging link health");
        (findings, request.read_steps())
    }

    /// The findings judged about the links `selection` selects, against key
    /// summaries of their own.
    fn selected(&mut self, selection: LinkSelection<'_>) -> Vec<FindingFacts> {
        self.store
            .begin_request()
            .judge_selected_links(selection, &mut KeySummaries::default(), &declared())
            .unwrap_or_else(|refusal| panic!("judging {selection:?}: {refusal}"))
    }

    /// The findings judged about the links the document at `at` holds, by
    /// the ordinal of the link each is about. The document is named twice,
    /// which judges its links once.
    fn by_ordinal(&mut self, at: &str) -> BTreeMap<u64, FindingFacts> {
        let (findings, _) = self.judged(&[at, at]);
        let mut by_ordinal = BTreeMap::new();
        for finding in findings {
            assert_eq!(
                finding.path.as_str(),
                at,
                "a finding about another document"
            );
            let ordinal = finding
                .ordinal
                .expect("a link-health finding names its link");
            assert!(
                by_ordinal.insert(ordinal, finding).is_none(),
                "two findings about link {ordinal} of `{at}`"
            );
        }
        by_ordinal
    }

    fn snapshot(&self) -> Snapshot {
        self.reader
            .try_take()
            .expect("a handle nothing is reading holds its connection")
            .establish()
            .expect("a snapshot")
    }

    /// The links the document at `at` holds, as a find's links column carries
    /// them.
    fn links(&self, at: &str) -> Vec<LinkRow> {
        let params = FindParams::new(vault())
            .with_predicates([Predicate::path(at)])
            .with_columns([Column::links()]);
        let found = self
            .snapshot()
            .find(&params, &declared())
            .unwrap_or_else(|refusal| panic!("a find of {params:?}: {refusal}"));
        let [row] = &found.rows[..] else {
            panic!("`{at}` is not one row: {:?}", found.rows);
        };
        let links = row.links.clone().expect("the links column");
        assert_eq!(links.total, links.items.len() as u64);
        links.items
    }

    /// Every finding a validate reads back.
    fn validated(&self) -> Vec<FindingRow> {
        match self
            .snapshot()
            .validate(&ValidateParams::new(vault()), &declared())
            .expect("a validate")
            .answer
        {
            Validation::Findings { rows, .. } => rows,
            Validation::Summary { .. } => panic!("a page answered a summary"),
        }
    }

    fn request(&mut self) -> Request<'_> {
        self.store.begin_request()
    }
}

fn vault() -> VaultAddress {
    VaultAddress::name(VaultName::new("notes").expect("a vault name"))
}

/// The documents every link below is judged against.
fn targets() -> Vec<DocumentFacts> {
    let mut all = vec![
        derived("notes/glossary.md", "# Heading\n\ntext ^blk\n"),
        derived("archive/glossary.md", "archived\n"),
        derived("notes/dup.md", "one\n"),
        derived("other/dup.md", "two\n"),
        derived("x/y.md", "y\n"),
        derived("x/my note.md", "note\n"),
        derived("r/report.md", "report\n"),
        derived("r/report.md.md", "report twice\n"),
        derived("pic/photo.md", "photo\n"),
        derived("v1.md.md", "dotted\n"),
        derived("v1.md", "undotted\n"),
    ];
    all.extend((1..=7).map(|at| derived(&format!("crowd/{at}/crowd.md"), "crowd\n")));
    all
}

/// Each link `src/a.md` holds, in order, beside the kind of finding it raises
/// on a root that tells spellings apart and on one that folds ASCII case.
const SOURCE: &[(&str, Option<FindingKind>, Option<FindingKind>)] = {
    use FindingKind::{Ambiguous, Broken, MissingAnchor, VaultScheme};
    &[
        // One document, `archive/glossary.md` being an ignored place.
        ("[[glossary]]", None, None),
        ("[[dup]]", Some(Ambiguous), Some(Ambiguous)),
        ("[[missing]]", Some(Broken), Some(Broken)),
        ("[[glossary#Heading]]", None, None),
        (
            "[[glossary#Nope]]",
            Some(MissingAnchor),
            Some(MissingAnchor),
        ),
        ("[[glossary#^blk]]", None, None),
        (
            "[[glossary#^nope]]",
            Some(MissingAnchor),
            Some(MissingAnchor),
        ),
        ("[[notes/dup]]", None, None),
        ("[y](../x/y.md)", None, None),
        ("[note](../x/my%20note.md)", None, None),
        // Above the vault root: no key at all, and a document target.
        ("[escape](../../escape.md)", Some(Broken), Some(Broken)),
        // Without `.md`, a path still names its document (ADR 0040), and
        // one naming none, with no extension, is a broken document target.
        ("[bare](../x/y)", None, None),
        ("[bare gone](../x/z)", Some(Broken), Some(Broken)),
        ("[web](https://example.com)", None, None),
        // A named file absent from the vault is broken.
        ("[[picture.png]]", Some(Broken), Some(Broken)),
        ("[self](./a.md)", None, None),
        ("[[report.md]]", Some(Ambiguous), Some(Ambiguous)),
        // Its own document, which holds no such heading.
        ("[[#Heading]]", Some(MissingAnchor), Some(MissingAnchor)),
        ("[img](img/pic.png)", Some(Broken), Some(Broken)),
        // A dotted target with no matching document or file is broken.
        ("[[photo.png]]", Some(Broken), Some(Broken)),
        ("[[crowd]]", Some(Ambiguous), Some(Ambiguous)),
        ("[[archive/glossary]]", None, None),
        ("[[vault://v1.md]]", Some(Ambiguous), Some(Ambiguous)),
        ("[gone](missing.md)", Some(Broken), Some(Broken)),
        ("[[GLOSSARY]]", None, None),
        ("![[dup]]", Some(Ambiguous), Some(Ambiguous)),
        ("![[missing]]", Some(Broken), Some(Broken)),
        ("[[Glossary#heading]]", None, None),
        ("[frag](../notes/glossary.md#Heading)", None, None),
        // A `vault://` link of either family naming one document is
        // resolved, and Obsidian cannot open it. Broken, ambiguous and
        // missing-anchor keep their own kinds.
        ("[[vault://x/y.md]]", Some(VaultScheme), Some(VaultScheme)),
        (
            "[rooted](vault://x/y.md)",
            Some(VaultScheme),
            Some(VaultScheme),
        ),
        ("![[vault://x/y]]", Some(VaultScheme), Some(VaultScheme)),
        ("[[vault://nowhere.md]]", Some(Broken), Some(Broken)),
        (
            "[[vault://notes/glossary.md#Heading]]",
            Some(VaultScheme),
            Some(VaultScheme),
        ),
        (
            "[[vault://notes/glossary.md#Nope]]",
            Some(MissingAnchor),
            Some(MissingAnchor),
        ),
    ]
};

/// The fixture: the targets, `src/a.md` holding [`SOURCE`], and `src/b.md`
/// holding a few more.
fn corpus() -> Vec<DocumentFacts> {
    let mut all = targets();
    let source: Vec<&str> = SOURCE.iter().map(|(line, ..)| *line).collect();
    all.push(derived("src/a.md", &body_of(&source)));
    all.push(derived(
        "src/b.md",
        &body_of(&["[[crowd]]", "[[glossary]]", "[[notes/glossary#Nope]]"]),
    ));
    all
}

/// A head's candidates as the paths and suffixes that name them.
fn named(candidates: impl Iterator<Item = (String, String)>) -> Vec<(String, String)> {
    candidates.collect()
}

fn finding_head(finding: &FindingFacts) -> Vec<(String, String)> {
    named(finding.candidates.iter().map(|candidate| {
        (
            candidate.path.as_str().to_string(),
            candidate.suffix.clone(),
        )
    }))
}

fn row_head(row: &LinkRow) -> Vec<(String, String)> {
    named(row.targets.candidates().iter().map(|candidate| {
        (
            candidate.path.as_str().to_string(),
            candidate.suffix.clone(),
        )
    }))
}

// ---- what the judgment finds ----

/// **The judgment agrees with the health a find's links column reads.** Over
/// every shape of link, on both roots and under an ambiguity-ignore glob: a
/// broken link raises a broken finding with no candidate, an ambiguous one an
/// ambiguous finding carrying the head and the total the links column carries,
/// a healthy one nothing or — where its anchor names a place the one document
/// does not hold — a missing-anchor finding naming that document, and a link
/// not judged raises nothing. Each finding is a warning about its link, at the
/// link's ordinal and span.
///
/// The kinds are also stated per link, so a rule both readings share cannot
/// invert without this failing: a document or vault-file target resolving
/// to nothing is broken.
#[test]
fn judgment_agrees_with_the_links_column_health() {
    for order in [Sensitive, Folding] {
        let mut judging = Judging::new(&format!("health-agrees-{order:?}"), order, &corpus());
        let mut seen: BTreeSet<Option<&str>> = BTreeSet::new();
        for holder in ["src/a.md", "src/b.md"] {
            let rows = judging.links(holder);
            let findings = judging.by_ordinal(holder);
            let stored = judging
                .request()
                .stored_facts(&path(holder))
                .expect("reading stored facts")
                .expect("the holder's row");
            assert!(
                findings
                    .keys()
                    .all(|ordinal| (*ordinal as usize) < rows.len()),
                "a finding about a link `{holder}` does not hold: {findings:?}"
            );
            for (ordinal, row) in rows.iter().enumerate() {
                let finding = findings.get(&(ordinal as u64));
                let kind = finding.map(|finding| finding.kind);
                seen.insert(kind.map(|kind| kind.as_str()));
                let context = format!("{order:?} `{holder}` link {ordinal} `{}`", row.target);
                match row.health() {
                    LinkHealth::Broken => {
                        assert_eq!(kind, Some(FindingKind::Broken), "{context}");
                    }
                    LinkHealth::Ambiguous => {
                        assert_eq!(kind, Some(FindingKind::Ambiguous), "{context}");
                    }
                    LinkHealth::Healthy => assert!(
                        matches!(
                            kind,
                            None | Some(FindingKind::MissingAnchor | FindingKind::VaultScheme)
                        ),
                        "{context}: {kind:?}"
                    ),
                    LinkHealth::NotJudged => assert_eq!(kind, None, "{context}"),
                    other => panic!("{context}: an unknown health {other:?}"),
                }
                let Some(finding) = finding else {
                    continue;
                };
                assert_eq!(finding.severity, Severity::Warning, "{context}");
                assert_eq!(finding.span, stored.links[ordinal].fact.span, "{context}");
                match finding.kind {
                    FindingKind::Broken => {
                        assert_eq!(
                            (finding_head(finding), finding.candidates_total),
                            (Vec::new(), 0),
                            "{context}"
                        );
                    }
                    FindingKind::Ambiguous
                    | FindingKind::MissingAnchor
                    | FindingKind::VaultScheme => {
                        assert_eq!(finding_head(finding), row_head(row), "{context}");
                        assert_eq!(finding.candidates_total, row.targets.total(), "{context}");
                    }
                    other => panic!("{context}: a finding of kind {other:?}"),
                }
                if finding.kind == FindingKind::MissingAnchor {
                    assert!(row.anchor.is_some(), "{context}: no anchor to miss");
                }
            }
        }
        assert_eq!(
            seen,
            [
                None,
                Some(FindingKind::Broken.as_str()),
                Some(FindingKind::Ambiguous.as_str()),
                Some(FindingKind::MissingAnchor.as_str()),
                Some(FindingKind::VaultScheme.as_str())
            ]
            .into_iter()
            .collect(),
            "{order:?}: the fixture does not reach every outcome"
        );

        let stated = judging.by_ordinal("src/a.md");
        for (ordinal, (line, sensitive, folding)) in SOURCE.iter().enumerate() {
            let expected = match order {
                Sensitive => *sensitive,
                Folding => *folding,
            };
            assert_eq!(
                stated.get(&(ordinal as u64)).map(|finding| finding.kind),
                expected,
                "{order:?}: `{line}`"
            );
        }
    }
}

/// **An embed carries the same kinds a link does.** Each target is linked and
/// embedded side by side, and the two raise the same finding: broken,
/// ambiguous, missing its anchor, or nothing.
#[test]
fn an_embed_carries_the_same_kinds() {
    let mut documents = targets();
    documents.push(derived(
        "src/e.md",
        &body_of(&[
            "[[missing]]",
            "![[missing]]",
            "[[dup]]",
            "![[dup]]",
            "[[glossary#Nope]]",
            "![[glossary#Nope]]",
            "[[glossary]]",
            "![[glossary]]",
        ]),
    ));
    let mut judging = Judging::new("health-embeds", Sensitive, &documents);
    let stored = judging
        .request()
        .stored_facts(&path("src/e.md"))
        .expect("reading stored facts")
        .expect("the holder's row");
    let embeds: Vec<bool> = stored.links.iter().map(|link| link.fact.embed).collect();
    assert_eq!(embeds, [false, true].repeat(4), "the fixture's embeds");

    let findings = judging.by_ordinal("src/e.md");
    let reading = |ordinal: u64| {
        findings.get(&ordinal).map(|finding| {
            (
                finding.kind,
                finding_head(finding),
                finding.candidates_total,
            )
        })
    };
    let kinds: Vec<Option<FindingKind>> = (0..8)
        .step_by(2)
        .map(|ordinal| {
            assert_eq!(
                reading(ordinal),
                reading(ordinal + 1),
                "link {ordinal} and the embed beside it"
            );
            reading(ordinal).map(|(kind, ..)| kind)
        })
        .collect();
    assert_eq!(
        kinds,
        [
            Some(FindingKind::Broken),
            Some(FindingKind::Ambiguous),
            Some(FindingKind::MissingAnchor),
            None
        ]
    );
}

/// **A Markdown image is judged as the Markdown link beside it is.** An image
/// naming a document raises what the link naming it raises (broken, ambiguous,
/// missing its anchor, or nothing); an image naming a missing vault file is
/// broken, as the link naming it is.
#[test]
fn a_markdown_image_is_judged_as_a_markdown_link_is() {
    let mut documents = targets();
    documents.push(derived(
        "src/i.md",
        &body_of(&[
            "[t](missing.md)",
            "![t](missing.md)",
            "[t](dup.md)",
            "![t](dup.md)",
            "[t](notes/glossary.md#Nope)",
            "![t](notes/glossary.md#Nope)",
            "[t](notes/glossary.md)",
            "![t](notes/glossary.md)",
            "![t](img/pic.png)",
        ]),
    ));
    let judging = &mut Judging::new("health-images", Sensitive, &documents);
    let stored = judging
        .request()
        .stored_facts(&path("src/i.md"))
        .expect("reading stored facts")
        .expect("the holder's row");
    let embeds: Vec<bool> = stored.links.iter().map(|link| link.fact.embed).collect();
    assert_eq!(
        embeds,
        [false, true]
            .repeat(4)
            .into_iter()
            .chain([true])
            .collect::<Vec<_>>()
    );

    let findings = judging.by_ordinal("src/i.md");
    let kind = |ordinal: u64| findings.get(&ordinal).map(|finding| finding.kind);
    for ordinal in (0..8).step_by(2) {
        assert_eq!(
            kind(ordinal),
            kind(ordinal + 1),
            "link {ordinal} and its image"
        );
    }
    assert_eq!(
        (0..8).step_by(2).map(kind).collect::<Vec<_>>(),
        [
            Some(FindingKind::Broken),
            Some(FindingKind::Ambiguous),
            Some(FindingKind::MissingAnchor),
            None
        ]
    );
    assert_eq!(
        kind(8),
        Some(FindingKind::Broken),
        "a missing image is broken"
    );
}

/// **A `vault://` link naming one document files a vault-scheme finding, in
/// either family, and only then.** Obsidian cannot open a `vault://` link, so
/// one that resolves is warned about; the same document named by a bare, a
/// rooted `/` or a relative link raises nothing. A `vault://` link that is
/// broken or ambiguous keeps that kind, and one whose anchor the document
/// lacks files the missing anchor alone: the store holds one finding per link,
/// so a missing anchor takes precedence over the scheme. The finding names the
/// one document, is a warning, and is read back by a validate with its
/// message.
#[test]
fn a_vault_scheme_link_naming_one_document_files_a_vault_scheme_finding() {
    let mut documents = targets();
    documents.push(derived(
        "src/v.md",
        &body_of(&[
            "[[vault://x/y.md]]",
            "[rooted](vault://x/y.md)",
            "[[x/y]]",
            "[bare](/x/y.md)",
            "[rel](../x/y.md)",
            "[[vault://missing.md]]",
            "[[vault://notes/dup.md]]",
            "[[vault://v1.md]]",
            "[[vault://notes/glossary.md#Nope]]",
            "[gone](vault://notes/glossary.md#Nope)",
            "[[vault://notes/glossary.md#Heading]]",
        ]),
    ));
    let mut judging = Judging::new("health-vault-scheme", Sensitive, &documents);
    let findings = judging.by_ordinal("src/v.md");
    let kinds: Vec<Option<FindingKind>> = (0..11)
        .map(|ordinal| findings.get(&ordinal).map(|finding| finding.kind))
        .collect();
    assert_eq!(
        kinds,
        [
            Some(FindingKind::VaultScheme),
            Some(FindingKind::VaultScheme),
            None,
            None,
            None,
            Some(FindingKind::Broken),
            Some(FindingKind::VaultScheme),
            Some(FindingKind::Ambiguous),
            Some(FindingKind::MissingAnchor),
            Some(FindingKind::MissingAnchor),
            Some(FindingKind::VaultScheme),
        ]
    );
    let scheme = &findings[&0];
    assert_eq!(scheme.severity, Severity::Warning);
    assert_eq!(scheme.target.as_deref(), Some("vault://x/y.md"));
    assert_eq!(scheme.candidates_total, 1);
    assert_eq!(
        scheme
            .candidates
            .iter()
            .map(|candidate| candidate.path.as_str())
            .collect::<Vec<_>>(),
        ["x/y.md"]
    );

    let (judged, _) = judging.judged(&["src/v.md"]);
    assert_filed(&judging.request(), "src/v.md", &judged);
    let rows = judging.validated();
    let read = rows
        .iter()
        .find(|row| {
            row.kind == FindingKind::VaultScheme && row.target.as_deref() == Some("vault://x/y.md")
        })
        .unwrap_or_else(|| panic!("no vault-scheme finding read back: {rows:?}"));
    assert_eq!(
        read.message,
        "the link resolves through `vault://`, a scheme Obsidian cannot open; the `/` form names the same document in both"
    );
}

/// **An ambiguous finding carries its head, its total and a hint; a broken one
/// carries none.** Recorded and read back by a validate: `[[crowd]]` names
/// seven documents and carries the first five in the resolution ladder's order,
/// the total seven, and the hint that enumerates the class; `[[missing]]`
/// carries an empty head of none and no hint, though it is keyed by the class
/// it names; a rooted name naming two documents carries both and no hint,
/// because no class holds it; and a missing anchor carries the one document it
/// names, and no hint.
#[test]
fn an_ambiguous_finding_carries_head_total_and_hint_and_a_broken_one_no_hint() {
    let mut documents = targets();
    documents.push(derived(
        "src/h.md",
        &body_of(&[
            "[[crowd]]",
            "[[missing]]",
            "[[vault://v1.md]]",
            "[[glossary#Nope]]",
        ]),
    ));
    let mut judging = Judging::new("health-heads", Sensitive, &documents);
    let (findings, _) = judging.judged(&["src/h.md"]);
    assert_eq!(findings.len(), 4, "{findings:?}");
    let broken = findings
        .iter()
        .find(|finding| finding.kind == FindingKind::Broken)
        .expect("a broken finding");
    assert!(
        !broken.class_keys.is_empty(),
        "a broken suffix link is keyed by the class it names"
    );
    assert_filed(&judging.request(), "src/h.md", &findings);

    let rows = judging.validated();
    let by_target: BTreeMap<String, &FindingRow> = rows
        .iter()
        .map(|row| (row.target.clone().expect("a target"), row))
        .collect();
    let read = |target: &str| {
        let row = by_target
            .get(target)
            .unwrap_or_else(|| panic!("no finding about `{target}`: {rows:?}"));
        (
            row.kind,
            row.head
                .candidates()
                .iter()
                .map(|candidate| candidate.path.as_str().to_string())
                .collect::<Vec<String>>(),
            row.head.total(),
            row.hint.clone(),
        )
    };
    let crowd: Vec<String> = (1..=CANDIDATE_HEAD)
        .map(|at| format!("crowd/{at}/crowd.md"))
        .collect();
    assert_eq!(
        read("crowd"),
        (
            FindingKind::Ambiguous,
            crowd,
            7,
            Some(Hint::resolves(
                ResolutionTarget::new("crowd").expect("a target")
            ))
        )
    );
    assert_eq!(read("missing"), (FindingKind::Broken, Vec::new(), 0, None));
    assert_eq!(
        read("vault://v1.md"),
        (
            FindingKind::Ambiguous,
            vec!["v1.md".to_string(), "v1.md.md".to_string()],
            2,
            None
        )
    );
    assert_eq!(
        read("glossary#Nope"),
        (
            FindingKind::MissingAnchor,
            vec!["notes/glossary.md".to_string()],
            1,
            None
        )
    );
}

/// **A link with no place in the source is stored and judged without one.**
/// A frontmatter wikilink whose value's bytes are not its text reaches the
/// store with no span: its row reads back with none, it is judged like any
/// other link, and the finding about it carries no span.
#[test]
fn an_unplaced_link_is_stored_and_judged_without_a_span() {
    let mut documents = targets();
    let mut holder = derived("src/u.md", &body_of(&["[[missing]]", "[[glossary]]"]));
    for link in &mut holder.links {
        link.span = None;
    }
    documents.push(holder);
    let mut judging = Judging::new("health-unplaced", Sensitive, &documents);
    let stored = judging
        .request()
        .stored_facts(&path("src/u.md"))
        .expect("reading a document")
        .expect("a written document");
    assert!(
        stored.links.len() == 2 && stored.links.iter().all(|link| link.fact.span.is_none()),
        "{:?}",
        stored.links
    );
    let (findings, _) = judging.judged(&["src/u.md"]);
    let kinds: Vec<(FindingKind, Option<Span>)> = findings
        .iter()
        .map(|finding| (finding.kind, finding.span))
        .collect();
    assert_eq!(kinds, [(FindingKind::Broken, None)]);
    let rows = judging.validated();
    let row = rows
        .iter()
        .find(|row| row.path.as_str() == "src/u.md")
        .expect("the broken link's finding row");
    assert_eq!(row.span, None);
}

/// A target ending in `.md` totals both document-name classes. The
/// ambiguous finding and links column agree on its three candidates.
#[test]
fn a_target_ending_in_md_totals_both_document_name_classes() {
    for order in [Sensitive, Folding] {
        let mut judging = Judging::new(
            &format!("health-dotted-{order:?}"),
            order,
            &[
                derived("a/v1.md.md", "a\n"),
                derived("b/v1.md.md", "b\n"),
                derived("c/v1.md", "c\n"),
                derived("h.md", &body_of(&["[[v1.md]]"])),
            ],
        );
        let findings = judging.by_ordinal("h.md");
        let finding = &findings[&0];
        assert_eq!(finding.kind, FindingKind::Ambiguous, "{order:?}");
        assert_eq!(finding.candidates_total, 3, "{order:?}");
        let rows = judging.links("h.md");
        assert_eq!(finding_head(finding), row_head(&rows[0]), "{order:?}");
        assert_eq!(
            finding
                .class_keys
                .iter()
                .map(|key| key.as_str())
                .collect::<Vec<_>>(),
            ["v1.md.md/", "v1.md/", "v1/"],
            "{order:?}"
        );
    }
}

/// The finding the judgment raises about the one link `h.md` holds, and the
/// links column's reading of that link, on a store under `order` holding
/// `documents` and `h.md` linking `link`.
fn judged_beside_the_links_column(
    label: &str,
    order: StoredPathOrder,
    documents: &[&str],
    link: &str,
) -> (FindingFacts, LinkRow) {
    let mut all: Vec<DocumentFacts> = documents.iter().map(|at| derived(at, "x\n")).collect();
    all.push(derived("h.md", &body_of(&[link])));
    let mut judging = Judging::new(&format!("{label}-{order:?}"), order, &all);
    let mut findings = judging.by_ordinal("h.md");
    let finding = findings.remove(&0).expect("a finding about the link");
    let [row] = &judging.links("h.md")[..] else {
        panic!("`h.md` holds one link");
    };
    (finding, row.clone())
}

/// **A head merged across two keys is cut to the bound.** `[[v1.md]]` over five
/// `*/v1.md.md` and three `*/v1.md` reads a full head from each class; the
/// finding carries the first [`CANDIDATE_HEAD`] of the eight in the ladder's
/// order, as the links column does, beside the total eight.
#[test]
fn a_head_merged_across_two_keys_is_cut_to_the_bound() {
    let mut documents: Vec<String> = (1..=5).map(|at| format!("d{at}/v1.md.md")).collect();
    documents.extend((1..=3).map(|at| format!("e{at}/v1.md")));
    let documents: Vec<&str> = documents.iter().map(String::as_str).collect();
    for order in [Sensitive, Folding] {
        let (finding, row) =
            judged_beside_the_links_column("health-merged-cut", order, &documents, "[[v1.md]]");
        assert_eq!(finding.kind, FindingKind::Ambiguous, "{order:?}");
        assert_eq!(finding.candidates.len(), CANDIDATE_HEAD, "{order:?}");
        assert_eq!(finding_head(&finding), row_head(&row), "{order:?}");
        assert_eq!(
            (finding.candidates_total, row.targets.total()),
            (8, 8),
            "{order:?}"
        );
    }
}

/// Mixed candidate kinds share one ordered head and exact total; exact paths
/// precede deeper suffixes, and optional `.md` admits documents alone.
#[test]
fn mixed_candidate_heads_preserve_order_totals_and_exact_paths() {
    for order in [Sensitive, Folding] {
        let mut judging = Judging::new(
            &format!("health-mixed-head-{order:?}"),
            order,
            &[
                derived("b/photo.png.md", "b\n"),
                derived("d/photo.png.md", "d\n"),
                derived("f/photo.png.md", "f\n"),
                derived(
                    "holder.md",
                    &body_of(&[
                        "[[photo.png]]",
                        "[[/b/photo.png]]",
                        "[[b/photo.png]]",
                        "[[photo.png.md]]",
                    ]),
                ),
            ],
        );
        judging
            .store
            .begin_request()
            .apply_increment(
                IncrementProvenance::Derived,
                [
                    "a/photo.png",
                    "c/photo.png",
                    "e/photo.png",
                    "g/photo.png",
                    "deep/b/photo.png",
                ]
                .map(|at| Change::FileBirth { path: path(at) }),
                &[],
                &declared(),
            )
            .expect("files sharing the documents' class");
        let (findings, _) = judging.judged(&["holder.md"]);
        assert_filed(&judging.request(), "holder.md", &findings);
        assert_eq!(findings.len(), 2, "{order:?}: {findings:?}");
        for (finding, ordinal, total, expected) in [
            (
                &findings[0],
                0,
                8,
                vec![
                    "a/photo.png",
                    "b/photo.png.md",
                    "deep/b/photo.png",
                    "c/photo.png",
                    "d/photo.png.md",
                ],
            ),
            (
                &findings[1],
                3,
                3,
                vec!["b/photo.png.md", "d/photo.png.md", "f/photo.png.md"],
            ),
        ] {
            assert_eq!(finding.kind, FindingKind::Ambiguous, "{order:?}");
            assert_eq!(finding.ordinal, Some(ordinal), "{order:?}");
            assert_eq!(finding.candidates_total, total, "{order:?}");
            assert_eq!(
                finding
                    .candidates
                    .iter()
                    .map(|candidate| candidate.path.as_str())
                    .collect::<Vec<_>>(),
                expected,
                "{order:?}"
            );
        }
        let links = judging.links("holder.md");
        for ordinal in [1, 2] {
            assert_eq!(links[ordinal].health(), LinkHealth::Healthy, "{order:?}");
            assert_eq!(
                row_head(&links[ordinal]),
                vec![("b/photo.png.md".into(), "b/photo.png".into())]
            );
        }
        for finding in &findings {
            let link = &links[finding.ordinal.expect("a link ordinal") as usize];
            assert_eq!(finding_head(finding), row_head(link), "{order:?}");
            assert_eq!(finding.candidates_total, link.targets.total(), "{order:?}");
        }
    }
}

/// **The exact path comes first across a link's keys, however many stand
/// there** (ADR 0040). On a root that tells spellings apart, eight case
/// variants of `c/abc.md` and `c/abc.md.md` all stand at an exact path of
/// `[[c/abc.md]]` — one under each of its two keys — so the link names those
/// nine and not `d/c/abc.md` or `d/c/abc.md.md`; the head fills with exact
/// members, so the count is the exact-path count, and the finding agrees
/// with the links column. `[[x.md]]` names `x.md.md` alone, exact under one
/// key, though its other key's class holds only the deeper `a/x.md`.
#[test]
fn the_exact_path_comes_first_across_a_links_keys() {
    let mut documents: Vec<String> = (0..8u8)
        .map(|bits| {
            let name: String = "abc"
                .chars()
                .enumerate()
                .map(|(at, letter)| {
                    if bits & (1 << at) == 0 {
                        letter
                    } else {
                        letter.to_ascii_uppercase()
                    }
                })
                .collect();
            format!("c/{name}.md")
        })
        .collect();
    documents.extend(["c/abc.md.md", "d/c/abc.md", "d/c/abc.md.md"].map(String::from));
    let documents: Vec<&str> = documents.iter().map(String::as_str).collect();
    let (finding, row) =
        judged_beside_the_links_column("health-exact-twins", Sensitive, &documents, "[[c/abc.md]]");
    assert_eq!(finding.kind, FindingKind::Ambiguous);
    assert_eq!((finding.candidates_total, row.targets.total()), (9, 9));
    assert_eq!(finding_head(&finding), row_head(&row));
    assert!(
        finding
            .candidates
            .iter()
            .all(|candidate| candidate.path.as_str().starts_with("c/")),
        "{:?}",
        finding.candidates
    );

    for order in [Sensitive, Folding] {
        let mut judging = Judging::new(
            &format!("health-exact-sibling-{order:?}"),
            order,
            &[
                derived("x.md.md", "x\n"),
                derived("a/x.md", "x\n"),
                derived("h.md", &body_of(&["[[x.md]]", "[[x]]"])),
            ],
        );
        assert_eq!(judging.by_ordinal("h.md"), BTreeMap::new(), "{order:?}");
        let names: Vec<Vec<String>> = judging
            .links("h.md")
            .iter()
            .map(|row| {
                row.targets
                    .candidates()
                    .iter()
                    .map(|candidate| candidate.path.as_str().to_string())
                    .collect()
            })
            .collect();
        assert_eq!(names, [vec!["x.md.md"], vec!["a/x.md"]], "{order:?}");
    }
}

/// A class head follows the comparison-key ladder rather than original
/// path order. `F/A.md` sorts first as a path but last as a suffix key.
#[test]
fn a_class_larger_than_the_head_is_cut_in_the_ladders_order() {
    let documents = ["F/A.md", "a/A.md", "b/A.md", "c/A.md", "d/A.md", "e/A.md"];
    let (finding, row) =
        judged_beside_the_links_column("health-ladder-cut", Folding, &documents, "[[A]]");
    assert_eq!(finding.candidates_total, 6);
    assert_eq!(finding_head(&finding), row_head(&row));
    assert_eq!(
        finding
            .candidates
            .iter()
            .map(|candidate| candidate.path.as_str())
            .collect::<Vec<_>>(),
        ["a/A.md", "b/A.md", "c/A.md", "d/A.md", "e/A.md"]
    );
}

/// A rooted `.md` target ranks candidates by normalized complete-path
/// keys. `v1.md` precedes `v1.MD.md`, despite their original byte order.
#[test]
fn a_rooted_names_documents_rank_by_the_key_that_reached_them() {
    let (finding, row) = judged_beside_the_links_column(
        "health-rooted-rung",
        Folding,
        &["v1.MD.md", "v1.md"],
        "[[vault://v1.MD]]",
    );
    assert_eq!(finding.kind, FindingKind::Ambiguous);
    assert_eq!(finding_head(&finding), row_head(&row));
    assert_eq!(
        finding
            .candidates
            .iter()
            .map(|candidate| candidate.path.as_str())
            .collect::<Vec<_>>(),
        ["v1.md", "v1.MD.md"]
    );
}

/// **A missing anchor is judged by the readings the store holds.** Against one
/// document holding `# My Heading`, `## Top`, `### Sub` and a block `^blk`: a
/// heading anchor names its heading by its text under the case and space fold,
/// by the anchor's marked reading — a heading chain's last heading — and by
/// the slug as written; a Markdown fragment arrives decoded; a block reference
/// names its block exactly; and an empty anchor is no anchor. Each anchor
/// naming nothing the document holds is missing.
#[test]
fn a_missing_anchor_is_judged_by_the_stored_readings() {
    let lines: &[(&str, bool)] = &[
        ("[[t#My Heading]]", false),
        ("[[t#my   HEADING]]", false),
        ("[[t#my-heading]]", false),
        ("[[t#Nope]]", true),
        ("[[t#^blk]]", false),
        ("[[t#^nope]]", true),
        ("[[t#Top#Sub]]", false),
        ("[[t#Top#Nope]]", true),
        ("[x](t.md#My%20Heading)", false),
        ("[x](t.md#Not%20Here)", true),
        ("[x](t.md#top)", false),
        ("[[t#]]", false),
        ("[[t#^]]", false),
    ];
    for order in [Sensitive, Folding] {
        let mut judging = Judging::new(
            &format!("health-anchors-{order:?}"),
            order,
            &[
                derived("t.md", "# My Heading\n\n## Top\n\n### Sub\n\npara ^blk\n"),
                derived(
                    "h.md",
                    &body_of(&lines.iter().map(|(line, _)| *line).collect::<Vec<_>>()),
                ),
            ],
        );
        let findings = judging.by_ordinal("h.md");
        for (ordinal, (line, missing)) in lines.iter().enumerate() {
            let expected = missing.then_some(FindingKind::MissingAnchor);
            let finding = findings.get(&(ordinal as u64));
            assert_eq!(
                finding.map(|finding| finding.kind),
                expected,
                "{order:?}: `{line}`"
            );
            if let Some(finding) = finding {
                assert_eq!(
                    (finding_head(finding), finding.candidates_total),
                    (vec![("t.md".to_string(), "t".to_string())], 1),
                    "{order:?}: `{line}`"
                );
            }
        }
    }
}

/// Link-health findings record normalized link keys and candidate naming classes.
#[test]
fn a_findings_keys_are_its_links_keys() {
    for order in [Sensitive, Folding] {
        let space = SuffixKey::Comparison;
        let mut judging = Judging::new(
            &format!("health-keys-{order:?}"),
            order,
            &[
                derived("a/V1.md.md", "a\n"),
                derived("b/V1.md", "b\n"),
                derived("V1.md.md", "rooted\n"),
                derived("V1.md", "rooted\n"),
                derived(
                    "Src/h.md",
                    &body_of(&[
                        "[[Missing/Thing]]",
                        "[[V1.md]]",
                        "[g](Gone/Dir.md)",
                        "[[vault://V1.md]]",
                        "[e](../../escape.md)",
                    ]),
                ),
            ],
        );
        let stored = judging
            .request()
            .stored_facts(&path("Src/h.md"))
            .expect("reading stored facts")
            .expect("the holder's row");
        let findings = judging.by_ordinal("Src/h.md");
        assert_eq!(findings.len(), 5, "{order:?}: {findings:?}");
        let (mut classes, mut paths) = (0, 0);
        for (ordinal, finding) in &findings {
            let keys: Vec<(String, bool)> = stored
                .link_keys
                .iter()
                .filter(|key| key.link == *ordinal)
                .map(|key| {
                    let spelled = match space {
                        SuffixKey::Comparison => key.key.clone(),
                    };
                    (spelled, key.segments.is_some())
                })
                .collect();
            let naming = |candidate: &str| -> &[&str] {
                match candidate.rsplit('/').next() {
                    Some("V1.md.md") => &["V1.md/", "V1/", "V1.md.md/"],
                    Some("V1.md") => &["V1/", "V1.md/"],
                    other => {
                        panic!("{order:?}: a candidate the fixture holds no name of: {other:?}")
                    }
                }
            };
            let in_space = |class: &str| match space {
                SuffixKey::Comparison => class.to_ascii_lowercase(),
            };
            let expected_classes: BTreeSet<String> = keys
                .iter()
                .filter(|(_, class)| *class)
                .map(|(key, _)| key.clone())
                .chain(finding.candidates.iter().flat_map(|candidate| {
                    naming(candidate.path.as_str())
                        .iter()
                        .map(|class| in_space(class))
                }))
                .collect();
            let expected_paths: BTreeSet<String> = keys
                .iter()
                .filter(|(_, class)| !*class)
                .map(|(key, _)| key.clone())
                .collect();
            let filed_classes: BTreeSet<String> = finding
                .class_keys
                .iter()
                .map(|key| key.as_str().to_string())
                .collect();
            let filed_paths: BTreeSet<String> = finding
                .path_keys
                .iter()
                .map(|key| key.as_str().to_string())
                .collect();
            assert_eq!(filed_classes, expected_classes, "{order:?} link {ordinal}");
            assert_eq!(filed_paths, expected_paths, "{order:?} link {ordinal}");
            classes = classes.max(filed_classes.len());
            paths = paths.max(filed_paths.len());
        }
        assert_eq!((classes, paths), (3, 2), "{order:?}: the fixture's reach");
        assert!(
            findings[&4].class_keys.is_empty() && findings[&4].path_keys.is_empty(),
            "{order:?}: a link naming no vault path is keyed by nothing"
        );
        let judged: Vec<FindingFacts> = findings.into_values().collect();
        assert_filed(&judging.request(), "Src/h.md", &judged);
    }
}

/// Assert the store filed, at `holder`, exactly the link-health findings
/// `judged` holds: each written inside the changeset that wrote the holder.
pub(crate) fn assert_filed(request: &Request<'_>, holder: &str, judged: &[FindingFacts]) {
    let filed: Vec<_> = request
        .stored_findings(&path(holder))
        .expect("reading findings")
        .into_iter()
        .map(|finding| {
            (
                finding.kind,
                finding.ordinal,
                finding.target,
                finding.class_keys,
                finding.path_keys,
                finding.candidates,
                finding.candidates_total,
                finding.message,
            )
        })
        .collect();
    let judged: Vec<_> = judged
        .iter()
        .map(|finding| {
            (
                finding.kind.as_str().to_string(),
                finding.ordinal,
                finding.target.clone(),
                finding.class_keys.clone(),
                finding.path_keys.clone(),
                finding.candidates.clone(),
                finding.candidates_total,
                finding.message.clone(),
            )
        })
        .collect();
    assert_eq!(filed, judged, "the findings the store filed at `{holder}`");
}

// ---- selecting the links judged ----

/// **A link is judged alike however it is selected.** The links a document
/// holds are selected by that document, and again by the class or the path
/// every one of them is held under, and the two judgments are the same
/// findings in the same order: broken, ambiguous and missing an anchor
/// through a class, and missing an anchor and healthy through a path.
#[test]
fn a_link_is_judged_alike_by_its_document_and_by_the_key_it_is_held_under() {
    for order in [Sensitive, Folding] {
        let mut documents = targets();
        documents.push(derived(
            "src/c.md",
            &body_of(&[
                "[[dup]]",
                "![[dup]]",
                "[[notes/dup]]",
                "[[notes/dup#Nope]]",
                "[[nowhere/dup]]",
                "[[dup.md]]",
            ]),
        ));
        documents.push(derived(
            "src/p.md",
            &body_of(&[
                "[a](../notes/dup.md)",
                "[b](../notes/dup.md#Nope)",
                "[[vault://notes/dup#^nope]]",
            ]),
        ));
        let mut judging = Judging::new(&format!("health-selected-{order:?}"), order, &documents);

        let by_document = judging.selected(LinkSelection::Documents(&[path("src/c.md")]));
        let class = ClassKey::new("dup/").expect("a class key");
        assert_eq!(
            judging.selected(LinkSelection::Class(&class)),
            by_document,
            "{order:?}"
        );
        let kinds: BTreeSet<&str> = by_document
            .iter()
            .map(|finding| finding.kind.as_str())
            .collect();
        assert_eq!(
            kinds,
            [
                FindingKind::Broken,
                FindingKind::Ambiguous,
                FindingKind::MissingAnchor
            ]
            .map(|kind| kind.as_str())
            .into_iter()
            .collect(),
            "{order:?}: the fixture's reach"
        );

        let by_document = judging.selected(LinkSelection::Documents(&[path("src/p.md")]));
        let at = PathKey::new("notes/dup.md").expect("a path key");
        assert_eq!(
            judging.selected(LinkSelection::Path(&at)),
            by_document,
            "{order:?}"
        );
        assert_eq!(
            by_document
                .iter()
                .map(|finding| (finding.ordinal, finding.kind))
                .collect::<Vec<_>>(),
            [
                (Some(1), FindingKind::MissingAnchor),
                (Some(2), FindingKind::MissingAnchor)
            ],
            "{order:?}"
        );
    }
}

/// **A class selection never reaches a path-addressed link.** `[[glossary]]`
/// is held under the suffix key that opens the class `glossary/`; the
/// Markdown link `[a](../glossary/y.md)` and the rooted wikilink
/// `[[vault://glossary/z]]` are held under the path keys `glossary/y.md` and
/// `glossary/z.md`, which sort inside that same class's byte range without
/// being one of its keys — `link_keys` shares one column for both key kinds
/// ([`crate::ddl::facts`]). All three name a document nothing at this vault
/// holds, so all three are broken were they judged; a class selection judges
/// only the first, and a path selection only the link keyed exactly at it.
#[test]
fn a_class_selection_never_reaches_a_path_keyed_link() {
    for order in [Sensitive, Folding] {
        let documents = vec![derived(
            "src/h.md",
            &body_of(&[
                "[[glossary]]",
                "[a](../glossary/y.md)",
                "[[vault://glossary/z]]",
            ]),
        )];
        let mut judging = Judging::new(&format!("health-class-path-{order:?}"), order, &documents);

        let class = ClassKey::new("glossary/").expect("a class key");
        let by_class = judging.selected(LinkSelection::Class(&class));
        let ordinals: BTreeSet<u64> = by_class
            .iter()
            .filter_map(|finding| finding.ordinal)
            .collect();
        assert_eq!(
            ordinals,
            BTreeSet::from([0]),
            "{order:?}: the class `glossary/` selects only its suffix-addressed link: {by_class:?}"
        );
        assert!(
            by_class
                .iter()
                .all(|finding| finding.kind == FindingKind::Broken),
            "{order:?}: {by_class:?}"
        );

        let at = PathKey::new("glossary/y.md").expect("a path key");
        let by_path = judging.selected(LinkSelection::Path(&at));
        let ordinals: BTreeSet<u64> = by_path
            .iter()
            .filter_map(|finding| finding.ordinal)
            .collect();
        assert_eq!(
            ordinals,
            BTreeSet::from([1]),
            "{order:?}: the path key `glossary/y.md` selects only the link spelling it: {by_path:?}"
        );
    }
}

/// **A judgment taken in chunks resolves each key once.** Ten documents each
/// hold `[[hub]]` and a path link to one of the documents it names, so their
/// links hold two keys between them. Judged in two chunks of five documents
/// against one set of key summaries, the two keys are resolved once each,
/// the second chunk reads less than it does against summaries of its own,
/// and the findings are the ones a single judgment of the ten finds.
#[test]
fn a_judgment_taken_in_chunks_resolves_each_key_once() {
    let mut documents: Vec<DocumentFacts> = (0..7)
        .map(|at| derived(&format!("m/{at}/hub.md"), "hub\n"))
        .collect();
    documents.extend((0..10).map(|at| {
        derived(
            &format!("h/{at}.md"),
            &body_of(&["[[hub]]", "[p](../m/0/hub#Nope)"]),
        )
    }));
    let mut judging = Judging::new("health-chunks", Sensitive, &documents);
    let holders: Vec<_> = (0..10).map(|at| path(&format!("h/{at}.md"))).collect();
    let (first, second) = holders.split_at(5);

    let request = judging.request();
    let mut summaries = KeySummaries::default();
    let judge = |chunk: &[norn_store::DocumentPath], summaries: &mut KeySummaries| {
        let before = request.read_steps();
        let findings = request
            .judge_selected_links(LinkSelection::Documents(chunk), summaries, &declared())
            .expect("judging a chunk");
        (findings, request.read_steps() - before)
    };
    let (mut chunked, _) = judge(first, &mut summaries);
    assert_eq!(summaries.keys_resolved(), 2, "the first chunk's keys");
    let (findings, warm) = judge(second, &mut summaries);
    chunked.extend(findings);
    assert_eq!(
        summaries.keys_resolved(),
        2,
        "the second chunk resolved a key the first already had"
    );

    let mut own = KeySummaries::default();
    let (alone, cold) = judge(second, &mut own);
    assert_eq!(own.keys_resolved(), 2);
    assert_eq!(alone, chunked[10..]);
    assert!(
        warm < cold,
        "reused summaries read as much: {warm} >= {cold}"
    );

    let (whole, _) = judging.judged(&holders.iter().map(|at| at.as_str()).collect::<Vec<_>>());
    assert_eq!(chunked, whole);
    assert_eq!(chunked.len(), 20);
}

// ---- the work bar ----

/// A store holding `links` documents `h/NNN.md` that each link `[[hub]]`, and
/// `multiplicity` documents `m/NNN/hub.md` the link names.
fn hub(links: usize, multiplicity: usize) -> Judging {
    let mut documents: Vec<DocumentFacts> = (0..multiplicity)
        .map(|at| derived(&format!("m/{at:03}/hub.md"), "hub\n"))
        .collect();
    documents.extend((0..links).map(|at| derived(&format!("h/{at:03}.md"), "[[hub]]\n")));
    Judging::new(
        &format!("health-work-{links}-{multiplicity}"),
        Sensitive,
        &documents,
    )
}

/// **A judgment's work adds its links and their candidates; it never
/// multiplies them.** Over a grid of fifty and five hundred links to one
/// stem, crossed with two and two hundred documents the stem names, the
/// steps each judgment takes are a cost per link plus a cost per candidate:
/// what the extra links cost is the same whatever the stem names, so the
/// grid's interaction term is zero. Each link is ambiguous among every
/// document the stem names.
#[test]
fn judgment_work_is_links_plus_candidates() {
    let mut steps = BTreeMap::new();
    for links in [50, 500] {
        for multiplicity in [2, 200] {
            let mut judging = hub(links, multiplicity);
            let holders: Vec<String> = (0..links).map(|at| format!("h/{at:03}.md")).collect();
            let holders: Vec<&str> = holders.iter().map(String::as_str).collect();
            let (findings, taken) = judging.judged(&holders);
            assert_eq!(findings.len(), links, "({links}, {multiplicity})");
            assert!(
                findings.iter().all(|finding| {
                    finding.kind == FindingKind::Ambiguous
                        && finding.candidates_total == multiplicity as u64
                }),
                "({links}, {multiplicity})"
            );
            steps.insert((links, multiplicity), taken as i64);
        }
    }
    let more_links_among = |multiplicity| steps[&(500, multiplicity)] - steps[&(50, multiplicity)];
    let more_candidates_under = |links| steps[&(links, 200)] - steps[&(links, 2)];
    eprintln!("judgment steps over the grid: {steps:?}");
    assert!(
        more_links_among(2) > 0 && more_candidates_under(50) > 0,
        "the grid does not move the work: {steps:?}"
    );
    assert_eq!(
        more_links_among(200),
        more_links_among(2),
        "four hundred and fifty more links cost more where the stem names more documents, so \
         the work multiplies links by candidates: {steps:?}"
    );
    assert_eq!(more_candidates_under(500), more_candidates_under(50));
}

// ---- the plan bars ----

/// The statements the judgment runs, each of which the bar below judges.
pub(crate) const LINK_HEALTH: [ExplainedStatement<'static>; 12] = [
    ExplainedStatement::LinkHealthLinks,
    ExplainedStatement::LinkHealthClassLinks,
    ExplainedStatement::LinkHealthPathLinks,
    ExplainedStatement::LinkHealthHeads,
    ExplainedStatement::LinkHealthTotals,
    ExplainedStatement::LinkHealthSuffixes,
    ExplainedStatement::LinkHealthAnchors,
    ExplainedStatement::LinkHealthWrittenLinks,
    ExplainedStatement::LinkHealthClassFindings,
    ExplainedStatement::LinkHealthFoundLinks,
    ExplainedStatement::LinkHealthDiscard,
    ExplainedStatement::LinkHealthOccupied,
];

fn plan(emitted: norn_store::EmittedPlan) -> QueryPlan {
    QueryPlan::new(
        emitted.sql,
        emitted
            .steps
            .into_iter()
            .map(|step| PlanRow::new(step.id, step.parent, step.detail))
            .collect(),
    )
}

/// One seek a statement is judged by: the alias it reads a table under, the
/// index — or the row id — it reads that table through, and the constraint
/// the search carries.
struct Seek {
    alias: &'static str,
    access: Access<'static>,
    constraint: String,
}

fn seek(alias: &'static str, access: Access<'static>, constraint: &str) -> Seek {
    Seek {
        alias,
        access,
        constraint: constraint.to_string(),
    }
}

/// The seeks each judgment statement is held to under `order`.
fn seeks(statement: ExplainedStatement<'_>, order: StoredPathOrder) -> Vec<Seek> {
    let (admitted_index, key, path_index, link_index, suffix_index, link_key) = match order {
        Sensitive => (
            "documents_admitted_suffix_key",
            "suffix_key",
            "documents_path_key",
            "link_keys_key",
            "link_keys_suffix_key",
            "key",
        ),
        Folding => (
            "documents_admitted_suffix_key",
            "suffix_key",
            "documents_path_key",
            "link_keys_key",
            "link_keys_suffix_key",
            "key",
        ),
    };
    let range = format!("(admitting_segments=? AND {key}>? AND {key}<?)");
    // A page's driver, then each link it reached, its holder, and every key
    // it is held under.
    let reached = |driver: Vec<Seek>| {
        driver
            .into_iter()
            .chain([
                seek("l", Access::RowId, "(rowid=?)"),
                seek("d", Access::RowId, "(rowid=?)"),
                seek("k", Access::Index("link_keys_link"), "(link=?)"),
            ])
            .collect()
    };
    match statement {
        ExplainedStatement::LinkHealthLinks => vec![
            seek("d", Access::Index("documents_path"), "(path=?)"),
            seek("l", Access::Index("links_document_ordinal"), "(document=?)"),
            seek("k", Access::Index("link_keys_link"), "(link=?)"),
        ],
        ExplainedStatement::LinkHealthClassLinks => reached(vec![seek(
            "s",
            Access::Index(suffix_index),
            &format!("(({link_key},document,link)>(?,?,?) AND {link_key}<?)"),
        )]),
        ExplainedStatement::LinkHealthPathLinks => reached(vec![seek(
            "s",
            Access::Index(link_index),
            &format!("({link_key}=? AND (document,link)>(?,?))"),
        )]),
        ExplainedStatement::LinkHealthWrittenLinks => reached(vec![
            seek(
                "dw",
                Access::Index("documents_change_feed"),
                "(generation=? AND path>?)",
            ),
            seek(
                "lw",
                Access::Index("links_document_ordinal"),
                "(document=? AND ordinal>?)",
            ),
        ]),
        ExplainedStatement::LinkHealthClassFindings => vec![
            seek(
                "s",
                Access::Index("finding_classes_class_key"),
                "((class_key,finding)>(?,?) AND class_key<?)",
            ),
            seek("f", Access::RowId, "(rowid=?)"),
            seek("d", Access::Index("documents_path"), "(path=?)"),
            seek(
                "l",
                Access::Index("links_document_ordinal"),
                "(document=? AND ordinal=?)",
            ),
        ],
        ExplainedStatement::LinkHealthFoundLinks => vec![
            seek("l", Access::RowId, "(rowid=?)"),
            seek("d", Access::RowId, "(rowid=?)"),
            seek("k", Access::Index("link_keys_link"), "(link=?)"),
        ],
        ExplainedStatement::LinkHealthDiscard => vec![seek("findings", Access::RowId, "(rowid=?)")],
        ExplainedStatement::LinkHealthOccupied => vec![
            seek(
                "sc",
                Access::Index(suffix_index),
                &format!("({link_key}>? AND {link_key}<?)"),
            ),
            seek(
                "fc",
                Access::Index("finding_classes_class_key"),
                "(class_key>? AND class_key<?)",
            ),
            seek("sp", Access::Index(link_index), &format!("({link_key}=?)")),
        ],
        ExplainedStatement::LinkHealthHeads => vec![
            seek(
                "dl",
                Access::Index(admitted_index),
                &format!("(admitting_segments=? AND {key}>? AND {key}<?)"),
            ),
            seek("da", Access::RowId, "(rowid=?)"),
            seek("dc", Access::RowId, "(rowid=?)"),
            seek("dp", Access::Index(path_index), "(path_key=?)"),
        ],
        // The exact-path count and the exact-path-first step each seek the
        // documents at a key's exact path by equality on the class index.
        ExplainedStatement::LinkHealthTotals => vec![
            seek("dl", Access::Index(admitted_index), &range),
            seek(
                "de",
                Access::Index("documents_suffix_key"),
                "(suffix_key=?)",
            ),
            seek("dp", Access::Index(path_index), "(path_key=?)"),
        ],
        // The exact-path-first step seeks the documents at a key's exact
        // path by equality on the suffix-key index.
        ExplainedStatement::LinkHealthSuffixes => vec![
            seek("dl", Access::Index(admitted_index), &range),
            seek(
                "de",
                Access::Index("documents_suffix_key"),
                "(suffix_key=?)",
            ),
        ],
        ExplainedStatement::LinkHealthAnchors => vec![
            seek("l", Access::RowId, "(rowid=?)"),
            seek(
                "hr",
                Access::Index("headings_document_reading"),
                "(document=? AND reading=?)",
            ),
            seek(
                "hs",
                Access::Index("headings_document_slug"),
                "(document=? AND slug=?)",
            ),
            seek(
                "hb",
                Access::Index("blocks_document_block_id"),
                "(document=? AND block_id=?)",
            ),
        ],
        other => panic!("{other:?} is no judgment statement"),
    }
}

/// Judge `statement`'s plan on `store`: nothing read end to end, and each
/// seek it is held to present on every step that reads its alias.
fn judge(store: &mut Store, statement: ExplainedStatement<'_>, order: StoredPathOrder) {
    let read = plan(
        store
            .begin_request()
            .emitted_plan(statement)
            .expect("a query plan"),
    );
    read.assert_no_full_scan();
    // A head is the ladder's first rows of a class, each admitting count's
    // run read in ladder order off the admitted-key index, which carries the
    // key and the path both, so a run sorts nothing and its limit stops the
    // read; the runs' union, at most a head per count the key's segments
    // reach, is sorted whole and cut again: a sort bounded by the key's
    // segments, never the class. Candidate suffix probes use the same merge
    // with a bound of two. A page of a write's links is read in path
    // order off the change-feed index, which does not say a path is one
    // document's, so SQLite sorts the links of one path by ordinal — the one
    // document's own links, which the changeset wrote — and the page's limit
    // stops the read. Every other statement sorts nothing.
    let sorted = match statement {
        ExplainedStatement::LinkHealthHeads | ExplainedStatement::LinkHealthSuffixes => {
            Some("USE TEMP B-TREE FOR ORDER BY")
        }
        ExplainedStatement::LinkHealthWrittenLinks => {
            Some("USE TEMP B-TREE FOR LAST TERM OF ORDER BY")
        }
        _ => None,
    };
    if let Some(sorted) = sorted {
        let sorts: Vec<&str> = read
            .rows()
            .iter()
            .map(|row| row.detail.as_str())
            .filter(|detail| detail.contains("TEMP B-TREE"))
            .collect();
        assert_eq!(
            sorts,
            vec![
                sorted;
                if matches!(statement, ExplainedStatement::LinkHealthHeads) {
                    2
                } else if matches!(statement, ExplainedStatement::LinkHealthSuffixes) {
                    8
                } else {
                    1
                }
            ],
            "{statement:?} under {order:?}: {:?}",
            read.rows()
        );
    } else {
        read.assert_no_temp_btree();
    }
    // A page is cut in its driver, which runs once as the outer loop: a
    // co-routine yielding a page, never a table of the whole selection.
    let paged = matches!(
        statement,
        ExplainedStatement::LinkHealthClassLinks
            | ExplainedStatement::LinkHealthPathLinks
            | ExplainedStatement::LinkHealthWrittenLinks
    );
    let details: Vec<&str> = read.rows().iter().map(|row| row.detail.as_str()).collect();
    assert!(
        !details
            .iter()
            .any(|detail| detail.starts_with("MATERIALIZE"))
            && details.contains(&"CO-ROUTINE p") == paged,
        "{statement:?} under {order:?}: {details:?}"
    );
    for Seek {
        alias,
        access,
        constraint,
    } in seeks(statement, order)
    {
        let steps: Vec<&PlanRow> = read
            .rows()
            .iter()
            .filter(|row| row.searches() == Some(alias) || row.scans() == Some(alias))
            .collect();
        assert!(
            !steps.is_empty()
                && steps.iter().all(|row| {
                    row.searches() == Some(alias)
                        && row.access() == Some(access)
                        && row.constraint() == Some(constraint.as_str())
                }),
            "{statement:?} under {order:?}: `{alias}` is not a seek through {access:?} on \
             {constraint}: {:?}\nemitted SQL: {}",
            read.rows(),
            read.sql()
        );
    }
}

/// A head seeks each candidate kind once before its bounded heads are merged.
#[test]
fn a_link_health_head_seeks_each_candidate_kind_once() {
    for order in [Sensitive, Folding] {
        let scratch = Scratch::new(&format!("health-head-seeks-once-{order:?}"));
        let mut store = scratch.open_under(order);
        let read = plan(
            store
                .begin_request()
                .emitted_plan(ExplainedStatement::LinkHealthHeads)
                .expect("a query plan"),
        );
        for (alias, index) in [
            ("dl", "documents_admitted_suffix_key"),
            ("fl", "vault_files_admitted_suffix_key"),
        ] {
            let seeks = read
                .rows()
                .iter()
                .filter(|row| {
                    row.searches() == Some(alias) && row.access() == Some(Access::Index(index))
                })
                .count();
            assert_eq!(seeks, 1, "{order:?}: {index}: {:?}", read.rows());
        }
    }
}

/// Link-health judgment uses indexed comparison-key seeks on every root.
/// Exact holding-document reads retain original path identity.
#[test]
fn the_link_health_judgment_seeks_every_row_it_reads() {
    for order in [Sensitive, Folding] {
        let scratch = Scratch::new(&format!("health-plans-{order:?}"));
        let mut store = scratch.open_under(order);
        for statement in LINK_HEALTH {
            judge(&mut store, statement, order);
        }
        for statement in LINK_HEALTH {
            for Seek { access, .. } in seeks(statement, order) {
                let Access::Index(index) = access else {
                    continue;
                };
                let scratch =
                    Scratch::new(&format!("health-plans-{order:?}-{statement:?}-{index}"));
                let mut store = scratch.open_under(order);
                induced_failure::execute_out_of_band(&mut store, &format!("DROP INDEX {index}"))
                    .unwrap_or_else(|problem| panic!("dropping {index}: {problem}"));
                failure_of(
                    &format!("{statement:?} under {order:?} without {index}"),
                    || judge(&mut store, statement, order),
                );
            }
        }
    }
}

/// A non-Markdown target keeps its extension when naming a document.
#[test]
fn a_vault_file_target_never_drops_its_extension() {
    for order in [Sensitive, Folding] {
        let documents = [
            derived("photo.md", "photo\n"),
            derived("holder.md", "[[photo.png]]\n"),
        ];
        let without = Judging::new("health-photo-without", order, &documents);
        let links = without.links("holder.md");
        assert_eq!(links[0].health(), LinkHealth::Broken);
        assert!(row_head(&links[0]).is_empty());
        let mut documents = documents.to_vec();
        documents.push(derived("photo.png.md", "named stem\n"));
        let with = Judging::new("health-photo-with", order, &documents);
        let links = with.links("holder.md");
        assert_eq!(links[0].health(), LinkHealth::Healthy);
        assert_eq!(
            row_head(&links[0]),
            vec![("photo.png.md".to_string(), "photo.png".to_string())]
        );
    }
}

/// Ambiguous Unicode spellings retain the hint that enumerates their class.
#[test]
fn ambiguous_unicode_targets_keep_their_enumeration_hint() {
    for order in [Sensitive, Folding] {
        let judging = Judging::new(
            "health-unicode-hints",
            order,
            &[
                derived("Été.md", "one\n"),
                derived("été.md", "two\n"),
                derived("ΟΣ.md", "one\n"),
                derived("Οσ.md", "two\n"),
                derived("holder.md", "[[ÉTÉ]]\n\n[[e\u{301}te\u{301}]]\n\n[[ΟΣ]]\n"),
            ],
        );
        let rows = judging.validated();
        for (written, address) in [("ÉTÉ", "été"), ("e\u{301}te\u{301}", "été"), ("ΟΣ", "οσ")]
        {
            let row = rows
                .iter()
                .find(|row| row.target.as_deref() == Some(written))
                .expect("an ambiguous finding");
            assert_eq!(
                row.hint,
                Some(Hint::resolves(
                    ResolutionTarget::new(address).expect("a target")
                )),
                "{order:?}: {written}"
            );
        }
    }
}
