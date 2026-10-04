//! Moves whose document the plan carries byte for byte, planned over a vault
//! on disk and the store beside it, then previewed and applied: what they
//! resolve to, what they hold while they do, and how they meet a vault that
//! moved under them.

use std::collections::BTreeSet;

use norn_wire::{
    AuthoredPlan, FileState, Operation, RefusedCheck, RootIdentity, TargetResult, UnresolvedReason,
};

use super::{
    Fixture, UNDECODABLE, applied, editing, moving, path, quarantined_fixture, refused, results,
};
use crate::applier::observe::{TargetState, observe, units};
use crate::applier::stage::{Stop, check, stage};
use crate::applier::{Applier, OwnWriteLedger};
use crate::planner::compose::content_hash;
use crate::planner::resolve::Resolution;
use crate::planner::view::{Body, TreeView, VaultView};

impl Fixture {
    /// `operations` resolved against the vault, its links judged on the
    /// store as it stands, whatever of them resolves.
    fn resolved_as_it_stands(&self, operations: Vec<Operation>) -> Resolution {
        let view = TreeView::open(&self.vault, &self.exclusions, &self.schema).expect("a vault");
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

    /// The applier over this fixture, its links judged on `links`.
    fn applier<'a>(&'a self, links: &'a crate::apply::PlanSnapshot<'a>) -> Applier<'a> {
        Applier {
            anchor: &self.vault,
            root: self.root,
            exclusions: &self.exclusions,
            schema: &self.schema,
            shadows: &self.shadows,
            own_writes: &self.recorded as &dyn OwnWriteLedger,
            publishing: &|| true,
            links,
        }
    }
}

fn present(content: &str) -> FileState {
    FileState::present(content_hash(content.as_bytes()))
}

/// A vault holding a document a byte-identical move carries into another
/// folder, a document renamed beside a relative link that still reaches from
/// where it lands, a document holding both a path link and a bare link to
/// the first, and the documents those links name.
fn carried_fixture() -> Fixture {
    Fixture::new(&[
        (
            "notes/a.md",
            "---\ntitle: A\ntags: [x]\n---\n# A\n\nSee [[c]] and [the root](/c.md).\n",
        ),
        ("notes/r.md", "# R\n\n[up](../c.md) and [[a]]\n"),
        ("c.md", "# C\n"),
        ("h.md", "[[a]], [x](notes/a.md) and [[notes/r]]\n"),
    ])
}

/// The resolved plan's JSON and its forecast's, with the root identity — the
/// one value a scratch directory gives differently each run — fixed.
fn pinned_json(resolution: &crate::planner::resolve::Resolution) -> (String, String) {
    let mut plan = resolution.plan.clone();
    plan.root = RootIdentity::from_device_and_inode(1, 2);
    (
        serde_json::to_string_pretty(&plan).expect("a plan"),
        serde_json::to_string_pretty(&resolution.forecast).expect("a forecast"),
    )
}

/// **A byte-identical move resolves to the plan it resolved to while
/// planning held the moved document's body.** Two moves — one into another
/// folder, carrying links that reach from either folder, one renaming a
/// document beside a relative link that still reaches — plan to the
/// resolved plan and forecast pinned here byte for byte, captured before
/// planning stopped holding a moved body; the plan applies, writing each
/// target, and the store equals a build from zero.
#[test]
fn a_byte_identical_move_resolves_to_the_plan_it_always_did() {
    let mut fixture = carried_fixture();
    let resolution = fixture.resolution(vec![
        moving("notes/a.md", "archive/a.md"),
        moving("notes/r.md", "notes/s.md"),
    ]);
    let (plan, forecast) = pinned_json(&resolution);
    assert_eq!(plan, PINNED_PLAN, "{plan}");
    assert_eq!(forecast, PINNED_FORECAST, "{forecast}");
    let finished = applied(fixture.apply(resolution.plan));
    assert!(
        results(&finished)
            .iter()
            .all(|(_, result)| *result == TargetResult::Wrote),
        "{:?}",
        results(&finished)
    );
    assert_eq!(
        fixture.read("archive/a.md").as_deref(),
        Some("---\ntitle: A\ntags: [x]\n---\n# A\n\nSee [[c]] and [the root](/c.md).\n")
    );
    assert_eq!(
        fixture.read("notes/s.md").as_deref(),
        Some("# R\n\n[up](../c.md) and [[a]]\n")
    );
    fixture.assert_store_is_a_build_from_zero();
}

/// The resolved plan [`a_byte_identical_move_resolves_to_the_plan_it_always_did`]
/// plans, captured while planning still held every moved document's body.
const PINNED_PLAN: &str = include_str!("pinned/byte-identical-move.plan.json");

/// Its forecast, captured with it.
const PINNED_FORECAST: &str = include_str!("pinned/byte-identical-move.forecast.json");

/// **A move of a document changed since the vault's index saw it does not
/// resolve, and writes nothing.** The move carries its document unread, so
/// the links it holds are the index's, and the index holds them for other
/// bytes than the file's: the move is left unresolved, saying to re-send
/// once the vault has indexed the change, never planned from a body read
/// instead. The plan it leaves applies nothing, and the file stands where
/// it stood; once the index takes the change in, the move resolves.
#[test]
fn a_move_of_a_document_changed_since_its_indexing_is_unresolved_and_writes_nothing() {
    let mut fixture = Fixture::new(&[("notes/a.md", "# A\n"), ("h.md", "[[a]]\n")]);
    fixture.write("notes/a.md", "# A, changed behind the index\n");
    let resolution = fixture.resolved_as_it_stands(vec![moving("notes/a.md", "archive/a.md")]);
    let [unresolved] = &resolution.unresolved[..] else {
        panic!("the move is unresolved: {:?}", resolution.unresolved);
    };
    let UnresolvedReason::NoLongerResolves { detail, .. } = &unresolved.reason else {
        panic!("no longer resolves: {:?}", unresolved.reason);
    };
    assert!(
        detail.contains("`notes/a.md`") && detail.contains("index"),
        "{detail}"
    );
    assert_eq!(resolution.plan.transitions, Vec::new());
    applied(fixture.apply(resolution.plan));
    assert_eq!(fixture.tree(), vec!["h.md", "notes", "notes/a.md"]);
    fixture.foreign("notes/a.md", "# A, changed behind the index\n");
    let resolution = fixture.resolved_as_it_stands(vec![moving("notes/a.md", "archive/a.md")]);
    assert_eq!(resolution.unresolved, Vec::new());
}

/// **A moved file whose bytes do not decode keeps its quarantined flag and
/// lands byte for byte.** It holds no link, which is what its bytes would
/// yield, so the index — which holds no document for it — is not asked to
/// vouch for any, and both sides of the move record its bytes as
/// quarantined.
#[test]
fn a_moved_quarantined_file_keeps_its_flag_and_lands_byte_identical() {
    let mut fixture = quarantined_fixture();
    let plan = fixture.plan(vec![moving("q.md", "elsewhere/q.md")]);
    let quarantined = FileState::quarantined(content_hash(UNDECODABLE));
    assert_eq!(
        plan.transitions,
        vec![
            norn_wire::Transition::new(
                path("elsewhere/q.md"),
                FileState::absent(),
                quarantined.clone()
            ),
            norn_wire::Transition::new(path("q.md"), quarantined, FileState::absent()),
        ]
    );
    applied(fixture.apply(plan));
    assert_eq!(
        std::fs::read(fixture.vault.join("elsewhere/q.md")).expect("the moved file"),
        UNDECODABLE
    );
    fixture.assert_store_is_a_build_from_zero();
}

/// **The applier reads a carried move's two ends streamed.** Observed for
/// its apply, a byte-identical move's source stands at its before-state
/// with no bytes held, only whether they decode, and its destination holds
/// nothing; the document a plan edits, and the holder its cascade rewrites,
/// are held whole.
#[test]
fn the_applier_observes_a_carried_move_without_holding_its_document() {
    let fixture = carried_fixture();
    let plan = fixture.plan(vec![
        moving("notes/a.md", "archive/a.md"),
        editing("c.md", "# C", "# See"),
    ]);
    let view =
        TreeView::open(&fixture.vault, &fixture.exclusions, &fixture.schema).expect("a vault");
    let units = units(&plan, view.normalizer());
    let (states, _) = observe(&plan, &units, &view).expect("a readable vault");
    let held: Vec<(&str, &str)> = plan
        .transitions
        .iter()
        .zip(&states)
        .map(|(transition, state)| {
            let held = match state {
                TargetState::AtBefore(None) => "nothing",
                TargetState::AtBefore(Some(Body::Streamed { decodes: true })) => "streamed",
                TargetState::AtBefore(Some(Body::Held(_))) => "held",
                other => panic!("{}: {other:?}", transition.path),
            };
            (transition.path.as_str(), held)
        })
        .collect();
    assert_eq!(
        held,
        vec![
            ("archive/a.md", "nothing"),
            ("c.md", "held"),
            ("h.md", "held"),
            ("notes/a.md", "streamed"),
        ]
    );
}

/// **A move and an edit of the moved document in one plan compose as they
/// always did**: the edit lands on the document where the move put it,
/// which composition reads whole once, and the plan applies.
#[test]
fn a_move_then_an_edit_of_the_moved_document_composes_the_edit_where_it_landed() {
    let mut fixture = carried_fixture();
    let plan = fixture.plan(vec![
        moving("notes/a.md", "archive/a.md"),
        editing("archive/a.md", "# A", "# A, moved"),
    ]);
    applied(fixture.apply(plan));
    assert_eq!(
        fixture.read("archive/a.md").as_deref(),
        Some("---\ntitle: A\ntags: [x]\n---\n# A, moved\n\nSee [[c]] and [the root](/c.md).\n")
    );
    assert_eq!(fixture.read("notes/a.md"), None);
    fixture.assert_store_is_a_build_from_zero();
}

/// **A moved document holding links to itself is rewritten as it always
/// was.** Its relative link to its own file and its bare wikilink to its own
/// stem both stop naming it once it lands under a new stem in another
/// folder, as the path link in the holder naming it does: the cascade
/// respells all three, the moved document read whole once for its own two.
#[test]
fn a_moved_document_naming_itself_is_respelled_with_its_in_links() {
    let mut fixture = Fixture::new(&[
        ("notes/a.md", "[me](a.md), [[a]] and [up](../c.md)\n"),
        ("c.md", "# C\n"),
        ("h.md", "[x](notes/a.md)\n"),
    ]);
    let plan = fixture.plan(vec![moving("notes/a.md", "archive/b.md")]);
    applied(fixture.apply(plan));
    assert_eq!(
        fixture.read("archive/b.md").as_deref(),
        Some("[me](b.md), [[b]] and [up](../c.md)\n")
    );
    assert_eq!(fixture.read("h.md").as_deref(), Some("[x](archive/b.md)\n"));
    fixture.assert_store_is_a_build_from_zero();
}

/// **A chain of moves carries its document through every name.** `b.md`
/// moves on to `c.md` and `a.md` into the name it vacates — the latter a
/// replace of `b.md`, which the write kernel publishes from bytes, so its
/// source is read whole for the apply — and `[[b]]` in `h.md`, naming the
/// document carried to `c.md`, follows it.
#[test]
fn a_chain_of_moves_carries_each_document_where_it_lands() {
    let mut fixture = Fixture::new(&[("a.md", "# A\n"), ("b.md", "# B\n"), ("h.md", "[[b]]\n")]);
    let plan = fixture.plan(vec![moving("b.md", "c.md"), moving("a.md", "b.md")]);
    applied(fixture.apply(plan));
    assert_eq!(fixture.read("b.md").as_deref(), Some("# A\n"));
    assert_eq!(fixture.read("c.md").as_deref(), Some("# B\n"));
    assert_eq!(fixture.read("h.md").as_deref(), Some("[[c]]\n"));
    assert_eq!(fixture.tree(), vec!["b.md", "c.md", "h.md"]);
    fixture.assert_store_is_a_build_from_zero();
}

/// **A carried move previews as the plan its apply lands.** The preview of
/// the resolved plan answers the same plan; applied, every target is
/// written; previewed again over what landed, it answers the same plan,
/// and applied again finds every target landed.
#[test]
fn a_carried_move_previews_as_its_apply_lands() {
    let mut fixture = carried_fixture();
    let plan = fixture.plan(vec![moving("notes/a.md", "archive/a.md")]);
    let (previewed, _) = fixture.preview(plan.clone()).expect("the preview answers");
    assert_eq!(previewed, plan);
    applied(fixture.apply(plan.clone()));
    let (previewed, _) = fixture
        .preview(plan.clone())
        .expect("the landed plan previews");
    assert_eq!(previewed, plan);
    let again = applied(fixture.apply(plan));
    assert!(
        again
            .targets
            .iter()
            .all(|target| target.result == TargetResult::Found),
        "{:?}",
        again.targets
    );
}

/// **A carried move re-sent after it landed in part is finished**, whatever
/// the index took in of the landing: with the destination landed and the
/// source still standing, with both landed and the index not yet told, and
/// with both landed and the index holding the document at its destination.
/// Each re-send finds what landed, writes the rest, and leaves the store a
/// build from zero.
#[test]
fn a_carried_move_re_sent_after_it_landed_in_part_is_finished() {
    let content = "# A\n\n[[c]]\n";
    for (landing, source_removed, indexed) in [
        ("destination landed", false, false),
        ("both landed, unindexed", true, false),
        ("both landed, indexed", true, true),
    ] {
        let mut fixture = Fixture::new(&[("notes/a.md", content), ("c.md", "# C\n")]);
        let plan = fixture.plan(vec![moving("notes/a.md", "archive/a.md")]);
        fixture.write("archive/a.md", content);
        if source_removed {
            std::fs::remove_file(fixture.vault.join("notes/a.md")).expect("the source goes");
        }
        if indexed {
            fixture.foreign("archive/a.md", content);
        }
        let finished = applied(fixture.apply(plan));
        assert_eq!(
            results(&finished),
            vec![
                ("archive/a.md".to_string(), TargetResult::Found),
                (
                    "notes/a.md".to_string(),
                    if source_removed {
                        TargetResult::Found
                    } else {
                        TargetResult::Wrote
                    }
                ),
            ],
            "{landing}"
        );
        assert_eq!(
            fixture.read("archive/a.md").as_deref(),
            Some(content),
            "{landing}"
        );
        fixture.assert_store_is_a_build_from_zero();
    }
}

/// **A copy's source that is not what the plan carries is reported as the
/// source drifting, never as the destination.** Between the check and the
/// staging of a carried move, another writer changes the source, or puts a
/// link at its name: the write kernel refuses the copy naming the source,
/// and the plan stops refused with the source's own path drifted — holding
/// what it holds, or nothing where a link stands — and nothing staged.
#[test]
fn a_copy_whose_source_drifted_before_staging_is_the_sources_drift() {
    for (what, holds) in [
        ("changed", present("# A, changed\n")),
        ("linked", FileState::absent()),
    ] {
        let fixture = carried_fixture();
        let plan = fixture.plan(vec![moving("notes/a.md", "archive/a.md")]);
        let view =
            TreeView::open(&fixture.vault, &fixture.exclusions, &fixture.schema).expect("a vault");
        let declared = crate::derivation::Declared::unpinned();
        let links = fixture.links();
        let index = links.index();
        let checked = check(&plan, &view, &declared, &index).expect("the plan checks");
        let source = fixture.vault.join("notes/a.md");
        match what {
            "changed" => std::fs::write(&source, "# A, changed\n").expect("a foreign edit"),
            _ => {
                let aside = fixture.vault.join("aside.md");
                std::fs::rename(&source, &aside).expect("the source moves aside");
                std::os::unix::fs::symlink(&aside, &source).expect("a link at its name");
            }
        }
        let applier = fixture.applier(&index);
        let stopped = stage(
            &applier.ground(),
            &fixture.shadows,
            &plan,
            view.normalizer(),
            checked,
        )
        .expect_err("staging refuses");
        let Stop::Refused(checks) = stopped else {
            panic!("{what}: refused, not {stopped:?}");
        };
        assert_eq!(
            checks,
            vec![RefusedCheck::drifted(path("notes/a.md"), holds)],
            "{what}"
        );
        assert_eq!(fixture.shadows_left(), Vec::<String>::new(), "{what}");
    }
}

/// **A refusal's fresh plan for a carried move whose source drifted is the
/// move over what the source holds now**, once the index holds it too.
#[test]
fn a_carried_move_whose_source_another_writer_changed_is_refused_with_a_fresh_plan() {
    let mut fixture = carried_fixture();
    let plan = fixture.plan(vec![moving("notes/a.md", "archive/a.md")]);
    fixture.foreign("notes/a.md", "# A, again\n");
    let refused = refused(fixture.apply(plan));
    assert!(
        refused.checks.contains(&RefusedCheck::drifted(
            path("notes/a.md"),
            present("# A, again\n")
        )),
        "{:?}",
        refused.checks
    );
    assert_eq!(fixture.read("notes/a.md").as_deref(), Some("# A, again\n"));
    assert_eq!(fixture.read("archive/a.md"), None);
}
