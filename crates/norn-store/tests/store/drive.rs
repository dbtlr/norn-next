//! Which part drives a page no part drives in its order, judged by what a
//! drain costs on the snapshot — the drive's own statements included — and by
//! what it answers.
//!
//! The fixture is one vault read by all three verbs: `size` documents under
//! `bulk/`, each with an undeclared-tag warning standing over it, every
//! [`SPREAD`]th of them matched by every part of [`spread_parts`] and the rest
//! by none, beside the one document the matched ones link to. So the matched
//! set is a fixed fraction of the vault, spread evenly through it, and a
//! drain's cost per matched document reads whether the drain is linear in the
//! vault.

use std::sync::Arc;

use crate::common::{Scratch, document, unread_block, write_documents};
use crate::find::{map, request, sorted, string};
use norn_store::{
    ContentModel, FindStatement, FindingFacts, LinkFact, LinkFamily, ReadStatement,
    RepairSelection, Snapshot, SnapshotReader, Store, StoredPathOrder, Validation,
};
use norn_wire::{
    ApplyMode, Cursor, Direction, FindParams, FindingKind, Predicate, RepairParams,
    ResolutionTarget, Severity, SortKey, ValidateParams, VaultAddress, VaultName,
};

// ---- fixtures ----

/// The fingerprint of the schema the fixture pins.
const DRIVE_SCHEMA: &str = "drive-schema";

/// One document in this many is matched.
const SPREAD: usize = 4;

/// The document every matched one links to.
const TARGET: &str = "target";

fn declared() -> ContentModel {
    ContentModel::under(DRIVE_SCHEMA).declare("status")
}

fn vault() -> VaultAddress {
    VaultAddress::name(VaultName::new("notes").expect("a vault name"))
}

/// The path of the `at`th bulk document.
fn bulk(at: usize) -> String {
    format!("bulk/{at:05}.md")
}

/// An undeclared-tag warning on the document at `at`.
fn warning(at: &str) -> FindingFacts {
    let mut finding = unread_block(at);
    finding.kind = FindingKind::UndeclaredTag;
    finding.severity = Severity::Warning;
    finding.target = Some("spread".to_string());
    finding.message = "`spread` is not a declared tag".to_string();
    finding.detail = None;
    finding
}

/// A document every part of [`spread_parts`] admits, at `at`: `status: open`,
/// the key `noted`, the word `marked` in its body and a wikilink to
/// [`TARGET`]. A document no part admits holds `status: zzz`, a plain body
/// and no link.
fn spread_document(at: &str, matched: bool, declared: &ContentModel) -> norn_store::DocumentFacts {
    if !matched {
        return document(at, &format!("hash-{at}"), "a plain body\n")
            .with_frontmatter(Some(map(vec![("status", string("zzz"))])), declared);
    }
    let mut facts = document(at, &format!("hash-{at}"), "a marked body\n").with_frontmatter(
        Some(map(vec![
            ("status", string("open")),
            ("noted", string("yes")),
        ])),
        declared,
    );
    facts.links.push(LinkFact {
        family: LinkFamily::Wikilink,
        embed: false,
        protocol: None,
        target: TARGET.to_string(),
        title: None,
        anchor: None,
        span: None,
    });
    facts
}

/// A store holding `size` bulk documents, those `matched` names matched, each
/// with a warning, beside [`TARGET`], and the read handle its snapshots are
/// established on.
struct Spread {
    _scratch: Scratch,
    _store: Store,
    reader: Arc<SnapshotReader>,
    /// The matched documents' paths, in the answer's path order.
    matched: Vec<String>,
}

impl Spread {
    /// Every [`SPREAD`]th of `size` documents matched, from the first.
    fn even(label: &str, size: usize) -> Self {
        Self::matching(label, size, |at| at % SPREAD == 0)
    }

    fn matching(label: &str, size: usize, matched: impl Fn(usize) -> bool) -> Self {
        let scratch = Scratch::new(label);
        let mut store = scratch.open_under(StoredPathOrder::Sensitive);
        store
            .begin_request()
            .pin_vault_schema(DRIVE_SCHEMA.as_bytes(), DRIVE_SCHEMA)
            .expect("pinning the fixture's schema");
        let declared = declared();
        let mut documents = vec![document(
            &format!("{TARGET}.md"),
            "hash-target",
            "the target\n",
        )];
        let mut paths = Vec::new();
        for at in 0..size {
            let path = bulk(at);
            if matched(at) {
                paths.push(path.clone());
            }
            documents.push(spread_document(&path, matched(at), &declared));
        }
        let mut request = store.begin_request();
        write_documents(&mut request, &documents);
        for at in 0..size {
            request
                .record_finding(&warning(&bulk(at)))
                .expect("recording a warning");
        }
        let reader = Arc::new(
            store
                .open_reader()
                .reader
                .expect("a live store mints a reader"),
        );
        Spread {
            _scratch: scratch,
            _store: store,
            reader,
            matched: paths,
        }
    }

    fn snapshot(&self) -> Snapshot {
        self.reader
            .try_take()
            .expect("a handle nothing is reading holds its connection")
            .establish()
            .expect("a snapshot")
    }
}

/// The parts no page drives in its order, each admitting exactly the matched
/// documents, beside the name a failure gives it: a range, a full-text match,
/// a links-to part and a membership of more than sixteen values.
fn spread_parts() -> Vec<(&'static str, Predicate)> {
    let mut members = vec!["open".to_string()];
    members.extend((0..16).map(|at| format!("v{at:02}")));
    vec![
        ("a range", Predicate::before("status", "p")),
        ("a full-text match", Predicate::matches("marked")),
        (
            "a links-to part",
            Predicate::links_to(ResolutionTarget::new(TARGET).expect("a target")),
        ),
        (
            "a membership of seventeen values",
            Predicate::in_any("status", members),
        ),
    ]
}

/// The control: an equality, which drives every path page in its order.
fn control() -> (&'static str, Predicate) {
    ("an equality", Predicate::equal_to("status", "open"))
}

/// The pages a find narrowed by no part in its order reads, beside the name a
/// failure gives each: the path ascending by each part of [`spread_parts`],
/// the path descending by a presence — which keeps its documents in the
/// path's order ascending only — and a field's order by a presence.
fn find_drains() -> Vec<(String, FindParams)> {
    let mut drains: Vec<(String, FindParams)> = spread_parts()
        .into_iter()
        .map(|(name, part)| {
            (
                format!("a path page by {name}"),
                request().with_predicates([part]),
            )
        })
        .collect();
    drains.push((
        "a path page descending by a presence".to_string(),
        sorted(SortKey::path(), Direction::Descending).with_predicates([Predicate::has("noted")]),
    ));
    drains.push((
        "a field's page by a presence".to_string(),
        sorted(SortKey::field("status"), Direction::Ascending)
            .with_predicates([Predicate::has("noted")]),
    ));
    drains
}

// ---- drains ----

/// What a drain answered and what it cost: the paths of its rows, in order,
/// and the VM steps every statement its snapshots ran took.
struct Drained {
    paths: Vec<String>,
    vm_steps: u64,
}

/// Every page of a find of `params`, `limit` rows at a time.
fn find_drain(spread: &Spread, params: &FindParams, limit: u32) -> Drained {
    let (mut paths, mut vm_steps) = (Vec::new(), 0);
    let mut after: Option<Cursor> = None;
    loop {
        let mut page = params.clone().with_limit(limit);
        if let Some(cursor) = after.take() {
            page = page.with_after(cursor);
        }
        let snapshot = spread.snapshot();
        let found = snapshot
            .find(&page, &declared())
            .unwrap_or_else(|refusal| panic!("a find of {page:?}: {refusal}"));
        vm_steps += snapshot.counters().vm_steps();
        paths.extend(found.rows.iter().map(|row| row.path.as_str().to_string()));
        match found.next {
            Some(next) => after = Some(next),
            None => return Drained { paths, vm_steps },
        }
        assert!(paths.len() <= 100_000, "the pages of {params:?} do not end");
    }
}

/// Every page of a validate of `params`, `limit` findings at a time.
fn validate_drain(spread: &Spread, params: &ValidateParams, limit: u32) -> Drained {
    let (mut paths, mut vm_steps) = (Vec::new(), 0);
    let mut after: Option<Cursor> = None;
    loop {
        let mut page = params.clone().with_limit(limit);
        if let Some(cursor) = after.take() {
            page = page.with_after(cursor);
        }
        let snapshot = spread.snapshot();
        let validated = snapshot
            .validate(&page, &declared())
            .unwrap_or_else(|refusal| panic!("a validate of {page:?}: {refusal}"));
        vm_steps += snapshot.counters().vm_steps();
        let Validation::Findings { rows, next, .. } = validated.answer else {
            panic!("a page of {page:?} answered a summary");
        };
        paths.extend(rows.iter().map(|row| row.path.as_str().to_string()));
        match next {
            Some(next) => after = Some(next),
            None => return Drained { paths, vm_steps },
        }
        assert!(paths.len() <= 100_000, "the pages of {params:?} do not end");
    }
}

/// Every batch of a repair of `params`, `limit` findings at a time.
fn repair_drain(spread: &Spread, params: &RepairParams, limit: u32) -> Drained {
    let (mut paths, mut vm_steps) = (Vec::new(), 0);
    let mut after: Option<Cursor> = None;
    loop {
        let mut batch = params.clone().with_limit(limit);
        if let Some(cursor) = after.take() {
            batch = batch.with_after(cursor);
        }
        let snapshot = spread.snapshot();
        let read = snapshot
            .repair_batch(&RepairSelection::from(&batch), &declared())
            .unwrap_or_else(|refusal| panic!("a batch of {batch:?}: {refusal}"));
        vm_steps += snapshot.counters().vm_steps();
        paths.extend(read.rows.iter().map(|row| row.path.as_str().to_string()));
        match read.next {
            Some(next) => after = Some(next),
            None => return Drained { paths, vm_steps },
        }
        assert!(
            paths.len() <= 100_000,
            "the batches of {params:?} do not end"
        );
    }
}

fn validating(part: &Predicate) -> ValidateParams {
    ValidateParams::new(vault())
        .with_kinds([FindingKind::UndeclaredTag])
        .with_predicates([part.clone()])
}

fn repairing(part: &Predicate) -> RepairParams {
    RepairParams::new(vault(), ApplyMode::Preview)
        .with_kinds([FindingKind::UndeclaredTag])
        .with_predicates([part.clone()])
}

/// Every drain judged over `spread`, each beside the name a failure gives it:
/// a validate and a repair narrowed by each part of [`spread_parts`] and by
/// the control, and each of [`find_drains`] and the control's path page.
fn every_drain(spread: &Spread, limit: u32) -> Vec<(String, Drained)> {
    let mut drains = Vec::new();
    for (name, part) in spread_parts().into_iter().chain([control()]) {
        drains.push((
            format!("a validate by {name}"),
            validate_drain(spread, &validating(&part), limit),
        ));
        drains.push((
            format!("a repair by {name}"),
            repair_drain(spread, &repairing(&part), limit),
        ));
    }
    let (name, part) = control();
    for (name, params) in find_drains().into_iter().chain([(
        format!("a path page by {name}"),
        request().with_predicates([part]),
    )]) {
        drains.push((name, find_drain(spread, &params, limit)));
    }
    drains
}

// ---- the bars ----

/// The vault sizes the drain pair compares: one, and four times it.
const SMALL: usize = 500;
const LARGE: usize = 4 * SMALL;

/// How much more a drain may cost per matched document over the larger vault
/// than over the smaller one, in VM steps: a linear drain costs the same per
/// document at both sizes, up to the pages that straddle the bulk's edges.
const LINEAR_SLACK: f64 = 1.1;

/// **A drain a part no page drives in its order narrows is linear in the
/// vault (NORN-393).** Over [`SMALL`], then [`LARGE`], documents one in
/// [`SPREAD`] of which every part matches, a validate, a repair and a find
/// narrowed by each such part — a range, a full-text match, a links-to part
/// and a membership of seventeen values, and for a find a presence on a page
/// descending and on a field's page — drained five at a time, costs the same
/// per matched document at both sizes, within [`LINEAR_SLACK`], counted over
/// every statement its snapshots ran: the drive's own counts and walks
/// included. An equality, which drives each page in its order, is the
/// control, and holds alike.
#[test]
fn a_drain_no_part_drives_in_its_order_is_linear_in_the_vault() {
    let (small, large) = (
        Spread::even("drive-linear-small", SMALL),
        Spread::even("drive-linear-large", LARGE),
    );
    let (at_small, at_large) = (every_drain(&small, 5), every_drain(&large, 5));
    let mut grew = Vec::new();
    for ((name, small_drain), (_, large_drain)) in at_small.iter().zip(&at_large) {
        let per = |drain: &Drained, spread: &Spread| {
            assert!(
                !drain.paths.is_empty() && drain.paths.len() >= spread.matched.len(),
                "{name} answered {} rows over {} matched documents",
                drain.paths.len(),
                spread.matched.len()
            );
            drain.vm_steps as f64 / spread.matched.len() as f64
        };
        let (per_small, per_large) = (per(small_drain, &small), per(large_drain, &large));
        eprintln!("{name}: {per_small:.1} then {per_large:.1} VM steps per matched document");
        if per_large > per_small * LINEAR_SLACK {
            grew.push(format!("{name}: {per_small:.1} -> {per_large:.1}"));
        }
    }
    assert!(
        grew.is_empty(),
        "a drain grew per matched document with the vault:\n{}",
        grew.join("\n")
    );
}

/// **A first page a selective part narrows costs about its rows, whatever
/// the vault (NORN-393).** Over 500, then 2000, documents of which only the
/// last three are matched, a first page of five narrowed by each part of
/// [`spread_parts`] — a validate's, a repair's and a find's — costs the same
/// VM steps at both sizes: it asks the part whether it keeps at most the rows
/// the page reads, and drives from the three it matched, walking nothing and
/// reading no span of the vault.
#[test]
fn a_selective_first_page_costs_its_rows_not_the_vault() {
    let last_three = |size: usize| move |at: usize| at + 3 >= size;
    let (small, large) = (
        Spread::matching("drive-selective-small", 500, last_three(500)),
        Spread::matching("drive-selective-large", 2000, last_three(2000)),
    );
    let first_page = |spread: &Spread, part: &Predicate| {
        let validate = {
            let snapshot = spread.snapshot();
            snapshot
                .validate(&validating(part).with_limit(5), &declared())
                .expect("a validate");
            snapshot.counters().vm_steps()
        };
        let repair = {
            let snapshot = spread.snapshot();
            let batch = repairing(part).with_limit(5);
            snapshot
                .repair_batch(&RepairSelection::from(&batch), &declared())
                .expect("a batch");
            snapshot.counters().vm_steps()
        };
        let find = {
            let snapshot = spread.snapshot();
            let found = snapshot
                .find(
                    &request().with_predicates([part.clone()]).with_limit(5),
                    &declared(),
                )
                .expect("a find");
            assert_eq!(found.rows.len(), 3, "{part:?} found other than its three");
            snapshot.counters().vm_steps()
        };
        [validate, repair, find]
    };
    for (name, part) in spread_parts() {
        let plans = large
            .snapshot()
            .find_plans(
                &request().with_predicates([part.clone()]).with_limit(5),
                &declared(),
            )
            .expect("the plans of a find");
        let ran = |statement: FindStatement| {
            plans
                .iter()
                .filter(|plan| plan.statement == ReadStatement::Find(statement))
                .count()
        };
        assert_eq!(
            (
                ran(FindStatement::PartCeiling),
                ran(FindStatement::DocumentSpan),
                ran(FindStatement::PathPage(
                    norn_store::PageDirection::Ascending
                )),
            ),
            (1, 0, 1),
            "a first page by {name} walked or asked past its rows"
        );
        assert_eq!(
            first_page(&small, &part),
            first_page(&large, &part),
            "a first page by {name}, which matches three documents, grew with the vault"
        );
    }
}

/// **Every drain answers what the vault holds, whichever drive each page
/// took (NORN-393).** Over a vault of 600 documents whose matched ones are
/// one in [`SPREAD`] across it, then the first two and twenty spread past
/// them — so a page of five meets two by its walk and then drives from what
/// the part matched — then only the last three, each drain of
/// [`every_drain`], one and five rows at a time, answers the matched
/// documents in the answer's path order, each once — a find's descending page
/// in reverse — and a validate's and a repair's their warnings.
#[test]
fn every_drain_answers_the_matched_documents_whichever_drive() {
    let size = 600;
    let vaults = [
        Spread::even("drive-answers-even", size),
        Spread::matching("drive-answers-switch", size, |at| {
            at < 2 || (at >= 100 && at % 25 == 0)
        }),
        Spread::matching("drive-answers-last", size, |at| at + 3 >= size),
    ];
    // The switching vault's first page of five walks a window holding two of
    // its matches, then drives from what the part matched for the rest.
    let first = vaults[1]
        .snapshot()
        .find_plans(
            &request()
                .with_predicates([Predicate::before("status", "p")])
                .with_limit(5),
            &declared(),
        )
        .expect("the plans of a find");
    let narrowed: Vec<&str> = first
        .iter()
        .filter(|plan| {
            matches!(
                plan.statement,
                ReadStatement::Find(FindStatement::PathPage(_))
            ) && !plan.filters.is_empty()
        })
        .map(|plan| plan.plan.sql.as_str())
        .collect();
    assert!(
        narrowed.len() == 2
            && !narrowed[0].contains("(+d.path")
            && narrowed[1].contains("(+d.path"),
        "the first page did not walk and then drive from what the part matched: {narrowed:#?}"
    );
    for spread in &vaults {
        for limit in [1, 5] {
            for (name, drained) in every_drain(spread, limit) {
                let mut expected = spread.matched.clone();
                if name.contains("descending") {
                    expected.reverse();
                }
                assert_eq!(
                    drained.paths,
                    expected,
                    "{name}, {limit} at a time, over {} matched",
                    spread.matched.len()
                );
            }
        }
    }
}
