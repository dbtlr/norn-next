//! Moves whose document the plan carries byte for byte, planned over a vault
//! on disk and the store beside it, then previewed and applied: what they
//! resolve to, what they hold while they do, and how they meet a vault that
//! moved under them.

use norn_wire::{RootIdentity, TargetResult};

use super::{Fixture, applied, moving, results};

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
