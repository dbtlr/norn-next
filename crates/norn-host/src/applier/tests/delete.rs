//! A delete's backlinks planned over a vault on disk and the store beside it,
//! then judged and applied by the applier: which links are a deleted
//! document's backlinks, what a delete forbidding them answers, what one
//! leaving them broken lands, and the cascade one rewriting them carries.

use norn_wire::{
    Candidate, CandidateHead, LinkAdvisory, LinkFamily, LinkKey, LinkRewrite, Operation,
    OperationKind, PlanCondition, ResolutionTarget, Resolves, UnresolvedOperation,
    UnresolvedReason,
};

use super::{Fixture, applied, breaking, creating, deleting, editing, path};

/// A delete of the document at `at` rewriting every link naming it to name
/// `to`.
fn rewriting(at: &str, to: &str) -> Operation {
    Operation::new(OperationKind::delete_document_rewriting(
        path(at),
        ResolutionTarget::new(to).expect("a target"),
    ))
}

fn wikilink(holder: &str, from: &str, to: &str) -> LinkRewrite {
    LinkRewrite::new(path(holder), LinkFamily::Wikilink, from, to)
}

fn markdown(holder: &str, from: &str, to: &str) -> LinkRewrite {
    LinkRewrite::new(path(holder), LinkFamily::Markdown, from, to)
}

/// The detail `resolution`'s one unresolved operation, left out because it
/// no longer resolves, says why.
fn unresolved_detail(resolution: &crate::planner::resolve::Resolution) -> String {
    match &resolution.unresolved[..] {
        [left] => match &left.reason {
            UnresolvedReason::NoLongerResolves { detail, .. } => detail.clone(),
            other => panic!("no longer resolves: {other:?}"),
        },
        other => panic!("one operation is unresolved: {other:?}"),
    }
}

fn key(holder: &str, syntax: LinkFamily, address: &str) -> LinkKey {
    LinkKey::new(path(holder), syntax, address)
}

/// **A plain delete of a document links name is left unresolved, naming
/// every holder and how many links name it.** `a.md` is named by a bare
/// wikilink and a Markdown link in `h.md`, an embed in `k.md` and the same
/// wikilink twice in `z/m.md`: five links in three holders, each holder
/// named once and in path order. The delete writes nothing, and a link to
/// another document counts toward none.
#[test]
fn a_plain_delete_of_a_linked_document_is_unresolved_naming_every_holder_and_the_total() {
    let fixture = Fixture::new(&[
        ("a.md", "A\n"),
        ("b.md", "B\n"),
        ("h.md", "[[a]] and [x](a.md)\n"),
        ("k.md", "![[a#Part]]\n"),
        ("z/m.md", "[[a]] [[a]] [[b]]\n"),
    ]);
    let resolution = fixture.planned(vec![deleting("a.md")]);
    assert_eq!(
        resolution.unresolved,
        [UnresolvedOperation::new(
            deleting("a.md"),
            UnresolvedReason::has_backlinks(vec![path("h.md"), path("k.md"), path("z/m.md")], 5),
        )]
    );
    assert!(resolution.plan.operations.is_empty());
    assert!(resolution.plan.transitions.is_empty());
}

/// **A plain delete of a document no link names lands**, recording nothing.
#[test]
fn a_plain_delete_of_an_unlinked_document_lands() {
    let mut fixture = Fixture::new(&[("a.md", "A\n"), ("h.md", "[[b]]\n"), ("b.md", "B\n")]);
    let resolution = fixture.resolution(vec![deleting("a.md")]);
    assert_eq!(resolution.plan.conditions, []);
    assert!(resolution.forecast.links.is_empty());
    applied(fixture.apply(resolution.plan));
    assert_eq!(fixture.read("a.md"), None);
    fixture.assert_store_is_a_build_from_zero();
}

/// **An ambiguous link to the deleted document is a backlink of none, and
/// is advised retargeted**: `[[a]]` names `x/a.md` and `y/a.md`, so deleting
/// `x/a.md` is not refused for it; after the delete it names `y/a.md` alone,
/// which the plan records and the forecast says.
#[test]
fn an_ambiguous_link_to_the_deleted_document_does_not_refuse_it_and_is_advised() {
    let mut fixture = Fixture::new(&[("x/a.md", "X\n"), ("y/a.md", "Y\n"), ("h.md", "[[a]]\n")]);
    let resolution = fixture.resolution(vec![deleting("x/a.md")]);
    assert_eq!(
        resolution.plan.conditions,
        [PlanCondition::link_resolution(
            key("h.md", LinkFamily::Wikilink, "a"),
            Resolves::several(),
            Resolves::one(path("y/a.md")),
        )]
    );
    assert_eq!(
        resolution.forecast.links,
        [LinkAdvisory::retargeted(key(
            "h.md",
            LinkFamily::Wikilink,
            "a"
        ))]
    );
    applied(fixture.apply(resolution.plan));
    assert_eq!(fixture.read("h.md").as_deref(), Some("[[a]]\n"));
    fixture.assert_store_is_a_build_from_zero();
}

/// **A link the deleted document holds to itself is no backlink**: it goes
/// with its document.
#[test]
fn a_self_link_of_the_deleted_document_does_not_count() {
    let mut fixture = Fixture::new(&[("a.md", "# Top\n[[a]] [[#Top]] [me](a.md)\n")]);
    let resolution = fixture.resolution(vec![deleting("a.md")]);
    assert_eq!(resolution.plan.conditions, []);
    applied(fixture.apply(resolution.plan));
    assert_eq!(fixture.read("a.md"), None);
}

/// **Backlinks are judged where the plan leaves the vault.** A holder the
/// same plan deletes, or whose link an edit of the plan writes away, holds
/// no backlink after it, so the delete lands; a document the plan creates
/// holding a link to it does, so the delete is refused for that one.
#[test]
fn backlinks_are_judged_at_the_plans_after_state() {
    let mut fixture = Fixture::new(&[
        ("a.md", "A\n"),
        ("b.md", "B\n"),
        ("h.md", "[[a]]\n"),
        ("k.md", "see [[a]]\n"),
    ]);
    let resolution = fixture.resolution(vec![
        deleting("a.md"),
        deleting("h.md"),
        editing("k.md", "[[a]]", "[[b]]"),
    ]);
    applied(fixture.apply(resolution.plan));
    assert_eq!(fixture.read("a.md"), None);
    assert_eq!(fixture.read("k.md").as_deref(), Some("see [[b]]\n"));
    fixture.assert_store_is_a_build_from_zero();

    let fixture = Fixture::new(&[("a.md", "A\n")]);
    let resolution = fixture.planned(vec![deleting("a.md"), creating("n.md", "[[a]]\n")]);
    assert_eq!(
        resolution.unresolved,
        [UnresolvedOperation::new(
            deleting("a.md"),
            UnresolvedReason::has_backlinks(vec![path("n.md")], 1),
        )]
    );
}

/// **A delete leaving its links broken lands, and says so of each.** Both
/// syntaxes in `h.md` and the embed in `k.md` are recorded from the
/// document to none, each advised left broken, and the applier computes the
/// same set again and lands the plan.
#[test]
fn a_delete_leaving_its_links_broken_lands_and_advises_each() {
    let mut fixture = Fixture::new(&[
        ("a.md", "A\n"),
        ("h.md", "[[a]] and [x](a.md)\n"),
        ("k.md", "![[a#Part]]\n"),
    ]);
    let resolution = fixture.resolution(vec![breaking("a.md")]);
    let broken = [
        key("h.md", LinkFamily::Markdown, "a.md"),
        key("h.md", LinkFamily::Wikilink, "a"),
        key("k.md", LinkFamily::Wikilink, "a"),
    ];
    assert_eq!(
        resolution.forecast.links,
        broken.clone().map(LinkAdvisory::left_broken)
    );
    assert_eq!(
        resolution.plan.conditions,
        broken.map(|link| PlanCondition::link_resolution(
            link,
            Resolves::one(path("a.md")),
            Resolves::none()
        ))
    );
    let (previewed, forecast) = fixture
        .preview(resolution.plan.clone())
        .expect("the delete previews");
    assert_eq!(previewed, resolution.plan);
    assert_eq!(forecast.links, resolution.forecast.links);
    applied(fixture.apply(resolution.plan));
    assert_eq!(fixture.read("a.md"), None);
    assert_eq!(
        fixture.read("h.md").as_deref(),
        Some("[[a]] and [x](a.md)\n")
    );
    fixture.assert_store_is_a_build_from_zero();
}

/// **A delete rewriting its links respells every backlink to name its
/// target, each in its own form**: a bare wikilink to the target's shortest
/// unique suffix, written with the extension only where it was; a
/// path-qualified one to a suffix of at least two segments; a `vault://` one
/// to the target's root path; a Markdown link to the target's path from its
/// holder's folder. An embed, a title and an anchor are kept — the anchor
/// even where the target holds no such heading, which link health finds
/// after the delete rather than the delete refusing it. Every rewrite rides
/// the delete as its cascade, and the plan lands as planned.
#[test]
fn a_delete_rewriting_its_links_respells_each_backlink_in_its_own_form() {
    let mut fixture = Fixture::new(&[
        ("notes/a.md", "A\n"),
        ("archive/c.md", "C\n"),
        (
            "h.md",
            "[[a]] ![[a#Part|it]] [[notes/a]] [[a.md]] [[vault://notes/a]]\n\n[t](notes/a.md) [u](notes/a.md#part \"Title\")\n",
        ),
        ("deep/k.md", "[v](../notes/a.md)\n"),
    ]);
    let resolution = fixture.resolution(vec![rewriting("notes/a.md", "c")]);
    assert_eq!(
        resolution.plan.operations[0].cascade,
        [
            markdown("deep/k.md", "../notes/a.md", "../archive/c.md"),
            markdown("h.md", "notes/a.md", "archive/c.md"),
            wikilink("h.md", "a", "c"),
            wikilink("h.md", "a.md", "c.md"),
            wikilink("h.md", "notes/a", "archive/c"),
            wikilink("h.md", "vault://notes/a", "vault://archive/c"),
        ]
    );
    assert!(
        resolution.forecast.links.is_empty(),
        "{:?}",
        resolution.forecast.links
    );
    let (previewed, forecast) = fixture
        .preview(resolution.plan.clone())
        .expect("the planned cascade previews");
    assert_eq!(previewed, resolution.plan);
    assert_eq!(forecast.links, resolution.forecast.links);
    applied(fixture.apply(resolution.plan));
    assert_eq!(fixture.read("notes/a.md"), None);
    assert_eq!(
        fixture.read("h.md").as_deref(),
        Some(
            "[[c]] ![[c#Part|it]] [[archive/c]] [[c.md]] [[vault://archive/c]]\n\n[t](archive/c.md) [u](archive/c.md#part \"Title\")\n"
        )
    );
    assert_eq!(
        fixture.read("deep/k.md").as_deref(),
        Some("[v](../archive/c.md)\n")
    );
    fixture.assert_store_is_a_build_from_zero();
}

/// **A delete's `rewrite_to` must name one document where the plan leaves
/// the vault.** Naming none, naming the very document the delete removes, or
/// naming one another operation of the plan removes leaves the delete
/// unresolved in words; naming several leaves it unresolved with the head of
/// them, as a read heads an ambiguous target. One the plan creates is named.
#[test]
fn a_delete_whose_target_names_no_one_document_is_unresolved_saying_why() {
    let fixture = Fixture::new(&[
        ("a.md", "A\n"),
        ("x/c.md", "X\n"),
        ("y/c.md", "Y\n"),
        ("e.md", "E\n"),
        ("h.md", "[[a]]\n"),
    ]);
    let none = fixture.planned(vec![rewriting("a.md", "zzz")]);
    assert!(
        unresolved_detail(&none).contains("names no one document"),
        "{none:?}"
    );
    let itself = fixture.planned(vec![rewriting("a.md", "a")]);
    let detail = unresolved_detail(&itself);
    assert!(
        detail.contains("the document the delete removes"),
        "{detail}"
    );
    let removed = fixture.planned(vec![rewriting("a.md", "e"), breaking("e.md")]);
    assert!(
        unresolved_detail(&removed).contains("names no one document"),
        "{removed:?}"
    );
    assert_eq!(removed.plan.operations, [breaking("e.md")]);

    let several = fixture.planned(vec![rewriting("a.md", "c")]);
    assert_eq!(
        several.unresolved,
        [UnresolvedOperation::new(
            rewriting("a.md", "c"),
            UnresolvedReason::ambiguous_target(
                CandidateHead::new(
                    [
                        Candidate::new(path("x/c.md"), "x/c"),
                        Candidate::new(path("y/c.md"), "y/c"),
                    ],
                    2,
                )
                .expect("a head of two"),
            ),
        )]
    );
    assert!(several.plan.transitions.is_empty());

    let created = fixture.planned(vec![rewriting("a.md", "n"), creating("n.md", "N\n")]);
    assert!(created.unresolved.is_empty(), "{:?}", created.unresolved);
    assert_eq!(
        created.plan.operations[0].cascade,
        [wikilink("h.md", "a", "n")]
    );
}

/// **A delete's `rewrite_to` is read whatever the document it removes
/// was.** A document the plan itself creates and then deletes stood nowhere
/// before the plan, so no link names it, yet a `rewrite_to` naming no
/// document still leaves the delete unresolved in words, as it would for a
/// document that stood, and the create sharing its file falls with it.
#[test]
fn a_delete_of_a_document_the_plan_creates_still_reads_its_target() {
    let fixture = Fixture::new(&[("h.md", "H\n")]);
    let resolution = fixture.planned(vec![
        creating("transient.md", "T\n"),
        rewriting("transient.md", "absent-replacement"),
    ]);
    let [create, delete] = &resolution.unresolved[..] else {
        panic!(
            "both operations are unresolved: {:?}",
            resolution.unresolved
        );
    };
    assert_eq!(create.operation, creating("transient.md", "T\n"));
    assert_eq!(
        delete.operation,
        rewriting("transient.md", "absent-replacement")
    );
    let UnresolvedReason::NoLongerResolves { detail, .. } = &delete.reason else {
        panic!("the delete no longer resolves: {:?}", delete.reason);
    };
    assert!(detail.contains("names no one document"), "{detail}");
    assert!(resolution.plan.transitions.is_empty());
}

/// **An ambiguous link that could name the deleted document is never
/// rewritten**, and the forecast says it was skipped for its ambiguity; a
/// link naming the document alone beside it is rewritten.
#[test]
fn an_ambiguous_link_is_skipped_ambiguous_by_a_rewriting_delete() {
    let mut fixture = Fixture::new(&[
        ("x/a.md", "X\n"),
        ("y/a.md", "Y\n"),
        ("c.md", "C\n"),
        ("h.md", "[[a]] [[x/a]]\n"),
    ]);
    let resolution = fixture.resolution(vec![rewriting("x/a.md", "c")]);
    assert_eq!(
        resolution.plan.operations[0].cascade,
        [wikilink("h.md", "x/a", "c")]
    );
    assert_eq!(
        resolution.forecast.links,
        [LinkAdvisory::skipped_ambiguous(key(
            "h.md",
            LinkFamily::Wikilink,
            "a"
        ))]
    );
    applied(fixture.apply(resolution.plan));
    assert_eq!(fixture.read("h.md").as_deref(), Some("[[a]] [[c]]\n"));
    fixture.assert_store_is_a_build_from_zero();
}

/// **A backlink already naming the target where the plan leaves the vault
/// needs no rewrite**: `[[c]]` named the deleted `x/c.md` alone, and names the
/// created `y/c.md` — the delete's target — alone after, so the cascade is
/// empty and the change is recorded without advice.
#[test]
fn a_backlink_already_naming_the_target_after_the_plan_is_left_as_written() {
    let mut fixture = Fixture::new(&[("x/c.md", "X\n"), ("h.md", "[[c]]\n")]);
    let resolution = fixture.resolution(vec![rewriting("x/c.md", "c"), creating("y/c.md", "Y\n")]);
    assert!(resolution.plan.operations[0].cascade.is_empty());
    assert_eq!(
        resolution.plan.conditions,
        [PlanCondition::link_resolution(
            key("h.md", LinkFamily::Wikilink, "c"),
            Resolves::one(path("x/c.md")),
            Resolves::one(path("y/c.md")),
        )]
    );
    assert!(resolution.forecast.links.is_empty());
    applied(fixture.apply(resolution.plan));
    assert_eq!(fixture.read("h.md").as_deref(), Some("[[c]]\n"));
}

/// **A backlink another writer adds after the preview refuses the apply, and
/// the fresh plan answers for it.** The applier computes the set again and
/// meets `k.md`'s link as an entry the plan does not record: for a rewriting
/// delete the fresh plan's cascade rewrites it too and applies; for a plain
/// delete the fresh plan leaves the delete unresolved, naming `k.md`.
#[test]
fn a_backlink_added_after_preview_refuses_and_the_fresh_plan_answers_for_it() {
    let unrecorded = || {
        norn_wire::RefusedCheck::condition_unrecorded(PlanCondition::link_resolution(
            key("k.md", LinkFamily::Wikilink, "a"),
            Resolves::one(path("a.md")),
            Resolves::none(),
        ))
    };

    let mut fixture = Fixture::new(&[("a.md", "A\n"), ("c.md", "C\n"), ("h.md", "[[a]]\n")]);
    let plan = fixture.plan(vec![rewriting("a.md", "c")]);
    assert_eq!(plan.operations[0].cascade, [wikilink("h.md", "a", "c")]);
    fixture.foreign("k.md", "[[a]]\n");
    let refused = super::refused(fixture.apply(plan));
    assert_eq!(refused.checks, [unrecorded()]);
    assert_eq!(
        refused.plan.operations,
        [rewriting("a.md", "c")
            .with_cascade(vec![wikilink("h.md", "a", "c"), wikilink("k.md", "a", "c")])]
    );
    applied(fixture.apply(refused.plan));
    assert_eq!(fixture.read("h.md").as_deref(), Some("[[c]]\n"));
    assert_eq!(fixture.read("k.md").as_deref(), Some("[[c]]\n"));
    fixture.assert_store_is_a_build_from_zero();

    let mut fixture = Fixture::new(&[("a.md", "A\n")]);
    let plan = fixture.plan(vec![deleting("a.md")]);
    fixture.foreign("k.md", "[[a]]\n");
    let refused = super::refused(fixture.apply(plan));
    assert_eq!(refused.checks, [unrecorded()]);
    assert_eq!(
        refused.unresolved,
        [UnresolvedOperation::new(
            deleting("a.md"),
            UnresolvedReason::has_backlinks(vec![path("k.md")], 1),
        )]
    );
    assert!(refused.plan.operations.is_empty());
    assert_eq!(fixture.read("a.md").as_deref(), Some("A\n"));
}

/// **A delete whose path the plan refills still reads its backlinks.**
/// Deleting `a.md` and creating another document there leaves a document at
/// the path on both sides, but not the one a link naming it named, so the
/// plan records every link naming it and the applier computes them again: a
/// backlink another writer adds after the preview is an entry the plan does
/// not record, and refuses it. A plain delete's fresh plan is left
/// unresolved for that backlink; one rewriting its links to the document
/// refilling the path records the backlink it already has, and its fresh
/// plan, recording the new one too, lands.
#[test]
fn a_backlink_added_after_preview_to_a_deleted_and_refilled_path_refuses_the_apply() {
    let unrecorded = || {
        norn_wire::RefusedCheck::condition_unrecorded(PlanCondition::link_resolution(
            key("k.md", LinkFamily::Wikilink, "a"),
            Resolves::one(path("a.md")),
            Resolves::one(path("a.md")),
        ))
    };

    let mut fixture = Fixture::new(&[("a.md", "Old\n")]);
    let plan = fixture.plan(vec![deleting("a.md"), creating("a.md", "New\n")]);
    assert_eq!(plan.conditions, []);
    fixture.foreign("k.md", "[[a]]\n");
    let refused = super::refused(fixture.apply(plan));
    assert_eq!(refused.checks, [unrecorded()]);
    assert!(
        refused.unresolved.contains(&UnresolvedOperation::new(
            deleting("a.md"),
            UnresolvedReason::has_backlinks(vec![path("k.md")], 1),
        )),
        "{:?}",
        refused.unresolved
    );
    assert_eq!(fixture.read("a.md").as_deref(), Some("Old\n"));

    let mut fixture = Fixture::new(&[("a.md", "Old\n"), ("h.md", "[[a]]\n")]);
    let plan = fixture.plan(vec![rewriting("a.md", "a"), creating("a.md", "New\n")]);
    assert_eq!(
        plan.conditions,
        [PlanCondition::link_resolution(
            key("h.md", LinkFamily::Wikilink, "a"),
            Resolves::one(path("a.md")),
            Resolves::one(path("a.md")),
        )]
    );
    fixture.foreign("k.md", "[[a]]\n");
    let refused = super::refused(fixture.apply(plan));
    assert_eq!(refused.checks, [unrecorded()]);
    assert_eq!(fixture.read("a.md").as_deref(), Some("Old\n"));
    applied(fixture.apply(refused.plan));
    assert_eq!(fixture.read("a.md").as_deref(), Some("New\n"));
    assert_eq!(fixture.read("h.md").as_deref(), Some("[[a]]\n"));
    assert_eq!(fixture.read("k.md").as_deref(), Some("[[a]]\n"));
    fixture.assert_store_is_a_build_from_zero();
}

/// **Re-sending an interrupted rewriting delete finishes its cascade.** One
/// holder landed before the interruption, and the store took it in; the
/// re-send finds it at its after-state, rewrites the holder that did not
/// land, removes the document, and commits what a build from zero holds.
#[test]
fn resending_an_interrupted_rewriting_delete_finishes_its_cascade() {
    let mut fixture = Fixture::new(&[
        ("a.md", "A\n"),
        ("c.md", "C\n"),
        ("h.md", "[[a]]\n"),
        ("k.md", "[[a]] too\n"),
    ]);
    let plan = fixture.plan(vec![rewriting("a.md", "c")]);
    fixture.foreign("h.md", "[[c]]\n");
    applied(fixture.apply(plan));
    assert_eq!(fixture.read("a.md"), None);
    assert_eq!(fixture.read("h.md").as_deref(), Some("[[c]]\n"));
    assert_eq!(fixture.read("k.md").as_deref(), Some("[[c]] too\n"));
    fixture.assert_store_is_a_build_from_zero();
}
