//! Folder moves planned over a vault on disk and the store beside it, then
//! applied: the document moves a folder move expands into, what it leaves
//! behind, and the folders it empties.

use norn_wire::{
    AuthoredPlan, FilePath, FolderPath, LinkFamily, LinkKey, LinkRewrite, Operation, OperationKind,
    PlanCondition, Resolves,
};

use super::{Fixture, applied, path};
use crate::planner::control::SchemaPlace;
use crate::planner::expand::resolve_expanding;
use crate::planner::resolve::Resolution;
use crate::planner::view::TreeView;

fn moving_folder(from: &str, to: &str) -> Operation {
    Operation::new(OperationKind::move_folder(
        FolderPath::new(from).expect("a folder path"),
        FolderPath::new(to).expect("a folder path"),
    ))
}

fn file(text: &str) -> FilePath {
    FilePath::new(text).expect("a file path")
}

impl Fixture {
    /// `operations` planned as an apply job plans them: each folder move and
    /// `where` target expanded first, on the store as it stands, every
    /// operation resolving.
    pub(super) fn expanded(&self, operations: Vec<Operation>) -> Resolution {
        let view = TreeView::open(&self.vault, &self.exclusions, &SchemaPlace::default())
            .expect("a vault");
        let links = self.links();
        let index = links.index();
        let resolution = resolve_expanding(
            AuthoredPlan::new(crate::planner::links::testing::vault(), operations),
            self.root_identity(),
            &view,
            &index,
            &index,
            &crate::planner::rule::testing::no_rules(),
        )
        .unwrap_or_else(|failure| panic!("the plan is planned: {failure:?}"));
        assert!(
            resolution.unresolved.is_empty(),
            "every operation resolves: {:?}",
            resolution.unresolved
        );
        resolution
    }
}

/// **A folder move names every file it leaves behind**: an attachment, a
/// file the vault does not read, and a link the vault's walk does not
/// follow, each at the spelling the tree lists, in path order.
#[test]
fn names_every_file_left_behind() {
    let fixture = Fixture::new(&[
        ("notes/a.md", "A\n"),
        ("notes/img.png", "png"),
        ("notes/deep/data.csv", "1,2\n"),
    ]);
    std::os::unix::fs::symlink("a.md", fixture.vault.join("notes/link.md")).expect("a link");
    let resolution = fixture.expanded(vec![moving_folder("notes", "archive")]);
    assert_eq!(
        resolution.forecast.left_behind,
        vec![
            file("notes/deep/data.csv"),
            file("notes/img.png"),
            file("notes/link.md"),
        ]
    );
    assert_eq!(resolution.plan.operations.len(), 1);
}

/// **Relative links between documents one folder move carries together
/// stay as written**: each still names the same document from where both
/// land, so the move generates no cascade, while a link from outside the
/// folder follows.
#[test]
fn relative_links_between_co_moved_documents_stay() {
    let mut fixture = Fixture::new(&[
        ("notes/a.md", "[b](sub/b.md) [[b]]\n"),
        ("notes/sub/b.md", "[a](../a.md) [[a]]\n"),
        ("h.md", "[a](notes/a.md)\n"),
    ]);
    let resolution = fixture.expanded(vec![moving_folder("notes", "archive/notes")]);
    let cascades: Vec<_> = resolution
        .plan
        .operations
        .iter()
        .map(|operation| operation.cascade.clone())
        .collect();
    assert_eq!(
        cascades,
        [
            vec![norn_wire::LinkRewrite::new(
                path("h.md"),
                norn_wire::LinkFamily::Markdown,
                "notes/a.md",
                "archive/notes/a.md",
            )],
            vec![],
        ]
    );
    applied(fixture.apply(resolution.plan));
    assert_eq!(
        fixture.read("archive/notes/a.md").as_deref(),
        Some("[b](sub/b.md) [[b]]\n")
    );
    assert_eq!(
        fixture.read("archive/notes/sub/b.md").as_deref(),
        Some("[a](../a.md) [[a]]\n")
    );
    fixture.assert_store_is_a_build_from_zero();
}

/// **A folder move removes the folders it empties**, and a folder still
/// holding a file it left behind stays, holding it.
#[test]
fn a_folder_move_removes_what_it_empties() {
    let mut fixture = Fixture::new(&[
        ("notes/a.md", "A\n"),
        ("notes/sub/b.md", "B\n"),
        ("keep/c.md", "C\n"),
        ("keep/img.png", "png"),
    ]);
    let resolution = fixture.expanded(vec![
        moving_folder("notes", "archive/notes"),
        moving_folder("keep", "archive/keep"),
    ]);
    assert_eq!(
        resolution.forecast.folders_removed,
        vec![
            FolderPath::new("notes").expect("a folder path"),
            FolderPath::new("notes/sub").expect("a folder path"),
        ]
    );
    assert_eq!(resolution.forecast.left_behind, vec![file("keep/img.png")]);
    applied(fixture.apply(resolution.plan));
    assert!(!fixture.vault.join("notes").exists());
    assert_eq!(fixture.read("keep/img.png").as_deref(), Some("png"));
    assert_eq!(
        fixture.read("archive/notes/sub/b.md").as_deref(),
        Some("B\n")
    );
    assert_eq!(fixture.read("archive/keep/c.md").as_deref(), Some("C\n"));
    fixture.assert_store_is_a_build_from_zero();
}

/// **A folder move carrying a quarantined file generates no cascade for the
/// links naming it.** `[o](notes/d.md)` names a document the move carries,
/// and follows it; `[q](notes/q.md)` and `[[notes/q]]` name a file whose
/// bytes do not decode, which no link resolves to before the move or after
/// it, so they stay as written and the plan records no entry for them,
/// though the folder move carries the file too. The preview recomputes the
/// plan, and it applies.
#[test]
fn a_folder_move_carrying_a_quarantined_file_rewrites_no_link_naming_it() {
    let mut fixture = Fixture::new(&[
        ("l.md", "[o](notes/d.md) [q](notes/q.md) [[notes/q]]\n"),
        ("notes/d.md", "d\n"),
    ]);
    fixture.foreign("notes/q.md", super::UNDECODABLE);
    let resolution = fixture.expanded(vec![moving_folder("notes", "archive")]);
    let cascades: Vec<_> = resolution
        .plan
        .operations
        .iter()
        .map(|operation| (operation.kind.clone(), operation.cascade.clone()))
        .collect();
    assert_eq!(
        cascades,
        vec![
            (
                OperationKind::move_document(path("notes/d.md"), path("archive/d.md")),
                vec![LinkRewrite::new(
                    path("l.md"),
                    LinkFamily::Markdown,
                    "notes/d.md",
                    "archive/d.md",
                )],
            ),
            (
                OperationKind::move_document(path("notes/q.md"), path("archive/q.md")),
                Vec::new(),
            ),
        ]
    );
    assert_eq!(
        resolution.plan.conditions,
        vec![PlanCondition::link_resolution(
            LinkKey::new(path("l.md"), LinkFamily::Markdown, "archive/d.md"),
            Resolves::none(),
            Resolves::one(path("archive/d.md")),
        )]
    );
    assert_eq!(resolution.forecast.links, vec![]);
    let (previewed, forecast) = fixture
        .preview(resolution.plan.clone())
        .expect("the plan previews");
    assert_eq!(previewed, resolution.plan);
    assert_eq!(forecast.links, resolution.forecast.links);
    applied(fixture.apply(resolution.plan));
    assert_eq!(
        fixture.read("l.md").as_deref(),
        Some("[o](archive/d.md) [q](notes/q.md) [[notes/q]]\n")
    );
    assert_eq!(
        std::fs::read(fixture.vault.join("archive/q.md"))
            .ok()
            .as_deref(),
        Some(super::UNDECODABLE)
    );
    fixture.assert_store_is_a_build_from_zero();
}
