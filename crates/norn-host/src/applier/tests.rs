//! The applier's cases, each over a vault on disk and a store beside it.
#![allow(clippy::disallowed_methods)] // Harness scaffolding: the trees the cases apply to.

use std::cell::RefCell;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use norn_fs::{AfterState, MaintainershipKey, Published, ShadowHome};
use norn_store::{Store, StoredPathOrder};
use norn_testkit::scratch::Scratch;
use norn_wire::{
    AuthoredPlan, DocumentPath, Operation, OperationKind, ResolvedPlan, RootIdentity, TargetResult,
    VaultAddress, VaultName,
};

use super::{Applier, ApplyOutcome, OwnWriteLedger};
use crate::planner::resolve::resolve;
use crate::planner::view::{TreeView, VaultView};
use crate::production::{heal_from_zero, shadow_exclusions};

pub(super) fn path(text: &str) -> DocumentPath {
    DocumentPath::new(text).expect("a legal document path")
}

pub(super) fn creating(at: &str, content: &str) -> Operation {
    Operation::new(OperationKind::create_document(path(at), content))
}

pub(super) fn editing(at: &str, old: &str, new: &str) -> Operation {
    Operation::new(OperationKind::str_replace(path(at), old, new))
}

pub(super) fn moving(from: &str, to: &str) -> Operation {
    Operation::new(OperationKind::move_document(path(from), path(to)))
}

pub(super) fn deleting(at: &str) -> Operation {
    Operation::new(OperationKind::delete_document(path(at)))
}

/// Every publication the applier recorded, with whether the file held the
/// published state at the moment it was recorded.
#[derive(Default)]
pub(super) struct Recorded {
    pub(super) calls: RefCell<Vec<(PathBuf, bool)>>,
    anchor: PathBuf,
}

impl OwnWriteLedger for Recorded {
    fn published(&self, path: &Path, published: &Published) {
        let holds = match &published.after {
            AfterState::Present(state) => std::fs::read(self.anchor.join(path))
                .is_ok_and(|bytes| norn_fs::ContentHash::of(&bytes) == state.content_hash),
            AfterState::Absent => !self.anchor.join(path).exists(),
        };
        self.calls.borrow_mut().push((path.to_owned(), holds));
    }
}

/// A vault on disk, its shadow home and a store derived from it.
pub(super) struct Fixture {
    _scratch: Option<Scratch>,
    pub(super) vault: PathBuf,
    pub(super) data: PathBuf,
    pub(super) shadows: ShadowHome,
    pub(super) root: norn_fs::Identity,
    pub(super) exclusions: Vec<PathBuf>,
    pub(super) store: Store,
    pub(super) recorded: Recorded,
}

impl Fixture {
    /// A vault holding `files`, with a store built from zero over it.
    pub(super) fn new(files: &[(&str, &str)]) -> Fixture {
        let scratch = Scratch::new("applier");
        let vault = scratch.join("vault");
        let data = scratch.join("data");
        std::fs::create_dir_all(&vault).expect("a vault");
        std::fs::create_dir_all(&data).expect("a data directory");
        for (at, content) in files {
            write_at(&vault, at, content);
        }
        Fixture::over(Some(scratch), vault, data, "store.sqlite3")
    }

    /// The fixture over a vault and data directory already on disk, with the
    /// store `database` opened there and built from zero where it is new.
    pub(super) fn over(
        scratch: Option<Scratch>,
        vault: PathBuf,
        data: PathBuf,
        database: &str,
    ) -> Fixture {
        let key = MaintainershipKey::new("test", "vault", "data").expect("a key");
        let shadows = ShadowHome::resolve(&vault, &data.join("tmp"), &key).expect("a shadow home");
        let exclusions = shadow_exclusions(&shadows, &vault);
        let root = norn_fs::path_identity(&vault)
            .expect("the root reads")
            .expect("the root stands");
        let order = order_of(&vault);
        let database = data.join(database);
        let fresh = !database.exists();
        let mut store = Store::open(&database, order, crate::DERIVATION_VERSION).expect("a store");
        if fresh {
            heal_from_zero(&mut store, &vault, &exclusions).expect("a heal");
        }
        Fixture {
            _scratch: scratch,
            recorded: Recorded {
                calls: RefCell::default(),
                anchor: vault.clone(),
            },
            vault,
            data,
            shadows,
            root,
            exclusions,
            store,
        }
    }

    pub(super) fn write(&self, at: &str, content: &str) {
        write_at(&self.vault, at, content);
    }

    /// Another writer writes `content` at `at`, and the store takes it in as
    /// the watcher would deliver it before an apply's intake.
    pub(super) fn foreign(&mut self, at: &str, content: &str) {
        self.write(at, content);
        heal_from_zero(&mut self.store, &self.vault, &self.exclusions).expect("a heal");
    }

    pub(super) fn read(&self, at: &str) -> Option<String> {
        std::fs::read_to_string(self.vault.join(at)).ok()
    }

    pub(super) fn root_identity(&self) -> RootIdentity {
        RootIdentity::from_device_and_inode(self.root.dev, self.root.ino)
    }

    /// `operations` resolved against the vault, every one of them resolving.
    pub(super) fn plan(&self, operations: Vec<Operation>) -> ResolvedPlan {
        let view = TreeView::open(&self.vault, &self.exclusions).expect("a vault");
        let name = VaultName::new("notes").expect("a legal vault name");
        let authored = AuthoredPlan::new(VaultAddress::name(name), operations);
        let resolution = resolve(authored, self.root_identity(), &BTreeSet::new(), &view)
            .unwrap_or_else(|failure| panic!("the plan resolves: {failure:?}"));
        assert!(
            resolution.unresolved.is_empty(),
            "every operation resolves: {:?}",
            resolution.unresolved
        );
        resolution.plan
    }

    pub(super) fn apply(&mut self, plan: ResolvedPlan) -> ApplyOutcome {
        let applier = Applier {
            anchor: &self.vault,
            root: self.root,
            exclusions: &self.exclusions,
            shadows: &self.shadows,
            own_writes: &self.recorded,
        };
        applier.apply(plan, &mut self.store)
    }

    /// Every document row and finding the store holds, with the generations
    /// and timestamps that say when they were written taken out.
    pub(super) fn derived(store: &mut Store) -> Vec<String> {
        let mut derived = Vec::new();
        let rows = store
            .begin_request()
            .stored_documents_after_ordered(None, 500, StoredPathOrder::Sensitive)
            .expect("rows");
        for row in rows {
            let facts = store
                .begin_request()
                .stored_facts(&row.path)
                .expect("facts")
                .expect("a listed row");
            derived.push(format!("{facts:?}"));
        }
        let findings = store
            .begin_request()
            .stored_findings_after(None, 500)
            .expect("findings");
        let mut findings: Vec<String> = findings
            .into_iter()
            .map(|(_, finding)| format!("{finding:?}"))
            .collect();
        findings.sort();
        derived.extend(findings);
        derived
            .into_iter()
            .map(|line| {
                without_numbers_after(
                    &without_numbers_after(&line, "generation: "),
                    "derived_at: ",
                )
            })
            .collect()
    }

    /// The store equals a build from zero over the tree as it stands.
    pub(super) fn assert_store_is_a_build_from_zero(&mut self) {
        let oracle = self.data.join(format!(
            "oracle-{}.sqlite3",
            norn_testkit::scratch::unique_name("from-zero")
        ));
        let mut built = Store::open(&oracle, order_of(&self.vault), crate::DERIVATION_VERSION)
            .expect("a store");
        if let Some(pin) = self
            .store
            .begin_request()
            .vault_schema_pin()
            .expect("a pin read")
        {
            built
                .begin_request()
                .pin_vault_schema(&pin.bytes, &pin.fingerprint)
                .expect("the schema pins");
        }
        heal_from_zero(&mut built, &self.vault, &self.exclusions).expect("a heal");
        assert_eq!(
            Fixture::derived(&mut self.store),
            Fixture::derived(&mut built),
            "the store equals a build from zero over the tree"
        );
    }

    /// Every name under the vault, files and folders, as vault-relative paths.
    pub(super) fn tree(&self) -> Vec<String> {
        fn walk(root: &Path, at: &Path, names: &mut Vec<String>) {
            for entry in std::fs::read_dir(at).expect("a folder") {
                let entry = entry.expect("an entry");
                let path = entry.path();
                let name = path.strip_prefix(root).expect("below the root");
                if name.starts_with(".norn") {
                    continue;
                }
                names.push(name.to_string_lossy().into_owned());
                if entry.file_type().expect("a type").is_dir() {
                    walk(root, &path, names);
                }
            }
        }
        let mut names = Vec::new();
        walk(&self.vault, &self.vault, &mut names);
        names.sort();
        names
    }
}

fn order_of(vault: &Path) -> StoredPathOrder {
    let view = TreeView::open(vault, &[]).expect("a vault");
    crate::stored_path_order(view.normalizer().case_sensitivity())
}

fn write_at(vault: &Path, at: &str, content: &str) {
    let full = vault.join(at);
    if let Some(parent) = full.parent() {
        std::fs::create_dir_all(parent).expect("a folder");
    }
    std::fs::write(full, content).expect("a file");
}

/// `line` with the digits after every `label` replaced by `_`.
fn without_numbers_after(line: &str, label: &str) -> String {
    let mut out = String::new();
    let mut rest = line;
    while let Some(at) = rest.find(label) {
        out.push_str(&rest[..at + label.len()]);
        rest =
            rest[at + label.len()..].trim_start_matches(|c: char| c.is_ascii_digit() || c == '-');
        out.push('_');
    }
    out.push_str(rest);
    out
}

pub(super) fn applied(outcome: ApplyOutcome) -> super::Applied {
    match outcome {
        ApplyOutcome::Applied(applied) => applied,
        other => panic!("the plan applies: {other:?}"),
    }
}

pub(super) fn results(applied: &super::Applied) -> Vec<(String, TargetResult)> {
    applied
        .targets
        .iter()
        .map(|target| (target.path.as_str().to_string(), target.result))
        .collect()
}

#[test]
fn a_plan_lands_every_target_and_commits_what_a_build_from_zero_holds() {
    let mut fixture = Fixture::new(&[
        ("a.md", "# A\n\nstatus draft\n"),
        ("inbox/b.md", "# B\n\nlinks [[a]]\n"),
        ("gone.md", "# Gone\n"),
    ]);
    let plan = fixture.plan(vec![
        editing("a.md", "draft", "final"),
        moving("inbox/b.md", "archive/b.md"),
        deleting("gone.md"),
        creating("new/c.md", "# C\n"),
    ]);
    let applied = applied(fixture.apply(plan));
    assert_eq!(
        results(&applied),
        vec![
            ("a.md".to_string(), TargetResult::Wrote),
            ("archive/b.md".to_string(), TargetResult::Wrote),
            ("gone.md".to_string(), TargetResult::Wrote),
            ("inbox/b.md".to_string(), TargetResult::Wrote),
            ("new/c.md".to_string(), TargetResult::Wrote),
        ]
    );
    assert_eq!(
        fixture.read("a.md").as_deref(),
        Some("# A\n\nstatus final\n")
    );
    assert_eq!(
        fixture.read("archive/b.md").as_deref(),
        Some("# B\n\nlinks [[a]]\n")
    );
    assert_eq!(fixture.read("new/c.md").as_deref(), Some("# C\n"));
    assert_eq!(
        fixture.tree(),
        vec!["a.md", "archive", "archive/b.md", "new", "new/c.md"],
        "the removed document's folder went with it"
    );
    let folders = |list: &[norn_wire::FolderPath]| -> Vec<String> {
        list.iter()
            .map(|folder| folder.as_str().to_string())
            .collect()
    };
    assert_eq!(folders(&applied.folders_made), vec!["archive", "new"]);
    assert_eq!(folders(&applied.folders_removed), vec!["inbox"]);
    assert_eq!(applied.changeset, norn_wire::ChangesetOutcome::Committed);
    fixture.assert_store_is_a_build_from_zero();
}

impl Fixture {
    /// The names in the shadow home: every shadow a stage left behind.
    pub(super) fn shadows_left(&self) -> Vec<String> {
        match std::fs::read_dir(self.shadows.directory()) {
            Ok(entries) => entries
                .map(|entry| {
                    entry
                        .expect("an entry")
                        .file_name()
                        .to_string_lossy()
                        .into_owned()
                })
                .collect(),
            Err(_) => Vec::new(),
        }
    }
}

pub(super) fn refused(outcome: ApplyOutcome) -> super::Refused {
    match outcome {
        ApplyOutcome::Refused(refused) => *refused,
        other => panic!("the plan is refused: {other:?}"),
    }
}

/// A refusal met while staging a later target discards what was staged
/// before it: the create staged ahead of it and the removal beside it publish
/// nothing, no folder is made, no shadow is left and no write is recorded.
#[test]
fn a_staging_refusal_publishes_nothing_creates_and_removals_included() {
    let mut fixture = Fixture::new(&[("c.md", "# C\n"), ("keep.md", "# Keep\n")]);
    let plan = fixture.plan(vec![
        creating("b.md", "# B\n"),
        deleting("c.md"),
        creating("x/c.md", "# X\n"),
    ]);
    // Another writer puts a document where the plan needs a folder, after the
    // plan was resolved.
    fixture.write("x", "a file where a folder goes\n");
    let before = fixture.tree();
    let refused = refused(fixture.apply(plan));
    assert_eq!(
        refused.checks,
        vec![norn_wire::RefusedCheck::name_taken(path("x/c.md"))]
    );
    assert_eq!(fixture.tree(), before, "nothing was published");
    assert!(
        fixture.shadows_left().is_empty(),
        "{:?}",
        fixture.shadows_left()
    );
    assert!(fixture.recorded.calls.borrow().is_empty());
}

fn present(content: &str) -> norn_wire::FileState {
    norn_wire::FileState::present(crate::planner::compose::content_hash(content.as_bytes()))
}

/// A foreign edit refuses the plan with a fresh one resolved against what the
/// vault holds now: an operation whose target already landed is dropped, the
/// drifted one is resolved again and marked, and applying the fresh plan lands
/// it over the foreign edit.
#[test]
fn a_refused_plan_answers_a_fresh_plan_that_applies() {
    let mut fixture = Fixture::new(&[
        ("a.md", "status draft\n"),
        ("b.md", "# B\nold\n"),
        ("c.md", "# C\ndraft\n"),
    ]);
    let plan = fixture.plan(vec![
        editing("a.md", "draft", "final"),
        editing("b.md", "old", "new"),
        editing("c.md", "draft", "done"),
    ]);
    // `a.md` already carries the plan's change; `b.md` is edited by another
    // writer.
    fixture.foreign("a.md", "status final\n");
    fixture.foreign("b.md", "# B\nold\nforeign\n");
    let refused = refused(fixture.apply(plan));
    assert_eq!(
        refused.checks,
        vec![norn_wire::RefusedCheck::drifted(
            path("b.md"),
            present("# B\nold\nforeign\n")
        )]
    );
    assert_eq!(refused.forecast.drifted, vec![path("b.md")]);
    assert!(refused.unresolved.is_empty(), "{:?}", refused.unresolved);
    assert_eq!(
        refused.plan.operations,
        vec![
            editing("b.md", "old", "new"),
            editing("c.md", "draft", "done")
        ],
        "the landed operation is dropped and the rest resolved again"
    );
    assert_eq!(
        refused.plan.transitions[0],
        norn_wire::Transition::new(
            path("b.md"),
            present("# B\nold\nforeign\n"),
            present("# B\nnew\nforeign\n")
        )
    );
    assert_eq!(
        fixture.read("c.md").as_deref(),
        Some("# C\ndraft\n"),
        "nothing was published"
    );
    applied(fixture.apply(refused.plan));
    assert_eq!(fixture.read("b.md").as_deref(), Some("# B\nnew\nforeign\n"));
    assert_eq!(fixture.read("c.md").as_deref(), Some("# C\ndone\n"));
    fixture.assert_store_is_a_build_from_zero();
}

/// A resolved plan applied to another root is refused with no plan.
#[test]
fn a_plan_for_another_root_is_refused_with_no_plan() {
    let mut fixture = Fixture::new(&[("a.md", "draft\n")]);
    let mut plan = fixture.plan(vec![editing("a.md", "draft", "final")]);
    plan.root = RootIdentity::from_device_and_inode(0, 0);
    match fixture.apply(plan) {
        ApplyOutcome::RootChanged { expected, found } => {
            assert_eq!(expected, RootIdentity::from_device_and_inode(0, 0));
            assert_eq!(found, fixture.root_identity());
        }
        other => panic!("the root refuses: {other:?}"),
    }
    assert_eq!(fixture.read("a.md").as_deref(), Some("draft\n"));
}

/// Re-sending a resolved plan whose publication a crash cut short finishes
/// it: the targets that landed are found, the rest are written, and the
/// store equals a build from zero. Re-sending it once more changes nothing.
#[test]
fn re_sending_a_resolved_plan_finishes_it() {
    let mut fixture = Fixture::new(&[("inbox/a.md", "# A\n"), ("b.md", "old\n")]);
    let plan = fixture.plan(vec![
        moving("inbox/a.md", "archive/a.md"),
        editing("b.md", "old", "new"),
    ]);
    // What a crash after the move's first leg leaves: the destination landed,
    // the source still standing, the edit not made.
    fixture.write("archive/a.md", "# A\n");
    let finished = applied(fixture.apply(plan.clone()));
    assert_eq!(
        results(&finished),
        vec![
            ("archive/a.md".to_string(), TargetResult::Found),
            ("b.md".to_string(), TargetResult::Wrote),
            ("inbox/a.md".to_string(), TargetResult::Wrote),
        ]
    );
    assert_eq!(fixture.tree(), vec!["archive", "archive/a.md", "b.md"]);
    fixture.assert_store_is_a_build_from_zero();
    let again = applied(fixture.apply(plan));
    assert!(
        again
            .targets
            .iter()
            .all(|target| target.result == TargetResult::Found),
        "{:?}",
        again.targets
    );
    assert!(
        fixture.recorded.calls.borrow().len() == 2,
        "a confirmed landing records nothing"
    );
    fixture.assert_store_is_a_build_from_zero();
}

/// Operations carry no confirmation beyond their own conditions: re-sent,
/// they are planned again as a new change, and an operation conditioned on
/// what its author saw no longer resolves once it applied.
#[test]
fn re_sending_conditioned_operations_is_refused_rather_than_repeated() {
    let mut fixture = Fixture::new(&[("a.md", "count 1\n")]);
    let seen = crate::planner::compose::content_hash(b"count 1\n");
    let operations = vec![editing("a.md", "1", "11").with_conditions(vec![
        norn_wire::AuthorCondition::content_hash(path("a.md"), seen),
    ])];
    applied(fixture.apply(fixture.plan(operations.clone())));
    let view = TreeView::open(&fixture.vault, &fixture.exclusions).expect("a vault");
    let name = VaultName::new("notes").expect("a legal vault name");
    let again = resolve(
        AuthoredPlan::new(VaultAddress::name(name), operations),
        fixture.root_identity(),
        &BTreeSet::new(),
        &view,
    )
    .expect("the operations plan");
    assert!(again.plan.operations.is_empty());
    assert_eq!(again.unresolved.len(), 1);
    assert_eq!(fixture.read("a.md").as_deref(), Some("count 11\n"));
}

impl Fixture {
    /// A vault holding `files` whose store pins the schema `schema`.
    pub(super) fn with_schema(schema: &str, files: &[(&str, &str)]) -> Fixture {
        let mut fixture = Fixture::new(files);
        fixture
            .store
            .begin_request()
            .pin_vault_schema(schema.as_bytes(), "applier-test-schema")
            .expect("the schema pins");
        heal_from_zero(&mut fixture.store, &fixture.vault, &fixture.exclusions).expect("a heal");
        fixture
    }
}

const TAG_SCHEMA: &str = "version: 1\ntags:\n  declared: [project]\n  undeclared: report\n";

/// A plan refuses a violation of the vault schema it introduces, and one on
/// what it writes, and not one that already stood in a target and that it
/// leaves as it was — a moved document's included.
#[test]
fn a_plan_refuses_the_schema_violations_it_introduces() {
    let mut fixture = Fixture::with_schema(
        TAG_SCHEMA,
        &[
            ("a.md", "# A\n#project\n"),
            ("b.md", "# B\n#legacy\nold\n"),
            ("inbox/c.md", "# C\n#legacy\n"),
        ],
    );
    let violation = |at: &str, tag: &str| {
        norn_wire::RefusedCheck::schema_violation(
            path(at),
            norn_wire::FindingKind::UndeclaredTag,
            Some(tag.to_string()),
            String::new(),
        )
    };
    let without_message = |checks: Vec<norn_wire::RefusedCheck>| -> Vec<norn_wire::RefusedCheck> {
        checks
            .into_iter()
            .map(|check| match check {
                norn_wire::RefusedCheck::SchemaViolation {
                    path, kind, target, ..
                } => norn_wire::RefusedCheck::schema_violation(path, kind, target, String::new()),
                other => other,
            })
            .collect()
    };
    let introduced = refused(fixture.apply(fixture.plan(vec![
        editing("a.md", "#project", "#project #stray"),
        creating("d.md", "# D\n#other\n"),
        editing("b.md", "#legacy", "#legacy #legacy"),
    ])));
    assert_eq!(
        without_message(introduced.checks),
        vec![
            violation("a.md", "stray"),
            violation("b.md", "legacy"),
            violation("d.md", "other"),
        ]
    );
    assert_eq!(fixture.read("a.md").as_deref(), Some("# A\n#project\n"));
    let standing = applied(fixture.apply(fixture.plan(vec![
        editing("b.md", "old", "new"),
        moving("inbox/c.md", "archive/c.md"),
    ])));
    assert_eq!(standing.targets.len(), 3);
    fixture.assert_store_is_a_build_from_zero();
}

/// A move's source found gone while its destination is not at its after-state
/// was taken by another writer, since the applier removes a source only after
/// its destination durably landed: the source is drift, and the move is left
/// unresolved rather than resolved again.
#[test]
fn a_move_whose_source_another_writer_removed_is_unresolved() {
    let mut fixture = Fixture::new(&[("a.md", "# A\n"), ("b.md", "# B\n"), ("e.md", "e\n")]);
    let plan = fixture.plan(vec![
        moving("b.md", "c.md"),
        moving("a.md", "b.md"),
        editing("e.md", "e", "E"),
    ]);
    // The chain's middle carries its new content while the document it held
    // never reached its destination: another writer's doing.
    fixture.foreign("b.md", "# A\n");
    let refused = refused(fixture.apply(plan));
    assert_eq!(
        refused.checks,
        vec![norn_wire::RefusedCheck::drifted(
            path("b.md"),
            present("# A\n")
        )]
    );
    let unresolved: Vec<&Operation> = refused
        .unresolved
        .iter()
        .map(|left| &left.operation)
        .collect();
    assert_eq!(
        unresolved,
        vec![&moving("b.md", "c.md"), &moving("a.md", "b.md")]
    );
    assert_eq!(refused.plan.operations, vec![editing("e.md", "e", "E")]);
    assert_eq!(
        fixture.tree(),
        vec!["a.md", "b.md", "e.md"],
        "nothing foreign is removed"
    );
}

#[test]
fn a_move_whose_source_is_gone_before_its_destination_landed_is_drift() {
    let mut fixture = Fixture::new(&[("x.md", "# X\n")]);
    let plan = fixture.plan(vec![moving("x.md", "y.md")]);
    std::fs::remove_file(fixture.vault.join("x.md")).expect("another writer removes it");
    heal_from_zero(&mut fixture.store, &fixture.vault, &fixture.exclusions).expect("a heal");
    let refused = refused(fixture.apply(plan));
    assert_eq!(
        refused.checks,
        vec![norn_wire::RefusedCheck::drifted(
            path("x.md"),
            norn_wire::FileState::absent()
        )]
    );
    assert_eq!(refused.forecast.drifted, vec![path("x.md")]);
    assert_eq!(refused.unresolved.len(), 1);
    assert!(refused.plan.operations.is_empty());
    assert!(fixture.tree().is_empty());
}

/// Every publication is recorded as an own write the moment it lands, at the
/// plan's path, and only a publication: a move is two, and a target found
/// already landed is none.
#[test]
fn each_publication_is_recorded_as_an_own_write_when_it_lands() {
    let mut fixture = Fixture::new(&[("a.md", "a\n"), ("inbox/b.md", "b\n")]);
    let plan = fixture.plan(vec![
        editing("a.md", "a", "A"),
        moving("inbox/b.md", "b.md"),
        creating("c.md", "c\n"),
    ]);
    fixture.write("c.md", "c\n");
    applied(fixture.apply(plan));
    assert_eq!(
        *fixture.recorded.calls.borrow(),
        vec![
            (PathBuf::from("b.md"), true),
            (PathBuf::from("a.md"), true),
            (PathBuf::from("inbox/b.md"), true),
        ],
        "creates, then replaces, then removals; the found create is not recorded"
    );
}

/// The kernel keeps a staged path as it was given, a leading `./` included,
/// and a document path's grammar admits one: a plan naming a target so, as a
/// hand-edited plan can, is refused before anything is staged, and the fresh
/// plan spells the target once. So nothing is published, recorded or derived
/// at a second spelling of one file.
#[test]
fn a_target_spelled_with_a_dot_component_is_refused_and_spelled_once_afresh() {
    let mut fixture = Fixture::new(&[("a.md", "a\n")]);
    let staged = norn_fs::stage(
        &fixture.vault,
        fixture.root,
        Path::new("./a.md"),
        norn_fs::Transition::Remove {
            before: norn_fs::ContentHash::of(b"a\n"),
        },
        &fixture.shadows,
    )
    .expect("the removal stages");
    let norn_fs::Staging::Staged(staged) = staged else {
        panic!("the removal is waiting");
    };
    assert_eq!(staged.path(), Path::new("./a.md"));
    norn_fs::discard(&fixture.vault, staged, &fixture.shadows);

    let mut plan = fixture.plan(vec![creating("new.md", "n\n")]);
    plan.transitions[0].path = path("./new.md");
    let refused = refused(fixture.apply(plan));
    assert_eq!(refused.plan.transitions[0].path, path("new.md"));
    assert!(fixture.recorded.calls.borrow().is_empty());
    assert_eq!(fixture.tree(), vec!["a.md"]);
}

/// **Mark invariance.** The same changes committed marked composed and marked
/// derived read the same derivation counters: the mark changes no statement
/// the store runs.
#[test]
fn a_changeset_reads_the_same_counters_marked_composed_or_derived() {
    use crate::production::{PlanEffect, commit_plan_changeset};
    use norn_store::IncrementProvenance;

    let reading = |provenance: IncrementProvenance| {
        let mut fixture = Fixture::with_schema(
            TAG_SCHEMA,
            &[
                ("a.md", "# A\n[[b]]\n"),
                ("b.md", "# B\n"),
                ("c.md", "# C\n"),
            ],
        );
        fixture.write("a.md", "# A\n[[c]] #stray\n");
        fixture.write("d.md", "---\ntitle: D\n---\n# D\n[[a]]\n");
        std::fs::remove_file(fixture.vault.join("c.md")).expect("a removal");
        let effect = |at: &str, content: Option<&str>| PlanEffect {
            path: norn_store::DocumentPath::new(at).expect("a path"),
            holds: content.map(|content| norn_fs::ContentHash::of(content.as_bytes())),
        };
        let effects = [
            effect("a.md", Some("# A\n[[c]] #stray\n")),
            effect("c.md", None),
            effect("d.md", Some("---\ntitle: D\n---\n# D\n[[a]]\n")),
        ];
        commit_plan_changeset(
            &mut fixture.store,
            &fixture.vault,
            &fixture.exclusions,
            &effects,
            provenance,
        )
        .expect("the changeset commits")
    };
    let composed = reading(IncrementProvenance::Composed);
    let derived = reading(IncrementProvenance::Derived);
    assert!(
        composed.readings().any(|(_, value)| value > 0),
        "the changeset moved something"
    );
    assert_eq!(composed, derived);
}
