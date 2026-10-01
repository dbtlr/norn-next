//! Link rewrites planned over a vault on disk and the store beside it, then
//! judged and applied by the applier: which wikilinks a `rewrite_wikilink`
//! retargets and how each is spelled, what it is left unresolved for, and
//! the cascade it carries.

use norn_wire::{
    AmbiguousEnd, Candidate, CandidateHead, LinkAdvisory, LinkFamily, LinkKey, LinkRewrite,
    Operation, OperationKind, PlanCondition, RefusedCheck, ResolutionTarget, Resolves,
    UnresolvedOperation, UnresolvedReason,
};

use norn_store::StoredPathOrder;

use super::{Fixture, applied, creating, deleting, path};

/// A rewrite of every wikilink naming `old` to name `new`.
fn retargeting(old: &str, new: &str) -> Operation {
    let target = |text: &str| ResolutionTarget::new(text).expect("a target");
    Operation::new(OperationKind::rewrite_wikilink(target(old), target(new)))
}

fn wikilink(holder: &str, from: &str, to: &str) -> LinkRewrite {
    LinkRewrite::new(path(holder), LinkFamily::Wikilink, from, to)
}

fn key(holder: &str, address: &str) -> LinkKey {
    LinkKey::new(path(holder), LinkFamily::Wikilink, address)
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

/// **An `old` naming one document retargets every wikilink resolving to it,
/// whatever its spelling, each in its own form**: a bare wikilink to the
/// shortest unique suffix of `new`'s document, an embed keeping its anchor
/// and title, a path-qualified one to a suffix of at least two segments, one
/// written with the extension keeping it, a `vault://` one to the root path,
/// and one in frontmatter. A Markdown link naming the same document is not a
/// wikilink, and is left as written. The rewrites ride the operation as its
/// cascade, the forecast says nothing, a preview of the plan forecasts as
/// planning did, and the plan lands.
#[test]
fn an_old_naming_one_document_retargets_every_spelling_of_its_wikilinks() {
    let mut fixture = Fixture::new(&[
        ("notes/a.md", "A\n"),
        ("archive/c.md", "C\n"),
        (
            "h.md",
            "[[a]] ![[a#Part|it]] [[notes/a]] [[a.md]] [[vault://notes/a]]\n\n[t](notes/a.md)\n",
        ),
        ("k.md", "---\nsee: \"[[a]]\"\n---\nbody\n"),
    ]);
    let resolution = fixture.resolution(vec![retargeting("a", "c")]);
    assert_eq!(
        resolution.plan.operations[0].cascade,
        [
            wikilink("h.md", "a", "c"),
            wikilink("h.md", "a.md", "c.md"),
            wikilink("h.md", "notes/a", "archive/c"),
            wikilink("h.md", "vault://notes/a", "vault://archive/c"),
            wikilink("k.md", "a", "c"),
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
    assert_eq!(
        fixture.read("h.md").as_deref(),
        Some(
            "[[c]] ![[c#Part|it]] [[archive/c]] [[c.md]] [[vault://archive/c]]\n\n[t](notes/a.md)\n"
        )
    );
    assert_eq!(
        fixture.read("k.md").as_deref(),
        Some("---\nsee: \"[[c]]\"\n---\nbody\n")
    );
    assert_eq!(fixture.read("notes/a.md").as_deref(), Some("A\n"));
    fixture.assert_store_is_a_build_from_zero();
}

/// **An `old` naming no document repairs the broken wikilinks filed under
/// it exactly, as the root reads it**: `[[Old Note]]` is filed under `Old
/// Note`, so it names `c.md` after; `[[old note|x]]` is too only where the
/// root folds ASCII case. Another broken link, a path-qualified one, one
/// written with the extension — read through a key `Old Note` is not — and
/// `[[Old]]`, sharing none of its keys, are left as they are; and a rewrite
/// of `v1.2` never repairs `[[v1]]`, whose one key is only one of `v1.2`'s.
#[test]
fn an_old_naming_nothing_repairs_the_broken_links_filed_under_it() {
    let mut fixture = Fixture::new(&[
        ("c.md", "C\n"),
        (
            "h.md",
            "[[Old Note]] [[old note|x]] [[Other Gone]] [[sub/Old Note]] [[Old Note.md]] [[v1]]\n",
        ),
    ]);
    let folds = fixture.store.path_order() == StoredPathOrder::AsciiCaseInsensitive;
    let resolution = fixture.resolution(vec![retargeting("Old Note", "c")]);
    let mut repaired = vec![wikilink("h.md", "Old Note", "c")];
    if folds {
        repaired.push(wikilink("h.md", "old note", "c"));
    }
    assert_eq!(resolution.plan.operations[0].cascade, repaired);
    assert!(resolution.forecast.links.is_empty());

    let versions = fixture.planned(vec![retargeting("v1.2", "c")]);
    let detail = unresolved_detail(&versions);
    assert!(detail.contains("no broken wikilink"), "{detail}");

    applied(fixture.apply(resolution.plan));
    let kept = if folds { "[[c|x]]" } else { "[[old note|x]]" };
    assert_eq!(
        fixture.read("h.md").as_deref(),
        Some(
            format!("[[c]] {kept} [[Other Gone]] [[sub/Old Note]] [[Old Note.md]] [[v1]]\n")
                .as_str()
        )
    );
    fixture.assert_store_is_a_build_from_zero();
}

/// **On a root telling spellings apart, a broken link is repaired only by
/// an `old` spelled in its own case**: `old note` repairs `[[old note]]` and
/// not `[[Old Note]]`; and `Old Note.md` repairs the link written with the
/// extension and nothing else. On a root folding ASCII case `old note`
/// repairs both spellings.
#[test]
fn a_broken_link_is_repaired_by_an_old_spelled_as_the_root_reads_it() {
    let fixture = Fixture::new(&[
        ("c.md", "C\n"),
        ("h.md", "[[Old Note]] [[old note]] [[Old Note.md]]\n"),
    ]);
    let folds = fixture.store.path_order() == StoredPathOrder::AsciiCaseInsensitive;
    let lower = fixture.resolution(vec![retargeting("old note", "c")]);
    let mut repaired = Vec::new();
    if folds {
        repaired.push(wikilink("h.md", "Old Note", "c"));
    }
    repaired.push(wikilink("h.md", "old note", "c"));
    assert_eq!(lower.plan.operations[0].cascade, repaired);

    let suffixed = fixture.resolution(vec![retargeting("Old Note.md", "c")]);
    assert_eq!(
        suffixed.plan.operations[0].cascade,
        [wikilink("h.md", "Old Note.md", "c.md")]
    );
}

/// **An `old` naming several documents is left unresolved with the head of
/// them**, as a read heads an ambiguous target, and writes nothing.
#[test]
fn an_old_naming_several_documents_is_unresolved_with_its_candidates() {
    let fixture = Fixture::new(&[
        ("x/a.md", "X\n"),
        ("y/a.md", "Y\n"),
        ("c.md", "C\n"),
        ("h.md", "[[x/a]]\n"),
    ]);
    let several = fixture.planned(vec![retargeting("a", "c")]);
    assert_eq!(
        several.unresolved,
        [UnresolvedOperation::new(
            retargeting("a", "c"),
            UnresolvedReason::ambiguous_target(
                AmbiguousEnd::Old,
                CandidateHead::new(
                    [
                        Candidate::new(path("x/a.md"), "x/a"),
                        Candidate::new(path("y/a.md"), "y/a"),
                    ],
                    2,
                )
                .expect("a head of two"),
            ),
        )]
    );
    assert!(several.plan.transitions.is_empty());
}

/// **A rewrite whose two ends are both ambiguous is answered for its
/// `old`**, the end the links it would rewrite are read by.
#[test]
fn a_rewrite_ambiguous_at_both_ends_is_answered_for_its_old() {
    let fixture = Fixture::new(&[
        ("x/a.md", "X\n"),
        ("y/a.md", "Y\n"),
        ("x/c.md", "X\n"),
        ("y/c.md", "Y\n"),
        ("h.md", "[[x/a]]\n"),
    ]);
    let both = fixture.planned(vec![retargeting("a", "c")]);
    let [left] = both.unresolved.as_slice() else {
        panic!("one operation is unresolved: {:?}", both.unresolved);
    };
    let UnresolvedReason::AmbiguousTarget {
        end, candidates, ..
    } = &left.reason
    else {
        panic!("an ambiguous end: {:?}", left.reason);
    };
    assert_eq!(*end, AmbiguousEnd::Old);
    assert_eq!(candidates.candidates()[0].path, path("x/a.md"));
}

/// **A `new` must name one document where the plan leaves the vault.**
/// Naming none leaves the rewrite unresolved in words, naming several leaves
/// it unresolved with the head of them, and one the plan creates is named.
/// An `old` and a `new` naming one document, and an `old` no wikilink names,
/// leave nothing to rewrite, and are unresolved in words rather than landing
/// as a change of nothing.
#[test]
fn a_rewrite_with_nothing_to_rewrite_to_or_from_is_unresolved_saying_why() {
    let fixture = Fixture::new(&[
        ("a.md", "A\n"),
        ("x/c.md", "X\n"),
        ("y/c.md", "Y\n"),
        ("e.md", "E\n"),
        ("h.md", "[[a]]\n"),
    ]);
    let none = fixture.planned(vec![retargeting("a", "zzz")]);
    let detail = unresolved_detail(&none);
    assert!(detail.contains("names no one document"), "{detail}");

    let several = fixture.planned(vec![retargeting("a", "c")]);
    assert_eq!(
        several.unresolved,
        [UnresolvedOperation::new(
            retargeting("a", "c"),
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

    let created = fixture.planned(vec![retargeting("a", "n"), creating("n.md", "N\n")]);
    assert!(created.unresolved.is_empty(), "{:?}", created.unresolved);
    assert_eq!(
        created.plan.operations[0].cascade,
        [wikilink("h.md", "a", "n")]
    );

    let itself = fixture.planned(vec![retargeting("a", "a.md")]);
    let detail = unresolved_detail(&itself);
    assert!(detail.contains("the same document"), "{detail}");

    let unnamed = fixture.planned(vec![retargeting("e", "a")]);
    let detail = unresolved_detail(&unnamed);
    assert!(detail.contains("no wikilink"), "{detail}");
    assert!(unnamed.plan.transitions.is_empty());
}

/// **An ambiguous wikilink is never rewritten**, and the forecast says it
/// was skipped for its ambiguity; a wikilink naming `old`'s document alone
/// beside it is rewritten.
#[test]
fn an_ambiguous_wikilink_is_skipped_and_advised() {
    let mut fixture = Fixture::new(&[
        ("x/a.md", "X\n"),
        ("y/a.md", "Y\n"),
        ("c.md", "C\n"),
        ("h.md", "[[a]] [[x/a]]\n"),
    ]);
    let resolution = fixture.resolution(vec![retargeting("x/a", "c")]);
    assert_eq!(
        resolution.plan.operations[0].cascade,
        [wikilink("h.md", "x/a", "c")]
    );
    assert_eq!(
        resolution.forecast.links,
        [LinkAdvisory::skipped_ambiguous(key("h.md", "a"))]
    );
    applied(fixture.apply(resolution.plan));
    assert_eq!(fixture.read("h.md").as_deref(), Some("[[a]] [[c]]\n"));
    fixture.assert_store_is_a_build_from_zero();
}

/// **A wikilink another writer adds after the preview refuses the apply,
/// and the fresh plan rewrites it too.** The applier computes the set again
/// and meets `k.md`'s `[[a]]`, which the rewrite would retarget and the plan
/// does not record: the refusal names it, and the fresh plan's cascade
/// carries it and applies.
#[test]
fn a_wikilink_added_after_preview_refuses_and_the_fresh_plan_rewrites_it() {
    let mut fixture = Fixture::new(&[("a.md", "A\n"), ("c.md", "C\n"), ("h.md", "[[a]]\n")]);
    let plan = fixture.plan(vec![retargeting("a", "c")]);
    assert_eq!(plan.operations[0].cascade, [wikilink("h.md", "a", "c")]);
    fixture.foreign("k.md", "[[a]]\n");
    let refused = super::refused(fixture.apply(plan));
    assert_eq!(
        refused.checks,
        [RefusedCheck::condition_unrecorded(
            PlanCondition::link_resolution(
                key("k.md", "a"),
                Resolves::one(path("a.md")),
                Resolves::one(path("a.md")),
            )
        )]
    );
    assert_eq!(
        refused.plan.operations,
        [retargeting("a", "c")
            .with_cascade(vec![wikilink("h.md", "a", "c"), wikilink("k.md", "a", "c")])]
    );
    applied(fixture.apply(refused.plan));
    assert_eq!(fixture.read("h.md").as_deref(), Some("[[c]]\n"));
    assert_eq!(fixture.read("k.md").as_deref(), Some("[[c]]\n"));
    fixture.assert_store_is_a_build_from_zero();
}

/// **Re-sending an interrupted wikilink rewrite finishes its cascade.** One
/// holder landed before the interruption, and the store took it in; the
/// re-send finds it at its after-state and rewrites the holder that did not
/// land.
#[test]
fn resending_an_interrupted_wikilink_rewrite_finishes_its_cascade() {
    let mut fixture = Fixture::new(&[
        ("a.md", "A\n"),
        ("c.md", "C\n"),
        ("h.md", "[[a]]\n"),
        ("k.md", "[[a]] too\n"),
    ]);
    let plan = fixture.plan(vec![retargeting("a", "c")]);
    fixture.foreign("h.md", "[[c]]\n");
    applied(fixture.apply(plan));
    assert_eq!(fixture.read("h.md").as_deref(), Some("[[c]]\n"));
    assert_eq!(fixture.read("k.md").as_deref(), Some("[[c]] too\n"));
    fixture.assert_store_is_a_build_from_zero();
}

/// A `rewrite_link` of the links of `syntax` in `holder` written `from`, to
/// `to`.
fn relinking(holder: &str, syntax: LinkFamily, from: &str, to: &str) -> Operation {
    Operation::new(OperationKind::rewrite_link(path(holder), syntax, from, to))
}

/// **An authored link rewrite respells its holder in place, in one batch
/// with a move's cascade on the same holder.** The move carries `a.md` to
/// `b.md`, a create refills `a.md`, and the move's cascade respells `[[a]]`
/// to `[[b]]`; the authored rewrite respells `[[c]]` to `[[a]]` over the same
/// parse, so the link it writes is never respelled again. Its link is a
/// written entry of the plan, naming the created `a.md`, and the plan
/// previews as planned and lands.
#[test]
fn an_authored_link_rewrite_composes_in_one_batch_with_a_cascade_on_its_holder() {
    let mut fixture = Fixture::new(&[("a.md", "A\n"), ("c.md", "C\n"), ("h.md", "[[a]] [[c]]\n")]);
    let resolution = fixture.resolution(vec![
        super::moving("a.md", "b.md"),
        creating("a.md", "new A\n"),
        relinking("h.md", LinkFamily::Wikilink, "c", "a"),
    ]);
    let moved = resolution
        .plan
        .operations
        .iter()
        .find(|operation| matches!(operation.kind, OperationKind::MoveDocument { .. }))
        .expect("the move");
    assert_eq!(moved.cascade, [wikilink("h.md", "a", "b")]);
    assert!(
        resolution
            .plan
            .conditions
            .contains(&PlanCondition::link_resolution(
                key("h.md", "a"),
                Resolves::one(path("a.md")),
                Resolves::one(path("a.md")),
            )),
        "{:?}",
        resolution.plan.conditions
    );
    assert!(
        resolution.forecast.links.is_empty(),
        "{:?}",
        resolution.forecast.links
    );
    let (previewed, forecast) = fixture
        .preview(resolution.plan.clone())
        .expect("the plan previews");
    assert_eq!(previewed, resolution.plan);
    assert_eq!(forecast.links, resolution.forecast.links);
    applied(fixture.apply(resolution.plan));
    assert_eq!(fixture.read("h.md").as_deref(), Some("[[b]] [[a]]\n"));
    assert_eq!(fixture.read("b.md").as_deref(), Some("A\n"));
    assert_eq!(fixture.read("a.md").as_deref(), Some("new A\n"));
    fixture.assert_store_is_a_build_from_zero();
}

/// **An authored link rewrite matching no link is unresolved, in words,
/// rather than landing as a change of nothing**, as an edit whose text does
/// not occur is: no link of its syntax is written `from` in its holder —
/// `[[zzz]]` nowhere, and `a` written as a wikilink and not as a Markdown
/// link — or no document stands where it names one. One matching a link
/// respelled to what it already holds lands found, as an edit rewriting what
/// its document holds does.
#[test]
fn an_authored_link_rewrite_matching_no_link_is_unresolved() {
    let mut fixture = Fixture::new(&[("a.md", "A\n"), ("h.md", "[[a]]\n")]);
    for (operation, says) in [
        (
            relinking("h.md", LinkFamily::Wikilink, "zzz", "a"),
            "no wikilink in `h.md` is written `zzz`",
        ),
        (
            relinking("h.md", LinkFamily::Markdown, "a", "b"),
            "no Markdown link in `h.md` is written `a`",
        ),
        (
            relinking("gone.md", LinkFamily::Wikilink, "a", "b"),
            "no document stands at `gone.md`",
        ),
    ] {
        let resolution = fixture.planned(vec![operation.clone()]);
        let detail = unresolved_detail(&resolution);
        assert!(detail.contains(says), "{operation:?}: {detail}");
        assert!(resolution.plan.transitions.is_empty(), "{operation:?}");
    }

    let same = fixture.resolution(vec![relinking("h.md", LinkFamily::Wikilink, "a", "a")]);
    assert_eq!(same.plan.transitions.len(), 1);
    assert_eq!(
        same.plan.transitions[0].before,
        same.plan.transitions[0].after
    );
    let applied = applied(fixture.apply(same.plan));
    assert_eq!(
        super::results(&applied),
        [("h.md".to_string(), norn_wire::TargetResult::Found)]
    );
    assert_eq!(fixture.read("h.md").as_deref(), Some("[[a]]\n"));
}

/// **A link an authored rewrite matches and the text layer leaves as written
/// is advised on**: a wikilink's address cannot hold `]]`, so `[[a]]` stays
/// as it is, the forecast says it is unrepresentable, and the holder lands
/// found.
#[test]
fn an_authored_link_rewrite_the_text_layer_refuses_is_advised() {
    let mut fixture = Fixture::new(&[("a.md", "A\n"), ("h.md", "[[a]]\n")]);
    let resolution = fixture.resolution(vec![relinking("h.md", LinkFamily::Wikilink, "a", "x]]y")]);
    assert_eq!(
        resolution.forecast.links,
        [LinkAdvisory::skipped_unrepresentable(key("h.md", "a"))]
    );
    applied(fixture.apply(resolution.plan));
    assert_eq!(fixture.read("h.md").as_deref(), Some("[[a]]\n"));
}

/// **Re-sending an authored link rewrite whose holder already landed
/// finishes the plan**: the holder holds its after-state, so its rewrite —
/// which matches nothing there — is not refused for it, and the other
/// target lands.
#[test]
fn resending_an_authored_link_rewrite_whose_holder_landed_finishes_it() {
    let mut fixture = Fixture::new(&[("a.md", "draft\n"), ("c.md", "C\n"), ("h.md", "[[b]]\n")]);
    let plan = fixture.plan(vec![
        relinking("h.md", LinkFamily::Wikilink, "b", "c"),
        super::editing("a.md", "draft", "final"),
    ]);
    fixture.foreign("h.md", "[[c]]\n");
    applied(fixture.apply(plan));
    assert_eq!(fixture.read("h.md").as_deref(), Some("[[c]]\n"));
    assert_eq!(fixture.read("a.md").as_deref(), Some("final\n"));
    fixture.assert_store_is_a_build_from_zero();
}

/// **A wikilink rewrite clears a forbidding delete's backlinks only by
/// respelling them.** With `a.md` deleted and every wikilink naming `a`
/// retargeted to `c`, the wikilinks name `c` after the plan, so the delete
/// needs no flag and the plan lands; a Markdown link naming `a.md` is no
/// wikilink and still holds the delete back. A wikilink the rewrite leaves
/// as written — its new spelling would read as emphasis in the heading
/// holding it — still names the deleted document, so it holds the delete
/// back too, at planning as the applier would judge it.
#[test]
fn a_wikilink_rewrite_clears_a_forbidding_deletes_backlinks_only_where_it_respells_them() {
    let mut fixture = Fixture::new(&[
        ("a.md", "A\n"),
        ("c.md", "C\n"),
        ("*x.md", "X\n"),
        ("h.md", "[[a]]\n"),
        ("k.md", "# See [[a]] and b*\n"),
    ]);
    let held_back = |resolution: &crate::planner::resolve::Resolution| {
        resolution
            .unresolved
            .iter()
            .find(|left| matches!(left.operation.kind, OperationKind::DeleteDocument { .. }))
            .map(|left| left.reason.clone())
    };

    let skipped = fixture.planned(vec![deleting("a.md"), retargeting("a", "*x")]);
    assert_eq!(
        held_back(&skipped),
        Some(UnresolvedReason::has_backlinks(vec![path("k.md")], 1))
    );

    fixture.foreign("k.md", "# See it\n");
    let resolution = fixture.resolution(vec![deleting("a.md"), retargeting("a", "c")]);
    assert_eq!(
        resolution.plan.operations[1].cascade,
        [wikilink("h.md", "a", "c")]
    );
    applied(fixture.apply(resolution.plan));
    assert_eq!(fixture.read("h.md").as_deref(), Some("[[c]]\n"));
    assert_eq!(fixture.read("a.md"), None);
    fixture.assert_store_is_a_build_from_zero();

    let fixture = Fixture::new(&[
        ("a.md", "A\n"),
        ("c.md", "C\n"),
        ("h.md", "[[a]] [t](a.md)\n"),
    ]);
    let markdown = fixture.planned(vec![deleting("a.md"), retargeting("a", "c")]);
    assert_eq!(
        held_back(&markdown),
        Some(UnresolvedReason::has_backlinks(vec![path("h.md")], 1))
    );
}
