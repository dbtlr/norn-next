//! The get builder: which document a target names under each path order, the
//! record it answers through a find's hydration, the section and the block an
//! anchor names through the document reader, the pages of one collection, and
//! the plan and work bars every statement it emits is judged by.
//!
//! Every store here is opened under the path order a case names, so a case
//! proves the same thing on a case-sensitive host as on a folding one.

use std::cell::RefCell;
use std::ops::Range;
use std::sync::Arc;

use norn_store::{
    BODY_ROW_CEILING, BlockFact, ContentModel, DocumentFacts, DocumentText, FindStatement,
    FindingFacts, GET_STATEMENTS, GetPlan, GetStatement, GetWork, Gotten, HeadingFact, LinkFact,
    LinkFamily, NESTED_ROW_CEILING, Nested, PageRefusal, ReadStatement, SectionAt, Snapshot,
    SnapshotReader, Store, StoredPathOrder, TagFact, TagSource, TargetAmbiguity, induced_failure,
};
use norn_testkit::explain::{Access, PlanRow, QueryPlan};
use norn_text::{BodyScan, Heading, SectionAddress, SourceSpan};
use norn_wire::{
    AnswerShape, Candidate, CollectionPage, CollectionSelector, Column, Cursor, CursorKey,
    Direction, DocumentRow, FindParams, FindingKind, GetParams, GetReport, Hint, PagedRows,
    Pattern, Predicate, RequestPart, ResolutionTarget, Severity, Sort, SortKey, Unsatisfied,
    VaultAddress, VaultName,
};

use crate::common::{Scratch, document, span, unread_block, write_documents};
use crate::find::{failure_of, map, rows_of, string};

use StoredPathOrder::{AsciiCaseInsensitive as Folding, Sensitive};

// ---- fixtures ----

/// The fingerprint the suite's schema is pinned under.
const SCHEMA: &str = "get-schema";

fn declared() -> ContentModel {
    ContentModel::under(SCHEMA).declare("status")
}

/// The declaration, keeping the places `globs` name out of every class.
fn ignoring(globs: &[&str]) -> ContentModel {
    globs.iter().fold(declared(), |declared, glob| {
        declared.declare_ambiguity_ignore(Pattern::parse(glob).expect("a glob"))
    })
}

/// The document reader this suite hands a get, mirroring the host's reader,
/// which is private to the host, across the test boundary: `norn-text`'s one
/// section resolver, taking the first heading an anchor matches, and its one
/// block reading.
struct Text;

impl DocumentText for Text {
    fn section(&self, headings: &[HeadingFact], body: &str, anchor: &str) -> Option<SectionAt> {
        let headings: Vec<Heading> = headings.iter().map(text_heading).collect();
        let span =
            norn_text::resolve_section(&headings, body, SectionAddress::first(anchor)).ok()?;
        Some(SectionAt {
            heading: span.heading,
            body: span.body_start..span.end,
        })
    }

    fn block(&self, body: &str, marker: usize) -> Range<usize> {
        BodyScan::new(body).block_extent(marker)
    }
}

fn text_heading(heading: &HeadingFact) -> Heading {
    let at = |value: u64| usize::try_from(value).expect("an offset fits usize");
    Heading {
        level: heading.level,
        text: heading.text.clone(),
        slug: heading.slug.clone(),
        span: SourceSpan {
            line: at(heading.span.line),
            column: at(heading.span.column),
            byte_offset: at(heading.span.byte_offset),
        },
        body_offset: at(heading.body_offset),
        inside_container: heading.inside_container,
    }
}

/// A document whose headings and block definitions are the ones `norn-text`
/// reads out of `body`, as a host derives them.
fn parsed(at: &str, body: &str) -> DocumentFacts {
    let scan = BodyScan::new(body);
    let mut facts = document(at, &format!("hash-{at}"), body);
    let wide = |value: usize| value as u64;
    facts.headings = scan
        .headings()
        .iter()
        .map(|heading| HeadingFact {
            level: heading.level,
            text: heading.text.clone(),
            slug: heading.slug.clone(),
            span: span(
                wide(heading.span.line),
                wide(heading.span.column),
                wide(heading.span.byte_offset),
            ),
            body_offset: wide(heading.body_offset),
            inside_container: heading.inside_container,
        })
        .collect();
    facts.blocks = scan
        .block_ids()
        .iter()
        .map(|block| BlockFact {
            block_id: block.id.clone(),
            span: Some(span(
                wide(block.span.line),
                wide(block.span.column),
                wide(block.span.byte_offset),
            )),
        })
        .collect();
    facts
}

/// A store over a root proven to have one case behaviour, holding `documents`,
/// its schema pinned, and a read handle over it.
struct Vault {
    _scratch: Scratch,
    store: Store,
    reader: Arc<SnapshotReader>,
}

impl Vault {
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
        Vault {
            _scratch: scratch,
            store,
            reader,
        }
    }

    /// A vault holding a bodiless document at each of `paths`.
    fn at(label: &str, order: StoredPathOrder, paths: &[&str]) -> Self {
        let documents: Vec<DocumentFacts> = paths
            .iter()
            .map(|at| document(at, &format!("hash-{at}"), "a body\n"))
            .collect();
        Self::holding(label, order, &documents)
    }

    fn snapshot(&self) -> Snapshot {
        let snapshot = self
            .reader
            .try_take()
            .expect("a handle nothing is reading holds its connection")
            .establish()
            .expect("a snapshot");
        assert_eq!(snapshot.path_order(), self.store.path_order());
        snapshot
    }

    fn get_under(&self, params: &GetParams, declared: &ContentModel) -> Gotten {
        self.snapshot()
            .get(params, declared, &Text)
            .unwrap_or_else(|refusal| panic!("a get of {params:?}: {refusal}"))
    }

    fn get(&self, params: &GetParams) -> Gotten {
        self.get_under(params, &declared())
    }

    fn refusal_under(&self, params: &GetParams, declared: &ContentModel) -> PageRefusal {
        self.snapshot()
            .get(params, declared, &Text)
            .expect_err("a refused get")
    }

    fn refusal(&self, params: &GetParams) -> PageRefusal {
        self.refusal_under(params, &declared())
    }

    fn plans(&self, params: &GetParams) -> Vec<GetPlan> {
        self.snapshot()
            .get_plans(params, &declared(), &Text)
            .unwrap_or_else(|refusal| panic!("the plans of {params:?}: {refusal}"))
    }

    /// The row a find of the document at `at` answers, projecting `columns`.
    fn found(&self, at: &str, columns: Vec<Column>) -> DocumentRow {
        let params = FindParams::new(address())
            .with_predicates([Predicate::path(at)])
            .with_columns(columns);
        let mut found = self
            .snapshot()
            .find(&params, &declared())
            .expect("a find")
            .rows;
        assert_eq!(found.len(), 1, "a find of `{at}`");
        found.remove(0)
    }

    fn stand(&mut self, finding: &FindingFacts) {
        self.store
            .begin_request()
            .record_finding(finding)
            .expect("recording a finding");
    }

    fn drop_index(&mut self, index: &str) {
        induced_failure::execute_out_of_band(&mut self.store, &format!("DROP INDEX {index}"))
            .unwrap_or_else(|problem| panic!("dropping {index}: {problem}"));
    }
}

fn address() -> VaultAddress {
    VaultAddress::name(VaultName::new("notes").expect("a vault name"))
}

fn target(text: &str) -> ResolutionTarget {
    ResolutionTarget::new(text).expect("a target")
}

fn getting(text: &str) -> GetParams {
    GetParams::new(address(), target(text))
}

/// The record a get answered.
fn record(gotten: &Gotten) -> &DocumentRow {
    match &gotten.report {
        GetReport::Record { document, .. } => document,
        other => panic!("a get answered {other:?}, not a record"),
    }
}

/// The path of the one document a get of `text` names.
fn named(vault: &Vault, text: &str) -> String {
    record(&vault.get(&getting(text).with_columns([Column::path()])))
        .path
        .as_str()
        .to_string()
}

// ---- which document a target names ----

/// **A target naming one document answers it, under either path order**, by
/// a suffix of any length and, where the root folds ASCII case, by any
/// spelling of it.
#[test]
fn a_target_naming_one_document_answers_it_under_either_order() {
    for order in [Sensitive, Folding] {
        let vault = Vault::at(
            &format!("get-unique-{order:?}"),
            order,
            &["notes/glossary.md", "notes/other.md", "docs/index.md"],
        );
        for spelled in ["glossary", "notes/glossary", "notes/glossary.md"] {
            assert_eq!(named(&vault, spelled), "notes/glossary.md", "{order:?}");
        }
        let folded = getting("Notes/GLOSSARY").with_columns([Column::path()]);
        match order {
            Folding => assert_eq!(
                record(&vault.get(&folded)).path.as_str(),
                "notes/glossary.md"
            ),
            Sensitive => assert_eq!(
                vault.refusal(&folded),
                PageRefusal::UnknownTarget {
                    target: target("Notes/GLOSSARY")
                }
            ),
        }
    }
}

/// **A target naming several documents refuses with the head of its class**:
/// the first five in the resolution ladder's order, each named by its minimal
/// disambiguating suffix, how many there were, and the hint naming the target
/// whose `find` resolves them all. The refusal echoes the target as the
/// request named it, anchor included; the hint names its address alone.
#[test]
fn a_target_naming_several_documents_refuses_with_the_bounded_head_and_the_hint() {
    // Written last to first, so the head's order is the ladder's and not the
    // order the rows were written in.
    let paths: Vec<String> = (1..=7)
        .rev()
        .map(|at| format!("d{at}/glossary.md"))
        .collect();
    let paths: Vec<&str> = paths.iter().map(String::as_str).collect();
    for order in [Sensitive, Folding] {
        let vault = Vault::at(&format!("get-ambiguous-{order:?}"), order, &paths);
        let PageRefusal::AmbiguousTarget(ambiguity) = vault.refusal(&getting("glossary#Terms"))
        else {
            panic!("an ambiguous target answered");
        };
        let TargetAmbiguity {
            target: named,
            head,
            hint,
        } = *ambiguity;
        assert_eq!(named, target("glossary#Terms"));
        assert_eq!(hint, Hint::resolves(target("glossary")));
        assert_eq!(head.total(), 7);
        let expected: Vec<Candidate> = (1..=5)
            .map(|at| {
                Candidate::new(
                    norn_wire::DocumentPath::new(format!("d{at}/glossary.md")).expect("a path"),
                    format!("d{at}/glossary"),
                )
            })
            .collect();
        assert_eq!(head.candidates(), expected.as_slice(), "{order:?}");
    }

    // A dotted target reduces two ways, so a candidate whose stem is dotted
    // is named apart by a suffix that reaches past the reduction the other
    // candidate's stem is.
    let vault = Vault::at(
        "get-ambiguous-dotted",
        Sensitive,
        &["v1.md", "two/v1.2.md", "one/v1.2.md"],
    );
    let PageRefusal::AmbiguousTarget(ambiguity) = vault.refusal(&getting("v1.2")) else {
        panic!("a dotted target naming three documents answered");
    };
    let head = ambiguity.head;
    let named: Vec<(&str, &str)> = head
        .candidates()
        .iter()
        .map(|candidate| (candidate.path.as_str(), candidate.suffix.as_str()))
        .collect();
    assert_eq!(
        named,
        [
            ("one/v1.2.md", "one/v1.2"),
            ("two/v1.2.md", "two/v1.2"),
            ("v1.md", "v1")
        ]
    );
    assert_eq!(head.total(), 3);
}

/// **On a root that folds ASCII case every folded spelling is one class**, so
/// an exact-case spelling takes no precedence; a root that tells spellings
/// apart answers each spelling alone.
#[test]
fn a_folding_root_resolves_every_case_spelling_as_one_class() {
    let paths = ["a/Glossary.md", "b/glossary.md"];
    let sensitive = Vault::at("get-case-sensitive", Sensitive, &paths);
    assert_eq!(named(&sensitive, "glossary"), "b/glossary.md");
    assert_eq!(named(&sensitive, "Glossary"), "a/Glossary.md");

    let folding = Vault::at("get-case-folding", Folding, &paths);
    for spelled in ["glossary", "Glossary", "GLOSSARY"] {
        let PageRefusal::AmbiguousTarget(ambiguity) = folding.refusal(&getting(spelled)) else {
            panic!("`{spelled}` answered one document on a folding root");
        };
        let head = ambiguity.head;
        let named: Vec<(&str, &str)> = head
            .candidates()
            .iter()
            .map(|candidate| (candidate.path.as_str(), candidate.suffix.as_str()))
            .collect();
        assert_eq!(
            named,
            [
                ("a/Glossary.md", "a/Glossary"),
                ("b/glossary.md", "b/glossary")
            ]
        );
    }
}

/// **A get and a find resolving one target read one class**: the documents a
/// get's refusal heads are documents a find resolving the same target answers,
/// and its total is how many that find answers, under either order and the
/// same ignored places.
#[test]
fn a_get_and_a_find_resolving_one_target_read_one_class() {
    for (order, class) in [(Sensitive, 2), (Folding, 3)] {
        let vault = Vault::at(
            &format!("get-one-class-{order:?}"),
            order,
            &[
                "a/Glossary.md",
                "b/glossary.md",
                "c/glossary.md",
                "archive/glossary.md",
                "notes/other.md",
            ],
        );
        let archived = ignoring(&["archive/**"]);
        let found: Vec<String> = vault
            .snapshot()
            .find(
                &FindParams::new(address())
                    .with_predicates([Predicate::resolves(target("glossary"))]),
                &archived,
            )
            .expect("a find resolving the target")
            .rows
            .iter()
            .map(|row| row.path.as_str().to_string())
            .collect();
        let PageRefusal::AmbiguousTarget(ambiguity) =
            vault.refusal_under(&getting("glossary"), &archived)
        else {
            panic!("a target naming {class} documents answered");
        };
        assert_eq!(ambiguity.head.total(), found.len() as u64, "{order:?}");
        assert_eq!(found.len(), class, "{order:?}");
        let mut headed: Vec<String> = ambiguity
            .head
            .candidates()
            .iter()
            .map(|candidate| candidate.path.as_str().to_string())
            .collect();
        headed.sort();
        assert_eq!(headed, found, "{order:?}");
    }
}

/// **A target naming no document refuses as unknown**, under either order,
/// and so does a target that is no suffix address at all.
#[test]
fn a_target_naming_no_document_refuses_as_unknown() {
    for order in [Sensitive, Folding] {
        let vault = Vault::at(&format!("get-unknown-{order:?}"), order, &["notes/a.md"]);
        for spelled in ["missing", "other/a", "../a", "missing#Heading"] {
            assert_eq!(
                vault.refusal(&getting(spelled)),
                PageRefusal::UnknownTarget {
                    target: target(spelled)
                },
                "{order:?}"
            );
        }
    }
}

/// **A place the schema ignores stays out of the class a get resolves**,
/// unless the target reaches it.
#[test]
fn an_ignored_place_stays_out_of_the_class_a_get_resolves() {
    for order in [Sensitive, Folding] {
        let vault = Vault::at(
            &format!("get-ignored-{order:?}"),
            order,
            &["archive/glossary.md", "notes/glossary.md"],
        );
        let archived = ignoring(&["archive/**"]);
        let path = |spelled: &str| {
            record(&vault.get_under(&getting(spelled).with_columns([Column::path()]), &archived))
                .path
                .as_str()
                .to_string()
        };
        assert_eq!(path("glossary"), "notes/glossary.md");
        assert_eq!(path("archive/glossary"), "archive/glossary.md");
        assert!(matches!(
            vault.refusal(&getting("glossary")),
            PageRefusal::AmbiguousTarget(_)
        ));
    }
}

// ---- the record ----

/// The body the record fixture's document holds.
const RECORD_BODY: &str = "intro ^top\n\n## Terms\nterm body\n";

/// A vault whose `notes/a.md` carries a field, a body, headings, a block, tags
/// and two findings, beside two other documents.
fn record_vault(label: &str, order: StoredPathOrder, bulk: usize) -> Vault {
    let mut a = parsed("notes/a.md", RECORD_BODY)
        .with_frontmatter(Some(map(vec![("status", string("open"))])), &declared());
    a.tags = ["draft", "idea"]
        .iter()
        .map(|name| TagFact {
            name: (*name).to_string(),
            source: TagSource::Body,
            span: None,
        })
        .collect();
    let mut documents = vec![
        a,
        document("notes/b.md", "hash-b", "b body\n"),
        document("docs/c.md", "hash-c", "c body\n"),
    ];
    documents.extend((0..bulk).map(|at| {
        document(
            &format!("bulk/{at:04}.md"),
            &format!("hash-bulk-{at}"),
            "a body\n",
        )
    }));
    let mut vault = Vault::holding(label, order, &documents);
    for (kind, tag) in [
        (FindingKind::UndeclaredTag, "draft"),
        (FindingKind::UndeclaredTag, "idea"),
    ] {
        let mut finding = unread_block("notes/a.md");
        finding.kind = kind;
        finding.severity = Severity::Warning;
        finding.target = Some(tag.to_string());
        finding.message = format!("`{tag}` is not a declared tag");
        finding.detail = None;
        vault.stand(&finding);
    }
    vault
}

/// **A record projects each column exactly as a find's row projects it**,
/// through the same hydration, and carries no column it was not asked for;
/// **a record asking for none is the whole row** a find projecting every
/// column answers.
#[test]
fn a_record_projects_each_column_as_a_find_row_projects_it() {
    let vault = record_vault("get-record-columns", Sensitive, 0);
    for column in [
        Column::path(),
        Column::field("status"),
        Column::fields(),
        Column::body(),
        Column::tags(),
        Column::headings(),
        Column::blocks(),
        Column::links(),
        Column::findings(),
    ] {
        let gotten = vault.get(&getting("notes/a").with_columns([column.clone()]));
        assert!(gotten.unsatisfied.is_empty());
        assert_eq!(
            record(&gotten),
            &vault.found("notes/a.md", vec![column.clone()]),
            "{column:?}"
        );
    }
    let whole = vault.get(&getting("a"));
    let row = record(&whole);
    assert_eq!(
        row,
        &vault.found(
            "notes/a.md",
            vec![
                Column::fields(),
                Column::body(),
                Column::tags(),
                Column::headings(),
                Column::blocks(),
                Column::links(),
                Column::findings(),
            ]
        )
    );
    assert_eq!(
        row.findings.as_ref().map(|findings| findings.total),
        Some(2)
    );
    assert_eq!(row.tags.as_ref().map(|tags| tags.total), Some(2));
    assert_eq!(
        row.body.as_ref().map(|body| body.text().to_string()),
        Some(RECORD_BODY.to_string())
    );
}

/// **A whole record holds each nested collection and the body to the per-row
/// ceiling**, reporting the true total and length in band.
#[test]
fn a_whole_record_holds_each_collection_and_the_body_to_the_row_ceiling() {
    let body = "x".repeat(BODY_ROW_CEILING + 100);
    let mut long = document("long.md", "hash-long", &body);
    long.tags = (0..NESTED_ROW_CEILING + 44)
        .map(|at| TagFact {
            name: format!("tag-{at:04}"),
            source: TagSource::Body,
            span: None,
        })
        .collect();
    let vault = Vault::holding("get-record-ceiling", Sensitive, &[long]);
    let whole = vault.get(&getting("long"));
    let row = record(&whole);
    let tags = row.tags.as_ref().expect("the tags");
    assert_eq!(tags.items.len(), NESTED_ROW_CEILING);
    assert_eq!(tags.total, (NESTED_ROW_CEILING + 44) as u64);
    let text = row.body.as_ref().expect("the body");
    assert_eq!(text.text().len(), BODY_ROW_CEILING);
    assert_eq!(text.byte_length(), body.len() as u64);
}

/// **A projected key outside the field universe is reported as a find reports
/// it.**
#[test]
fn an_unknown_projected_key_is_reported() {
    let vault = record_vault("get-record-unknown", Sensitive, 0);
    let gotten = vault.get(&getting("notes/a").with_columns([Column::field("statsu")]));
    assert!(
        matches!(
            gotten.unsatisfied.as_slice(),
            [Unsatisfied::UnknownProjectionKey { key, .. }] if key == "statsu"
        ),
        "{:?}",
        gotten.unsatisfied
    );
}

// ---- sections and blocks ----

/// The body the section fixture's document holds.
const SECTIONS: &str = "intro\n\n# Guide\n\nguide text\n\n## Design  Notes\n\ndesign body\n\
### Deep Dive\ndeep body\n\n## Design Notes\n\nsecond design\n\n> ### Quoted\n> quoted body\n\n\
## Last\nlast body\n";

/// The section a get of `text` answers: its heading's text, level and slug,
/// and its body.
fn section(vault: &Vault, text: &str) -> (String, u8, String, String) {
    let gotten = vault.get(&getting(text));
    assert!(gotten.unsatisfied.is_empty(), "{:?}", gotten.unsatisfied);
    match gotten.report {
        GetReport::Section { heading, body, .. } => (
            heading.text,
            heading.level,
            heading.slug,
            body.text().to_string(),
        ),
        other => panic!("`{text}` answered {other:?}, not a section"),
    }
}

/// **A heading anchor answers the section it names**: matched by text with
/// case and whitespace folded, the first of several matching headings in
/// document order, the heading named exactly, and the body running to the
/// next heading at its level or above — its deeper headings included, a
/// heading inside a container read as any other, and the last section to the
/// end of the body. **A Markdown fragment matches a slug**, only where no
/// heading's text matches. **A section the document does not carry is
/// answered in band** beside the record of its path alone.
#[test]
fn a_heading_anchor_answers_the_section_it_names() {
    for order in [Sensitive, Folding] {
        let vault = Vault::holding(
            &format!("get-sections-{order:?}"),
            order,
            &[parsed("notes/guide.md", SECTIONS)],
        );
        assert_eq!(
            section(&vault, "guide#  design   NOTES "),
            (
                "Design  Notes".to_string(),
                2,
                "design--notes".to_string(),
                "\ndesign body\n### Deep Dive\ndeep body\n\n".to_string()
            ),
            "{order:?}"
        );
        assert_eq!(
            section(&vault, "guide#Deep Dive").3,
            "deep body\n\n",
            "a section ends at a heading above its level"
        );
        assert_eq!(
            section(&vault, "guide#design-notes"),
            (
                "Design Notes".to_string(),
                2,
                "design-notes".to_string(),
                "\nsecond design\n\n> ### Quoted\n> quoted body\n\n".to_string()
            ),
            "a slug answers the heading it is the slug of"
        );
        assert_eq!(section(&vault, "guide#quoted").3, "> quoted body\n\n");
        assert_eq!(section(&vault, "guide#Last").3, "last body\n");
        assert!(
            section(&vault, "guide#GUIDE").3.ends_with("last body\n"),
            "the only first-level section runs to the end of the body"
        );

        let missing = vault.get(&getting("guide#Nope"));
        assert_eq!(
            missing.report,
            GetReport::record(DocumentRow::new(
                norn_wire::DocumentPath::new("notes/guide.md").expect("a path")
            ))
        );
        assert_eq!(missing.unsatisfied, [Unsatisfied::missing_section("Nope")]);
    }
}

/// **A document broken by lone CR answers a section's bytes as written**: the
/// heading offsets the store holds were read off the normalized parse, which
/// moves no byte.
#[test]
fn a_lone_cr_document_answers_its_section_bytes_as_written() {
    let body = "intro\r\r## A\ra body\r\r## B\rb body\r";
    let vault = Vault::holding("get-section-cr", Sensitive, &[parsed("cr.md", body)]);
    assert_eq!(section(&vault, "cr#a").3, "a body\r\r");
    assert_eq!(section(&vault, "cr#b").3, "b body\r");
}

/// **A section ended by a heading inside a container ends at that heading's
/// line**: the container's prefix on it is the next section's, never a byte of
/// the section answered.
#[test]
fn a_section_ended_inside_a_container_answers_no_byte_of_its_prefix() {
    let body = "## A\ntext\n> ## Q\nquoted\n\nafter\n## B\nb\n";
    let listed = "## A\ntext\n- # L\n  item\n## B\nb\n";
    let vault = Vault::holding(
        "get-section-container",
        Sensitive,
        &[parsed("c.md", body), parsed("l.md", listed)],
    );
    assert_eq!(section(&vault, "c#A").3, "text\n");
    assert_eq!(section(&vault, "l#A").3, "text\n");
}

/// How many headings each crowd document of [`crowded_sections`] carries.
const CROWD_HEADINGS: usize = 12;

/// A vault holding the section fixture's document at `notes/guide.md` beside
/// `crowd` documents at `crowd/`, each carrying [`CROWD_HEADINGS`] headings,
/// among them headings whose text the fixture's anchors name.
fn crowded_sections(label: &str, order: StoredPathOrder, crowd: usize) -> Vault {
    let body = crowd_body();
    let mut documents = vec![parsed("notes/guide.md", SECTIONS)];
    documents.extend((0..crowd).map(|at| parsed(&format!("crowd/{at:04}.md"), &body)));
    Vault::holding(label, order, &documents)
}

/// The body each crowd document of [`crowded_sections`] holds.
fn crowd_body() -> String {
    ["Design Notes", "Last", "Nope"]
        .into_iter()
        .map(str::to_string)
        .chain((3..CROWD_HEADINGS).map(|at| format!("Topic {at}")))
        .map(|text| format!("## {text}\n{text} body\n\n"))
        .collect()
}

/// The suite's document reader, keeping every run of headings a get hands
/// its section lookup, one run per call.
#[derive(Default)]
struct Recording {
    handed: RefCell<Vec<Vec<HeadingFact>>>,
}

impl DocumentText for Recording {
    fn section(&self, headings: &[HeadingFact], body: &str, anchor: &str) -> Option<SectionAt> {
        self.handed.borrow_mut().push(headings.to_vec());
        Text.section(headings, body, anchor)
    }

    fn block(&self, body: &str, marker: usize) -> Range<usize> {
        Text.block(body, marker)
    }
}

impl Vault {
    /// A get of `params` read through a [`Recording`] reader, and the runs
    /// of headings the get handed it.
    fn get_recorded(&self, params: &GetParams) -> (Gotten, Vec<Vec<HeadingFact>>) {
        let reader = Recording::default();
        let gotten = self
            .snapshot()
            .get(params, &declared(), &reader)
            .unwrap_or_else(|refusal| panic!("a get of {params:?}: {refusal}"));
        (gotten, reader.handed.into_inner())
    }
}

/// **A heading anchor is matched in memory under its counter ceiling**: a
/// section lookup hands the document reader the named document's headings,
/// each once, in one call, and no heading of another document, and the get's
/// work counts every heading row the reader was handed. So the heading rows
/// the in-memory match runs over are exactly that document's headings, whether
/// the anchor names a heading by its text or its slug or names none, however
/// many documents the vault holds beside it, and however many of their
/// headings carry the anchor's text. The plans of the lookup count the rows
/// the lookup counts.
///
/// Controls: the counter follows the document named, reading a crowd
/// document's headings where the anchor names one; a get that looks up no
/// section, a record or a page of the headings collection, hands the reader
/// nothing and counts no heading row.
#[test]
fn a_heading_anchor_is_matched_in_memory_under_its_counter_ceiling() {
    let guide = parsed("notes/guide.md", SECTIONS).headings;
    assert!(
        guide.len() > 1,
        "the fixture's document carries several headings"
    );
    for order in [Sensitive, Folding] {
        let few = crowded_sections(&format!("get-anchor-few-{order:?}"), order, 1);
        let many = crowded_sections(&format!("get-anchor-many-{order:?}"), order, 40);
        let crowd = parsed("crowd/0000.md", &crowd_body()).headings;
        assert_eq!(crowd.len(), CROWD_HEADINGS);
        for (anchor, own) in [
            ("guide#design notes", &guide),
            ("guide#design-notes", &guide),
            ("guide#Nope", &guide),
            ("crowd/0000#Topic 7", &crowd),
        ] {
            let params = getting(anchor);
            for (vault, vault_is) in [(&few, "beside one"), (&many, "beside forty")] {
                let (gotten, handed) = vault.get_recorded(&params);
                assert_eq!(
                    handed.iter().map(Vec::len).collect::<Vec<_>>(),
                    [own.len()],
                    "`{anchor}` {vault_is} under {order:?}: the heading rows handed the reader, \
                     call by call, held to the ceiling of its document's headings"
                );
                assert_eq!(
                    handed,
                    std::slice::from_ref(own),
                    "`{anchor}` {vault_is} under {order:?} hands the reader its document's \
                     headings, each once, in one call"
                );
                assert_eq!(
                    gotten.work.anchor_headings,
                    own.len() as u64,
                    "`{anchor}` {vault_is} under {order:?} counts every heading row the reader \
                     was handed"
                );
                assert_eq!(
                    planned_work(vault, &params),
                    gotten.work,
                    "`{anchor}` {vault_is}"
                );
            }
        }
        for params in [
            getting("guide"),
            getting("guide")
                .with_collection(CollectionSelector::Headings)
                .with_limit(2),
        ] {
            let (gotten, handed) = many.get_recorded(&params);
            assert!(handed.is_empty(), "{params:?} handed the reader {handed:?}");
            assert_eq!(gotten.work.anchor_headings, 0, "{params:?}");
        }
    }
}

/// **A block anchor answers the block its identifier defines**: the leaf
/// block its marker trails, or the fenced block above a marker on the line
/// after its closing fence. **A block the document does not define is
/// answered in band** beside the record of its path alone.
#[test]
fn a_block_anchor_answers_its_block_or_reports_it_missing() {
    let body = "para one\npara two ^p1\n\n- item ^i1\n- other\n\n```\ncode\n```\n^c1\n";
    let vault = Vault::holding("get-blocks", Sensitive, &[parsed("notes/b.md", body)]);
    for (anchor, held) in [
        ("p1", "para one\npara two ^p1"),
        ("i1", "item ^i1"),
        ("c1", "```\ncode\n```"),
    ] {
        let gotten = vault.get(&getting(&format!("b#^{anchor}")));
        let GetReport::Block { block, body, .. } = gotten.report else {
            panic!("`^{anchor}` answered no block");
        };
        assert_eq!(block.id, anchor);
        assert_eq!(body.text(), held, "^{anchor}");
        assert!(block.span.is_some());
    }
    let missing = vault.get(&getting("b#^zz"));
    assert_eq!(
        missing.report,
        GetReport::record(DocumentRow::new(
            norn_wire::DocumentPath::new("notes/b.md").expect("a path")
        ))
    );
    assert_eq!(missing.unsatisfied, [Unsatisfied::missing_block("zz")]);
}

// ---- collection pages ----

/// How many rows each collection the paging fixture's document holds.
const HELD: usize = 5;

/// A vault whose `paged.md` holds [`HELD`] rows of every collection, beside
/// two documents the target `twin` names and `bulk` more documents.
fn paged_vault(label: &str, bulk: usize) -> Vault {
    let body: String = (0..HELD)
        .map(|at| format!("## Heading {at}\npara {at} ^b{at}\n\n"))
        .collect();
    let mut paged = parsed("paged.md", &body);
    paged.links = (0..HELD)
        .map(|at| LinkFact {
            family: if at % 2 == 0 {
                LinkFamily::Wikilink
            } else {
                LinkFamily::Markdown
            },
            embed: false,
            protocol: None,
            target: format!("target-{at}"),
            title: None,
            anchor: (at == 1).then(|| "Heading".to_string()),
            block_ref: (at == 2).then(|| "b0".to_string()),
            span: span(1, 1, at as u64),
        })
        .collect();
    paged.tags = (0..HELD)
        .map(|at| TagFact {
            name: format!("tag-{at}"),
            source: TagSource::Body,
            span: None,
        })
        .collect();
    let mut documents = vec![
        paged,
        document("other.md", "hash-other", "o\n"),
        document("a/twin.md", "hash-twin-a", "a\n"),
        document("b/twin.md", "hash-twin-b", "b\n"),
    ];
    documents.extend((0..bulk).map(|at| {
        document(
            &format!("bulk/{at:04}.md"),
            &format!("hash-bulk-{at}"),
            "a body\n",
        )
    }));
    let mut vault = Vault::holding(label, Sensitive, &documents);
    for at in 0..HELD {
        let mut finding = unread_block("paged.md");
        finding.kind = if at % 2 == 0 {
            FindingKind::UndeclaredTag
        } else {
            FindingKind::FrontmatterUnreadable
        };
        finding.target = Some(format!("finding-{at}"));
        vault.stand(&finding);
    }
    let mut elsewhere = unread_block("other.md");
    elsewhere.target = Some("elsewhere".to_string());
    vault.stand(&elsewhere);
    vault
}

/// Every collection a get pages.
const SELECTORS: [CollectionSelector; 5] = [
    CollectionSelector::Links,
    CollectionSelector::Headings,
    CollectionSelector::Blocks,
    CollectionSelector::Tags,
    CollectionSelector::Findings,
];

/// A page's rows, each as it reads, and the cursor the next page continues.
fn page_of(report: &GetReport) -> (Vec<String>, Option<Cursor>) {
    fn read<T: std::fmt::Debug>(rows: &[T]) -> Vec<String> {
        rows.iter().map(|row| format!("{row:?}")).collect()
    }
    let GetReport::Collection { page, .. } = report else {
        panic!("a get answered {report:?}, not a collection page");
    };
    match page {
        CollectionPage::Links { page, .. } => (read(&page.rows), page.next.clone()),
        CollectionPage::Headings { page, .. } => (read(&page.rows), page.next.clone()),
        CollectionPage::Blocks { page, .. } => (read(&page.rows), page.next.clone()),
        CollectionPage::Tags { page, .. } => (read(&page.rows), page.next.clone()),
        CollectionPage::Findings { page, .. } => (read(&page.rows), page.next.clone()),
        other => panic!("a page of no collection this suite knows: {other:?}"),
    }
}

/// **Every collection paged at one, two and three rows a page is its one
/// whole page**: no row repeated, none dropped, each page within its bound,
/// and the last page naming no next. The page's own tag names the collection
/// it pages.
#[test]
fn each_collection_paged_at_one_two_and_three_is_its_one_whole_page() {
    let vault = paged_vault("get-collections", 0);
    for selector in SELECTORS {
        let whole = vault.get(&getting("paged").with_collection(selector));
        let GetReport::Collection { page, .. } = &whole.report else {
            panic!("{selector:?} answered no page");
        };
        assert_eq!(page.selector(), selector);
        let (rows, next) = page_of(&whole.report);
        assert_eq!(rows.len(), HELD, "{selector:?}");
        assert!(next.is_none());
        for limit in 1..=3u32 {
            let mut drained: Vec<String> = Vec::new();
            let mut after: Option<Cursor> = None;
            loop {
                let mut params = getting("paged").with_collection(selector).with_limit(limit);
                if let Some(cursor) = after.take() {
                    params = params.with_after(cursor);
                }
                let (read, next) = page_of(&vault.get(&params).report);
                assert!(read.len() <= limit as usize, "{selector:?} at {limit}");
                drained.extend(read);
                match next {
                    Some(cursor) => after = Some(cursor),
                    None => break,
                }
                assert!(drained.len() <= HELD, "{selector:?} at {limit} ran past");
            }
            assert_eq!(drained, rows, "{selector:?} at {limit}");
        }
    }
}

/// **A cursor names a position in the collection it was minted in**: an
/// ordinal continues the collection it names, and a finding's cursor
/// continues the findings at the document's own path. A cursor minted paging
/// one collection refuses on another, naming both; anything else is refused
/// as no position in the collection paged, an ordinal naming the findings
/// among it, since the findings are paged by a finding's cursor.
#[test]
fn a_cursor_that_names_no_position_in_the_collection_is_refused() {
    let vault = paged_vault("get-cursors", 0);
    let first = |selector| {
        page_of(
            &vault
                .get(&getting("paged").with_collection(selector).with_limit(1))
                .report,
        )
        .1
        .expect("a next page")
    };
    let headings = first(CollectionSelector::Headings);
    let finding = first(CollectionSelector::Findings);
    let snapshot = headings.snapshot().clone();
    let collection = |of| PagedRows::Collection { of };
    for (selector, cursor, minted, paged) in [
        (
            CollectionSelector::Blocks,
            &headings,
            collection(CollectionSelector::Headings),
            collection(CollectionSelector::Blocks),
        ),
        (
            CollectionSelector::Tags,
            &headings,
            collection(CollectionSelector::Headings),
            collection(CollectionSelector::Tags),
        ),
        (
            CollectionSelector::Findings,
            &headings,
            collection(CollectionSelector::Headings),
            PagedRows::Finding,
        ),
        (
            CollectionSelector::Tags,
            &finding,
            PagedRows::Finding,
            collection(CollectionSelector::Tags),
        ),
        (
            CollectionSelector::Headings,
            &first(CollectionSelector::Tags),
            collection(CollectionSelector::Tags),
            collection(CollectionSelector::Headings),
        ),
    ] {
        assert_eq!(
            vault.refusal(
                &getting("paged")
                    .with_collection(selector)
                    .with_after(cursor.clone())
            ),
            PageRefusal::CursorNotTaken {
                cursor: minted,
                paged
            },
            "{selector:?}"
        );
    }
    for (selector, cursor, minted, paged) in [
        (
            CollectionSelector::Findings,
            Cursor::new(
                snapshot.clone(),
                CursorKey::finding(FindingKind::UndeclaredTag, "other.md", 1),
            ),
            PagedRows::Finding,
            PagedRows::Finding,
        ),
        (
            CollectionSelector::Findings,
            Cursor::new(
                snapshot.clone(),
                CursorKey::ordinal(CollectionSelector::Findings, 1),
            ),
            collection(CollectionSelector::Findings),
            PagedRows::Finding,
        ),
        (
            CollectionSelector::Tags,
            Cursor::new(
                snapshot,
                CursorKey::document(
                    Sort::new(SortKey::path(), Direction::Ascending),
                    None,
                    "paged.md",
                ),
            ),
            PagedRows::Document,
            collection(CollectionSelector::Tags),
        ),
    ] {
        assert_eq!(
            vault.refusal(
                &getting("paged")
                    .with_collection(selector)
                    .with_after(cursor)
            ),
            PageRefusal::CursorNotTaken {
                cursor: minted,
                paged
            },
            "{selector:?}"
        );
    }
    assert_eq!(
        PageRefusal::CursorNotTaken {
            cursor: collection(CollectionSelector::Headings),
            paged: PagedRows::Finding,
        }
        .to_string(),
        "the cursor names a position among a document's headings, and the request pages findings"
    );
}

/// **A cursor whose position is past what the store counts is refused** as
/// no position among the rows the get pages: an ordinal or a finding's id one
/// above the largest the store holds a row at.
#[test]
fn a_cursor_past_what_the_store_counts_names_no_position_in_the_collection() {
    let vault = paged_vault("get-cursor-past", 0);
    let snapshot = page_of(
        &vault
            .get(
                &getting("paged")
                    .with_collection(CollectionSelector::Headings)
                    .with_limit(1),
            )
            .report,
    )
    .1
    .expect("a next page")
    .snapshot()
    .clone();
    let past = u64::try_from(i64::MAX).expect("a positive bound") + 1;
    let headings = PagedRows::Collection {
        of: CollectionSelector::Headings,
    };
    for (selector, key, paged) in [
        (
            CollectionSelector::Headings,
            CursorKey::ordinal(CollectionSelector::Headings, past),
            headings,
        ),
        (
            CollectionSelector::Findings,
            CursorKey::finding(FindingKind::UndeclaredTag, "paged.md", past),
            PagedRows::Finding,
        ),
    ] {
        assert_eq!(
            vault.refusal(
                &getting("paged")
                    .with_collection(selector)
                    .with_after(Cursor::new(snapshot.clone(), key))
            ),
            PageRefusal::CursorNotTaken {
                cursor: paged,
                paged
            },
            "{selector:?}"
        );
    }
}

/// **An ordinal's or a finding's cursor carrying a schema fingerprint is
/// refused as not taken**: no page of a document's collection mints one, so
/// it names no position among the rows the get pages, even under the
/// fingerprint the snapshot pins.
#[test]
fn a_collection_cursor_carrying_a_fingerprint_is_not_taken() {
    let vault = paged_vault("get-cursor-fingerprint", 0);
    let reading = page_of(
        &vault
            .get(
                &getting("paged")
                    .with_collection(CollectionSelector::Headings)
                    .with_limit(1),
            )
            .report,
    )
    .1
    .expect("a next page")
    .snapshot()
    .clone();
    let forged = norn_wire::Snapshot::new(
        reading.epoch.clone(),
        reading.generation,
        Some(SCHEMA.to_string()),
        None,
    );
    let headings = PagedRows::Collection {
        of: CollectionSelector::Headings,
    };
    for (selector, key, paged) in [
        (
            CollectionSelector::Headings,
            CursorKey::ordinal(CollectionSelector::Headings, 1),
            headings,
        ),
        (
            CollectionSelector::Findings,
            CursorKey::finding(FindingKind::UndeclaredTag, "paged.md", 1),
            PagedRows::Finding,
        ),
    ] {
        let refusal = vault.refusal(
            &getting("paged")
                .with_collection(selector)
                .with_after(Cursor::new(forged.clone(), key)),
        );
        assert_eq!(
            refusal,
            PageRefusal::CursorNotTaken {
                cursor: paged,
                paged
            },
            "{selector:?}"
        );
        if paged == headings {
            assert_eq!(
                refusal.to_string(),
                "the cursor names no position among a document's headings the request pages"
            );
        }
    }
}

/// **An ordinal cursor continues its collection positionally**: minted on
/// one document's headings, it continues another document's headings from
/// the same position, as every builder's cursor continues its row type's
/// order, and an ordinal past the last row answers an empty last page.
#[test]
fn an_ordinal_cursor_continues_its_collection_by_position() {
    let body: String = (0..HELD).map(|at| format!("## Heading {at}\n")).collect();
    let vault = Vault::holding(
        "get-ordinal-position",
        Sensitive,
        &[
            parsed("one.md", &body),
            parsed("two.md", &body.to_uppercase()),
        ],
    );
    let texts = |gotten: &Gotten| -> Vec<String> {
        let GetReport::Collection {
            page: CollectionPage::Headings { page, .. },
            ..
        } = &gotten.report
        else {
            panic!("a headings page answered {:?}", gotten.report);
        };
        page.rows.iter().map(|row| row.text.clone()).collect()
    };
    let headings = |at: &str| getting(at).with_collection(CollectionSelector::Headings);
    let first = vault.get(&headings("one").with_limit(2));
    let (_, next) = page_of(&first.report);
    let cursor = next.expect("a next page");
    let continued = vault.get(&headings("two").with_after(cursor.clone()));
    assert_eq!(texts(&continued), ["HEADING 2", "HEADING 3", "HEADING 4"]);

    let past = Cursor::new(
        cursor.snapshot().clone(),
        CursorKey::ordinal(CollectionSelector::Headings, 99),
    );
    let beyond = vault.get(&headings("one").with_after(past));
    assert!(texts(&beyond).is_empty());
    assert!(page_of(&beyond.report).1.is_none());
}

/// **A part the answer asked for does not take is refused by name**, naming
/// the answer that does not take it: an anchor or a column on a collection
/// page, a column on a section or a block, and a cursor or a limit on a
/// record, a section or a block. The answer is classified before any part is
/// judged, so a cursor or a limit names the one answer the target asks for.
#[test]
fn a_part_the_answer_does_not_take_is_refused() {
    let vault = paged_vault("get-parts", 0);
    let cursor = page_of(
        &vault
            .get(
                &getting("paged")
                    .with_collection(CollectionSelector::Tags)
                    .with_limit(1),
            )
            .report,
    )
    .1
    .expect("a next page");
    for (params, part, answer, told) in [
        (
            getting("paged#Heading 0").with_collection(CollectionSelector::Tags),
            RequestPart::Anchor,
            AnswerShape::CollectionPage,
            "a collection page takes no anchor",
        ),
        (
            getting("paged")
                .with_collection(CollectionSelector::Tags)
                .with_columns([Column::body()]),
            RequestPart::Column,
            AnswerShape::CollectionPage,
            "a collection page takes no column",
        ),
        (
            getting("paged#Heading 0").with_columns([Column::body()]),
            RequestPart::Column,
            AnswerShape::Section,
            "a section takes no column",
        ),
        (
            getting("paged#^b0").with_columns([Column::body()]),
            RequestPart::Column,
            AnswerShape::Block,
            "a block takes no column",
        ),
        (
            getting("paged").with_after(cursor.clone()),
            RequestPart::Cursor,
            AnswerShape::Record,
            "a record takes no cursor",
        ),
        (
            getting("paged#Heading 0").with_after(cursor.clone()),
            RequestPart::Cursor,
            AnswerShape::Section,
            "a section takes no cursor",
        ),
        (
            getting("paged#^b0")
                .with_after(cursor)
                .with_columns([Column::body()]),
            RequestPart::Cursor,
            AnswerShape::Block,
            "a block takes no cursor",
        ),
        (
            getting("paged").with_limit(2),
            RequestPart::Limit,
            AnswerShape::Record,
            "a record takes no limit",
        ),
        (
            getting("paged#Heading 0").with_limit(1),
            RequestPart::Limit,
            AnswerShape::Section,
            "a section takes no limit",
        ),
        (
            getting("paged#^b0").with_limit(1),
            RequestPart::Limit,
            AnswerShape::Block,
            "a block takes no limit",
        ),
    ] {
        let refusal = vault.refusal(&params);
        assert_eq!(
            refusal,
            PageRefusal::PartNotTaken { part, answer },
            "{params:?}"
        );
        assert_eq!(refusal.to_string(), told);
    }
}

// ---- the work bar ----

/// **A get's work follows the target's class, the one document it answers
/// and the page it reads, not the vault**: every shape, over a vault four
/// hundred documents larger whose classes did not grow, runs the same
/// statements and SQLite counts the same work stepping them — an ambiguous
/// target's refusal among them, read off the statements its plans ran.
///
/// Controls: the plans of a get count the work the get counts, so the
/// refusal's work is read by the same measure; a find paging the whole vault,
/// read by the same kind of counters, grows with it, so the counters see a
/// vault's size where a read pays it.
#[test]
fn a_gets_work_follows_its_document_not_the_vault() {
    let small = paged_vault("get-work-small", 0);
    let large = paged_vault("get-work-large", 400);
    for params in [
        getting("paged"),
        getting("paged").with_columns([Column::field("status"), Column::tags()]),
        getting("paged#Heading 3"),
        getting("paged#Nope"),
        getting("paged#^b2"),
        getting("paged")
            .with_collection(CollectionSelector::Headings)
            .with_limit(2),
        getting("paged")
            .with_collection(CollectionSelector::Findings)
            .with_limit(2),
        getting("paged")
            .with_collection(CollectionSelector::Links)
            .with_limit(2),
    ] {
        let (at_small, at_large) = (small.get(&params).work, large.get(&params).work);
        assert_eq!(at_small, at_large, "{params:?}");
        assert!(at_small.statements > 0);
        assert_eq!(planned_work(&small, &params), at_small, "{params:?}");
    }
    let ambiguous = getting("twin#Heading");
    assert!(matches!(
        small.refusal(&ambiguous),
        PageRefusal::AmbiguousTarget(_)
    ));
    let at_small = planned_work(&small, &ambiguous);
    assert_eq!(at_small, planned_work(&large, &ambiguous));
    assert!(at_small.statements > 0 && at_small.vm_steps > 0);

    let paging = FindParams::new(address()).with_limit(500);
    let work = |vault: &Vault| {
        vault
            .snapshot()
            .find(&paging, &declared())
            .expect("a find")
            .work
            .page_vm_steps
    };
    assert!(work(&large) > work(&small));
}

/// The work the statements a get of `params` ran counted, summed, read off
/// its plans.
fn planned_work(vault: &Vault, params: &GetParams) -> GetWork {
    vault
        .plans(params)
        .iter()
        .fold(GetWork::default(), |sum, plan| GetWork {
            statements: sum.statements + plan.work.statements,
            full_scan_steps: sum.full_scan_steps + plan.work.full_scan_steps,
            sorts: sum.sorts + plan.work.sorts,
            vm_steps: sum.vm_steps + plan.work.vm_steps,
            anchor_headings: sum.anchor_headings + plan.work.anchor_headings,
            link_candidates_read: sum.link_candidates_read + plan.work.link_candidates_read,
        })
}

// ---- the plan bars ----

/// The plan the store reported for one statement, in the harness's shape.
fn plan(emitted: &GetPlan) -> QueryPlan {
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

/// Every plan `plans` holds for `statement`, in the order the get ran them.
fn plans_of(plans: &[GetPlan], statement: GetStatement) -> Vec<QueryPlan> {
    let matching: Vec<QueryPlan> = plans
        .iter()
        .filter(|plan| plan.statement == ReadStatement::Get(statement))
        .map(plan)
        .collect();
    assert!(
        !matching.is_empty(),
        "the get runs no {statement:?}: {plans:?}"
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

/// Which test bars each statement the builder names. Exhaustive, so a
/// statement added to [`GetStatement`] does not compile until its author names
/// the bar.
fn statement_barred_by(statement: GetStatement) -> &'static str {
    match statement {
        GetStatement::DocumentHeadings
        | GetStatement::BlockDefinition
        | GetStatement::DocumentBody => "a_section_and_a_block_are_read_within_one_document",
        GetStatement::CollectionPage(_) | GetStatement::FindingPage => {
            "a_collection_page_seeks_the_document_from_its_cursor"
        }
    }
}

/// **Every statement the get builder names carries a bar**: the enumeration's
/// slots are each claimed once, and every statement names the bar that judges
/// it.
#[test]
fn the_get_bars_cover_every_statement() {
    let mut slots: Vec<usize> = GetStatement::all()
        .into_iter()
        .map(GetStatement::slot)
        .collect();
    slots.sort_unstable();
    assert_eq!(slots, (0..GET_STATEMENTS).collect::<Vec<usize>>());
    for (slot, statement) in GetStatement::all().into_iter().enumerate() {
        assert_eq!(statement.slot(), slot, "{statement:?} claims another slot");
        assert!(!statement_barred_by(statement).is_empty());
    }
}

/// Every plan `plans` holds for the find statement `statement`, which a get
/// runs to resolve its target as every read resolves one.
fn find_plans_of(plans: &[GetPlan], statement: FindStatement) -> Vec<QueryPlan> {
    let matching: Vec<QueryPlan> = plans
        .iter()
        .filter(|plan| plan.statement == ReadStatement::Find(statement))
        .map(plan)
        .collect();
    assert!(
        !matching.is_empty(),
        "the get runs no {statement:?}: {plans:?}"
    );
    matching
}

/// Judge a read of a class: a search of `documents` through `index`, the key
/// the root probes, bounded on both sides, and nothing read end to end.
fn judge_class(plan: &QueryPlan, index: &str, key: &str) {
    plan.assert_no_full_scan();
    let rows = rows_of(plan, "dr");
    rows.assert_searches_through("documents", Access::Index(index));
    rows.assert_search_constraint("documents", &format!("({key}>? AND {key}<?)"));
}

/// **A get's class is read through the suffix key the root probes**: the
/// head, the total and each candidate's suffix probe a get's target runs —
/// the find builder's class statements, which every resolution of a target
/// runs — are each a seek of `documents_suffix_key` where the root tells
/// spellings apart and of `documents_folded_suffix_key` where it folds ASCII
/// case, bounded on both sides by the target's range.
///
/// Controls: a head read end to end fails; the probed index dropped, the head
/// reads something else, and fails.
#[test]
fn a_class_is_read_through_the_suffix_key_the_root_probes() {
    let paths: Vec<String> = (1..=7).map(|at| format!("d{at}/glossary.md")).collect();
    let paths: Vec<&str> = paths.iter().map(String::as_str).collect();
    for (order, index, key) in [
        (Sensitive, "documents_suffix_key", "suffix_key"),
        (Folding, "documents_folded_suffix_key", "folded_suffix_key"),
    ] {
        let mut vault = Vault::at(&format!("get-class-plan-{order:?}"), order, &paths);
        let plans = vault.plans(&getting("glossary"));
        for statement in [
            FindStatement::ClassHead,
            FindStatement::ClassTotal,
            FindStatement::CandidateSuffixes,
        ] {
            for plan in find_plans_of(&plans, statement) {
                judge_class(&plan, index, key);
            }
        }
        let head = find_plans_of(&plans, FindStatement::ClassHead).remove(0);
        let walked = rewritten(&head, |detail| {
            if detail.starts_with("SEARCH dr ") {
                "SCAN dr".to_string()
            } else {
                detail.to_string()
            }
        });
        failure_of("a head read end to end", || {
            judge_class(&walked, index, key)
        });
        vault.drop_index(index);
        let unindexed = find_plans_of(&vault.plans(&getting("glossary")), FindStatement::ClassHead);
        failure_of(&format!("{index} dropped"), || {
            judge_class(&unindexed[0], index, key)
        });
    }
}

/// **A section and a block are read within one document**: its headings are
/// one seek of `headings_document_ordinal` at the document, a block's
/// definition one seek of `blocks_document_ordinal` at it, and its body one
/// read by row id.
///
/// Controls: each index dropped, the lookup it served reads something else
/// and fails; the body read end to end fails.
#[test]
fn a_section_and_a_block_are_read_within_one_document() {
    let mut vault = paged_vault("get-within-plan", 0);
    let section = vault.plans(&getting("paged#heading 2"));
    let block = vault.plans(&getting("paged#^b2"));
    let judge_headings = |plan: &QueryPlan| {
        plan.assert_no_full_scan();
        let rows = rows_of(plan, "headings");
        rows.assert_searches_through("headings", Access::Index("headings_document_ordinal"));
        rows.assert_search_constraint("headings", "(document=?)");
    };
    let judge_block = |plan: &QueryPlan| {
        plan.assert_no_full_scan();
        plan.assert_no_temp_btree();
        let rows = rows_of(plan, "b");
        rows.assert_searches_through("blocks", Access::Index("blocks_document_ordinal"));
        rows.assert_search_constraint("blocks", "(document=?)");
    };
    let judge_body = |plan: &QueryPlan| {
        plan.assert_no_full_scan();
        let rows = rows_of(plan, "d");
        rows.assert_searches_through("documents", Access::RowId);
        rows.assert_search_constraint("documents", "(rowid=?)");
    };
    let headings = plans_of(&section, GetStatement::DocumentHeadings).remove(0);
    judge_headings(&headings);
    for plans in [&section, &block] {
        judge_body(&plans_of(plans, GetStatement::DocumentBody)[0]);
    }
    let definition = plans_of(&block, GetStatement::BlockDefinition).remove(0);
    judge_block(&definition);

    let body = plans_of(&section, GetStatement::DocumentBody).remove(0);
    let walked = rewritten(&body, |detail| {
        if detail.starts_with("SEARCH d ") {
            "SCAN d".to_string()
        } else {
            detail.to_string()
        }
    });
    failure_of("a body read end to end", || judge_body(&walked));
    vault.drop_index("headings_document_ordinal");
    vault.drop_index("blocks_document_ordinal");
    let section = vault.plans(&getting("paged#heading 2"));
    let block = vault.plans(&getting("paged#^b2"));
    failure_of("headings_document_ordinal dropped", || {
        judge_headings(&plans_of(&section, GetStatement::DocumentHeadings)[0])
    });
    failure_of("blocks_document_ordinal dropped", || {
        judge_block(&plans_of(&block, GetStatement::BlockDefinition)[0])
    });
}

/// **A get resolves its target once and reads its document once.** Whatever it
/// answers, a record, a section found or missing, a block, or a page of one
/// collection, the target's class is read by one statement, and no statement
/// the get runs is run twice: the document's row, its body and each collection
/// it reads are each read by one statement.
#[test]
fn a_get_resolves_its_target_once_and_reads_its_document_once() {
    let vault = paged_vault("get-once", 3);
    for params in [
        getting("paged"),
        getting("paged").with_columns([Column::body(), Column::links(), Column::findings()]),
        getting("paged#Heading 3"),
        getting("paged#Nope"),
        getting("paged#^b2"),
        getting("paged")
            .with_collection(CollectionSelector::Headings)
            .with_limit(2),
        getting("paged")
            .with_collection(CollectionSelector::Findings)
            .with_limit(2),
        getting("paged")
            .with_collection(CollectionSelector::Links)
            .with_limit(2),
    ] {
        let ran: Vec<ReadStatement> = vault
            .plans(&params)
            .into_iter()
            .map(|plan| plan.statement)
            .collect();
        let resolved = ran
            .iter()
            .filter(|statement| **statement == ReadStatement::Find(FindStatement::ClassHead))
            .count();
        assert_eq!(
            resolved, 1,
            "{params:?} resolved its target {resolved} times: {ran:?}"
        );
        let distinct: std::collections::BTreeSet<String> = ran
            .iter()
            .map(|statement| format!("{statement:?}"))
            .collect();
        assert_eq!(
            distinct.len(),
            ran.len(),
            "{params:?} ran a statement twice: {ran:?}"
        );
    }
}

/// **A collection page seeks the document from its cursor**: a collection
/// paged by ordinal is one seek of its table's `(document, ordinal)` index
/// bounded below by the ordinal the page continues after, and the findings
/// are one seek of `findings_path` at the document's path and the active
/// fingerprint, bounded below by the kind the page continues in, the id
/// tested within it.
/// Either hands its rows back in page order, so nothing sorts.
///
/// Controls: a continuation's plan with its position taken out of the seek
/// fails; each index dropped, the page reads something else and fails.
#[test]
fn a_collection_page_seeks_the_document_from_its_cursor() {
    let mut vault = paged_vault("get-page-plan", 0);
    let continued = |vault: &Vault, selector| {
        let (_, next) = page_of(
            &vault
                .get(&getting("paged").with_collection(selector).with_limit(1))
                .report,
        );
        let next = next.expect("a next page");
        vault.plans(
            &getting("paged")
                .with_collection(selector)
                .with_limit(2)
                .with_after(next),
        )
    };
    let judge_ordinal = |plan: &QueryPlan, table: &str| {
        plan.assert_no_full_scan();
        plan.assert_no_temp_btree();
        let rows = rows_of(plan, "n");
        rows.assert_searches_through(table, Access::Index(&format!("{table}_document_ordinal")));
        rows.assert_search_constraint(table, "(document=? AND ordinal>?)");
    };
    let findings_seek = "(path=? AND vault_schema_fingerprint=? AND kind>?)";
    let judge_findings = |plan: &QueryPlan| {
        plan.assert_no_full_scan();
        plan.assert_no_temp_btree();
        let rows = rows_of(plan, "f");
        rows.assert_searches_through("findings", Access::Index("findings_path"));
        rows.assert_search_constraint("findings", findings_seek);
    };
    let mut ordinal_pages = Vec::new();
    for (selector, collection) in [
        (CollectionSelector::Links, Nested::Links),
        (CollectionSelector::Headings, Nested::Headings),
        (CollectionSelector::Blocks, Nested::Blocks),
        (CollectionSelector::Tags, Nested::Tags),
    ] {
        let page = plans_of(
            &continued(&vault, selector),
            GetStatement::CollectionPage(collection),
        )
        .remove(0);
        judge_ordinal(&page, collection.table());
        ordinal_pages.push((collection, page));
    }
    let findings = plans_of(
        &continued(&vault, CollectionSelector::Findings),
        GetStatement::FindingPage,
    )
    .remove(0);
    judge_findings(&findings);

    let (collection, page) = &ordinal_pages[1];
    let unbounded = rewritten(page, |detail| detail.replace(" AND ordinal>?", ""));
    failure_of("a continuation that seeks from no ordinal", || {
        judge_ordinal(&unbounded, collection.table())
    });
    let unpositioned = rewritten(&findings, |detail| detail.replace(" AND kind>?", ""));
    failure_of("a findings continuation that seeks from no finding", || {
        judge_findings(&unpositioned)
    });
    vault.drop_index("links_document_ordinal");
    vault.drop_index("findings_path");
    let links = plans_of(
        &continued(&vault, CollectionSelector::Links),
        GetStatement::CollectionPage(Nested::Links),
    );
    failure_of("links_document_ordinal dropped", || {
        judge_ordinal(&links[0], "links")
    });
    let findings = plans_of(
        &continued(&vault, CollectionSelector::Findings),
        GetStatement::FindingPage,
    );
    failure_of("findings_path dropped", || judge_findings(&findings[0]));
}

/// **A get's work reads out whole, each count under its own name**, for the
/// reason a find's does.
#[test]
fn a_gets_work_reads_out_every_count_by_name() {
    let work = GetWork {
        statements: 1,
        full_scan_steps: 2,
        sorts: 3,
        vm_steps: 4,
        anchor_headings: 5,
        link_candidates_read: 6,
    };
    assert_eq!(
        work.readings().collect::<Vec<_>>(),
        vec![
            ("get_statements", 1),
            ("get_full_scan_steps", 2),
            ("get_sorts", 3),
            ("get_vm_steps", 4),
            ("get_anchor_headings", 5),
            ("get_link_candidates_read", 6),
        ]
    );
}
