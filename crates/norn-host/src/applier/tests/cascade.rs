//! Link cascades planned over a vault on disk and the store beside it, then
//! judged and applied by the applier: what a move respells, what it leaves
//! as written and why, and that a planned cascade recomposes and its
//! resolution change set reproduces.

use std::collections::BTreeSet;

use norn_wire::{
    AuthoredPlan, LinkAdvisory, LinkFamily, LinkKey, LinkRewrite, Operation, PlanCondition,
    Resolves, UnresolvedReason,
};

use super::{Fixture, applied, deleting, editing, moving, path};
use crate::planner::control::SchemaPlace;
use crate::planner::resolve::Resolution;
use crate::planner::view::TreeView;

fn wikilink(holder: &str, from: &str, to: &str) -> LinkRewrite {
    LinkRewrite::new(path(holder), LinkFamily::Wikilink, from, to)
}

fn markdown(holder: &str, from: &str, to: &str) -> LinkRewrite {
    LinkRewrite::new(path(holder), LinkFamily::Markdown, from, to)
}

fn key(holder: &str, syntax: LinkFamily, address: &str) -> LinkKey {
    LinkKey::new(path(holder), syntax, address)
}

impl Fixture {
    /// `operations` planned against the vault, its links judged on the store
    /// as it stands, whether or not every operation resolves.
    pub(super) fn planned(&self, operations: Vec<Operation>) -> Resolution {
        let view = TreeView::open(&self.vault, &self.exclusions, &SchemaPlace::default())
            .expect("a vault");
        let links = self.links();
        crate::planner::resolve::resolve(
            AuthoredPlan::new(crate::planner::links::testing::vault(), operations),
            self.root_identity(),
            &BTreeSet::new(),
            &view,
            &links.index(),
        )
        .unwrap_or_else(|failure| panic!("the plan is planned: {failure:?}"))
    }

    /// Plan `operations`, apply the plan, and hand back the cascade each
    /// operation carried, once the store is shown to be a build from zero
    /// over what landed.
    fn moved(&mut self, operations: Vec<Operation>) -> Vec<Vec<LinkRewrite>> {
        let plan = self.plan(operations);
        let cascades = plan
            .operations
            .iter()
            .map(|operation| operation.cascade.clone())
            .collect();
        applied(self.apply(plan));
        self.assert_store_is_a_build_from_zero();
        cascades
    }
}

/// **A move that changes its document's stem respells a bare backlink** to
/// the shortest suffix naming the document alone where it lands.
#[test]
fn a_stem_changing_move_rewrites_a_bare_backlink() {
    let mut fixture = Fixture::new(&[("a.md", "A\n"), ("h.md", "See [[a]] and ![[a#Part|it]].\n")]);
    let cascades = fixture.moved(vec![moving("a.md", "x/b.md")]);
    assert_eq!(cascades, [vec![wikilink("h.md", "a", "b")]]);
    assert_eq!(
        fixture.read("h.md").as_deref(),
        Some("See [[b]] and ![[b#Part|it]].\n")
    );
}

/// **A stem-changing move rewrites a block-reference backlink and keeps all
/// but its name**: `[[a#^block-id|alias]]` and `![[a#^block-id]]` name `b`
/// where `a` lands, each keeping its `#^block-id`, the one its alias and the
/// other its embed marker.
#[test]
fn a_stem_changing_move_rewrites_a_block_reference_keeping_its_anchor_alias_and_embed() {
    let mut fixture = Fixture::new(&[
        ("a.md", "A paragraph ^block-id\n"),
        ("h.md", "See [[a#^block-id|alias]].\n\n![[a#^block-id]]\n"),
    ]);
    let cascades = fixture.moved(vec![moving("a.md", "x/b.md")]);
    assert_eq!(cascades, [vec![wikilink("h.md", "a", "b")]]);
    assert_eq!(
        fixture.read("h.md").as_deref(),
        Some("See [[b#^block-id|alias]].\n\n![[b#^block-id]]\n")
    );
}

/// **A cascade rewrites every occurrence of a backlink, not the first it
/// matches**: one holder names `a` three times under three aliases — once in
/// a double-quoted frontmatter value and twice in its body — and the one
/// rewrite of its key respells all three.
#[test]
fn a_cascade_rewrites_every_occurrence_of_a_backlink_in_its_holder() {
    let mut fixture = Fixture::new(&[
        ("a.md", "A\n"),
        (
            "h.md",
            "---\nsee: \"[[a|first]]\"\n---\n[[a|second]] and later [[a|third]]\n",
        ),
    ]);
    let cascades = fixture.moved(vec![moving("a.md", "b.md")]);
    assert_eq!(cascades, [vec![wikilink("h.md", "a", "b")]]);
    assert_eq!(
        fixture.read("h.md").as_deref(),
        Some("---\nsee: \"[[b|first]]\"\n---\n[[b|second]] and later [[b|third]]\n")
    );
}

/// **Every cascade rewrite publishes before the move removes its source**:
/// the own writes the applier records, in the order they land, put each
/// holder the cascade respells after the moved document's destination and
/// before the removal of the name it left, so a crash between any two leaves
/// every link naming a document that stands.
#[test]
fn a_moves_cascade_rewrites_publish_before_its_source_is_removed() {
    let mut fixture = Fixture::new(&[
        ("a.md", "A\n"),
        ("h.md", "[[a]]\n"),
        ("deep/k.md", "![[a#Part]]\n"),
    ]);
    let plan = fixture.plan(vec![moving("a.md", "x/b.md")]);
    assert_eq!(
        plan.operations[0].cascade,
        [wikilink("deep/k.md", "a", "b"), wikilink("h.md", "a", "b")]
    );
    applied(fixture.apply(plan));
    let recorded = fixture.recorded.calls.borrow();
    let landed = |at: &str| {
        recorded
            .iter()
            .position(|(published, holds)| *holds && published == std::path::Path::new(at))
            .unwrap_or_else(|| panic!("`{at}` is recorded landing: {recorded:?}"))
    };
    let removed = landed("a.md");
    for holder in ["deep/k.md", "h.md"] {
        assert!(
            landed("x/b.md") < landed(holder) && landed(holder) < removed,
            "`{holder}` lands between the destination and the source's removal: {recorded:?}"
        );
    }
    assert_eq!(recorded.len(), 4, "{recorded:?}");
}

/// **A move keeping its document's stem leaves a backlink naming it alone as
/// written**: `[[a]]` names it where it lands, so nothing is rewritten, and
/// the change of what the link names is recorded and not advised on.
#[test]
fn a_stem_preserving_move_leaves_a_unique_bare_backlink_alone() {
    let mut fixture = Fixture::new(&[("a.md", "A\n"), ("h.md", "[[a]]\n")]);
    let resolution = fixture.resolution(vec![moving("a.md", "x/a.md")]);
    assert!(resolution.plan.operations[0].cascade.is_empty());
    assert_eq!(
        resolution.plan.conditions,
        vec![PlanCondition::link_resolution(
            key("h.md", LinkFamily::Wikilink, "a"),
            Resolves::one(path("a.md")),
            Resolves::one(path("x/a.md")),
        )]
    );
    assert!(resolution.forecast.links.is_empty());
    applied(fixture.apply(resolution.plan));
    assert_eq!(fixture.read("h.md").as_deref(), Some("[[a]]\n"));
}

/// **A backlink that would be ambiguous where the document lands takes the
/// minimal suffix naming it alone**, keeping the extension only where the
/// link was written with it.
#[test]
fn a_backlink_ambiguous_after_takes_the_minimal_suffix() {
    let mut fixture = Fixture::new(&[
        ("a.md", "A\n"),
        ("y/b.md", "B\n"),
        ("h.md", "[[a]] [[a.md]]\n"),
    ]);
    let cascades = fixture.moved(vec![moving("a.md", "x/b.md")]);
    assert_eq!(
        cascades,
        [vec![
            wikilink("h.md", "a", "x/b"),
            wikilink("h.md", "a.md", "x/b.md"),
        ]]
    );
    assert_eq!(
        fixture.read("h.md").as_deref(),
        Some("[[x/b]] [[x/b.md]]\n")
    );
}

/// **A path-qualified backlink stays path-qualified**: it is respelled to the
/// shortest suffix of at least two segments naming the document alone, even
/// where its stem alone would.
#[test]
fn a_path_qualified_backlink_stays_path_qualified() {
    let mut fixture = Fixture::new(&[("notes/a.md", "A\n"), ("h.md", "[[notes/a]]\n")]);
    let cascades = fixture.moved(vec![moving("notes/a.md", "archive/deep/b.md")]);
    assert_eq!(cascades, [vec![wikilink("h.md", "notes/a", "deep/b")]]);
    assert_eq!(fixture.read("h.md").as_deref(), Some("[[deep/b]]\n"));
}

/// **A `vault://` backlink keeps its protocol** and names the document's root
/// path where it lands, with the extension where it was written with one.
#[test]
fn a_vault_protocol_backlink_keeps_its_protocol() {
    let mut fixture = Fixture::new(&[
        ("notes/a.md", "A\n"),
        ("h.md", "[[vault://notes/a]] [[vault://notes/a.md]]\n"),
    ]);
    let cascades = fixture.moved(vec![moving("notes/a.md", "archive/b.md")]);
    assert_eq!(
        cascades,
        [vec![
            wikilink("h.md", "vault://notes/a", "vault://archive/b"),
            wikilink("h.md", "vault://notes/a.md", "vault://archive/b.md"),
        ]]
    );
    assert_eq!(
        fixture.read("h.md").as_deref(),
        Some("[[vault://archive/b]] [[vault://archive/b.md]]\n")
    );
}

/// **A Markdown backlink is respelled from its holder**: a relative one from
/// the holder's folder, a rooted one from the root, each in the escaping it
/// was written in, its anchor kept.
#[test]
fn a_markdown_backlink_is_respelled_from_its_holder() {
    let mut fixture = Fixture::new(&[
        ("notes/my a.md", "A\n"),
        (
            "sub/h.md",
            "[r](../notes/my%20a.md#Part) [s](/notes/my%20a.md) [t](<../notes/my a.md>)\n",
        ),
    ]);
    let cascades = fixture.moved(vec![moving("notes/my a.md", "archive/my b.md")]);
    assert_eq!(
        cascades,
        [vec![
            markdown("sub/h.md", "../notes/my a.md", "../archive/my b.md"),
            markdown("sub/h.md", "../notes/my%20a.md", "../archive/my%20b.md"),
            markdown("sub/h.md", "/notes/my%20a.md", "/archive/my%20b.md"),
        ]]
    );
    assert_eq!(
        fixture.read("sub/h.md").as_deref(),
        Some("[r](../archive/my%20b.md#Part) [s](/archive/my%20b.md) [t](<../archive/my b.md>)\n")
    );
}

/// **An ambiguous backlink is skipped as ambiguous**: `[[a]]` names two
/// documents, one of them moved, so which it names is not known; it is left
/// as written, and the forecast says why in place of saying it is
/// retargeted.
#[test]
fn an_ambiguous_backlink_is_skipped_ambiguous() {
    let mut fixture = Fixture::new(&[("x/a.md", "X\n"), ("y/a.md", "Y\n"), ("h.md", "[[a]]\n")]);
    let resolution = fixture.resolution(vec![moving("x/a.md", "z/c.md")]);
    assert!(resolution.plan.operations[0].cascade.is_empty());
    assert_eq!(
        resolution.forecast.links,
        vec![LinkAdvisory::skipped_ambiguous(key(
            "h.md",
            LinkFamily::Wikilink,
            "a"
        ))]
    );
    applied(fixture.apply(resolution.plan));
    assert_eq!(fixture.read("h.md").as_deref(), Some("[[a]]\n"));
}

/// **A link the text layer cannot respell keeps its entry and says why**: a
/// backlink in a single-quoted frontmatter value cannot carry a quote, so the
/// moved document's new name leaves it as written, recorded going from the
/// document to none, and advised on with the text layer's reason.
#[test]
fn a_backlink_the_text_layer_cannot_respell_is_left_with_its_reason() {
    let mut fixture = Fixture::new(&[("a.md", "A\n"), ("h.md", "---\nsee: '[[a]]'\n---\nbody\n")]);
    let resolution = fixture.resolution(vec![moving("a.md", "it's.md")]);
    let link = key("h.md", LinkFamily::Wikilink, "a");
    assert_eq!(
        resolution.plan.conditions,
        vec![PlanCondition::link_resolution(
            link.clone(),
            Resolves::one(path("a.md")),
            Resolves::none(),
        )]
    );
    assert_eq!(
        resolution.forecast.links,
        vec![LinkAdvisory::skipped_would_corrupt_frontmatter(link)]
    );
    applied(fixture.apply(resolution.plan));
    assert_eq!(
        fixture.read("h.md").as_deref(),
        Some("---\nsee: '[[a]]'\n---\nbody\n")
    );
}

/// **A moved document's relative links reach the same files, attachments
/// included**: each is read from where the document stood and respelled from
/// where it lands, an anchor-only link and a wikilink left as they are.
#[test]
fn a_moved_documents_relative_links_reach_the_same_files_attachments_included() {
    let mut fixture = Fixture::new(&[
        (
            "notes/a.md",
            "# H\n[i](img.png) [c](./c.md) [s](#H) [[c]]\n",
        ),
        ("notes/img.png", "png"),
        ("notes/c.md", "C\n"),
    ]);
    let cascades = fixture.moved(vec![moving("notes/a.md", "archive/deep/a.md")]);
    assert_eq!(
        cascades,
        [vec![
            markdown("archive/deep/a.md", "./c.md", "../../notes/c.md"),
            markdown("archive/deep/a.md", "img.png", "../../notes/img.png"),
        ]]
    );
    assert_eq!(
        fixture.read("archive/deep/a.md").as_deref(),
        Some("# H\n[i](../../notes/img.png) [c](../../notes/c.md) [s](#H) [[c]]\n")
    );
}

/// **An edit that does not resolve on a holder leaves the move unresolved**:
/// the move's cascade rewrites the holder, so the two stand or fall
/// together, while a move whose holders nothing else touches resolves.
#[test]
fn an_unresolved_edit_on_a_holder_leaves_the_move_unresolved() {
    let fixture = Fixture::new(&[
        ("a.md", "A\n"),
        ("b.md", "B\n"),
        ("h.md", "[[a]]\n"),
        ("k.md", "[[b]]\n"),
    ]);
    let operations = vec![
        moving("a.md", "x/a2.md"),
        editing("h.md", "missing", "x"),
        moving("b.md", "x/b2.md"),
    ];
    let resolution = fixture.planned(operations.clone());
    let left: Vec<&Operation> = resolution
        .unresolved
        .iter()
        .map(|unresolved| &unresolved.operation)
        .collect();
    assert_eq!(left, [&operations[0], &operations[1]]);
    let UnresolvedReason::NoLongerResolves { detail, .. } = &resolution.unresolved[0].reason else {
        panic!("the move no longer resolves: {:?}", resolution.unresolved);
    };
    assert!(detail.contains("h.md"), "{detail}");
    assert_eq!(
        resolution.plan.operations,
        [moving("b.md", "x/b2.md").with_cascade(vec![wikilink("k.md", "b", "b2")])]
    );
}

/// **A planned cascade recomposes, and its set reproduces**: the applier's
/// judgment of the plan — every holder recomposed from its before-state, the
/// change set computed again — answers the same plan and the same link
/// advisories its planning did, and the plan lands as previewed.
#[test]
fn a_planned_cascade_recomposes_and_its_set_reproduces() {
    let mut fixture = Fixture::new(&[
        ("x/a.md", "A [[c]]\n"),
        ("y/a.md", "Y\n"),
        ("c.md", "C\n"),
        ("h.md", "[[x/a]] [[a]] [t](c.md)\n"),
    ]);
    let resolution = fixture.resolution(vec![
        moving("x/a.md", "z/q.md"),
        moving("c.md", "w/c2.md"),
        editing("h.md", "[t]", "[u]"),
    ]);
    assert!(
        resolution
            .plan
            .operations
            .iter()
            .any(|operation| !operation.cascade.is_empty())
    );
    let (previewed, forecast) = fixture
        .preview(resolution.plan.clone())
        .expect("the planned cascade previews");
    assert_eq!(previewed, resolution.plan);
    assert_eq!(forecast.links, resolution.forecast.links);
    applied(fixture.apply(resolution.plan));
    assert_eq!(
        fixture.read("h.md").as_deref(),
        Some("[[z/q]] [[a]] [u](w/c2.md)\n")
    );
    assert_eq!(fixture.read("z/q.md").as_deref(), Some("A [[c2]]\n"));
    fixture.assert_store_is_a_build_from_zero();
}

/// **An operation carrying a cascade into planning is a fault in the plan**:
/// planning writes every cascade from the links the vault holds, and the one
/// caller re-resolving a planned operation — a refusal's refresh — hands it
/// over as its caller authored it, so a cascade arriving here was never
/// planning's and is not silently dropped.
#[test]
fn a_cascade_carried_into_planning_is_a_fault() {
    let fixture = Fixture::new(&[("a.md", "A\n"), ("h.md", "[[a]]\n")]);
    let carrying = moving("a.md", "b.md").with_cascade(vec![wikilink("h.md", "zzz", "b")]);
    let view = TreeView::open(&fixture.vault, &fixture.exclusions, &SchemaPlace::default())
        .expect("a vault");
    let links = fixture.links();
    let failure = crate::planner::resolve::resolve(
        AuthoredPlan::new(crate::planner::links::testing::vault(), vec![carrying]),
        fixture.root_identity(),
        &BTreeSet::new(),
        &view,
        &links.index(),
    )
    .expect_err("a carried cascade is no plan");
    let crate::planner::resolve::PlanningFailure::Fault(fault) = failure else {
        panic!("a fault in the plan: {failure:?}");
    };
    assert_eq!(fault, norn_wire::PlanFault::misplaced_cascade(vec![0]));
}

/// **A backlink another writer adds after the preview refuses the plan, and
/// the fresh plan's cascade holds it.** The applier computes the set again
/// and meets `k.md`'s link to the moved document as an entry the plan does
/// not record; the fresh plan re-resolves the move, generating its cascade
/// from the links that stand now, and applies.
#[test]
fn a_backlink_added_after_preview_refuses_and_the_fresh_cascade_holds_it() {
    let mut fixture = Fixture::new(&[("a.md", "A\n"), ("h.md", "[[a]]\n")]);
    let plan = fixture.plan(vec![moving("a.md", "b.md")]);
    assert_eq!(plan.operations[0].cascade, [wikilink("h.md", "a", "b")]);
    fixture.foreign("k.md", "[[a]]\n");

    let refused = super::refused(fixture.apply(plan));
    assert_eq!(
        refused.checks,
        vec![norn_wire::RefusedCheck::condition_unrecorded(
            PlanCondition::link_resolution(
                key("k.md", LinkFamily::Wikilink, "a"),
                Resolves::one(path("a.md")),
                Resolves::none(),
            )
        )]
    );
    assert!(refused.unresolved.is_empty(), "{:?}", refused.unresolved);
    assert_eq!(
        refused.plan.operations,
        [moving("a.md", "b.md")
            .with_cascade(vec![wikilink("h.md", "a", "b"), wikilink("k.md", "a", "b"),])]
    );
    applied(fixture.apply(refused.plan));
    assert_eq!(fixture.read("h.md").as_deref(), Some("[[b]]\n"));
    assert_eq!(fixture.read("k.md").as_deref(), Some("[[b]]\n"));
    fixture.assert_store_is_a_build_from_zero();
}

/// **A landed holder with an unlanded destination is part-landed.** The
/// holder already reads as the cascade leaves it and the move's destination
/// does not; another writer then changes the source, so the apply refuses,
/// and the move — one operation with its cascade — is listed as part-landed
/// as the refused plan carried it, cascade and all, since part of what it
/// says has landed.
#[test]
fn a_landed_holder_with_an_unlanded_destination_is_part_landed() {
    let mut fixture = Fixture::new(&[("a.md", "A\n"), ("h.md", "[[a]]\n")]);
    let plan = fixture.plan(vec![moving("a.md", "b.md")]);
    fixture.foreign("h.md", "[[b]]\n");
    fixture.foreign("a.md", "A, edited\n");

    let refused = super::refused(fixture.apply(plan));
    let [left] = &refused.unresolved[..] else {
        panic!("the move is left unresolved: {:?}", refused.unresolved);
    };
    assert_eq!(left.reason, UnresolvedReason::part_landed());
    assert_eq!(
        left.operation,
        moving("a.md", "b.md").with_cascade(vec![wikilink("h.md", "a", "b")])
    );
    assert!(refused.plan.operations.is_empty());
    assert_eq!(fixture.read("a.md").as_deref(), Some("A, edited\n"));
}

/// **Re-sending an interrupted move finishes its cascade.** One holder and
/// the destination landed before the interruption, and the store took them
/// in; the re-send finds them at their after-states, rewrites the holder
/// that did not land, removes the source, and commits what a build from zero
/// holds.
#[test]
fn resending_an_interrupted_move_finishes_its_cascade() {
    let mut fixture = Fixture::new(&[
        ("a.md", "A\n"),
        ("h.md", "[[a]]\n"),
        ("k.md", "[[a]] too\n"),
    ]);
    let plan = fixture.plan(vec![moving("a.md", "x/b.md")]);
    assert_eq!(
        plan.operations[0].cascade,
        [wikilink("h.md", "a", "b"), wikilink("k.md", "a", "b")]
    );
    fixture.write("x/b.md", "A\n");
    fixture.foreign("h.md", "[[b]]\n");

    applied(fixture.apply(plan));
    assert_eq!(fixture.read("a.md"), None);
    assert_eq!(fixture.read("x/b.md").as_deref(), Some("A\n"));
    assert_eq!(fixture.read("h.md").as_deref(), Some("[[b]]\n"));
    assert_eq!(fixture.read("k.md").as_deref(), Some("[[b]] too\n"));
    fixture.assert_store_is_a_build_from_zero();
}

/// **A link written twice in one holder is one rewrite**: the cascade
/// respells every link of its key at once, so it carries the key once.
#[test]
fn a_link_written_twice_is_one_rewrite() {
    let mut fixture = Fixture::new(&[("a.md", "A\n"), ("h.md", "[[a]] then [[a|again]]\n")]);
    let cascades = fixture.moved(vec![moving("a.md", "b.md")]);
    assert_eq!(cascades, [vec![wikilink("h.md", "a", "b")]]);
    assert_eq!(
        fixture.read("h.md").as_deref(),
        Some("[[b]] then [[b|again]]\n")
    );
}

/// **A holder's rewrites land as one batch, never one over another**: `[[a]]`
/// respelled `b` is not respelled again by the rewrite the plan writes for
/// the `[[b]]` the holder already held, whichever move the plan names first,
/// so each link names the document it named where that document lands.
#[test]
fn rewrites_on_one_holder_never_chain_whatever_the_order_of_the_moves() {
    for operations in [
        vec![moving("a.md", "r/b.md"), moving("p/b.md", "q/z.md")],
        vec![moving("p/b.md", "q/z.md"), moving("a.md", "r/b.md")],
    ] {
        let mut fixture = Fixture::new(&[
            ("p/b.md", "B\n"),
            ("a.md", "A\n"),
            ("h.md", "[[a]] [[b]]\n"),
        ]);
        fixture.moved(operations);
        assert_eq!(fixture.read("h.md").as_deref(), Some("[[b]] [[z]]\n"));
    }
}

/// **A moved document's own relative links reach the files they named even
/// where one's new spelling is another's old one**: `../N2.md` from `a/b/`
/// names `a/N2.md` and `../../N2.md` the root's, and from `z/` the first is
/// spelled `../a/N2.md` and the second `../N2.md`.
#[test]
fn own_relative_links_whose_spellings_cross_each_reach_the_file_they_named() {
    let mut fixture = Fixture::new(&[
        ("N2.md", "root\n"),
        ("a/N2.md", "under a\n"),
        ("a/b/m.md", "[p](../N2.md) [q](../../N2.md)\n"),
    ]);
    fixture.moved(vec![moving("a/b/m.md", "z/m.md")]);
    assert_eq!(
        fixture.read("z/m.md").as_deref(),
        Some("[p](../a/N2.md) [q](../N2.md)\n")
    );
}

/// **A refreshed move's crossing rewrites land as one batch too**: another
/// writer edits the moved document after the preview, the refusal's fresh
/// plan generates the cascade again from what it holds now, and each of its
/// relative links still names the file it named — `x.md` the nested one,
/// `../x.md` the root's.
#[test]
fn a_refreshed_moves_crossing_rewrites_each_name_the_file_they_named() {
    let mut fixture = Fixture::new(&[
        ("old/a.md", "[nested](x.md) [root](../x.md)\n"),
        ("old/x.md", "Nested\n"),
        ("x.md", "Root\n"),
    ]);
    let plan = fixture.plan(vec![moving("old/a.md", "a.md")]);
    fixture.foreign("old/a.md", "foreign [nested](x.md) [root](../x.md)\n");
    let refused = super::refused(fixture.apply(plan));
    applied(fixture.apply(refused.plan));
    assert_eq!(
        fixture.read("a.md").as_deref(),
        Some("foreign [nested](old/x.md) [root](x.md)\n")
    );
    fixture.assert_store_is_a_build_from_zero();
}

/// **Each syntax's candidate is judged as that syntax reads it**: `b.md` as
/// a Markdown link from the root names the root's `b.md` alone, while as a
/// wikilink it is a suffix `y/b.md` answers too, so the wikilink has no
/// spelling naming the moved document alone and is left as written, said
/// to be unrepresentable, while the Markdown link is respelled.
#[test]
fn a_candidate_spelled_alike_in_two_syntaxes_is_judged_once_per_syntax() {
    for written in ["[[a.md]] [t](a.md)\n", "[t](a.md) [[a.md]]\n"] {
        let mut fixture = Fixture::new(&[("a.md", "A\n"), ("y/b.md", "Y\n"), ("h.md", written)]);
        let resolution = fixture.resolution(vec![moving("a.md", "b.md")]);
        assert_eq!(
            resolution.plan.operations[0].cascade,
            [markdown("h.md", "a.md", "b.md")]
        );
        assert_eq!(
            resolution.forecast.links,
            vec![LinkAdvisory::skipped_unrepresentable(key(
                "h.md",
                LinkFamily::Wikilink,
                "a.md"
            ))]
        );
        applied(fixture.apply(resolution.plan));
        assert_eq!(
            fixture.read("h.md").as_deref(),
            Some(written.replace("(a.md)", "(b.md)").as_str())
        );
    }
}

/// **A path a move vacates and another operation refills changes what its
/// links name**: `a.md`'s document goes to `c.md` and another lands where it
/// stood, so every link that named it — a bare wikilink, a relative Markdown
/// link and a `vault://` one — follows it to `c.md` rather than naming the
/// newcomer, whether a move or a create refills the path.
#[test]
fn backlinks_to_a_vacated_and_refilled_path_follow_the_document_that_left() {
    for refill in [moving("b.md", "a.md"), super::creating("a.md", "new\n")] {
        let mut fixture = Fixture::new(&[
            ("a.md", "A\n"),
            ("b.md", "B\n"),
            ("h.md", "[[a]] [t](a.md) [[vault://a]]\n"),
        ]);
        fixture.moved(vec![moving("a.md", "c.md"), refill]);
        assert_eq!(
            fixture.read("h.md").as_deref(),
            Some("[[c]] [t](c.md) [[vault://c]]\n")
        );
    }
}

/// **A moved document's rooted self-link follows it off a refilled path**:
/// `[me](/n/a.md)` named the document now at `c.md`, not the one another
/// move puts at `n/a.md`.
#[test]
fn a_rooted_self_link_follows_its_document_off_a_refilled_path() {
    let mut fixture = Fixture::new(&[("n/a.md", "[me](/n/a.md)\n"), ("b.md", "B\n")]);
    fixture.moved(vec![moving("n/a.md", "c.md"), moving("b.md", "n/a.md")]);
    assert_eq!(fixture.read("c.md").as_deref(), Some("[me](/c.md)\n"));
}

/// **A link left naming a refilled path is recorded and advised on.** The
/// path it resolves to is the same on both sides, but the document there is
/// not the one it named: the link is an entry naming that path on both
/// sides, the forecast says why the cascade left it, and the applier's
/// judgment of the plan computes the same entry and the same advice.
#[test]
fn a_link_left_naming_a_refilled_path_is_recorded_and_advised_on() {
    let mut fixture = Fixture::new(&[
        ("a.md", "A\n"),
        ("b.md", "B\n"),
        ("h.md", "---\nsee: '[[a]]'\n---\nbody\n"),
    ]);
    let resolution = fixture.resolution(vec![moving("a.md", "it's.md"), moving("b.md", "a.md")]);
    let link = key("h.md", LinkFamily::Wikilink, "a");
    assert!(
        resolution
            .plan
            .conditions
            .contains(&PlanCondition::link_resolution(
                link.clone(),
                Resolves::one(path("a.md")),
                Resolves::one(path("a.md")),
            )),
        "{:?}",
        resolution.plan.conditions
    );
    assert_eq!(
        resolution.forecast.links,
        vec![LinkAdvisory::skipped_would_corrupt_frontmatter(link)]
    );
    let (previewed, forecast) = fixture
        .preview(resolution.plan.clone())
        .expect("the plan previews");
    assert_eq!(previewed, resolution.plan);
    assert_eq!(forecast.links, resolution.forecast.links);
    applied(fixture.apply(resolution.plan));
    fixture.assert_store_is_a_build_from_zero();
}

/// **A rewrite onto an address another rewrite vacates keeps no link
/// behind**: `[[n]]` is respelled `[[m]]` where `n.md` moves, and `[[x]]`
/// becomes `[[n]]` where `x.md` takes its address, so the `[[n]]` the holder
/// held is a link a rewrite matched rather than one left under a key the
/// plan writes, and the forecast has no advisory for it.
#[test]
fn a_rewrite_onto_an_address_another_rewrite_vacates_keeps_no_link_behind() {
    let mut fixture = Fixture::new(&[("x.md", "X\n"), ("n.md", "N\n"), ("h.md", "[[x]] [[n]]\n")]);
    let resolution = fixture.resolution(vec![moving("n.md", "m.md"), moving("x.md", "n.md")]);
    assert_eq!(resolution.forecast.links, Vec::<LinkAdvisory>::new());
    let (previewed, forecast) = fixture
        .preview(resolution.plan.clone())
        .expect("the plan previews");
    assert_eq!(previewed, resolution.plan);
    assert_eq!(forecast.links, resolution.forecast.links);
    applied(fixture.apply(resolution.plan));
    assert_eq!(fixture.read("h.md").as_deref(), Some("[[n]] [[m]]\n"));
    fixture.assert_store_is_a_build_from_zero();
}

/// **A backlink another writer adds to a path the plan refills refuses the
/// plan**, as one to a path the plan vacates does, and the fresh plan's
/// cascade follows it.
#[test]
fn a_backlink_added_to_a_refilled_path_after_preview_refuses_the_plan() {
    let mut fixture = Fixture::new(&[("a.md", "A\n"), ("b.md", "B\n"), ("h.md", "[[a]]\n")]);
    let plan = fixture.plan(vec![moving("a.md", "c.md"), moving("b.md", "a.md")]);
    fixture.foreign("k.md", "[[a]]\n");
    let refused = super::refused(fixture.apply(plan));
    assert_eq!(
        refused.checks,
        vec![norn_wire::RefusedCheck::condition_unrecorded(
            PlanCondition::link_resolution(
                key("k.md", LinkFamily::Wikilink, "a"),
                Resolves::one(path("a.md")),
                Resolves::one(path("a.md")),
            )
        )]
    );
    applied(fixture.apply(refused.plan));
    assert_eq!(fixture.read("h.md").as_deref(), Some("[[c]]\n"));
    assert_eq!(fixture.read("k.md").as_deref(), Some("[[c]]\n"));
    fixture.assert_store_is_a_build_from_zero();
}

/// **A link is respelled only in the extension style it was written in**: a
/// wikilink written without the document extension whose every spelling of
/// the destination without one reads as something else — `Ü/v1.2` names an
/// attachment — is left as written and said to be unrepresentable, never
/// given an extension its author did not write.
#[test]
fn a_link_with_no_spelling_in_its_own_extension_style_is_unrepresentable() {
    let mut fixture = Fixture::new(&[
        ("b/note one.md", "N\n"),
        ("Ü/v1.md", "V\n"),
        ("h.md", "[[b/note one]]\n"),
    ]);
    let resolution = fixture.resolution(vec![moving("b/note one.md", "Ü/v1.2.md")]);
    assert_eq!(resolution.plan.operations[0].cascade, []);
    assert_eq!(
        resolution.forecast.links,
        vec![LinkAdvisory::skipped_unrepresentable(key(
            "h.md",
            LinkFamily::Wikilink,
            "b/note one"
        ))]
    );
    applied(fixture.apply(resolution.plan));
    assert_eq!(fixture.read("h.md").as_deref(), Some("[[b/note one]]\n"));
}

/// **A moved document's relative link still reaching its file is left as
/// written**: from `b/`, `../b/n.md` names `b/n.md` as it did from `x/`, so
/// moving the document into `b/` rewrites nothing, though `n.md` would be
/// shorter.
#[test]
fn an_own_relative_link_still_reaching_its_file_is_left_as_written() {
    let mut fixture = Fixture::new(&[("x/a.md", "[n](../b/n.md)\n"), ("b/n.md", "N\n")]);
    let cascades = fixture.moved(vec![moving("x/a.md", "b/a.md")]);
    assert_eq!(cascades, [Vec::<LinkRewrite>::new()]);
    assert_eq!(fixture.read("b/a.md").as_deref(), Some("[n](../b/n.md)\n"));
}

/// **A path-qualified backlink to a document moved to the vault root is
/// respelled bare**: the root holds no folder to qualify it with, so its one
/// segment is the most it can keep.
#[test]
fn a_path_qualified_backlink_to_a_document_moved_to_the_root_is_respelled_bare() {
    let mut fixture = Fixture::new(&[("x/a.md", "A\n"), ("h.md", "[[x/a]]\n")]);
    let cascades = fixture.moved(vec![moving("x/a.md", "b.md")]);
    assert_eq!(cascades, [vec![wikilink("h.md", "x/a", "b")]]);
    assert_eq!(fixture.read("h.md").as_deref(), Some("[[b]]\n"));
}

/// **A move that makes a link to a document it does not move ambiguous
/// rewrites nothing**: `[[n]]` named `p/n.md`, which stays where it is, and
/// the moved document landing as another `n.md` makes it ambiguous, which the
/// forecast says; the link is no backlink of the moved document.
#[test]
fn a_move_making_a_link_to_an_unmoved_document_ambiguous_rewrites_nothing() {
    let mut fixture = Fixture::new(&[("p/n.md", "P\n"), ("a.md", "A\n"), ("h.md", "[[n]]\n")]);
    let resolution = fixture.resolution(vec![moving("a.md", "q/n.md")]);
    assert_eq!(resolution.plan.operations[0].cascade, []);
    assert_eq!(
        resolution.forecast.links,
        vec![LinkAdvisory::made_ambiguous(key(
            "h.md",
            LinkFamily::Wikilink,
            "n"
        ))]
    );
    applied(fixture.apply(resolution.plan));
    assert_eq!(fixture.read("h.md").as_deref(), Some("[[n]]\n"));
}

/// **A link to a document the plan edits but does not move is no backlink
/// of a move**: `[[n]]` named `p/n.md`, which the plan edits where it stands,
/// and the moved document landing as another `n.md` makes it ambiguous. The
/// edit makes `p/n.md` a file the plan writes, so the link is judged beside
/// the moved one; it is still not respelled toward the document it named, and
/// the forecast says it is made ambiguous.
#[test]
fn a_link_to_an_edited_unmoved_document_made_ambiguous_rewrites_nothing() {
    let mut fixture = Fixture::new(&[("p/n.md", "P\n"), ("a.md", "A\n"), ("h.md", "[[n]]\n")]);
    let resolution =
        fixture.previewed_and_applied(vec![moving("a.md", "q/n.md"), editing("p/n.md", "P", "P2")]);
    assert_eq!(resolution.plan.operations[0].cascade, []);
    assert_eq!(
        resolution.forecast.links,
        vec![LinkAdvisory::made_ambiguous(key(
            "h.md",
            LinkFamily::Wikilink,
            "n"
        ))]
    );
    assert_eq!(fixture.read("h.md").as_deref(), Some("[[n]]\n"));
}

/// **A move that falls takes down only what its own operation touches**: the
/// cascade it would have carried is discarded with it, so a holder only that
/// cascade shared with another move does not take the other move down. Both
/// moves rewrite `idx.md`; an edit failing on `h.md`, which only the first
/// rewrites, leaves the first unresolved, and the second resolves with its
/// own cascade.
#[test]
fn a_fallen_moves_discarded_cascade_takes_down_no_other_move() {
    let fixture = Fixture::new(&[
        ("a.md", "A\n"),
        ("b.md", "B\n"),
        ("idx.md", "[[a]] [[b]]\n"),
        ("h.md", "[[a]]\n"),
    ]);
    let operations = vec![
        moving("a.md", "x/a2.md"),
        moving("b.md", "x/b2.md"),
        editing("h.md", "missing", "x"),
    ];
    let resolution = fixture.planned(operations.clone());
    let left: Vec<&Operation> = resolution
        .unresolved
        .iter()
        .map(|unresolved| &unresolved.operation)
        .collect();
    assert_eq!(left, [&operations[0], &operations[2]]);
    assert_eq!(
        resolution.plan.operations,
        [moving("b.md", "x/b2.md").with_cascade(vec![wikilink("idx.md", "b", "b2")])]
    );
}

/// **A cascade that cannot compose leaves its move unresolved, in every
/// build.** The store has not yet taken in that `h.md` is gone, so the move's
/// cascade names a holder no document stands at; the move is left out in the
/// composition's words rather than planned with a transition its cascade
/// does not make.
#[test]
fn a_cascade_naming_a_holder_gone_from_the_vault_leaves_its_move_unresolved() {
    let fixture = Fixture::new(&[("a.md", "A\n"), ("h.md", "[[a]]\n")]);
    std::fs::remove_file(fixture.vault.join("h.md")).expect("h.md is removed");
    let resolution = fixture.planned(vec![moving("a.md", "b.md")]);
    let [left] = &resolution.unresolved[..] else {
        panic!("the move is left out: {:?}", resolution.unresolved);
    };
    let UnresolvedReason::NoLongerResolves { detail, .. } = &left.reason else {
        panic!("the move no longer resolves: {left:?}");
    };
    assert!(detail.contains("h.md"), "{detail}");
    assert!(resolution.plan.operations.is_empty());
    assert!(resolution.plan.transitions.is_empty());
}

impl Fixture {
    /// Plan `operations`, preview the plan — whose judgment recomputes it and
    /// its forecast, which must agree with planning's — and apply it, handing
    /// back what planning resolved, once the store is shown to be a build
    /// from zero over what landed.
    fn previewed_and_applied(&mut self, operations: Vec<Operation>) -> Resolution {
        let resolution = self.resolution(operations);
        let (previewed, forecast) = self
            .preview(resolution.plan.clone())
            .expect("the plan previews");
        assert_eq!(previewed, resolution.plan);
        assert_eq!(forecast.links, resolution.forecast.links);
        applied(self.apply(resolution.plan.clone()));
        self.assert_store_is_a_build_from_zero();
        resolution
    }
}

/// **A link a cascade left behind is advised on though a respelled link now
/// shares its key.** `[[x]]` named `x.md`, which moves where no spelling
/// without the extension reads back, and `t.md` refills `x.md`, so `[[t]]`
/// is respelled `[[x]]`: in `h.md` the kept link and the written one are one
/// key, and the kept one's change of meaning is said as it is in `c.md`,
/// which holds the kept link alone.
#[test]
fn a_left_behind_link_sharing_its_key_with_a_respelled_one_is_still_unrepresentable() {
    let mut fixture = Fixture::new(&[
        ("x.md", "S\n"),
        ("t.md", "T\n"),
        ("Ü/v1.md", "V\n"),
        ("h.md", "[[x]] [[t]]\n"),
        ("c.md", "[[x]]\n"),
    ]);
    let resolution =
        fixture.previewed_and_applied(vec![moving("x.md", "Ü/v1.2.md"), moving("t.md", "x.md")]);
    assert_eq!(
        resolution.plan.operations[1].cascade,
        [wikilink("h.md", "t", "x")]
    );
    let shared = key("h.md", LinkFamily::Wikilink, "x");
    assert!(
        resolution
            .plan
            .conditions
            .contains(&PlanCondition::link_resolution(
                shared.clone(),
                Resolves::one(path("x.md")),
                Resolves::one(path("x.md")),
            )),
        "{:?}",
        resolution.plan.conditions
    );
    assert_eq!(
        resolution.forecast.links,
        vec![
            LinkAdvisory::skipped_unrepresentable(key("c.md", LinkFamily::Wikilink, "x")),
            LinkAdvisory::skipped_unrepresentable(shared),
        ]
    );
    assert_eq!(fixture.read("h.md").as_deref(), Some("[[x]] [[x]]\n"));
    assert_eq!(fixture.read("c.md").as_deref(), Some("[[x]]\n"));
}

/// **An ambiguous link is advised on though a respelled link now shares its
/// key.** `[[n]]` could name `p/n.md` or `q/n.md`, both moved away, and
/// `t.md` lands as `n.md`, so `[[t]]` is respelled `[[n]]`: in `h.md` the
/// kept ambiguous link and the written one are one key, and the kept one is
/// said to be skipped for its ambiguity as it is in `c.md`, which holds it
/// alone.
#[test]
fn an_ambiguous_link_sharing_its_key_with_a_respelled_one_is_still_skipped_ambiguous() {
    let mut fixture = Fixture::new(&[
        ("p/n.md", "P\n"),
        ("q/n.md", "Q\n"),
        ("t.md", "T\n"),
        ("h.md", "[[n]] [[t]]\n"),
        ("c.md", "[[n]]\n"),
    ]);
    let resolution = fixture.previewed_and_applied(vec![
        moving("p/n.md", "r/k.md"),
        moving("q/n.md", "r/j.md"),
        moving("t.md", "n.md"),
    ]);
    assert_eq!(
        resolution.plan.operations[2].cascade,
        [wikilink("h.md", "t", "n")]
    );
    let shared = key("h.md", LinkFamily::Wikilink, "n");
    assert!(
        resolution
            .plan
            .conditions
            .contains(&PlanCondition::link_resolution(
                shared.clone(),
                Resolves::several(),
                Resolves::one(path("n.md")),
            )),
        "{:?}",
        resolution.plan.conditions
    );
    assert_eq!(
        resolution.forecast.links,
        vec![
            LinkAdvisory::skipped_ambiguous(key("c.md", LinkFamily::Wikilink, "n")),
            LinkAdvisory::skipped_ambiguous(shared),
        ]
    );
    assert_eq!(fixture.read("h.md").as_deref(), Some("[[n]] [[n]]\n"));
    assert_eq!(fixture.read("c.md").as_deref(), Some("[[n]]\n"));
}

/// **An ambiguous link the plan retargets is advised on though a respelled
/// link now shares its key.** `[[n]]` could name `p/n.md` or `q/n.md`, both
/// deleted, and `t.md` lands as `n.md`, so `[[t]]` is respelled `[[n]]`: the
/// kept link comes to name `n.md`, which is said in `h.md` as in `c.md`.
#[test]
fn a_retargeted_link_sharing_its_key_with_a_respelled_one_is_still_retargeted() {
    let mut fixture = Fixture::new(&[
        ("p/n.md", "P\n"),
        ("q/n.md", "Q\n"),
        ("t.md", "T\n"),
        ("h.md", "[[n]] [[t]]\n"),
        ("c.md", "[[n]]\n"),
    ]);
    let resolution = fixture.previewed_and_applied(vec![
        deleting("p/n.md"),
        deleting("q/n.md"),
        moving("t.md", "n.md"),
    ]);
    assert_eq!(
        resolution.plan.operations[2].cascade,
        [wikilink("h.md", "t", "n")]
    );
    assert_eq!(
        resolution.forecast.links,
        vec![
            LinkAdvisory::retargeted(key("c.md", LinkFamily::Wikilink, "n")),
            LinkAdvisory::retargeted(key("h.md", LinkFamily::Wikilink, "n")),
        ]
    );
    assert_eq!(fixture.read("h.md").as_deref(), Some("[[n]] [[n]]\n"));
}

impl Fixture {
    /// Plan `operations`, preview the plan — whose judgment recomputes it and
    /// its forecast, which must agree with planning's — and apply it,
    /// handing back what planning resolved. The store must equal a build
    /// from zero after it.
    fn previewed_and_applied_over_quarantine(&mut self, operations: Vec<Operation>) -> Resolution {
        let resolution = self.resolution(operations);
        let (previewed, forecast) = self
            .preview(resolution.plan.clone())
            .expect("the plan previews");
        assert_eq!(previewed, resolution.plan);
        assert_eq!(forecast.links, resolution.forecast.links);
        applied(self.apply(resolution.plan.clone()));
        self.assert_store_is_a_build_from_zero();
        resolution
    }
}

/// **A move of a quarantined file generates no cascade.** No link resolves
/// to a file whose bytes do not decode, before the move or after it, so
/// `[[q]]`, `[[notes/q]]` and `[q](notes/q.md)` were broken and stay broken
/// as written, though the move changes the file's stem and folder: the plan
/// carries no rewrite and records no entry, the preview recomputes it, and
/// it applies with the linker untouched.
#[test]
fn a_move_of_a_quarantined_file_generates_no_cascade() {
    let linker = "[[q]] [[notes/q]] [q](notes/q.md)\n";
    let mut fixture = Fixture::new(&[("l.md", linker)]);
    fixture.foreign("notes/q.md", super::UNDECODABLE);
    let resolution =
        fixture.previewed_and_applied_over_quarantine(vec![moving("notes/q.md", "archive/r.md")]);
    assert_eq!(resolution.plan.operations[0].cascade, []);
    assert_eq!(resolution.plan.conditions, []);
    assert_eq!(resolution.forecast.links, []);
    assert_eq!(fixture.read("l.md").as_deref(), Some(linker));
    assert_eq!(
        std::fs::read(fixture.vault.join("archive/r.md"))
            .ok()
            .as_deref(),
        Some(super::UNDECODABLE)
    );
}

/// **A quarantined holder moved respells none of its links.** Its bytes do
/// not decode, so it holds no link the index derives, before the move or
/// after it: the relative `[d](./d.md)` it carries is no link of the plan's
/// to keep naming `notes/d.md`, the move carries no rewrite and records no
/// entry, the preview recomputes it, and it lands the bytes as they were.
#[test]
fn a_quarantined_holder_moved_respells_none_of_its_links() {
    let holder: &[u8] = b"\xff [d](./d.md) [[d]]\n";
    let mut fixture = Fixture::new(&[("notes/d.md", "d\n")]);
    fixture.foreign("notes/q.md", holder);
    let resolution =
        fixture.previewed_and_applied_over_quarantine(vec![moving("notes/q.md", "archive/q.md")]);
    assert_eq!(resolution.plan.operations[0].cascade, []);
    assert_eq!(resolution.plan.conditions, []);
    assert_eq!(resolution.forecast.links, []);
    assert_eq!(
        std::fs::read(fixture.vault.join("archive/q.md"))
            .ok()
            .as_deref(),
        Some(holder)
    );
}
