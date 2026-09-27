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
    AnchorReadings, BlockFact, CANDIDATE_HEAD, ContentModel, DocumentFacts, ExplainedStatement,
    FindingFacts, HeadingFact, LinkAnchor, LinkFact, LinkFamily, Request, Snapshot, SnapshotReader,
    Span, Store, StoredPathOrder, SuffixKey, Validation, induced_failure,
};
use norn_testkit::explain::{Access, PlanRow, QueryPlan};
use norn_wire::{
    Column, FindParams, FindingKind, FindingRow, Hint, LinkHealth, LinkRow, Pattern, Predicate,
    ResolutionTarget, Severity, ValidateParams, VaultAddress, VaultName,
};

use crate::common::{Scratch, path, write_documents};
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
fn derived(at: &str, body: &str) -> DocumentFacts {
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
            span: span(link.span),
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
fn body_of(lines: &[&str]) -> String {
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
        write_documents(&mut store.begin_request(), documents);
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
        derived("v1.2.md", "dotted\n"),
        derived("v1.md", "undotted\n"),
    ];
    all.extend((1..=7).map(|at| derived(&format!("crowd/{at}/crowd.md"), "crowd\n")));
    all
}

/// Each link `src/a.md` holds, in order, beside the kind of finding it raises
/// on a root that tells spellings apart and on one that folds ASCII case.
const SOURCE: &[(&str, Option<FindingKind>, Option<FindingKind>)] = {
    use FindingKind::{Ambiguous, Broken, MissingAnchor};
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
        // Never reduced, and no extension: a document target.
        ("[bare](../x/y)", Some(Broken), Some(Broken)),
        ("[web](https://example.com)", None, None),
        // An attachment resolving to nothing is not judged.
        ("[[picture.png]]", None, None),
        ("[self](./a.md)", None, None),
        ("[[report.md]]", Some(Ambiguous), Some(Ambiguous)),
        // Its own document, which holds no such heading.
        ("[[#Heading]]", Some(MissingAnchor), Some(MissingAnchor)),
        ("[img](img/pic.png)", None, None),
        // An attachment resolving to a document is judged by it.
        ("[[photo.png]]", None, None),
        ("[[crowd]]", Some(Ambiguous), Some(Ambiguous)),
        ("[[archive/glossary]]", None, None),
        ("[[vault://v1.2]]", Some(Ambiguous), Some(Ambiguous)),
        ("[gone](missing.md)", Some(Broken), Some(Broken)),
        ("[[GLOSSARY]]", Some(Broken), None),
        ("![[dup]]", Some(Ambiguous), Some(Ambiguous)),
        ("![[missing]]", Some(Broken), Some(Broken)),
        ("[[Glossary#heading]]", Some(Broken), None),
        ("[frag](../notes/glossary.md#Heading)", None, None),
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
/// invert without this failing: an attachment resolving to nothing raises
/// nothing, and a document target resolving to nothing is broken.
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
                        matches!(kind, None | Some(FindingKind::MissingAnchor)),
                        "{context}: {kind:?}"
                    ),
                    LinkHealth::NotJudged => assert_eq!(kind, None, "{context}"),
                    other => panic!("{context}: an unknown health {other:?}"),
                }
                let Some(finding) = finding else {
                    continue;
                };
                assert_eq!(finding.severity, Severity::Warning, "{context}");
                assert_eq!(
                    finding.span,
                    Some(stored.links[ordinal].fact.span),
                    "{context}"
                );
                match finding.kind {
                    FindingKind::Broken => {
                        assert_eq!(
                            (finding_head(finding), finding.candidates_total),
                            (Vec::new(), 0),
                            "{context}"
                        );
                    }
                    FindingKind::Ambiguous | FindingKind::MissingAnchor => {
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
                Some(FindingKind::MissingAnchor.as_str())
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
            "[[vault://v1.2]]",
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
    {
        let mut request = judging.request();
        for finding in &findings {
            request
                .record_finding(finding)
                .unwrap_or_else(|refusal| panic!("recording {finding:?}: {refusal}"));
        }
    }

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
        read("vault://v1.2"),
        (
            FindingKind::Ambiguous,
            vec!["v1.2.md".to_string(), "v1.md".to_string()],
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

/// **A dotted leaf totals both of its reductions.** `[[v1.2]]` names every
/// `**/v1.2.md` and every `**/v1.md`, so over two of the first and one of the
/// second it is ambiguous among three, its head merged across the two classes
/// in the resolution ladder's order, as the links column reads it, and it is
/// keyed by both classes.
#[test]
fn a_dotted_leaf_totals_both_reductions() {
    for order in [Sensitive, Folding] {
        let mut judging = Judging::new(
            &format!("health-dotted-{order:?}"),
            order,
            &[
                derived("a/v1.2.md", "a\n"),
                derived("b/v1.2.md", "b\n"),
                derived("c/v1.md", "c\n"),
                derived("h.md", &body_of(&["[[v1.2]]"])),
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
            ["v1.2/", "v1/"],
            "{order:?}"
        );
    }
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

/// **A finding's keys are its link's keys.** On either root, each finding is
/// keyed by exactly the keys the link index holds its link under, in the key
/// space the root probes: a suffix address's classes, one per reduction; a
/// path's exact path, or each path a rooted name's reductions spell; and none
/// for a link that names no vault path. The store files each of them.
#[test]
fn a_findings_keys_are_its_links_keys() {
    for order in [Sensitive, Folding] {
        let space = SuffixKey::under(order);
        let mut judging = Judging::new(
            &format!("health-keys-{order:?}"),
            order,
            &[
                derived("a/V1.2.md", "a\n"),
                derived("b/V1.md", "b\n"),
                derived("V1.2.md", "rooted\n"),
                derived("V1.md", "rooted\n"),
                derived(
                    "Src/h.md",
                    &body_of(&[
                        "[[Missing/Thing]]",
                        "[[V1.2]]",
                        "[g](Gone/Dir.md)",
                        "[[vault://V1.2]]",
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
                        SuffixKey::Raw => key.key.clone(),
                        SuffixKey::Folded => key.folded_key.clone(),
                    };
                    (spelled, key.segments.is_some())
                })
                .collect();
            let expected_classes: BTreeSet<String> = keys
                .iter()
                .filter(|(_, class)| *class)
                .map(|(key, _)| key.clone())
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
        assert_eq!((classes, paths), (2, 2), "{order:?}: the fixture's reach");
        assert!(
            findings[&4].class_keys.is_empty() && findings[&4].path_keys.is_empty(),
            "{order:?}: a link naming no vault path is keyed by nothing"
        );
        let mut request = judging.request();
        for finding in findings.values() {
            request
                .record_finding(finding)
                .unwrap_or_else(|refusal| panic!("{order:?}: recording {finding:?}: {refusal}"));
        }
    }
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
pub(crate) const LINK_HEALTH: [ExplainedStatement<'static>; 5] = [
    ExplainedStatement::LinkHealthLinks,
    ExplainedStatement::LinkHealthHeads,
    ExplainedStatement::LinkHealthTotals,
    ExplainedStatement::LinkHealthSuffixes,
    ExplainedStatement::LinkHealthAnchors,
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
    let (class_index, key, path_index) = match order {
        Sensitive => ("documents_suffix_key", "suffix_key", "documents_path"),
        Folding => (
            "documents_folded_suffix_key",
            "folded_suffix_key",
            "documents_path_nocase",
        ),
    };
    let range = format!("({key}>? AND {key}<?)");
    match statement {
        ExplainedStatement::LinkHealthLinks => vec![
            seek("d", Access::Index("documents_path"), "(path=?)"),
            seek("l", Access::Index("links_document_ordinal"), "(document=?)"),
            seek("k", Access::Index("link_keys_link"), "(link=?)"),
        ],
        ExplainedStatement::LinkHealthHeads | ExplainedStatement::LinkHealthTotals => vec![
            seek("dl", Access::Index(class_index), &range),
            seek("dp", Access::Index(path_index), "(path=?)"),
        ],
        ExplainedStatement::LinkHealthSuffixes => {
            vec![seek("dr", Access::Index(class_index), &range)]
        }
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

/// **Every statement the judgment runs seeks what it reads.** The links a set
/// of documents holds are an equality seek of `documents_path` per document,
/// of `links_document_ordinal` per document row, and of `link_keys_link` per
/// link; each distinct key's head and total a range seek of the suffix key the
/// root probes, or a seek of the path at a path key; each candidate's
/// suffixes a range seek of the same suffix key; and each anchor a link
/// reached by its row id and a handful of equality seeks into the one
/// document it names. Nothing is read end to end.
///
/// Controls: each index a statement seeks dropped on a store of its own, and
/// the bar fails for that statement.
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
