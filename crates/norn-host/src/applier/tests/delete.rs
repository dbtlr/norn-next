//! A delete's backlinks planned over a vault on disk and the store beside it,
//! then judged and applied by the applier: which links are a deleted
//! document's backlinks, what a delete forbidding them answers, what one
//! leaving them broken lands, and the cascade one rewriting them carries.

use norn_wire::{
    AmbiguousEnd, Candidate, CandidateHead, LinkAdvisory, LinkFamily, LinkKey, LinkRewrite,
    Operation, OperationKind, PlanCondition, ResolutionTarget, ResolvedPlan, Resolves,
    UnresolvedOperation, UnresolvedReason,
};

use super::{Fixture, applied, breaking, creating, deleting, editing, moving, path};

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

/// **Every cascade rewrite of a rewriting delete publishes before the
/// document is removed**: the own writes the applier records, in the order
/// they land, put each holder the delete respells ahead of the removal, so a
/// crash between any two leaves no link naming a document already gone.
#[test]
fn a_rewriting_deletes_cascade_publishes_before_its_document_is_removed() {
    let mut fixture = Fixture::new(&[
        ("a.md", "A\n"),
        ("c.md", "C\n"),
        ("h.md", "[[a]]\n"),
        ("deep/k.md", "[k](../a.md)\n"),
    ]);
    let plan = fixture.plan(vec![rewriting("a.md", "c")]);
    assert_eq!(
        plan.operations[0].cascade,
        [
            markdown("deep/k.md", "../a.md", "../c.md"),
            wikilink("h.md", "a", "c")
        ]
    );
    applied(fixture.apply(plan));
    let recorded = fixture.recorded.calls.borrow();
    let landed = |at: &str| {
        recorded
            .iter()
            .position(|(published, holds)| *holds && published == std::path::Path::new(at))
            .unwrap_or_else(|| panic!("`{at}` is recorded landing: {recorded:?}"))
    };
    for holder in ["deep/k.md", "h.md"] {
        assert!(
            landed(holder) < landed("a.md"),
            "`{holder}` lands before the removal: {recorded:?}"
        );
    }
    assert_eq!(recorded.len(), 3, "{recorded:?}");
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
                AmbiguousEnd::Target,
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

/// **A holder the plan moves names the delete's target from where it
/// lands.** `x/h.md` names `x/a.md` by a relative Markdown link and by a bare
/// wikilink, and moves to `deep/h.md` in the plan deleting `x/a.md` rewriting
/// its links to `x/c.md`: each link is respelled once, to the target, from
/// the holder's new folder — the Markdown link keeping its anchor and title —
/// whichever of the move and the delete comes first.
#[test]
fn a_moved_holder_of_a_rewriting_deletes_backlink_names_its_target_from_where_it_lands() {
    for delete_first in [true, false] {
        let mut fixture = Fixture::new(&[
            ("x/a.md", "A\n"),
            ("x/c.md", "C\n"),
            ("x/h.md", "[keep text](a.md#part \"Title\") [[a]]\n"),
        ]);
        let (delete, moved) = (rewriting("x/a.md", "x/c"), moving("x/h.md", "deep/h.md"));
        let operations = if delete_first {
            vec![delete, moved]
        } else {
            vec![moved, delete]
        };
        let resolution = fixture.resolution(operations);
        assert!(
            resolution.forecast.links.is_empty(),
            "delete first {delete_first}: {:?}",
            resolution.forecast.links
        );
        let mut cascade: Vec<LinkRewrite> = resolution
            .plan
            .operations
            .iter()
            .flat_map(|operation| operation.cascade.iter().cloned())
            .collect();
        cascade.sort_by(|left, right| left.path.cmp(&right.path).then(left.from.cmp(&right.from)));
        assert_eq!(
            cascade,
            [
                wikilink("deep/h.md", "a", "c"),
                markdown("deep/h.md", "a.md", "../x/c.md"),
            ],
            "delete first {delete_first}"
        );
        applied(fixture.apply(resolution.plan));
        assert_eq!(fixture.read("x/a.md"), None);
        assert_eq!(
            fixture.read("deep/h.md").as_deref(),
            Some("[keep text](../x/c.md#part \"Title\") [[c]]\n"),
            "delete first {delete_first}"
        );
        fixture.assert_store_is_a_build_from_zero();
    }
}

/// **A document moved and then deleted in one plan has the backlinks it had
/// before the move**, read by the delete's choice and never by the move:
/// `[[a]]` and `[x](a.md)` named `a.md`, which moves to `b.md` and is
/// deleted there. Forbidding them, the delete is left unresolved naming
/// their holder; rewriting them, each is respelled to the delete's target,
/// never to `b.md`; leaving them broken, each is advised left broken, and
/// the move carries no cascade toward a document the plan removes.
#[test]
fn a_document_moved_then_deleted_has_its_backlinks_read_by_the_deletes_choice() {
    let files = [
        ("a.md", "A\n"),
        ("c.md", "C\n"),
        ("h.md", "[[a]] [x](a.md)\n"),
    ];

    let forbidden = Fixture::new(&files).planned(vec![moving("a.md", "b.md"), deleting("b.md")]);
    assert!(
        forbidden.unresolved.contains(&UnresolvedOperation::new(
            deleting("b.md"),
            UnresolvedReason::has_backlinks(vec![path("h.md")], 2),
        )),
        "{:?}",
        forbidden.unresolved
    );
    assert!(forbidden.plan.transitions.is_empty());

    let mut fixture = Fixture::new(&files);
    let resolution = fixture.resolution(vec![moving("a.md", "b.md"), rewriting("b.md", "c")]);
    assert!(resolution.plan.operations[0].cascade.is_empty());
    assert_eq!(
        resolution.plan.operations[1].cascade,
        [markdown("h.md", "a.md", "c.md"), wikilink("h.md", "a", "c"),]
    );
    applied(fixture.apply(resolution.plan));
    assert_eq!(fixture.read("h.md").as_deref(), Some("[[c]] [x](c.md)\n"));
    assert_eq!(fixture.read("a.md"), None);
    assert_eq!(fixture.read("b.md"), None);
    fixture.assert_store_is_a_build_from_zero();

    let mut fixture = Fixture::new(&files);
    let resolution = fixture.resolution(vec![moving("a.md", "b.md"), breaking("b.md")]);
    assert!(
        resolution
            .plan
            .operations
            .iter()
            .all(|operation| operation.cascade.is_empty()),
        "{:?}",
        resolution.plan.operations
    );
    assert_eq!(
        resolution.forecast.links,
        [
            LinkAdvisory::left_broken(key("h.md", LinkFamily::Markdown, "a.md")),
            LinkAdvisory::left_broken(key("h.md", LinkFamily::Wikilink, "a")),
        ]
    );
    applied(fixture.apply(resolution.plan));
    assert_eq!(fixture.read("h.md").as_deref(), Some("[[a]] [x](a.md)\n"));
    fixture.assert_store_is_a_build_from_zero();
}

/// **A link a cascade respells is judged by the text it had, never by what
/// its new spelling named before the plan.** In each plan a plain delete
/// removes `v/a.md` while a move carries `v/b.md` to a path that `v/h.md`'s
/// links to it are respelled to name as `a`: into the deleted document's
/// path directly, through a stand-in name, or into another folder. Those
/// links resolved to `v/b.md` before the plan, so none is a backlink of the
/// deleted document: each plan previews as planned, the applier's check of
/// it agrees, and it lands with the links naming the moved document.
#[test]
fn a_link_a_move_respells_toward_a_deleted_documents_name_is_no_backlink_of_it() {
    let plans = [
        (
            "[x](b.md) [[b]]\n",
            vec![deleting("v/a.md"), moving("v/b.md", "v/a.md")],
            "v/a.md",
            "[x](a.md) [[a]]\n",
        ),
        (
            "[x](b.md) [[b]]\n",
            vec![
                moving("v/b.md", "v/z.md"),
                deleting("v/a.md"),
                moving("v/z.md", "v/a.md"),
            ],
            "v/a.md",
            "[x](a.md) [[a]]\n",
        ),
        (
            "[[b]]\n",
            vec![deleting("v/a.md"), moving("v/b.md", "v/x/a.md")],
            "v/x/a.md",
            "[[a]]\n",
        ),
    ];
    for (held, operations, landed, respelled) in plans {
        let mut fixture = Fixture::new(&[("v/a.md", "A\n"), ("v/b.md", "B\n"), ("v/h.md", held)]);
        let resolution = fixture.resolution(operations.clone());
        let (previewed, forecast) = fixture
            .preview(resolution.plan.clone())
            .unwrap_or_else(|refusal| panic!("{operations:?} previews: {refusal:?}"));
        assert_eq!(previewed, resolution.plan, "{operations:?}");
        assert_eq!(forecast.links, resolution.forecast.links, "{operations:?}");
        applied(fixture.apply(resolution.plan));
        assert_eq!(
            fixture.read(landed).as_deref(),
            Some("B\n"),
            "{operations:?}"
        );
        assert_eq!(fixture.read("v/b.md"), None, "{operations:?}");
        assert_eq!(
            fixture.read("v/h.md").as_deref(),
            Some(respelled),
            "{operations:?}"
        );
        fixture.assert_store_is_a_build_from_zero();
    }
}

/// **A link that itself named the deleted document stays its backlink where
/// a cascade respells another link to the same address beside it.** `[[a]]`
/// in `v/h.md` named `v/a.md` before the plan, and the move's cascade
/// respells `[[b]]` to `[[a]]` next to it: a plain delete of `v/a.md` is
/// left unresolved for that one link. A plan leaving it broken, sent back
/// with its delete forbidding it, is refused by the applier as invalid for
/// the same link.
#[test]
fn a_backlink_beside_a_link_respelled_to_its_address_still_refuses_a_plain_delete() {
    let files = [
        ("v/a.md", "A\n"),
        ("v/b.md", "B\n"),
        ("v/h.md", "[x](b.md) [[b]] [[a]]\n"),
    ];
    let refused =
        Fixture::new(&files).planned(vec![deleting("v/a.md"), moving("v/b.md", "v/a.md")]);
    assert!(
        refused.unresolved.contains(&UnresolvedOperation::new(
            deleting("v/a.md"),
            UnresolvedReason::has_backlinks(vec![path("v/h.md")], 1),
        )),
        "{:?}",
        refused.unresolved
    );

    let mut fixture = Fixture::new(&files);
    let mut plan = fixture.plan(vec![breaking("v/a.md"), moving("v/b.md", "v/a.md")]);
    assert_eq!(
        plan.operations[1].cascade,
        [
            markdown("v/h.md", "b.md", "a.md"),
            wikilink("v/h.md", "b", "a")
        ]
    );
    let OperationKind::DeleteDocument { backlinks, .. } = &mut plan.operations[0].kind else {
        panic!(
            "the plan's first operation is the delete: {:?}",
            plan.operations
        );
    };
    *backlinks = norn_wire::Backlinks::Forbidden;
    assert_eq!(fixture.refuses_disagreeing(plan), [path("v/a.md")]);
    assert_eq!(
        fixture.read("v/h.md").as_deref(),
        Some("[x](b.md) [[b]] [[a]]\n")
    );
}

/// The plan `operations` resolve to, with the delete at `position` sent back
/// forbidding the links naming its document.
fn forbidding_at(fixture: &Fixture, operations: Vec<Operation>, position: usize) -> ResolvedPlan {
    let mut plan = fixture.plan(operations);
    let OperationKind::DeleteDocument { backlinks, .. } = &mut plan.operations[position].kind
    else {
        panic!(
            "the plan's operation {position} is the delete: {:?}",
            plan.operations
        );
    };
    *backlinks = norn_wire::Backlinks::Forbidden;
    plan
}

/// **A link a cascade respells is judged by the text it had, so one whose
/// text named the deleted document stays its backlink however it is
/// respelled.** `v/h.md`'s `[y](a.md)` named `v/a.md`, and moving `v/h.md` to
/// `w/h.md` respells it `../v/a.md` from there — alone, and beside `[[b]]`
/// respelled `[[a]]` toward `v/b.md` moving into the deleted document's
/// path. A plain delete of `v/a.md` is left unresolved for that one link;
/// a plan leaving it broken, sent back with its delete forbidding it, is
/// refused by the applier as invalid, and nothing is written.
#[test]
fn a_backlink_a_moved_holder_respells_still_refuses_a_plain_delete() {
    let plans = [
        (
            "[y](a.md)\n",
            vec![deleting("v/a.md"), moving("v/h.md", "w/h.md")],
        ),
        (
            "[[b]] [y](a.md)\n",
            vec![
                deleting("v/a.md"),
                moving("v/b.md", "v/a.md"),
                moving("v/h.md", "w/h.md"),
            ],
        ),
    ];
    for (held, operations) in plans {
        let files = [("v/a.md", "A\n"), ("v/b.md", "B\n"), ("v/h.md", held)];
        let refused = Fixture::new(&files).planned(operations.clone());
        assert!(
            refused.unresolved.contains(&UnresolvedOperation::new(
                deleting("v/a.md"),
                UnresolvedReason::has_backlinks(vec![path("w/h.md")], 1),
            )),
            "{operations:?}: {:?}",
            refused.unresolved
        );

        let mut fixture = Fixture::new(&files);
        let mut breaking_first = operations.clone();
        breaking_first[0] = breaking("v/a.md");
        let plan = forbidding_at(&fixture, breaking_first, 0);
        assert!(
            plan.operations
                .iter()
                .flat_map(|operation| &operation.cascade)
                .any(|rewrite| *rewrite == markdown("w/h.md", "a.md", "../v/a.md")),
            "{:?}",
            plan.operations
        );
        assert_eq!(
            fixture.refuses_disagreeing(plan),
            [path("v/a.md")],
            "{operations:?}"
        );
        assert_eq!(fixture.read("v/a.md").as_deref(), Some("A\n"));
        assert_eq!(fixture.read("v/h.md").as_deref(), Some(held));
        assert_eq!(fixture.read("w/h.md"), None);
    }
}

/// **A hand-built cascade that respells a backlink away from a forbidding
/// delete's document leaves it a backlink**: a plan rewriting `a.md`'s links
/// to `c` beside a move of `m.md`, sent back with its delete forbidding them
/// and its cascade respelling `h.md`'s `[[a]]` to `[[c]]` carried by the
/// move, still holds a link whose text named `a.md`. The applier refuses it
/// as invalid, and nothing is written.
#[test]
fn a_cascade_respelling_a_forbidding_deletes_backlink_elsewhere_is_invalid() {
    let mut fixture = Fixture::new(&[
        ("a.md", "A\n"),
        ("c.md", "C\n"),
        ("h.md", "[[a]]\n"),
        ("m.md", "M\n"),
    ]);
    let mut plan = forbidding_at(
        &fixture,
        vec![rewriting("a.md", "c"), moving("m.md", "n.md")],
        0,
    );
    let cascade = std::mem::take(&mut plan.operations[0].cascade);
    assert_eq!(cascade, [wikilink("h.md", "a", "c")]);
    plan.operations[1].cascade = cascade;
    assert_eq!(fixture.refuses_disagreeing(plan), [path("a.md")]);
    assert_eq!(fixture.read("a.md").as_deref(), Some("A\n"));
    assert_eq!(fixture.read("h.md").as_deref(), Some("[[a]]\n"));
}

/// **A resolved plan whose delete says another link choice than its plan
/// does is invalid**, judged from the set the applier computes again and
/// nothing more. A plan previewed leaving `a.md`'s links broken, sent back
/// with its delete forbidding them, records a link naming `a.md` that its
/// delete forbids; a plan previewed rewriting them to `c`, sent back with
/// its delete rewriting them to a name no document holds, or to `a` itself,
/// names no one document to rewrite them to. Each answers
/// `request/plan-invalid` naming the deleted document, and nothing is
/// written.
#[test]
fn a_resolved_delete_whose_link_choice_its_plan_does_not_keep_is_invalid() {
    let files = [("a.md", "A\n"), ("c.md", "C\n"), ("h.md", "[[a]]\n")];

    let mut fixture = Fixture::new(&files);
    let mut plan = fixture.plan(vec![breaking("a.md")]);
    let OperationKind::DeleteDocument { backlinks, .. } = &mut plan.operations[0].kind else {
        panic!(
            "the plan's one operation is the delete: {:?}",
            plan.operations
        );
    };
    *backlinks = norn_wire::Backlinks::Forbidden;
    assert_eq!(fixture.refuses_disagreeing(plan), [path("a.md")]);
    assert_eq!(fixture.read("a.md").as_deref(), Some("A\n"));
    assert_eq!(fixture.read("h.md").as_deref(), Some("[[a]]\n"));

    for retargeted in ["zzz", "a"] {
        let mut fixture = Fixture::new(&files);
        let mut plan = fixture.plan(vec![rewriting("a.md", "c")]);
        let OperationKind::DeleteDocument { backlinks, .. } = &mut plan.operations[0].kind else {
            panic!(
                "the plan's one operation is the delete: {:?}",
                plan.operations
            );
        };
        *backlinks =
            norn_wire::Backlinks::RewrittenTo(ResolutionTarget::new(retargeted).expect("a target"));
        assert_eq!(
            fixture.refuses_disagreeing(plan),
            [path("a.md")],
            "rewritten to {retargeted}"
        );
        assert_eq!(fixture.read("a.md").as_deref(), Some("A\n"));
        assert_eq!(fixture.read("h.md").as_deref(), Some("[[a]]\n"));
    }
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

/// **A delete of a quarantined file is never refused for backlinks.** No
/// link resolves to a file whose bytes do not decode, so `[[q]]`,
/// `[x](q.md)` and `![[q]]` name nothing before the delete and are no
/// backlinks of it: the plain delete, which forbids the links naming its
/// document, resolves with no entry and no advisory, previews as planned,
/// and applies with the linkers untouched.
#[test]
fn a_plain_delete_of_a_quarantined_file_is_never_refused_for_backlinks() {
    let mut fixture = Fixture::new(&[("h.md", "[[q]] and [x](q.md)\n"), ("k.md", "![[q]]\n")]);
    fixture.foreign("q.md", super::UNDECODABLE);
    let resolution = fixture.planned(vec![deleting("q.md")]);
    assert_eq!(resolution.unresolved, []);
    assert_eq!(resolution.plan.operations, [deleting("q.md")]);
    assert_eq!(resolution.plan.conditions, []);
    assert!(resolution.forecast.links.is_empty());
    let (previewed, forecast) = fixture
        .preview(resolution.plan.clone())
        .expect("the plan previews");
    assert_eq!(previewed, resolution.plan);
    assert_eq!(forecast.links, resolution.forecast.links);
    applied(fixture.apply(resolution.plan));
    assert_eq!(fixture.read("q.md"), None);
    assert_eq!(
        fixture.read("h.md").as_deref(),
        Some("[[q]] and [x](q.md)\n")
    );
    assert_eq!(fixture.read("k.md").as_deref(), Some("![[q]]\n"));
    fixture.assert_store_is_a_build_from_zero();
}

/// **A `rewrite_to` naming a quarantined file names no document.** The
/// bytes at `q.md` do not decode, so no link can be respelled toward them:
/// a delete rewriting the links naming `a.md` to `q`, or to `q.md`, is left
/// unresolved as one whose target names no one document, and writes
/// nothing.
#[test]
fn a_rewrite_to_naming_a_quarantined_file_is_unresolved() {
    let mut fixture = Fixture::new(&[("a.md", "A\n"), ("h.md", "[[a]]\n")]);
    fixture.foreign("q.md", super::UNDECODABLE);
    for target in ["q", "q.md"] {
        let resolution = fixture.planned(vec![rewriting("a.md", target)]);
        assert!(
            unresolved_detail(&resolution).contains("names no one document"),
            "{resolution:?}"
        );
        assert!(resolution.plan.transitions.is_empty());
    }
}

/// **An unplaced frontmatter link is a backlink like any other**: a plain
/// delete of the document a flow-sequence wikilink names is unresolved naming
/// its holder, a delete leaving links broken advises it, and a rewriting
/// delete leaves it as written, skipped with its reason.
#[test]
fn an_unplaced_backlink_counts_for_every_delete() {
    let holder = "---\nsee: [\"[[a]]\"]\n---\nbody\n";
    let fixture = Fixture::new(&[("a.md", "A\n"), ("c.md", "C\n"), ("h.md", holder)]);
    let resolution = fixture.planned(vec![deleting("a.md")]);
    assert_eq!(
        resolution.unresolved,
        [UnresolvedOperation::new(
            deleting("a.md"),
            UnresolvedReason::has_backlinks(vec![path("h.md")], 1),
        )]
    );

    let link = key("h.md", LinkFamily::Wikilink, "a");
    let broken = fixture.resolution(vec![breaking("a.md")]);
    assert_eq!(
        broken.forecast.links,
        vec![LinkAdvisory::left_broken(link.clone())]
    );

    let mut fixture = fixture;
    let rewritten = fixture.resolution(vec![rewriting("a.md", "c")]);
    assert_eq!(
        rewritten.forecast.links,
        vec![LinkAdvisory::skipped_unplaced(link)]
    );
    applied(fixture.apply(rewritten.plan));
    assert_eq!(fixture.read("a.md"), None);
    assert_eq!(fixture.read("h.md").as_deref(), Some(holder));
    fixture.assert_store_is_a_build_from_zero();
}
