//! Moves whose document the plan carries byte for byte, planned over a vault
//! on disk and the store beside it, then previewed and applied: what they
//! resolve to, what they hold while they do, and how they meet a vault that
//! moved under them.

use std::collections::BTreeSet;

use norn_wire::{AuthoredPlan, FileState, Operation, RefusedCheck, RootIdentity, TargetResult};

use super::{
    Fixture, UNDECODABLE, applied, breaking, editing, moving, path, quarantined_fixture, refused,
    results,
};
use crate::applier::observe::{TargetState, observe, units};
use crate::applier::schema::Citations;
use crate::applier::stage::{Stop, check, stage};
use crate::applier::{Applier, OwnWriteLedger};
use crate::planner::compose::content_hash;
use crate::planner::view::{Body, Entry, TreeView, VaultView};

impl Fixture {
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

/// **A move of a document the vault's index has not taken in plans and
/// applies exactly as it did before a move carried anything, holding one
/// copy of it.** The index holds the moved document at other bytes than the
/// file, so it vouches for none of the links the document holds: planning
/// reads the file whole once, beside the streamed read that found its hash,
/// and reads its links from those bytes; the plan it answers is the one it
/// answers once the index has taken the change in. The applier observes the
/// file streamed and reads it whole once, for its links, where the index
/// still lags; the plan applies, writing each target.
#[test]
fn a_move_of_a_document_changed_since_its_indexing_plans_and_applies_holding_one_copy() {
    let changed = "# A, changed behind the index [[h]]\n";
    let mut fixture = Fixture::new(&[("notes/a.md", "# A\n"), ("h.md", "[[a]]\n")]);
    fixture.write("notes/a.md", changed);
    let operations = vec![moving("notes/a.md", "archive/a.md")];

    let tree =
        TreeView::open(&fixture.vault, &fixture.exclusions, &fixture.schema).expect("a vault");
    let counted = Counted::over(&tree);
    let links = fixture.links();
    let resolution = crate::planner::resolve::resolve(
        AuthoredPlan::new(crate::planner::links::testing::vault(), operations.clone()),
        fixture.root_identity(),
        &BTreeSet::new(),
        &counted,
        &links.index(),
    )
    .unwrap_or_else(|failure| panic!("the plan is planned: {failure:?}"));
    assert_eq!(resolution.unresolved, Vec::new());
    assert_eq!(
        counted.reads("notes/a.md"),
        Reads {
            whole: 1,
            streamed: 1
        },
        "planning holds one copy"
    );

    let declared = crate::derivation::Declared::unpinned();
    let counted = Counted::over(&tree);
    check(
        &resolution.plan,
        &counted,
        &declared,
        &links.index(),
        &mut Citations::default(),
    )
    .expect("the plan checks");
    assert_eq!(
        counted.reads("notes/a.md"),
        Reads {
            whole: 1,
            streamed: 1
        },
        "the applier holds one copy"
    );
    drop(links);

    let finished = applied(fixture.apply(resolution.plan.clone()));
    assert!(
        results(&finished)
            .iter()
            .all(|(_, result)| *result == TargetResult::Wrote),
        "{:?}",
        results(&finished)
    );
    assert_eq!(fixture.read("archive/a.md").as_deref(), Some(changed));
    fixture.assert_store_is_a_build_from_zero();

    let mut indexed = Fixture::new(&[("notes/a.md", "# A\n"), ("h.md", "[[a]]\n")]);
    indexed.foreign("notes/a.md", changed);
    let mut as_indexed = indexed.resolution(operations).plan;
    as_indexed.root = resolution.plan.root.clone();
    assert_eq!(
        resolution.plan, as_indexed,
        "the plan is the one planned once the index took the change in"
    );
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

/// **A chain of moves carries each document end to end.** `b.md` moves on
/// to `c.md` and `a.md` into the name it vacates — a replace of `b.md`,
/// which the write kernel stages as a copy of `a.md` as it stages the create
/// of `c.md` as a copy of `b.md` — so the applier observes every target of
/// the chain streamed and holds no moved body; `[[b]]` in `h.md`, naming the
/// document carried to `c.md`, follows it.
#[test]
fn a_chain_of_moves_carries_each_document_where_it_lands() {
    let mut fixture = Fixture::new(&[("a.md", "# A\n"), ("b.md", "# B\n"), ("h.md", "[[b]]\n")]);
    let plan = fixture.plan(vec![moving("b.md", "c.md"), moving("a.md", "b.md")]);
    let tree =
        TreeView::open(&fixture.vault, &fixture.exclusions, &fixture.schema).expect("a vault");
    let counted = Counted::over(&tree);
    let units = units(&plan, tree.normalizer());
    observe(&plan, &units, &counted).expect("a readable vault");
    for at in ["a.md", "b.md", "c.md"] {
        assert_eq!(counted.reads(at).whole, 0, "{at} is read whole");
    }
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
/// staging of a carried move, another writer changes the source, puts a
/// link at its name, puts a folder there, or puts a link where its folder
/// stood: the write kernel refuses the copy naming the source — as drift,
/// a link, a non-file or a linked folder — and the plan stops refused with
/// the source's own path drifted, holding what it holds or nothing where no
/// document stands, and nothing staged.
#[test]
fn a_copy_whose_source_drifted_before_staging_is_the_sources_drift() {
    for (what, holds) in [
        ("changed", present("# A, changed\n")),
        ("linked", FileState::absent()),
        ("a folder", FileState::absent()),
        ("beneath a linked folder", FileState::absent()),
    ] {
        let fixture = carried_fixture();
        let plan = fixture.plan(vec![moving("notes/a.md", "archive/a.md")]);
        let view =
            TreeView::open(&fixture.vault, &fixture.exclusions, &fixture.schema).expect("a vault");
        let declared = crate::derivation::Declared::unpinned();
        let links = fixture.links();
        let index = links.index();
        let checked = check(&plan, &view, &declared, &index, &mut Citations::default())
            .expect("the plan checks");
        let source = fixture.vault.join("notes/a.md");
        match what {
            "changed" => std::fs::write(&source, "# A, changed\n").expect("a foreign edit"),
            "linked" => {
                let aside = fixture.vault.join("aside.md");
                std::fs::rename(&source, &aside).expect("the source moves aside");
                std::os::unix::fs::symlink(&aside, &source).expect("a link at its name");
            }
            "a folder" => {
                std::fs::remove_file(&source).expect("the source goes");
                std::fs::create_dir(&source).expect("a folder at its name");
            }
            _ => {
                let folder = fixture.vault.join("notes");
                let aside = fixture.vault.join("aside");
                std::fs::rename(&folder, &aside).expect("the source's folder moves aside");
                std::os::unix::fs::symlink(&aside, &folder).expect("a link at the folder's name");
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

/// **A check whose index no longer vouches for a carried document reads it
/// whole rather than refuse it.** Planned while the index held the moved
/// document at its bytes, the plan meets an index that took in another
/// writer's change since, though the file is back at the bytes the plan
/// carries: the source stands at its before-state, so nothing drifted, and
/// the check reads the links it holds from the file itself. The plan
/// applies, and the store equals a build from zero.
#[test]
fn a_carried_move_whose_index_moved_on_is_checked_from_the_file() {
    let mut fixture = carried_fixture();
    let plan = fixture.plan(vec![moving("notes/a.md", "archive/a.md")]);
    let original = fixture.read("notes/a.md").expect("the source");
    fixture.foreign("notes/a.md", "# A, for a moment\n");
    fixture.write("notes/a.md", &original);
    let finished = applied(fixture.apply(plan));
    assert!(
        results(&finished)
            .iter()
            .all(|(_, result)| *result == TargetResult::Wrote),
        "{:?}",
        results(&finished)
    );
    assert_eq!(fixture.read("archive/a.md"), Some(original));
    fixture.assert_store_is_a_build_from_zero();
}

/// **A check whose index moved on reads the links of every carried document
/// from its file.** Two carried moves are planned while the index holds both
/// documents at their bytes; another writer then changes `notes/r.md`, the
/// index takes the change in, and the file goes back to the bytes the plan
/// carries. The index vouches for `notes/a.md` alone, so the check reads
/// `notes/r.md`'s links — a relative link and a wikilink — from the file,
/// and the change set it computes again is the one the plan records: the
/// plan applies, and the store equals a build from zero.
#[test]
fn every_carried_document_the_index_no_longer_vouches_for_is_checked_from_its_file() {
    let mut fixture = carried_fixture();
    let plan = fixture.plan(vec![
        moving("notes/a.md", "archive/a.md"),
        moving("notes/r.md", "notes/s.md"),
    ]);
    let original = fixture.read("notes/r.md").expect("the source");
    fixture.foreign("notes/r.md", "# R, for a moment\n");
    fixture.write("notes/r.md", &original);
    let finished = applied(fixture.apply(plan));
    assert!(
        results(&finished)
            .iter()
            .all(|(_, result)| *result == TargetResult::Wrote),
        "{:?}",
        results(&finished)
    );
    assert_eq!(fixture.read("notes/s.md"), Some(original));
    fixture.assert_store_is_a_build_from_zero();
}

/// **A re-sent carried move whose index took in only its source's removal
/// is finished from its landed destination.** The destination landed and
/// the source is gone, and the index holds neither document at the bytes
/// the move carries — it took in the removal, not the landing — so the
/// check reads the copy's links from the destination standing at that hash,
/// never from the source that is gone: every target is found, and the
/// store equals a build from zero.
#[test]
fn a_re_sent_carried_move_indexed_only_at_its_source_is_finished_from_its_destination() {
    let content = "# A\n\n[[c]] and [up](../c.md)\n";
    let mut fixture = Fixture::new(&[("notes/a.md", content), ("c.md", "# C\n")]);
    let plan = fixture.plan(vec![moving("notes/a.md", "archive/a.md")]);
    std::fs::remove_file(fixture.vault.join("notes/a.md")).expect("the source goes");
    crate::production::heal_from_zero(&mut fixture.store, &fixture.vault, &fixture.exclusions)
        .expect("the index takes the removal in");
    fixture.write("archive/a.md", content);
    let finished = applied(fixture.apply(plan));
    assert_eq!(
        results(&finished),
        vec![
            ("archive/a.md".to_string(), TargetResult::Found),
            ("notes/a.md".to_string(), TargetResult::Found),
        ]
    );
    fixture.assert_store_is_a_build_from_zero();
}

/// How many times a view was asked for one name, whole and streamed.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct Reads {
    whole: usize,
    streamed: usize,
}

/// A view over `inner` that counts what it is asked for, name by name: what
/// a case holds planning and the applier to, by the reads they take.
struct Counted<'a, V> {
    inner: &'a V,
    reads: std::cell::RefCell<std::collections::BTreeMap<String, Reads>>,
}

impl<'a, V: VaultView> Counted<'a, V> {
    fn over(inner: &'a V) -> Self {
        Counted {
            inner,
            reads: std::cell::RefCell::default(),
        }
    }

    /// The reads of the name `at` spells.
    fn reads(&self, at: &str) -> Reads {
        self.reads.borrow().get(at).copied().unwrap_or_default()
    }

    fn count(&self, path: &norn_fs::NormalizedPath, streamed: bool) {
        let mut reads = self.reads.borrow_mut();
        let reads = reads
            .entry(path.as_path().to_string_lossy().into_owned())
            .or_default();
        if streamed {
            reads.streamed += 1;
        } else {
            reads.whole += 1;
        }
    }
}

impl<V: VaultView> VaultView for Counted<'_, V> {
    type Error = V::Error;

    fn normalizer(&self) -> &norn_fs::PathNormalizer {
        self.inner.normalizer()
    }

    fn entry(&self, path: &norn_fs::NormalizedPath) -> Result<Entry, V::Error> {
        self.count(path, false);
        self.inner.entry(path)
    }

    fn streamed_entry(&self, path: &norn_fs::NormalizedPath) -> Result<Entry, V::Error> {
        self.count(path, true);
        self.inner.streamed_entry(path)
    }

    fn control_entry(&self, path: &norn_fs::NormalizedPath) -> Result<Entry, V::Error> {
        self.inner.control_entry(path)
    }

    fn folder_stands(&self, folder: &norn_fs::NormalizedPath) -> Result<bool, V::Error> {
        self.inner.folder_stands(folder)
    }

    fn visit_folder_names(
        &self,
        folder: &norn_fs::NormalizedPath,
        visit: &mut dyn FnMut(&std::ffi::OsStr) -> std::ops::ControlFlow<()>,
    ) -> Result<(), V::Error> {
        self.inner.visit_folder_names(folder, visit)
    }

    fn visit_root_names(
        &self,
        visit: &mut dyn FnMut(&std::ffi::OsStr) -> std::ops::ControlFlow<()>,
    ) -> Result<(), V::Error> {
        self.inner.visit_root_names(visit)
    }

    fn folder_contents(
        &self,
        folder: &norn_fs::NormalizedPath,
    ) -> Result<Option<crate::planner::view::FolderContents>, V::Error> {
        self.inner.folder_contents(folder)
    }
}

/// **A document moved away and back plans, previews and applies as found.**
/// Its content ends where it began, drawn from its own before-state, so the
/// plan carries it unread end to end — through one name, and through two
/// names, one of them in a folder the plan makes and takes away again — and
/// the applier, holding the same rule, observes it streamed: the preview
/// answers the plan, and the apply finds every target landed and writes
/// nothing.
#[test]
fn a_document_moved_away_and_back_previews_and_applies_found() {
    for operations in [
        vec![moving("a.md", "z.md"), moving("z.md", "a.md")],
        vec![
            moving("a.md", "x/z.md"),
            moving("x/z.md", "y.md"),
            moving("y.md", "a.md"),
        ],
    ] {
        let mut fixture = Fixture::new(&[("a.md", "# A\n"), ("b.md", "x\n")]);
        let plan = fixture.plan(operations.clone());
        let (previewed, _) = fixture
            .preview(plan.clone())
            .unwrap_or_else(|refused| panic!("{operations:?} previews: {refused:?}"));
        assert_eq!(previewed, plan, "{operations:?}");
        let finished = applied(fixture.apply(plan));
        assert!(
            results(&finished)
                .iter()
                .all(|(_, result)| *result == TargetResult::Found),
            "{operations:?}: {:?}",
            results(&finished)
        );
        assert_eq!(fixture.tree(), vec!["a.md", "b.md"], "{operations:?}");
        assert_eq!(fixture.read("a.md").as_deref(), Some("# A\n"));
    }
}

/// **A carried name refilled by a document composition writes is read
/// streamed.** `c.md` moves on to `n.md`, which the write kernel stages as a
/// copy of it, and `h.md`, whose link to `c.md` the cascade respells, moves
/// into the name it vacates. No composition reads the bytes leaving `c.md`:
/// planning reads it streamed once, and the applier's check — observing,
/// recomposing and judging links — reads it streamed once too, whatever
/// refills it. The preview answers the plan and the apply writes every
/// target. Observed again once the plan landed, `c.md` holds the bytes the
/// composition wrote there, not carried ones, so it is read whole for them
/// after the streamed read that found it landed; applied again, every target
/// is found.
#[test]
fn a_carried_name_refilled_by_a_composed_document_is_read_streamed() {
    let mut fixture = Fixture::new(&[("c.md", "# C\n"), ("h.md", "[x](c.md)\n")]);
    let operations = vec![moving("c.md", "n.md"), moving("h.md", "c.md")];
    let plan = {
        let tree =
            TreeView::open(&fixture.vault, &fixture.exclusions, &fixture.schema).expect("a vault");
        let links = fixture.links();
        let planning = Counted::over(&tree);
        let resolution = crate::planner::resolve::resolve(
            AuthoredPlan::new(crate::planner::links::testing::vault(), operations.clone()),
            fixture.root_identity(),
            &BTreeSet::new(),
            &planning,
            &links.index(),
        )
        .unwrap_or_else(|failure| panic!("the plan is planned: {failure:?}"));
        assert_eq!(resolution.unresolved, Vec::new());
        assert_eq!(
            planning.reads("c.md"),
            Reads {
                whole: 0,
                streamed: 1
            },
            "planning holds no copy"
        );
        let applying = Counted::over(&tree);
        let declared = crate::derivation::Declared::unpinned();
        check(
            &resolution.plan,
            &applying,
            &declared,
            &links.index(),
            &mut Citations::default(),
        )
        .expect("the plan checks");
        assert_eq!(
            applying.reads("c.md"),
            Reads {
                whole: 0,
                streamed: 1
            },
            "the applier holds no copy"
        );
        resolution.plan
    };

    let (previewed, _) = fixture.preview(plan.clone()).expect("the preview answers");
    assert_eq!(previewed, plan);
    let finished = applied(fixture.apply(plan.clone()));
    assert!(
        results(&finished)
            .iter()
            .all(|(_, result)| *result == TargetResult::Wrote),
        "{:?}",
        results(&finished)
    );
    assert_eq!(fixture.read("n.md").as_deref(), Some("# C\n"));
    assert_eq!(fixture.read("c.md").as_deref(), Some("[x](n.md)\n"));
    assert_eq!(fixture.tree(), vec!["c.md", "n.md"]);
    fixture.assert_store_is_a_build_from_zero();

    {
        let tree =
            TreeView::open(&fixture.vault, &fixture.exclusions, &fixture.schema).expect("a vault");
        let landed = Counted::over(&tree);
        let units = units(&plan, tree.normalizer());
        let (states, _) = observe(&plan, &units, &landed).expect("a readable vault");
        let at = plan
            .transitions
            .iter()
            .position(|transition| transition.path.as_str() == "c.md")
            .expect("a transition at c.md");
        assert!(
            matches!(states[at], TargetState::Landed(Some(Body::Held(_)))),
            "{:?}",
            states[at]
        );
        assert_eq!(
            landed.reads("c.md"),
            Reads {
                whole: 1,
                streamed: 1
            }
        );
    }
    let again = applied(fixture.apply(plan));
    assert!(
        results(&again)
            .iter()
            .all(|(_, result)| *result == TargetResult::Found),
        "{:?}",
        results(&again)
    );
}

/// **A carried name a case-only rename refills is read streamed.** On a
/// root that folds case, `a.md` moves on to `t.md`, which the write kernel
/// stages as a copy of it, `b.md` moves into the name it vacates, and the
/// rename to `A.md` publishes `b.md`'s content as one respell, which holds
/// it — so `b.md` is not carried, while `a.md` is. No composition reads the
/// bytes leaving `a.md`, so the applier observes the respell's name
/// streamed once and recomposes the plan as itself. Observed once the plan
/// landed, the new spelling holds `b.md`'s bytes, which are not carried, so
/// they are read whole after the streamed read that found them.
#[test]
fn a_carried_name_a_respell_refills_is_read_streamed() {
    use crate::planner::view::memory::MemoryVault;

    let operations = vec![
        moving("a.md", "t.md"),
        moving("b.md", "a.md"),
        moving("a.md", "A.md"),
    ];
    let before = MemoryVault::with(&[("a.md", "# A\n"), ("b.md", "# B\n")])
        .folding_case()
        .streaming();
    let plan = crate::planner::links::testing::resolve_over_files(
        AuthoredPlan::new(crate::planner::links::testing::vault(), operations),
        RootIdentity::from_device_and_inode(1, 2),
        &BTreeSet::new(),
        &before,
    )
    .unwrap_or_else(|failure| panic!("the plan is planned: {failure:?}"))
    .plan;
    let units = units(&plan, before.normalizer());
    assert!(
        units
            .iter()
            .any(|unit| matches!(unit, crate::applier::observe::Unit::Respell { .. })),
        "{units:?}"
    );

    let applying = Counted::over(&before);
    let (states, _) = observe(&plan, &units, &applying).expect("an infallible view");
    assert_eq!(
        applying.reads("a.md"),
        Reads {
            whole: 0,
            streamed: 1
        }
    );
    let lineage = crate::applier::observe::recorded_lineage(&plan, before.normalizer());
    assert!(matches!(
        crate::applier::recompose::recompose(&plan, &states, &lineage, &applying)
            .expect("an infallible view"),
        crate::applier::recompose::Recomposed::Sound(_)
    ));
    assert_eq!(
        applying.reads("a.md"),
        Reads {
            whole: 0,
            streamed: 1
        },
        "the recomposition holds no copy"
    );

    let landed = MemoryVault::with(&[("A.md", "# B\n"), ("t.md", "# A\n")])
        .folding_case()
        .streaming();
    let applying = Counted::over(&landed);
    let (states, _) = observe(&plan, &units, &applying).expect("an infallible view");
    let at = plan
        .transitions
        .iter()
        .position(|transition| transition.path.as_str() == "A.md")
        .expect("a transition at A.md");
    assert!(
        matches!(states[at], TargetState::Landed(Some(Body::Held(_)))),
        "{:?}",
        states[at]
    );
    assert_eq!(
        applying.reads("a.md"),
        Reads {
            whole: 1,
            streamed: 1
        }
    );
}

/// **On a volume that folds case, a carried name a respell refills previews
/// and applies as its plan.** The preview answers the plan, the apply writes
/// every target, and applied again every target is found.
#[test]
fn a_carried_name_a_respell_refills_applies_on_a_folding_root() {
    if !super::volume_folds("a_carried_name_a_respell_refills_applies_on_a_folding_root") {
        return;
    }
    let mut fixture = Fixture::new(&[("a.md", "# A\n"), ("b.md", "# B\n")]);
    let plan = fixture.plan(vec![
        moving("a.md", "t.md"),
        moving("b.md", "a.md"),
        moving("a.md", "A.md"),
    ]);
    let (previewed, _) = fixture.preview(plan.clone()).expect("the preview answers");
    assert_eq!(previewed, plan);
    let finished = applied(fixture.apply(plan.clone()));
    assert!(
        results(&finished)
            .iter()
            .all(|(_, result)| *result == TargetResult::Wrote),
        "{:?}",
        results(&finished)
    );
    assert_eq!(fixture.tree(), vec!["A.md", "t.md"]);
    assert_eq!(fixture.read("t.md").as_deref(), Some("# A\n"));
    assert_eq!(fixture.read("A.md").as_deref(), Some("# B\n"));
    fixture.assert_store_is_a_build_from_zero();
    let again = applied(fixture.apply(plan));
    assert!(
        results(&again)
            .iter()
            .all(|(_, result)| *result == TargetResult::Found),
        "{:?}",
        results(&again)
    );
}

/// **Planning and the applier carry the same documents.** Over a corpus of
/// plans — chains, a rotation through a temporary name, a document moved
/// away and back, a move beside an edit or a delete of what it moved, a move
/// whose cascade rewrites holders, a moved document naming itself, a carried
/// name refilled by a document composition writes, by a move or by a
/// case-only rename — the targets planning composes as carried are exactly
/// those the applier's recomposition does, and the applier reads each
/// target once, streamed exactly where planning held no byte of what it
/// finds there: a name whose before-state planning read only streamed,
/// whatever refills it, and a name nothing stood at whose after-state is
/// absent or carried. Every other target is read whole once.
///
/// On a root that does not fold case, the case-only rename is a move to a
/// name of its own; on a root that folds case it is one respell, held on
/// every host by
/// `planning_and_the_applier_carry_the_same_documents_on_a_folding_root`.
#[test]
fn planning_and_the_applier_carry_the_same_documents() {
    let (files, corpus) = carrying_corpus();
    for operations in corpus {
        let fixture = Fixture::new(&files);
        let tree =
            TreeView::open(&fixture.vault, &fixture.exclusions, &fixture.schema).expect("a vault");
        let links = fixture.links();
        assert_carried_alike(&tree, &links.index(), fixture.root_identity(), operations);
    }
}

/// **On a root that folds case, planning and the applier carry the same
/// documents.** The corpus of
/// `planning_and_the_applier_carry_the_same_documents`, planned and observed
/// over a vault in memory that folds case and streams as a vault on disk
/// does, holds to the same rule on every host.
#[test]
fn planning_and_the_applier_carry_the_same_documents_on_a_folding_root() {
    use crate::planner::view::memory::MemoryVault;

    let (files, corpus) = carrying_corpus();
    let index = crate::planner::links::testing::IndexedFiles::of(&files);
    for operations in corpus {
        let vault = MemoryVault::with(&files).folding_case().streaming();
        assert_carried_alike(
            &vault,
            &index,
            RootIdentity::from_device_and_inode(1, 2),
            operations,
        );
    }
}

/// The vault and the plans the carrying corpus runs over.
fn carrying_corpus() -> ([(&'static str, &'static str); 5], Vec<Vec<Operation>>) {
    let files = [
        ("a.md", "# A [[c]]\n"),
        ("b.md", "# B\n"),
        ("c.md", "# C\n"),
        ("me.md", "[me](me.md) and [[me]]\n"),
        ("h.md", "[[a]] [[b]] [x](c.md)\n"),
    ];
    let corpus: Vec<Vec<Operation>> = vec![
        vec![moving("b.md", "n.md"), moving("a.md", "b.md")],
        vec![
            moving("c.md", "n.md"),
            moving("b.md", "c.md"),
            moving("a.md", "b.md"),
        ],
        vec![
            moving("a.md", "t.md"),
            moving("b.md", "a.md"),
            moving("t.md", "n.md"),
        ],
        vec![moving("a.md", "z.md"), moving("z.md", "a.md")],
        vec![
            moving("a.md", "x/z.md"),
            moving("x/z.md", "y.md"),
            moving("y.md", "a.md"),
        ],
        vec![moving("a.md", "m.md"), editing("m.md", "# A", "# M")],
        vec![editing("a.md", "# A", "# M"), moving("a.md", "m.md")],
        vec![moving("a.md", "m.md"), breaking("m.md")],
        vec![moving("a.md", "m.md"), breaking("b.md")],
        vec![
            moving("b.md", "n.md"),
            moving("a.md", "b.md"),
            editing("n.md", "# B", "# N"),
        ],
        vec![moving("c.md", "archive/c2.md")],
        vec![moving("me.md", "archive/me2.md")],
        vec![moving("me.md", "archive/me2.md"), moving("b.md", "me.md")],
        vec![moving("c.md", "n.md"), moving("h.md", "c.md")],
        vec![
            moving("a.md", "t.md"),
            moving("b.md", "a.md"),
            moving("a.md", "A.md"),
        ],
    ];
    (files, corpus)
}

/// Plan `operations` over `view`, judging links on `links`, and hold
/// planning and the applier to one carried set and the applier to one read
/// of each target, streamed exactly where planning held no byte of what it
/// finds there.
fn assert_carried_alike<V, I>(view: &V, links: &I, root: RootIdentity, operations: Vec<Operation>)
where
    V: VaultView,
    V::Error: std::fmt::Debug,
    I: crate::planner::links::LinkIndex,
    I::Error: std::fmt::Debug,
{
    let planning = Counted::over(view);
    let resolution = crate::planner::resolve::resolve(
        AuthoredPlan::new(crate::planner::links::testing::vault(), operations.clone()),
        root,
        &BTreeSet::new(),
        &planning,
        links,
    )
    .unwrap_or_else(|failure| panic!("{operations:?} is planned: {failure:?}"));
    assert_eq!(resolution.unresolved, Vec::new(), "{operations:?}");
    let plan = resolution.plan;
    let recorded: Vec<usize> = (0..plan.operations.len()).collect();
    let planned = crate::planner::compose::compose(
        &plan.operations,
        &recorded,
        &crate::planner::view::Remembered::over(view),
    )
    .expect("a readable vault");

    let applying = Counted::over(view);
    let units = units(&plan, view.normalizer());
    let (states, _) = observe(&plan, &units, &applying).expect("a readable vault");
    let lineage = crate::applier::observe::recorded_lineage(&plan, view.normalizer());
    let crate::applier::recompose::Recomposed::Sound(recomposed) =
        crate::applier::recompose::recompose(&plan, &states, &lineage, &applying)
            .expect("a readable vault")
    else {
        panic!("{operations:?} recomposes");
    };
    assert_eq!(
        carried_in(&planned),
        carried_in(&recomposed),
        "{operations:?}"
    );

    // A file is read once whatever spells it: on a root that folds case, a
    // respell's two spellings name one file, read once under one of them.
    // What a fresh apply finds there is the before-state of the spelling
    // something stood at, which planning held a byte of where it read any of
    // the file's spellings whole.
    let mut files: std::collections::BTreeMap<
        norn_fs::NormalizedPath,
        Vec<&norn_wire::Transition>,
    > = std::collections::BTreeMap::new();
    for transition in &plan.transitions {
        let file = view
            .normalizer()
            .normalize(std::path::Path::new(transition.path.as_str()))
            .expect("a target is a vault path");
        files.entry(file).or_default().push(transition);
    }
    let carried = carried_in(&planned);
    let spellings = |transitions: &[&norn_wire::Transition]| -> Vec<String> {
        transitions
            .iter()
            .map(|transition| transition.path.as_str().to_string())
            .collect()
    };
    let expected: Vec<(Vec<String>, Reads)> = files
        .values()
        .map(|transitions| {
            let found = transitions
                .iter()
                .find(|transition| transition.before != FileState::absent())
                .unwrap_or(&transitions[0]);
            let at = found.path.as_str();
            let streamed = if found.before == FileState::absent() {
                found.after == FileState::absent() || carried.iter().any(|(path, _)| path == at)
            } else {
                transitions
                    .iter()
                    .all(|transition| planning.reads(transition.path.as_str()).whole == 0)
            };
            let reads = if streamed {
                Reads {
                    whole: 0,
                    streamed: 1,
                }
            } else {
                Reads {
                    whole: 1,
                    streamed: 0,
                }
            };
            (spellings(transitions), reads)
        })
        .collect();
    let observed: Vec<(Vec<String>, Reads)> = files
        .values()
        .map(|transitions| {
            let reads = transitions
                .iter()
                .fold(Reads::default(), |sum, transition| {
                    let at = applying.reads(transition.path.as_str());
                    Reads {
                        whole: sum.whole + at.whole,
                        streamed: sum.streamed + at.streamed,
                    }
                });
            (spellings(transitions), reads)
        })
        .collect();
    assert_eq!(observed, expected, "{operations:?}");
}

/// Each target `composition` carries unread, and the file it carries.
fn carried_in(composition: &crate::planner::compose::Composition) -> Vec<(String, String)> {
    composition
        .targets
        .iter()
        .filter_map(|(path, target)| match &target.after {
            crate::planner::compose::After::Carried { from, .. } => {
                Some((path.as_str().to_string(), from.as_str().to_string()))
            }
            _ => None,
        })
        .collect()
}

/// A vault schema whose rules read where a document stands: a task belongs
/// under `tasks/`, a document under `open/` takes `status: todo` and one
/// under `shut/` `status: done`, and one under `kept/` — its drafts excluded
/// — needs an owner.
const PLACED: &str = "version: 1
rules:
  placed: { match: { frontmatter: { kind: task } }, allowed_paths: { paths: ['tasks/**'] } }
  open: { match: { path: 'open/**' }, one_of: { status: { values: [todo] } } }
  shut: { match: { path: 'shut/**' }, one_of: { status: { values: [done] } } }
  kept: { match: { path: 'kept/**' }, exclude: { path: ['kept/drafts/**'] }, required: { owner: } }
";

/// The documents [`PLACED`] judges where they stand, each holding exactly
/// what a move to the next place would activate.
fn placed_fixture() -> Fixture {
    Fixture::with_schema(
        PLACED,
        &[
            ("tasks/a.md", "---\nkind: task\n---\n# A\n"),
            ("open/b.md", "---\nstatus: unknown\n---\n# B\n"),
            ("kept/drafts/c.md", "# C\n"),
            ("open/d.md", "---\nstatus: unknown\n---\n# D\n"),
        ],
    )
}

/// Each schema violation among `checks`, as its path, kind and field.
fn schema_checks(checks: &[RefusedCheck]) -> Vec<(String, norn_wire::FindingKind, Option<String>)> {
    checks
        .iter()
        .map(|check| match check {
            RefusedCheck::SchemaViolation { violation, .. } => (
                violation.path.as_str().to_string(),
                violation.kind,
                violation.target.clone(),
            ),
            other => panic!("a schema check: {other:?}"),
        })
        .collect()
}

/// **A carried move is judged again where it lands**: the same bytes breach
/// `allowed_paths` at one place and not another, a `match.path` changes the
/// combined constraint a held value is outside of, and an `exclude.path`
/// that no longer excludes activates a requirement — each refuses, though no
/// byte changed. A move keeping every identity it held applies, carried, and
/// a forced plan lets each violation through and lists it.
#[test]
fn a_carried_move_refuses_each_violation_its_destination_activates() {
    let mut fixture = placed_fixture();
    let activating = fixture.plan(vec![
        moving("tasks/a.md", "notes/a.md"),
        moving("open/b.md", "shut/b.md"),
        moving("kept/drafts/c.md", "kept/c.md"),
    ]);
    let checks = refused(fixture.apply(activating.clone())).checks;
    assert_eq!(
        schema_checks(&checks),
        [
            (
                "kept/c.md".to_string(),
                norn_wire::FindingKind::RequiredMissing,
                Some("owner".to_string())
            ),
            (
                "notes/a.md".to_string(),
                norn_wire::FindingKind::Misplaced,
                None
            ),
            (
                "shut/b.md".to_string(),
                norn_wire::FindingKind::NotOneOf,
                Some("status".to_string())
            ),
        ]
    );
    assert_eq!(
        fixture.read("tasks/a.md").as_deref(),
        Some("---\nkind: task\n---\n# A\n")
    );

    let kept = applied(fixture.apply(fixture.plan(vec![moving("open/d.md", "open/sub/d.md")])));
    assert!(kept.forced.is_empty());

    let mut forced = activating;
    forced.force = true;
    let landed = applied(fixture.apply(forced));
    assert_eq!(landed.forced.len(), 3, "{:?}", landed.forced);
    assert_eq!(
        fixture.read("shut/b.md").as_deref(),
        Some("---\nstatus: unknown\n---\n# B\n")
    );
    fixture.assert_store_is_a_build_from_zero();
}

/// **Where the index vouches for a carried document, it is judged from the
/// store's projection, holding no copy of it**: the applier streams the
/// moved file once and never reads it whole, and still refuses the
/// violation its destination activates.
#[test]
fn a_carried_move_the_index_vouches_for_is_judged_from_its_projection() {
    let mut fixture = placed_fixture();
    let plan = fixture.plan(vec![moving("open/b.md", "shut/b.md")]);
    let tree =
        TreeView::open(&fixture.vault, &fixture.exclusions, &fixture.schema).expect("a vault");
    let counted = Counted::over(&tree);
    let declared = crate::production::pinned_declaration(&mut fixture.store).expect("a pin");
    let links = fixture.links();
    let Err(crate::applier::stage::Unfit::Refused(checks)) = check(
        &plan,
        &counted,
        &declared,
        &links.index(),
        &mut Citations::default(),
    ) else {
        panic!("the carried move refuses on its destination's constraint");
    };
    assert_eq!(
        schema_checks(&checks),
        [(
            "shut/b.md".to_string(),
            norn_wire::FindingKind::NotOneOf,
            Some("status".to_string())
        )]
    );
    assert_eq!(
        counted.reads("open/b.md"),
        Reads {
            whole: 0,
            streamed: 1
        },
        "the applier holds no copy of the carried document"
    );
}

/// **Where the index lags a carried document, it is judged from its bytes,
/// read whole once** — the one copy the move already reads for its links: a
/// draft rewritten behind the index to hold `status: unknown` is judged by
/// what its bytes say, so a move under `open/`, which the stale projection
/// would pass, refuses; a move keeping it a draft checks, holding one copy.
#[test]
fn a_carried_move_the_index_lags_is_judged_from_its_bytes_once() {
    let mut fixture = placed_fixture();
    fixture.write(
        "kept/drafts/c.md",
        "---\nstatus: unknown\n---\n# C, changed\n",
    );
    let kept = fixture.plan(vec![moving("kept/drafts/c.md", "kept/drafts/sub/c.md")]);
    let tree =
        TreeView::open(&fixture.vault, &fixture.exclusions, &fixture.schema).expect("a vault");
    let declared = crate::production::pinned_declaration(&mut fixture.store).expect("a pin");
    let links = fixture.links();
    let counted = Counted::over(&tree);
    check(
        &kept,
        &counted,
        &declared,
        &links.index(),
        &mut Citations::default(),
    )
    .unwrap_or_else(|_| panic!("a move keeping the draft a draft checks"));
    assert_eq!(
        counted.reads("kept/drafts/c.md"),
        Reads {
            whole: 1,
            streamed: 1
        },
        "one copy, read for its links and its frontmatter alike"
    );
    drop(links);
    let checks =
        refused(fixture.apply(fixture.plan(vec![moving("kept/drafts/c.md", "open/c.md")]))).checks;
    assert_eq!(
        schema_checks(&checks),
        [(
            "open/c.md".to_string(),
            norn_wire::FindingKind::NotOneOf,
            Some("status".to_string())
        )]
    );
}

/// **Judgment on the indexed projection equals judgment on the bytes, under
/// churn**: documents rewritten through every shape of frontmatter the rules
/// read — typed scalars, strings of digits, nested containers, nulls, an
/// absent block, a block that does not read — each indexed by the store
/// after each rewrite, judge to the same findings, identities included, from
/// the store's projection as from their bytes, at every place the rules
/// tell apart.
#[test]
fn judgment_on_the_indexed_projection_equals_judgment_on_bytes_under_churn() {
    const SCHEMA: &str = "version: 1
fields:
  rank: { type: number }
  due: { type: date }
  tags: { type: tags, shape: list }
rules:
  open: { match: { path: 'open/**' }, one_of: { status: { values: [todo, '7'] } }, required: { owner: }, max_length: { title: 5 } }
  shut: { match: { path: 'shut/**' }, forbidden: { scratch: }, one_of: { tags: { values: [work] } } }
  placed: { match: { frontmatter: { kind: task } }, allowed_paths: { paths: ['open/**'] } }
";
    let shapes = [
        "---\nstatus: todo\nowner: me\n---\nbody\n",
        "---\nstatus: 7\nrank: high\ndue: soon\ntitle: a long title\n---\nbody\n",
        "---\nstatus: '7'\nrank: 2.0\ntags: [Work, '#play', work]\nscratch: null\n---\nbody\n",
        "---\nkind: task\nnested: [[1, '1'], {b: 2, a: [x]}]\nowner: ~\n---\nbody\n",
        "no block at all\n",
        "---\nstatus: [unclosed\n---\nbody\n",
        "---\ntags: single\ntitle: 12345\nstatus: [todo, bogus, BOGUS]\n---\nbody\n",
    ];
    let mut fixture = Fixture::with_schema(SCHEMA, &[("open/x.md", shapes[0])]);
    let declared = crate::production::pinned_declaration(&mut fixture.store).expect("a pin");
    let case = crate::stored_path_order(
        TreeView::open(&fixture.vault, &fixture.exclusions, &fixture.schema)
            .expect("a vault")
            .normalizer()
            .case_sensitivity(),
    )
    .glob_case();
    let summary = |findings: Vec<crate::derivation::PlannedFinding>| -> Vec<String> {
        findings
            .into_iter()
            .map(|finding| {
                format!(
                    "{:?} {:?} {:?} {:?} {:?}",
                    finding.cause, finding.target, finding.value, finding.rules, finding.identity
                )
            })
            .collect()
    };
    for round in 0..3 {
        for (at, shape) in shapes.iter().enumerate() {
            let shape = if round % 2 == 1 {
                shapes[shapes.len() - 1 - at]
            } else {
                shape
            };
            fixture.foreign("open/x.md", shape);
            let links = fixture.links();
            let index = links.index();
            let held =
                crate::planner::links::LinkIndex::held_frontmatter(&index, &path("open/x.md"))
                    .expect("the index reads")
                    .expect("the index holds the document");
            assert_eq!(held.content_hash, content_hash(shape.as_bytes()).hex());
            let bytes =
                crate::derivation::frontmatter_block(shape.as_bytes()).expect("the bytes decode");
            let canonical = |block: &norn_store::HeldBlock| match block {
                norn_store::HeldBlock::Read(value) => {
                    norn_store::canonical_json(value).expect("a projection")
                }
                other => format!("{other:?}"),
            };
            assert_eq!(canonical(&held.block), canonical(&bytes), "{shape:?}");
            for place in ["open/x.md", "shut/x.md", "elsewhere/x.md"] {
                let subject = norn_store::DocumentPath::from(&path(place));
                let (projected, _) =
                    crate::derivation::judge_block(&subject, &held.block, &declared, case);
                let (read, _) = crate::derivation::judge_block(&subject, &bytes, &declared, case);
                assert_eq!(summary(projected), summary(read), "{shape:?} at {place}");
            }
        }
    }
}

/// **A folder move judges each document it carries where it lands**: moving
/// `open/` to `shut/` carries both documents holding `status: unknown` under
/// `[done]`, and refuses on each.
#[test]
fn a_folder_move_judges_each_document_it_carries_where_it_lands() {
    let mut fixture = placed_fixture();
    let folder = Operation::new(norn_wire::OperationKind::move_folder(
        norn_wire::FolderPath::new("open").expect("a folder path"),
        norn_wire::FolderPath::new("shut").expect("a folder path"),
    ));
    let checks = refused(fixture.apply(fixture.expanded(vec![folder]).plan)).checks;
    assert_eq!(
        schema_checks(&checks),
        [
            (
                "shut/b.md".to_string(),
                norn_wire::FindingKind::NotOneOf,
                Some("status".to_string())
            ),
            (
                "shut/d.md".to_string(),
                norn_wire::FindingKind::NotOneOf,
                Some("status".to_string())
            ),
        ]
    );
}

/// **A carried document whose frontmatter block does not read is judged
/// against nothing where it lands**: its fields are unknown rather than
/// absent, so a move bringing it under a rule requiring `owner` introduces
/// no missing field, and applies.
#[test]
fn a_carried_document_whose_block_does_not_read_is_judged_against_nothing_where_it_lands() {
    let mut fixture = Fixture::with_schema(
        "version: 1
rules:
  kept: { match: { path: 'kept/**' }, exclude: { path: ['kept/drafts/**'] }, required: { owner: } }
",
        &[("kept/drafts/e.md", "---\nowner: [unclosed\n---\n# E\n")],
    );
    let landed =
        applied(fixture.apply(fixture.plan(vec![moving("kept/drafts/e.md", "kept/e.md")])));
    assert!(landed.forced.is_empty());
    fixture.assert_store_is_a_build_from_zero();
}
