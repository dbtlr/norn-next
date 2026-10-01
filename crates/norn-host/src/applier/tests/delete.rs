//! A delete's backlinks planned over a vault on disk and the store beside it,
//! then judged and applied by the applier: which links are a deleted
//! document's backlinks, what a delete forbidding them answers, and what one
//! leaving them broken lands.

use norn_wire::{
    LinkAdvisory, LinkFamily, LinkKey, PlanCondition, Resolves, UnresolvedOperation,
    UnresolvedReason,
};

use super::{Fixture, applied, breaking, creating, deleting, editing, path};

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
