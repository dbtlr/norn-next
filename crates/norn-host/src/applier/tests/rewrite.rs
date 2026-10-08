//! Link rewrites planned over a vault on disk and the store beside it, then
//! judged and applied by the applier: which wikilinks a `rewrite_wikilink`
//! retargets and how each is spelled, what it is left unresolved for, and
//! the cascade it carries.

use norn_wire::{
    AmbiguousEnd, Candidate, CandidateHead, LinkAdvisory, LinkFamily, LinkKey, LinkRewrite,
    Operation, OperationId, OperationKind, PlanCondition, RefusedCheck, ResolutionTarget, Resolves,
    UnresolvedOperation, UnresolvedReason,
};

use norn_store::StoredPathOrder;

use super::{Fixture, applied, breaking, creating, deleting, path};

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

/// **An `old` naming no document repairs the broken wikilinks that would
/// name a document standing at the place it spells**, as the root reads
/// them: with nothing at `Old Note.md`, `[[Old Note]]`, `[[Old Note.md]]`
/// and `[[vault://Old Note]]` each name `c.md` after, in their own forms,
/// and `[[old note|x]]` too only where the root folds ASCII case. Another
/// broken link, `[[sub/Old Note]]` — which names a deeper place — and
/// `[[v1]]` are left as they are; and a rewrite of `v1.2`, spelling
/// `v1.2.md`, never repairs `[[v1]]`.
#[test]
fn an_old_naming_nothing_repairs_the_broken_links_that_would_name_its_place() {
    let mut fixture = Fixture::new(&[
        ("c.md", "C\n"),
        (
            "h.md",
            "[[Old Note]] [[old note|x]] [[Other Gone]] [[sub/Old Note]] [[Old Note.md]] [[vault://Old Note]] [[v1]]\n",
        ),
    ]);
    let folds = fixture.store.path_order() == StoredPathOrder::AsciiCaseInsensitive;
    let resolution = fixture.resolution(vec![retargeting("Old Note", "c")]);
    let mut repaired = vec![
        wikilink("h.md", "Old Note", "c"),
        wikilink("h.md", "Old Note.md", "c.md"),
    ];
    if folds {
        repaired.push(wikilink("h.md", "old note", "c"));
    }
    repaired.push(wikilink("h.md", "vault://Old Note", "vault://c"));
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
            format!("[[c]] {kept} [[Other Gone]] [[sub/Old Note]] [[c.md]] [[vault://c]] [[v1]]\n")
                .as_str()
        )
    );
    fixture.assert_store_is_a_build_from_zero();
}

/// **A broken link is repaired by an `old` spelling its place as the root
/// reads it**: on a root telling spellings apart, `old note` repairs
/// `[[old note]]` alone, and `Old Note.md`, spelling the same place as `Old
/// Note`, repairs `[[Old Note]]` and `[[Old Note.md]]`; on a root folding
/// ASCII case either repairs all three.
#[test]
fn a_broken_link_is_repaired_by_an_old_spelled_as_the_root_reads_it() {
    let fixture = Fixture::new(&[
        ("c.md", "C\n"),
        ("h.md", "[[Old Note]] [[old note]] [[Old Note.md]]\n"),
    ]);
    let folds = fixture.store.path_order() == StoredPathOrder::AsciiCaseInsensitive;
    let every = [
        wikilink("h.md", "Old Note", "c"),
        wikilink("h.md", "Old Note.md", "c.md"),
        wikilink("h.md", "old note", "c"),
    ];
    let lower = fixture.resolution(vec![retargeting("old note", "c")]);
    if folds {
        assert_eq!(lower.plan.operations[0].cascade, every);
    } else {
        assert_eq!(
            lower.plan.operations[0].cascade,
            [wikilink("h.md", "old note", "c")]
        );
    }

    let suffixed = fixture.resolution(vec![retargeting("Old Note.md", "c")]);
    if folds {
        assert_eq!(suffixed.plan.operations[0].cascade, every);
    } else {
        assert_eq!(suffixed.plan.operations[0].cascade, every[..2]);
    }
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

/// **Two wikilink rewrites selecting one wikilink are not both planned.**
/// `a` and `a.md` name one document, so both rewrites would retarget
/// `[[a]]`: the earlier in plan order retargets it, and the later is left
/// unresolved naming the earlier, by its identifier where it carries one and
/// else by its position, rather than saying it retargets nothing.
#[test]
fn a_later_wikilink_rewrite_selecting_an_earlier_ones_wikilink_is_unresolved_naming_it() {
    let fixture = Fixture::new(&[
        ("a.md", "A\n"),
        ("c.md", "C\n"),
        ("d.md", "D\n"),
        ("h.md", "[[a]]\n"),
    ]);
    let unnamed = fixture.planned(vec![retargeting("a", "c"), retargeting("a.md", "d")]);
    assert_eq!(
        unnamed.plan.operations[0].cascade,
        [wikilink("h.md", "a", "c")]
    );
    let detail = unresolved_detail(&unnamed);
    assert!(detail.contains("at position 0"), "{detail}");
    assert!(!detail.contains("no wikilink"), "{detail}");

    let first = OperationId::new("first").expect("an identifier");
    let named = fixture.planned(vec![
        retargeting("a", "c").with_id(first),
        retargeting("a.md", "d"),
    ]);
    let detail = unresolved_detail(&named);
    assert!(detail.contains("`first`"), "{detail}");
}

/// **A wikilink rewrite left unresolved takes no wikilink from another.**
/// `zzz` names no document, so the rewrite of `a` to it is left unresolved
/// saying so, and the later rewrite of `a.md` to `d`, selecting the same
/// `[[a]]`, retargets it rather than being left unresolved naming the first.
#[test]
fn a_wikilink_rewrite_left_unresolved_takes_no_wikilink_from_a_later_one() {
    let fixture = Fixture::new(&[("a.md", "A\n"), ("d.md", "D\n"), ("h.md", "[[a]]\n")]);
    let resolution = fixture.planned(vec![retargeting("a", "zzz"), retargeting("a.md", "d")]);
    let detail = unresolved_detail(&resolution);
    assert!(detail.contains("names no one document"), "{detail}");
    assert_eq!(
        resolution.plan.operations[0].cascade,
        [wikilink("h.md", "a", "d")]
    );
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

/// **`old` and `new` naming one document across a move are the same
/// document.** `a` names `a.md` before the plan and `b` names `b.md` after
/// it, where the move lands that same document, so no wikilink would change:
/// the rewrite is left unresolved saying so, and the move lands with its own
/// cascade.
#[test]
fn a_rewrite_naming_a_moved_document_before_and_after_its_move_is_unresolved_as_one_document() {
    let fixture = Fixture::new(&[("a.md", "A\n"), ("h.md", "[[a]]\n")]);
    let resolution = fixture.planned(vec![super::moving("a.md", "b.md"), retargeting("a", "b")]);
    let detail = unresolved_detail(&resolution);
    assert!(detail.contains("the same document"), "{detail}");
    assert_eq!(
        resolution.plan.operations[0].cascade,
        [wikilink("h.md", "a", "b")]
    );
}

/// **A wikilink rewrite whose wikilinks need nothing from it says so, not
/// that none names `old`.** One whose only wikilink is an authored link
/// rewrite's, and one whose wikilink already names `new`'s document where
/// the plan leaves the vault — `a.md` deleted and `c.md` moved into its
/// path — each change nothing and are left unresolved saying why.
#[test]
fn a_wikilink_rewrite_whose_wikilinks_need_nothing_says_why() {
    let fixture = Fixture::new(&[("a.md", "A\n"), ("c.md", "C\n"), ("h.md", "[[a]]\n")]);
    let authored = fixture.planned(vec![
        relinking("h.md", LinkFamily::Wikilink, "a", "x]]y"),
        retargeting("a", "c"),
    ]);
    let detail = unresolved_detail(&authored);
    assert!(detail.contains("authored link rewrite"), "{detail}");
    assert!(!detail.contains("no wikilink"), "{detail}");

    let named = fixture.planned(vec![
        breaking("a.md"),
        super::moving("c.md", "a.md"),
        retargeting("a", "a"),
    ]);
    let detail = unresolved_detail(&named);
    assert!(detail.contains("already names"), "{detail}");
    assert!(!detail.contains("no wikilink"), "{detail}");
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
/// link — or no document stands where it names one.
#[test]
fn an_authored_link_rewrite_matching_no_link_is_unresolved() {
    let fixture = Fixture::new(&[("a.md", "A\n"), ("h.md", "[[a]]\n")]);
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
}

/// **An authored link rewrite respelling a link to the text it already holds
/// lands found**, as an edit rewriting what its document holds does: it
/// matches a link, so it acts, and its holder's after-state is its
/// before-state.
#[test]
fn an_authored_link_rewrite_to_the_text_its_link_holds_lands_found() {
    let mut fixture = Fixture::new(&[("a.md", "A\n"), ("h.md", "[[a]]\n")]);
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

/// **A wikilink rewrite selects a wikilink by what it is before the plan,
/// never by the text an authored link rewrite of the same plan writes.**
/// `[[d]]`, respelled to `[[a]]` by hand, named `d.md` before the plan, so a
/// rewrite of `a` to `c` retargets nothing — a holder's rewrites match the
/// text it held — and is left unresolved saying so, while the authored
/// rewrite lands alone.
#[test]
fn a_wikilink_rewrite_never_selects_the_text_an_authored_rewrite_writes() {
    let mut fixture = Fixture::new(&[
        ("a.md", "A\n"),
        ("c.md", "C\n"),
        ("d.md", "D\n"),
        ("h.md", "[[d]]\n"),
    ]);
    let resolution = fixture.planned(vec![
        relinking("h.md", LinkFamily::Wikilink, "d", "a"),
        retargeting("a", "c"),
    ]);
    let detail = unresolved_detail(&resolution);
    assert!(detail.contains("no wikilink"), "{detail}");
    assert!(
        resolution
            .plan
            .operations
            .iter()
            .all(|operation| operation.cascade.is_empty()),
        "{:?}",
        resolution.plan.operations
    );
    applied(fixture.apply(resolution.plan));
    assert_eq!(fixture.read("h.md").as_deref(), Some("[[a]]\n"));
    fixture.assert_store_is_a_build_from_zero();
}

/// **An authored link rewrite decides the link it names, whatever a move or
/// a delete of the plan would do with it**: its author said what it names.
/// With `a.md` moved to `b.md`, a respelling of `[[a]]` the text layer
/// refuses leaves it as written for the author's reason, and the move's
/// cascade does not also respell it — two rewrites of one link would leave
/// it as written for their conflict. With `a.md` deleted forbidding the
/// links naming it, `[[a]]` respelled to `[[c]]` names `c.md`, so the delete
/// needs no flag and the plan lands, while one the text layer refuses still
/// names `a.md` and holds the delete back.
#[test]
fn an_authored_link_rewrite_decides_its_link_over_a_move_or_a_delete() {
    let mut fixture = Fixture::new(&[("a.md", "A\n"), ("c.md", "C\n"), ("h.md", "[[a]]\n")]);
    let moved = fixture.resolution(vec![
        super::moving("a.md", "b.md"),
        relinking("h.md", LinkFamily::Wikilink, "a", "x]]y"),
    ]);
    assert!(
        moved
            .plan
            .operations
            .iter()
            .all(|operation| operation.cascade.is_empty()),
        "{:?}",
        moved.plan.operations
    );
    assert_eq!(
        moved.forecast.links,
        [LinkAdvisory::skipped_unrepresentable(key("h.md", "a"))]
    );

    let refused = fixture.planned(vec![
        deleting("a.md"),
        relinking("h.md", LinkFamily::Wikilink, "a", "x]]y"),
    ]);
    assert_eq!(
        refused
            .unresolved
            .iter()
            .map(|left| left.reason.clone())
            .collect::<Vec<_>>(),
        [UnresolvedReason::has_backlinks(vec![path("h.md")], 1)]
    );

    let cleared = fixture.resolution(vec![
        deleting("a.md"),
        relinking("h.md", LinkFamily::Wikilink, "a", "c"),
    ]);
    let (previewed, forecast) = fixture
        .preview(cleared.plan.clone())
        .expect("the plan previews");
    assert_eq!(previewed, cleared.plan);
    assert_eq!(forecast.links, cleared.forecast.links);
    applied(fixture.apply(cleared.plan));
    assert_eq!(fixture.read("h.md").as_deref(), Some("[[c]]\n"));
    assert_eq!(fixture.read("a.md"), None);
    fixture.assert_store_is_a_build_from_zero();
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

/// **An `old` naming a quarantined file's place names no document**, so it
/// repairs the broken wikilinks naming that place: no link resolves to the
/// bytes at `q.md`, which do not decode, so `[[q]]` and `[[q.md]]` are
/// broken before the plan and each is retargeted to `c.md` in its own form,
/// recorded as written at its new address, while the Markdown `[x](q.md)`
/// is no wikilink and stays. The preview
/// recomputes the plan, and it lands with the quarantined file untouched.
#[test]
fn an_old_naming_a_quarantined_file_repairs_the_broken_wikilinks_naming_its_place() {
    let mut fixture = Fixture::new(&[("c.md", "C\n"), ("h.md", "[[q]] [[q.md]] [x](q.md)\n")]);
    fixture.foreign("q.md", super::UNDECODABLE);
    let resolution = fixture.resolution(vec![retargeting("q", "c")]);
    assert_eq!(
        resolution.plan.operations[0].cascade,
        [wikilink("h.md", "q", "c"), wikilink("h.md", "q.md", "c.md")]
    );
    assert_eq!(
        resolution.plan.conditions,
        [
            PlanCondition::link_resolution(
                key("h.md", "c"),
                Resolves::one(path("c.md")),
                Resolves::one(path("c.md")),
            ),
            PlanCondition::link_resolution(
                key("h.md", "c.md"),
                Resolves::one(path("c.md")),
                Resolves::one(path("c.md")),
            ),
        ]
    );
    assert!(resolution.forecast.links.is_empty());
    let (previewed, forecast) = fixture
        .preview(resolution.plan.clone())
        .expect("the planned cascade previews");
    assert_eq!(previewed, resolution.plan);
    assert_eq!(forecast.links, resolution.forecast.links);
    applied(fixture.apply(resolution.plan));
    assert_eq!(
        fixture.read("h.md").as_deref(),
        Some("[[c]] [[c.md]] [x](q.md)\n")
    );
    assert_eq!(
        std::fs::read(fixture.vault.join("q.md")).ok().as_deref(),
        Some(super::UNDECODABLE)
    );
}

/// **A `new` naming a quarantined file names no document.** The bytes at
/// `q.md` do not decode, so no wikilink can be retargeted toward them: a
/// rewrite of `[[a]]` to `q`, or to `q.md`, is left unresolved as one whose
/// `new` names no one document, and writes nothing.
#[test]
fn a_new_naming_a_quarantined_file_is_unresolved() {
    let mut fixture = Fixture::new(&[("a.md", "A\n"), ("h.md", "[[a]]\n")]);
    fixture.foreign("q.md", super::UNDECODABLE);
    for new in ["q", "q.md"] {
        let resolution = fixture.planned(vec![retargeting("a", new)]);
        let detail = unresolved_detail(&resolution);
        assert!(detail.contains("names no one document"), "{detail}");
        assert!(resolution.plan.transitions.is_empty());
    }
}

/// **A wikilink rewrite reaches a frontmatter link not written literally and
/// leaves it with its reason**: an escaped scalar's link is in the link
/// graph and names the old target, but no bytes of the value are its text,
/// so it is skipped as not written literally and the holder is unchanged.
#[test]
fn a_frontmatter_link_not_written_literally_is_skipped_with_its_reason() {
    let holder = "---\nsee: \"\\x5B[a]]\"\n---\nbody\n";
    let mut fixture = Fixture::new(&[("a.md", "A\n"), ("c.md", "C\n"), ("h.md", holder)]);
    let resolution = fixture.resolution(vec![retargeting("a", "c")]);
    assert_eq!(
        resolution.forecast.links,
        vec![LinkAdvisory::skipped_not_written_literally(key(
            "h.md", "a"
        ))]
    );
    applied(fixture.apply(resolution.plan));
    assert_eq!(fixture.read("h.md").as_deref(), Some(holder));
    fixture.assert_store_is_a_build_from_zero();
}
