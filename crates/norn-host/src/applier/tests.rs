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
            publishing: &|| true,
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

    /// Every path the store holds a document row at, at its stored spelling.
    pub(super) fn stored_paths(&mut self) -> Vec<String> {
        self.store
            .begin_request()
            .stored_documents_after_ordered(None, 500, StoredPathOrder::Sensitive)
            .expect("rows")
            .into_iter()
            .map(|row| row.path.as_str().to_owned())
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

pub(super) fn refused(outcome: ApplyOutcome) -> super::outcome::Refused {
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
                norn_wire::RefusedCheck::SchemaViolation { violation, .. } => {
                    norn_wire::RefusedCheck::schema_violation(
                        violation.path,
                        violation.kind,
                        violation.target,
                        String::new(),
                    )
                }
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

/// A target whose content is drawn from another target's before-state
/// publishes before that source is replaced, whatever the plan's own order
/// says (ADR 0031): moving `a.md` onto `b.md` and creating `a.md` afresh
/// lands `b.md` first, so a crash between the two leaves the moved content
/// standing at one of its names.
#[test]
fn a_target_drawing_on_another_publishes_before_its_source_is_replaced() {
    let mut fixture = Fixture::new(&[("a.md", "A\n"), ("b.md", "B\n")]);
    let plan = fixture.plan(vec![
        deleting("b.md"),
        moving("a.md", "b.md"),
        creating("a.md", "N\n"),
    ]);
    applied(fixture.apply(plan));
    assert_eq!(
        *fixture.recorded.calls.borrow(),
        vec![(PathBuf::from("b.md"), true), (PathBuf::from("a.md"), true)],
        "the target drawing on a.md lands before a.md is replaced"
    );
    assert_eq!(fixture.read("b.md").as_deref(), Some("A\n"));
    assert_eq!(fixture.read("a.md").as_deref(), Some("N\n"));
}

/// The kernel keeps a staged path as it was given, a leading `./` included,
/// and a document path's grammar admits one: a plan naming a target so, as a
/// hand-edited plan can, is invalid as a plan that does not describe its
/// operations, never drift, and names the target as spelled. So nothing is
/// published, recorded or derived at a second spelling of one file.
#[test]
#[allow(clippy::disallowed_methods)] // The kernel itself: the case pins how it keeps a staged path's spelling.
fn a_target_spelled_with_a_dot_component_is_invalid() {
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
    assert_eq!(fixture.refuses_disagreeing(plan), vec![path("./new.md")]);
    assert_eq!(fixture.tree(), vec!["a.md"]);

    // A removal spelled so is refused the same way, and not as drift: the
    // document stands, holding its before-state.
    let mut plan = fixture.plan(vec![deleting("a.md")]);
    plan.transitions[0].path = path("./a.md");
    plan.operations[0] = deleting("./a.md");
    assert_eq!(fixture.refuses_disagreeing(plan), vec![path("./a.md")]);
    assert_eq!(fixture.read("a.md").as_deref(), Some("a\n"));
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

const BETWEEN_PHASES_CHILD: &str = "NORN_APPLIER_BETWEEN_PHASES_CHILD";

/// **Plan memory.** Between staging and publication the applier holds the
/// plan and one fixed-size record per target: no open handle, and none of the
/// bytes of a document it rewrites, however large. The descriptor count is
/// read in a child of its own, where no other case opens a file beside it.
#[test]
fn between_staging_and_publication_the_applier_holds_no_handle_and_no_content() {
    if std::env::var_os(BETWEEN_PHASES_CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().expect("this binary"))
            .args([
                "--exact",
                "applier::tests::between_staging_and_publication_the_applier_holds_no_handle_and_no_content",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(BETWEEN_PHASES_CHILD, "1")
            .output()
            .expect("the child runs");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stdout)
        );
        return;
    }
    let large = format!("# Large\nstatus draft\n{}", "x".repeat(4 << 20));
    let mut fixture = Fixture::new(&[("large.md", &large), ("b.md", "b\n")]);
    let plan = fixture.plan(vec![
        editing("large.md", "draft", "final"),
        creating("new/c.md", "c\n"),
        deleting("b.md"),
    ]);
    let view = TreeView::open(&fixture.vault, &fixture.exclusions).expect("a vault");
    let declared = crate::production::pinned_declaration(&mut fixture.store).expect("a schema");
    let before = norn_testkit::process::open_fd_count().expect("a count");
    let staged = super::stage::check_and_stage(
        &fixture.vault,
        fixture.root,
        &fixture.shadows,
        &plan,
        &view,
        &declared,
    )
    .expect("the plan stages");
    let between = norn_testkit::process::open_fd_count().expect("a count");
    assert_eq!(between, before, "staging leaves no handle open");
    assert_eq!(staged.targets.len(), 3);
    let held = format!("{staged:?}");
    assert!(
        held.len() < 3 * 1024,
        "the staged records hold no content: {} bytes of record",
        held.len()
    );
    let publisher = super::publish::Publisher {
        anchor: &fixture.vault,
        root: fixture.root,
        shadows: &fixture.shadows,
        own_writes: &fixture.recorded,
    };
    let (_, stopped) = publisher.publish(&plan, staged);
    assert!(stopped.is_none(), "{stopped:?}");
    assert!(
        fixture
            .read("large.md")
            .expect("landed")
            .contains("status final")
    );
}

/// A changeset that cannot commit after every target landed answers applied,
/// with the entry left to heal from what the paths hold.
#[test]
fn a_changeset_that_cannot_commit_after_every_target_landed_answers_applied_healing() {
    let mut fixture = Fixture::new(&[("a.md", "draft\n")]);
    let plan = fixture.plan(vec![editing("a.md", "draft", "final")]);
    norn_store::induced_failure::execute_out_of_band(
        &mut fixture.store,
        "CREATE TRIGGER refuse_every_write BEFORE UPDATE ON documents \
         BEGIN SELECT RAISE(ABORT, 'induced'); END;",
    )
    .expect("the trigger lands");
    let applied = applied(fixture.apply(plan));
    assert_eq!(applied.changeset, norn_wire::ChangesetOutcome::Healing);
    assert_eq!(fixture.read("a.md").as_deref(), Some("final\n"));
}

/// Whether the scratch volume folds case, standing the case down where it
/// does not (and failing it on macOS, whose volume folds).
pub(super) fn volume_folds(case: &str) -> bool {
    let scratch = Scratch::new("applier-folding");
    let folding = norn_testkit::churn::folding(scratch.root()).expect("the volume answers");
    norn_testkit::churn::runs_where_the_volume_folds(folding, case)
}

/// On a root that folds case, a case-only rename — with an edit composed
/// into it — publishes as the kernel's one respell: the document stands at
/// its new spelling only, the rename is recorded once, at the new spelling,
/// and the store holds the document there alone: the old spelling's row dies
/// although the volume resolves that spelling to the renamed file.
#[test]
fn a_case_only_rename_on_a_folding_root_publishes_as_one_respell() {
    if !volume_folds("a_case_only_rename_on_a_folding_root_publishes_as_one_respell") {
        return;
    }
    let mut fixture = Fixture::new(&[("Note.md", "# Note\ndraft\n")]);
    let plan = fixture.plan(vec![
        editing("Note.md", "draft", "final"),
        moving("Note.md", "note.md"),
    ]);
    let respelled = applied(fixture.apply(plan.clone()));
    assert_eq!(
        results(&respelled),
        vec![
            ("Note.md".to_string(), TargetResult::Wrote),
            ("note.md".to_string(), TargetResult::Wrote),
        ]
    );
    assert_eq!(fixture.tree(), vec!["note.md"]);
    assert_eq!(fixture.read("note.md").as_deref(), Some("# Note\nfinal\n"));
    assert_eq!(
        *fixture.recorded.calls.borrow(),
        vec![(PathBuf::from("note.md"), true)]
    );
    assert_eq!(fixture.stored_paths(), vec!["note.md".to_string()]);
    fixture.assert_store_is_a_build_from_zero();
    let again = applied(fixture.apply(plan));
    assert!(
        again
            .targets
            .iter()
            .all(|target| target.result == TargetResult::Found)
    );
}

/// Every outcome crosses under the wire's code for it, and every one given
/// after the plan was checked carries a resolved plan.
#[test]
fn each_outcome_crosses_under_its_wire_code() {
    use norn_wire::{ChangesetOutcome, ErrorDetail, Forecast, InterruptionCause, ReasonCode};

    let fixture = Fixture::new(&[("a.md", "a\n")]);
    let plan = fixture.plan(vec![deleting("a.md")]);
    let applied_report = ApplyOutcome::Applied(super::Applied {
        plan: plan.clone(),
        changeset: ChangesetOutcome::Committed,
        targets: Vec::new(),
        folders_made: Vec::new(),
        folders_removed: Vec::new(),
    })
    .into_wire()
    .expect("an applied plan is answered")
    .expect("an applied plan is a report");
    assert!(
        matches!(applied_report, norn_wire::ApplyReport::Applied { plan: carried, .. } if carried == plan)
    );
    let code = |outcome: ApplyOutcome| {
        let envelope = outcome
            .into_wire()
            .expect("a refusal is answered")
            .expect_err("a refusal");
        let carries_plan = match envelope.detail() {
            ErrorDetail::PlanRefused { .. }
            | ErrorDetail::PlanInterrupted { .. }
            | ErrorDetail::WriteFailed { .. } => true,
            ErrorDetail::RootChanged { .. } | ErrorDetail::PlanInvalid { .. } => false,
            other => panic!("an apply's own detail: {other:?}"),
        };
        (envelope.code().clone(), carries_plan)
    };
    assert_eq!(
        code(ApplyOutcome::Refused(Box::new(super::outcome::Refused {
            plan: plan.clone(),
            forecast: Forecast::new(Vec::new(), Vec::new(), Vec::new()),
            checks: Vec::new(),
            unresolved: Vec::new(),
        }))),
        (ReasonCode::VaultPlanRefused, true)
    );
    assert_eq!(
        code(ApplyOutcome::RootChanged {
            expected: fixture.root_identity(),
            found: RootIdentity::from_device_and_inode(0, 0),
        }),
        (ReasonCode::VaultRootChanged, false)
    );
    assert_eq!(
        code(ApplyOutcome::Invalid(
            norn_wire::PlanFault::transitions_disagree(vec![path("a.md")])
        )),
        (ReasonCode::RequestPlanInvalid, false)
    );
    assert_eq!(
        code(ApplyOutcome::Interrupted(Box::new(super::Interrupted {
            plan: plan.clone(),
            landed: vec![path("a.md")],
            cause: InterruptionCause::io_failure("a sync failed"),
            changeset: ChangesetOutcome::Committed,
        }))),
        (ReasonCode::VaultPlanInterrupted, true)
    );
    assert_eq!(
        code(ApplyOutcome::WriteFailed {
            plan,
            detail: "a disk filled".to_string(),
        }),
        (ReasonCode::VaultWriteFailed, true)
    );
}

impl Fixture {
    /// Apply `plan`, which does not describe its operations, and hold that it
    /// answers `request/plan-invalid` with nothing published: no name
    /// changed, no content changed, no write recorded and no shadow left.
    /// The files it disagrees at are returned.
    fn refuses_disagreeing(&mut self, plan: ResolvedPlan) -> Vec<DocumentPath> {
        let before: Vec<(String, Option<String>)> = self
            .tree()
            .into_iter()
            .map(|name| {
                let content = self.read(&name);
                (name, content)
            })
            .collect();
        let envelope = self
            .apply(plan)
            .into_wire()
            .expect("a refusal is answered")
            .expect_err("the plan is refused");
        assert_eq!(envelope.code(), &norn_wire::ReasonCode::RequestPlanInvalid);
        let paths = match envelope.detail() {
            norn_wire::ErrorDetail::PlanInvalid {
                fault: norn_wire::PlanFault::TransitionsDisagree { paths, .. },
                ..
            } => paths.clone(),
            other => panic!("the plan's transitions disagree with its operations: {other:?}"),
        };
        let after: Vec<(String, Option<String>)> = self
            .tree()
            .into_iter()
            .map(|name| {
                let content = self.read(&name);
                (name, content)
            })
            .collect();
        assert_eq!(after, before, "nothing was published");
        assert!(self.recorded.calls.borrow().is_empty(), "no write recorded");
        assert!(self.shadows_left().is_empty(), "{:?}", self.shadows_left());
        paths
    }
}

/// A move whose destination's transition is taken out of the plan would
/// only remove its document: refused, and the document stays.
#[test]
fn a_move_whose_destination_transition_is_dropped_is_refused() {
    let mut fixture = Fixture::new(&[("a.md", "# A\n")]);
    let mut plan = fixture.plan(vec![moving("a.md", "b.md")]);
    plan.transitions
        .retain(|transition| transition.path != path("b.md"));
    assert_eq!(fixture.refuses_disagreeing(plan), vec![path("b.md")]);
    assert_eq!(fixture.read("a.md").as_deref(), Some("# A\n"));
}

/// **A resolved plan still carrying a `where` target is invalid.** Planning
/// expands every `where` target into operations on paths, so such a plan was
/// not made by planning: it answers `request/plan-invalid` naming the
/// operation, before the vault is read, and nothing is published.
#[test]
fn a_resolved_plan_carrying_a_where_target_is_invalid() {
    let mut fixture = Fixture::new(&[("a.md", "draft\n")]);
    let mut plan = fixture.plan(vec![editing("a.md", "draft", "final")]);
    plan.operations
        .push(Operation::new(norn_wire::OperationKind::set_frontmatter(
            norn_wire::WriteTarget::matching(Vec::new()),
            "status",
            norn_wire::AuthoredValue::string("done"),
        )));
    let envelope = fixture
        .apply(plan)
        .into_wire()
        .expect("a refusal is answered")
        .expect_err("the plan is refused");
    assert_eq!(envelope.code(), &norn_wire::ReasonCode::RequestPlanInvalid);
    assert_eq!(
        envelope.detail(),
        &norn_wire::ErrorDetail::plan_invalid(norn_wire::PlanFault::unexpanded_target(vec![1]))
    );
    assert_eq!(fixture.read("a.md").as_deref(), Some("draft\n"));
    assert!(
        fixture.recorded.calls.borrow().is_empty(),
        "no write recorded"
    );
}

/// A removal no operation makes, added to a plan, is refused.
#[test]
fn an_extra_removal_no_operation_makes_is_refused() {
    let mut fixture = Fixture::new(&[("a.md", "draft\n"), ("c.md", "# C\n")]);
    let mut plan = fixture.plan(vec![editing("a.md", "draft", "final")]);
    plan.transitions.push(norn_wire::Transition::new(
        path("c.md"),
        present("# C\n"),
        norn_wire::FileState::absent(),
    ));
    assert_eq!(fixture.refuses_disagreeing(plan), vec![path("c.md")]);
}

/// A transition for a file no operation touches is the plan's shape, not
/// the vault's: it is invalid whatever before-state it names, so a wrong
/// guess at the file's hash is never reported as drift.
#[test]
fn an_extra_transition_with_a_wrong_before_state_is_invalid_not_drift() {
    let mut fixture = Fixture::new(&[("a.md", "draft\n"), ("c.md", "# C\n")]);
    let mut plan = fixture.plan(vec![editing("a.md", "draft", "final")]);
    plan.transitions.push(norn_wire::Transition::new(
        path("c.md"),
        present("not what c holds\n"),
        norn_wire::FileState::absent(),
    ));
    assert_eq!(fixture.refuses_disagreeing(plan), vec![path("c.md")]);
    assert_eq!(fixture.read("c.md").as_deref(), Some("# C\n"));
}

/// A transition spelled in another case than the file its operation edits
/// names another file where the root tells case apart: invalid, naming the
/// spelling no operation touches and the file left without a transition,
/// never drift holding absence. Where the root folds case the two spellings
/// are one file, and a document standing at the other spelling is what a
/// foreign case-only rename leaves, which only the vault can tell.
#[test]
fn a_transition_spelled_in_another_case_is_invalid_where_case_is_told_apart() {
    let mut fixture = Fixture::new(&[("a.md", "draft\n")]);
    let folds = TreeView::open(&fixture.vault, &[])
        .expect("a vault")
        .normalizer()
        .case_sensitivity()
        == norn_fs::CaseSensitivity::Insensitive;
    let mut plan = fixture.plan(vec![editing("a.md", "draft", "final")]);
    plan.transitions[0].path = path("A.md");
    if folds {
        assert!(
            fixture
                .apply(plan)
                .into_wire()
                .is_some_and(|answer| answer.is_err())
        );
    } else {
        assert_eq!(
            fixture.refuses_disagreeing(plan),
            vec![path("A.md"), path("a.md")]
        );
    }
    assert_eq!(fixture.read("a.md").as_deref(), Some("draft\n"));
    assert!(fixture.recorded.calls.borrow().is_empty());
}

/// A transition whose before-state was changed on a file its operation does
/// touch is drift, as a foreign edit is: the two cannot be told apart, so it
/// answers a fresh plan with the target marked.
#[test]
fn a_changed_before_state_on_a_touched_file_is_drift() {
    let mut fixture = Fixture::new(&[("a.md", "draft\n")]);
    let mut plan = fixture.plan(vec![editing("a.md", "draft", "final")]);
    plan.transitions[0].before = present("something else\n");
    let refused = refused(fixture.apply(plan));
    assert_eq!(
        refused.checks,
        vec![norn_wire::RefusedCheck::drifted(
            path("a.md"),
            present("draft\n")
        )]
    );
    assert_eq!(refused.forecast.drifted, vec![path("a.md")]);
    assert_eq!(fixture.read("a.md").as_deref(), Some("draft\n"));
}

/// An edit whose after-state is changed to absence would remove its
/// document: refused.
#[test]
fn an_edit_whose_after_state_is_made_absent_is_refused() {
    let mut fixture = Fixture::new(&[("a.md", "draft\n")]);
    let mut plan = fixture.plan(vec![editing("a.md", "draft", "final")]);
    plan.transitions[0].after = norn_wire::FileState::absent();
    assert_eq!(fixture.refuses_disagreeing(plan), vec![path("a.md")]);
}

/// A plan disagreeing with its operations at more than one file names every
/// one its first failing check finds, sorted. The shape check runs before
/// any vault read and stops the plan there, so a transition no operation
/// accounts for and one the operations would compose otherwise are found by
/// two checks, each naming all of its own.
#[test]
fn a_plan_disagreeing_at_several_files_names_every_one() {
    let mut fixture = Fixture::new(&[("a.md", "draft\n"), ("c.md", "# C\n")]);
    let mut plan = fixture.plan(vec![
        editing("a.md", "draft", "final"),
        creating("n.md", "# N\n"),
    ]);
    plan.transitions
        .retain(|transition| transition.path != path("n.md"));
    plan.transitions.push(norn_wire::Transition::new(
        path("c.md"),
        present("# C\n"),
        norn_wire::FileState::absent(),
    ));
    assert_eq!(
        fixture.refuses_disagreeing(plan),
        vec![path("c.md"), path("n.md")]
    );

    let mut plan = fixture.plan(vec![
        editing("a.md", "draft", "final"),
        creating("n.md", "# N\n"),
    ]);
    for transition in &mut plan.transitions {
        transition.after = norn_wire::FileState::absent();
    }
    assert_eq!(
        fixture.refuses_disagreeing(plan),
        vec![path("a.md"), path("n.md")]
    );
}

/// A target named by two transitions is refused, whatever they say.
#[test]
fn a_target_named_twice_is_refused() {
    let mut fixture = Fixture::new(&[("a.md", "draft\n")]);
    let mut plan = fixture.plan(vec![editing("a.md", "draft", "final")]);
    plan.transitions.push(plan.transitions[0].clone());
    assert_eq!(fixture.refuses_disagreeing(plan), vec![path("a.md")]);
}

/// A plan missing the transition of a file an operation writes is refused:
/// here a create whose target was taken out.
#[test]
fn a_plan_missing_a_transition_is_refused() {
    let mut fixture = Fixture::new(&[("a.md", "draft\n")]);
    let mut plan = fixture.plan(vec![
        editing("a.md", "draft", "final"),
        creating("n.md", "# N\n"),
    ]);
    plan.transitions
        .retain(|transition| transition.path != path("n.md"));
    assert_eq!(fixture.refuses_disagreeing(plan), vec![path("n.md")]);
}

/// An edit whose after-state is changed to its before-state would answer
/// found and write nothing, as if the edit had landed: refused.
#[test]
fn an_edit_whose_after_state_is_its_before_state_is_refused() {
    let mut fixture = Fixture::new(&[("a.md", "draft\n")]);
    let mut plan = fixture.plan(vec![editing("a.md", "draft", "final")]);
    plan.transitions[0].after = plan.transitions[0].before.clone();
    assert_eq!(fixture.refuses_disagreeing(plan), vec![path("a.md")]);
}

impl Fixture {
    /// A resolved plan written by hand: `operations` and `transitions` as
    /// given, under this vault's root, with no condition.
    pub(super) fn by_hand(
        &self,
        operations: Vec<Operation>,
        transitions: Vec<norn_wire::Transition>,
    ) -> ResolvedPlan {
        let name = VaultName::new("notes").expect("a legal vault name");
        ResolvedPlan::new(
            VaultAddress::name(name),
            self.root_identity(),
            operations,
            transitions,
            Vec::new(),
        )
    }
}

/// A create at a place the vault does not read documents at — the shadow
/// fallback beneath `.norn`, or a root the host excludes — is refused whatever
/// its before-state says, and nothing is made there; so is a removal there.
#[test]
fn a_target_where_the_vault_reads_no_documents_is_refused() {
    let mut fixture = Fixture::new(&[("a.md", "a\n")]);
    fixture.write("private/p.md", "# P\n");
    fixture.exclusions.push(PathBuf::from("private"));
    for at in [".norn/tmp/x.md", "private/x.md"] {
        let plan = fixture.by_hand(
            vec![creating(at, "# X\n")],
            vec![norn_wire::Transition::new(
                path(at),
                norn_wire::FileState::absent(),
                present("# X\n"),
            )],
        );
        assert_eq!(fixture.refuses_disagreeing(plan), vec![path(at)], "{at}");
        assert!(fixture.read(at).is_none(), "{at}: nothing is made there");
    }
    let plan = fixture.by_hand(
        vec![deleting("private/p.md")],
        vec![norn_wire::Transition::new(
            path("private/p.md"),
            present("# P\n"),
            norn_wire::FileState::absent(),
        )],
    );
    assert_eq!(
        fixture.refuses_disagreeing(plan),
        vec![path("private/p.md")]
    );
    assert_eq!(fixture.read("private/p.md").as_deref(), Some("# P\n"));
}

/// Two documents exchanging places is a content cycle: a plan carrying one
/// is no plan, and nothing is published.
#[test]
fn a_plan_whose_moves_exchange_two_documents_is_invalid() {
    let mut fixture = Fixture::new(&[("a.md", "# A\n"), ("b.md", "# B\n")]);
    let plan = fixture.by_hand(
        vec![moving("a.md", "b.md"), moving("b.md", "a.md")],
        vec![
            norn_wire::Transition::new(path("a.md"), present("# A\n"), present("# B\n")),
            norn_wire::Transition::new(path("b.md"), present("# B\n"), present("# A\n")),
        ],
    );
    match fixture.apply(plan) {
        ApplyOutcome::Invalid(norn_wire::PlanFault::ContentCycle { .. }) => {}
        other => panic!("the plan is invalid as a content cycle: {other:?}"),
    }
    assert_eq!(fixture.read("a.md").as_deref(), Some("# A\n"));
    assert_eq!(fixture.read("b.md").as_deref(), Some("# B\n"));
    assert!(fixture.recorded.calls.borrow().is_empty());
    assert!(fixture.shadows_left().is_empty());
}

/// Two documents exchanging places through a third name the plan makes and
/// takes away is the same content cycle: each draws on the other's
/// before-state, so neither can publish first without destroying what the
/// other needs. Planning refuses it; a caller can still send the resolved
/// plan by hand, and the applier refuses it by the planner's own rule, the
/// moves named from the file whose moves hold the lowest position.
#[test]
fn a_plan_exchanging_two_documents_through_a_third_name_is_invalid() {
    let mut fixture = Fixture::new(&[("a.md", "# A\n"), ("b.md", "# B\n")]);
    let operations = vec![
        moving("a.md", "t.md"),
        moving("b.md", "a.md"),
        moving("t.md", "b.md"),
    ];
    let view = TreeView::open(&fixture.vault, &fixture.exclusions).expect("a vault");
    let name = VaultName::new("notes").expect("a legal vault name");
    let authored = AuthoredPlan::new(VaultAddress::name(name), operations.clone());
    match resolve(authored, fixture.root_identity(), &BTreeSet::new(), &view) {
        Err(crate::planner::resolve::PlanningFailure::Fault(
            norn_wire::PlanFault::ContentCycle { .. },
        )) => {}
        other => panic!("planning refuses the plan as a content cycle: {other:?}"),
    }
    let plan = fixture.by_hand(
        operations,
        vec![
            norn_wire::Transition::new(path("a.md"), present("# A\n"), present("# B\n")),
            norn_wire::Transition::new(path("b.md"), present("# B\n"), present("# A\n")),
            norn_wire::Transition::new(
                path("t.md"),
                norn_wire::FileState::absent(),
                norn_wire::FileState::absent(),
            ),
        ],
    );
    match fixture.apply(plan) {
        ApplyOutcome::Invalid(norn_wire::PlanFault::ContentCycle { positions, .. }) => {
            assert_eq!(positions, vec![0, 2, 1]);
        }
        other => panic!("the plan is invalid as a content cycle: {other:?}"),
    }
    assert_eq!(fixture.read("a.md").as_deref(), Some("# A\n"));
    assert_eq!(fixture.read("b.md").as_deref(), Some("# B\n"));
    assert!(fixture.read("t.md").is_none());
    assert!(fixture.recorded.calls.borrow().is_empty());
    assert!(fixture.shadows_left().is_empty());
}

/// A plan whose recorded operation order is not one its requirements allow
/// is refused: the order it composes in is the recorded one, and it must be
/// an order the operations can run in.
#[test]
fn a_plan_recorded_out_of_its_requirements_order_is_refused() {
    let mut fixture = Fixture::new(&[("a.md", "# A\n"), ("b.md", "# B\n")]);
    let mut plan = fixture.plan(vec![moving("a.md", "b.md"), moving("b.md", "c.md")]);
    // Planned as `b → c` then `a → b`; recorded the other way round, the
    // move onto `b.md` meets its document still standing.
    plan.operations.reverse();
    assert_eq!(
        fixture.refuses_disagreeing(plan),
        vec![path("a.md"), path("b.md"), path("c.md")]
    );
}

/// A target at a path the store cannot name — a leaf whose stem is `.`, a
/// name carrying a backslash — is refused before anything is staged. A
/// removal there was published and then, with no path to record it at,
/// panicked the apply.
#[test]
fn a_target_the_store_cannot_name_is_refused() {
    for at in ["..md", "x\\y.md"] {
        let mut fixture = Fixture::new(&[("a.md", "a\n")]);
        fixture.write(at, "gone\n");
        let plan = fixture.by_hand(
            vec![deleting(at)],
            vec![norn_wire::Transition::new(
                path(at),
                present("gone\n"),
                norn_wire::FileState::absent(),
            )],
        );
        assert_eq!(fixture.refuses_disagreeing(plan), vec![path(at)], "{at}");
        assert_eq!(fixture.read(at).as_deref(), Some("gone\n"), "{at}");
    }
}

/// A written document's schema is judged against the document its content
/// came from: a create writes content no document had, so a violation it
/// writes refuses even where a document the plan removes carried the same
/// one; a move's destination is judged against the moved document alone.
#[test]
fn a_result_is_judged_against_the_document_its_content_came_from() {
    let mut fixture = Fixture::with_schema(
        TAG_SCHEMA,
        &[
            ("old.md", "# Old\n#legacy\n"),
            ("inbox/c.md", "# C\n#project\n"),
        ],
    );
    let refused = refused(fixture.apply(fixture.plan(vec![
        deleting("old.md"),
        creating("new.md", "# New\n#legacy\n"),
    ])));
    assert_eq!(
        refused
            .checks
            .iter()
            .map(|check| match check {
                norn_wire::RefusedCheck::SchemaViolation { violation, .. } => (
                    violation.path.as_str().to_string(),
                    violation.target.clone()
                ),
                other => panic!("a schema check: {other:?}"),
            })
            .collect::<Vec<_>>(),
        vec![("new.md".to_string(), Some("legacy".to_string()))]
    );
    let refused = refused_for(fixture.apply(fixture.plan(vec![
        deleting("old.md"),
        moving("inbox/c.md", "archive/c.md"),
        editing("archive/c.md", "#project", "#project #legacy"),
    ])));
    assert_eq!(refused.len(), 1, "{refused:?}");
    assert_eq!(fixture.read("old.md").as_deref(), Some("# Old\n#legacy\n"));
}

fn refused_for(outcome: ApplyOutcome) -> Vec<norn_wire::RefusedCheck> {
    refused(outcome).checks
}

/// A ledger that lets another writer act on one path the moment the applier
/// publishes there: between the publication and the changeset.
struct Meddling {
    anchor: PathBuf,
    at: PathBuf,
    /// What the other writer leaves at the path.
    leaves: &'static str,
}

impl OwnWriteLedger for Meddling {
    fn published(&self, path: &Path, _: &Published) {
        if path == self.at {
            std::fs::write(self.anchor.join(path), self.leaves).expect("the foreign write");
        }
    }
}

impl Fixture {
    /// Apply `plan` while another writer leaves `leaves` at `at` the moment
    /// the applier publishes there.
    fn apply_meddled(
        &mut self,
        plan: ResolvedPlan,
        at: &str,
        leaves: &'static str,
    ) -> ApplyOutcome {
        let meddling = Meddling {
            anchor: self.vault.clone(),
            at: PathBuf::from(at),
            leaves,
        };
        let applier = Applier {
            anchor: &self.vault,
            root: self.root,
            exclusions: &self.exclusions,
            shadows: &self.shadows,
            own_writes: &meddling,
            publishing: &|| true,
        };
        applier.apply(plan, &mut self.store)
    }

    /// The content hash the store holds for `at`, where it holds a row.
    fn stored_hash(&mut self, at: &str) -> Option<String> {
        self.store
            .begin_request()
            .stored_document(&norn_store::DocumentPath::new(at).expect("a path"))
            .expect("a read")
            .map(|row| row.content_hash)
    }
}

/// Another writer's edit between a target's publication and the changeset is
/// not committed as the plan's: the target's row keeps what it held, and the
/// edit is the watcher's to report — the own-write ledger's entry names the
/// published bytes, which the path no longer holds, so it does not absorb the
/// event — and once it is taken in the store is what a build from zero holds.
#[test]
fn a_foreign_edit_after_publication_is_left_to_the_watcher() {
    let mut fixture = Fixture::new(&[("a.md", "draft\n"), ("b.md", "old\n")]);
    let plan = fixture.plan(vec![
        editing("a.md", "draft", "final"),
        editing("b.md", "old", "new"),
    ]);
    let before = fixture.stored_hash("a.md");
    applied(fixture.apply_meddled(plan, "a.md", "foreign\n"));
    assert_eq!(fixture.read("a.md").as_deref(), Some("foreign\n"));
    assert_eq!(
        fixture.stored_hash("a.md"),
        before,
        "the row is not the plan's"
    );
    assert_eq!(
        fixture.stored_hash("b.md"),
        Some(norn_fs::ContentHash::of(b"new\n").to_string())
    );
    heal_from_zero(&mut fixture.store, &fixture.vault, &fixture.exclusions).expect("a heal");
    fixture.assert_store_is_a_build_from_zero();
}

/// A document another writer puts back between the plan's removal and the
/// changeset is not recorded as removed: its row stands until the watcher
/// reports the new document, and once that is taken in the store is what a
/// build from zero holds.
#[test]
fn a_document_put_back_after_its_removal_is_not_committed_as_removed() {
    let mut fixture = Fixture::new(&[("gone.md", "# Gone\n"), ("b.md", "old\n")]);
    let plan = fixture.plan(vec![deleting("gone.md"), editing("b.md", "old", "new")]);
    applied(fixture.apply_meddled(plan, "gone.md", "# Back\n"));
    assert_eq!(fixture.read("gone.md").as_deref(), Some("# Back\n"));
    assert_eq!(
        fixture.stored_hash("gone.md"),
        Some(norn_fs::ContentHash::of(b"# Gone\n").to_string()),
        "no death is committed for a document that stands"
    );
    heal_from_zero(&mut fixture.store, &fixture.vault, &fixture.exclusions).expect("a heal");
    fixture.assert_store_is_a_build_from_zero();
}

/// A condition the plan carries on a file it does not write is checked at
/// apply: another writer's change to that file refuses the plan, naming the
/// condition, and nothing is published.
#[test]
fn a_plan_condition_another_writer_broke_refuses() {
    let mut fixture = Fixture::new(&[("a.md", "draft\n"), ("b.md", "seen\n")]);
    let seen = crate::planner::compose::content_hash(b"seen\n");
    let plan = fixture.plan(vec![editing("a.md", "draft", "final").with_conditions(
        vec![norn_wire::AuthorCondition::content_hash(
            path("b.md"),
            seen.clone(),
        )],
    )]);
    assert_eq!(
        plan.conditions,
        vec![norn_wire::PlanCondition::content_hash(
            path("b.md"),
            seen.clone()
        )]
    );
    fixture.foreign("b.md", "changed\n");
    let refused = refused(fixture.apply(plan));
    assert_eq!(
        refused.checks,
        vec![norn_wire::RefusedCheck::condition_failed(
            norn_wire::PlanCondition::content_hash(path("b.md"), seen)
        )]
    );
    assert_eq!(fixture.read("a.md").as_deref(), Some("draft\n"));
    assert!(fixture.recorded.calls.borrow().is_empty());
}

/// A plan that drops a condition its operations carry is refused: the
/// condition is what its author's operation depends on.
#[test]
fn a_plan_dropping_its_operations_condition_is_refused() {
    let mut fixture = Fixture::new(&[("a.md", "draft\n"), ("b.md", "seen\n")]);
    let seen = crate::planner::compose::content_hash(b"seen\n");
    let mut plan = fixture.plan(vec![editing("a.md", "draft", "final").with_conditions(
        vec![norn_wire::AuthorCondition::content_hash(path("b.md"), seen)],
    )]);
    plan.conditions.clear();
    assert_eq!(fixture.refuses_disagreeing(plan), vec![path("b.md")]);
}

/// A hash of another algorithm never reaches the applier: a resolved plan
/// carrying one is refused as it is read, so no hash is mapped to a stand-in
/// the kernel would compare a file against.
#[test]
fn a_plan_carrying_a_hash_of_another_algorithm_does_not_read() {
    let fixture = Fixture::new(&[("a.md", "draft\n")]);
    let plan = fixture.plan(vec![deleting("a.md")]);
    let mut json = serde_json::to_value(&plan).expect("a plan");
    let spelled = json["transitions"][0]["before"]["hash"].clone();
    assert!(
        spelled
            .as_str()
            .is_some_and(|hash| hash.starts_with("sha256:"))
    );
    assert!(serde_json::from_value::<ResolvedPlan>(json.clone()).is_ok());
    json["transitions"][0]["before"]["hash"] =
        serde_json::Value::String(format!("md5:{}", "0".repeat(32)));
    assert!(serde_json::from_value::<ResolvedPlan>(json.clone()).is_err());
    json["transitions"][0]["before"]["hash"] =
        serde_json::Value::String(format!("sha256:{}", "A".repeat(64)));
    assert!(serde_json::from_value::<ResolvedPlan>(json).is_err());
}

/// **An apply whose leg no longer stands when it would begin publishing
/// stops there, removes every shadow it staged and publishes nothing.** The
/// plan stages a create, an edit and a removal; the question publication
/// asks before its first target is answered no, as a teardown answers it.
/// No target is written, no shadow is left, no publication is recorded, and
/// the outcome carries the resolved plan.
#[test]
fn an_apply_stood_down_before_publication_removes_its_shadows_and_publishes_nothing() {
    let mut fixture = Fixture::new(&[("a.md", "# A\n"), ("c.md", "# C\n")]);
    let plan = fixture.plan(vec![
        creating("b.md", "# B\n"),
        editing("a.md", "# A", "# A2"),
        deleting("c.md"),
    ]);
    let before = fixture.tree();
    let applier = Applier {
        anchor: &fixture.vault,
        root: fixture.root,
        exclusions: &fixture.exclusions,
        shadows: &fixture.shadows,
        own_writes: &fixture.recorded,
        publishing: &|| false,
    };
    let outcome = applier.apply(plan.clone(), &mut fixture.store);
    match outcome {
        ApplyOutcome::StoodDown => {}
        other => panic!("the apply answered {other:?}"),
    }
    assert_eq!(fixture.tree(), before, "nothing was published");
    assert_eq!(fixture.read("a.md").as_deref(), Some("# A\n"));
    assert!(
        fixture.shadows_left().is_empty(),
        "{:?}",
        fixture.shadows_left()
    );
    assert!(fixture.recorded.calls.borrow().is_empty());
}

/// **A heal owed over a path the vault's spelling refuses heals the vault
/// whole.** A plan's paths are wire paths, and one no vault path normalizes
/// to names nothing the heal could read again; leaving it out would leave
/// the store believing whatever it held there, so the heal widens to the
/// whole vault, as it does where the root cannot be walked.
#[test]
fn a_heal_over_a_path_no_vault_path_normalizes_to_heals_the_vault_whole() {
    let path = DocumentPath::new("../outside.md").expect("a wire path");
    let plan = ResolvedPlan::new(
        VaultAddress::name(VaultName::new("notes").expect("a legal vault name")),
        RootIdentity::from_device_and_inode(1, 1),
        Vec::new(),
        vec![norn_wire::Transition::new(
            path,
            norn_wire::FileState::absent(),
            norn_wire::FileState::present(norn_wire::ContentHash::from_sha256([7; 32])),
        )],
        Vec::new(),
    );
    let outcome = ApplyOutcome::Applied(super::Applied {
        plan,
        changeset: norn_wire::ChangesetOutcome::Healing,
        targets: Vec::new(),
        folders_made: Vec::new(),
        folders_removed: Vec::new(),
    });
    let normalizer = norn_fs::PathNormalizer::for_sensitivity(norn_fs::CaseSensitivity::Sensitive);

    assert_eq!(
        outcome.heal(&normalizer),
        norn_fs::Batch::rescan(norn_fs::RescanScope::Vault)
    );
}

impl Fixture {
    /// Preview `plan` as the apply seam previews a resolved plan: the same
    /// plan and its forecast, or the envelope the preview answers with.
    fn preview(
        &mut self,
        plan: ResolvedPlan,
    ) -> Result<(ResolvedPlan, norn_wire::Forecast), norn_wire::ErrorEnvelope> {
        let declared = crate::production::pinned_declaration(&mut self.store).expect("a pin");
        super::preview(plan, &self.vault, self.root, &self.exclusions, &declared).map_err(
            |outcome| {
                outcome
                    .into_wire()
                    .expect("a preview never stands down")
                    .expect_err("a preview's refusal is no report")
            },
        )
    }
}

/// Close `closed` in `fixture`'s vault to this account, then preview and
/// apply the plan editing `sub/a.md`, and hold that the preview answers
/// exactly what the apply does: `vault/write-failed`, carrying the plan, with
/// nothing landed. Skipped where this account reads a mode-000 entry.
#[cfg(unix)]
fn previews_as_its_apply_answers_with(closed: &str) {
    use std::os::unix::fs::PermissionsExt;

    let mut fixture = Fixture::new(&[("sub/a.md", "# A\n")]);
    let plan = fixture.plan(vec![editing("sub/a.md", "# A", "# A2")]);
    let closed = fixture.vault.join(closed);
    let reopened = std::fs::metadata(&closed).unwrap().permissions();
    std::fs::set_permissions(&closed, std::fs::Permissions::from_mode(0o000)).unwrap();
    if std::fs::read_dir(&closed).is_ok() || std::fs::read(&closed).is_ok() {
        eprintln!("skipped: this account reads a mode-000 entry");
        std::fs::set_permissions(&closed, reopened).unwrap();
        return;
    }

    let previewed = fixture.preview(plan.clone());
    let applied = fixture
        .apply(plan.clone())
        .into_wire()
        .expect("a refusal is answered");
    std::fs::set_permissions(&closed, reopened).unwrap();

    let applied = applied.expect_err("an apply over an unreadable target is refused");
    assert_eq!(applied.code(), &norn_wire::ReasonCode::VaultWriteFailed);
    assert_eq!(
        previewed.expect_err("a preview over an unreadable target is refused"),
        applied
    );
    assert_eq!(fixture.read("sub/a.md").as_deref(), Some("# A\n"));
}

/// **A target whose bytes cannot be read while a resolved plan is checked is
/// answered by its preview as its apply answers it**: `vault/write-failed`
/// with the plan, since nothing landed and sending it again may apply it.
#[cfg(unix)]
#[test]
fn a_target_unreadable_while_a_resolved_plan_is_checked_previews_as_its_apply_answers() {
    previews_as_its_apply_answers_with("sub/a.md");
}

/// **A vault root that cannot be walked when a resolved plan is checked is
/// answered by its preview as its apply answers it**: both answer
/// `vault/write-failed` with the plan.
#[cfg(unix)]
#[test]
fn a_root_unwalkable_when_a_resolved_plan_is_checked_previews_as_its_apply_answers() {
    previews_as_its_apply_answers_with("");
}

/// One operation of every document-local kind, each over its own document of
/// `DOCUMENT_LOCAL_VAULT`.
fn one_of_each_document_local_kind() -> Vec<Operation> {
    use norn_wire::{AuthoredValue, WriteTarget};
    let at = |text: &str| WriteTarget::path(path(text));
    vec![
        Operation::new(OperationKind::set_frontmatter(
            at("set.md"),
            "status",
            AuthoredValue::string("done"),
        )),
        Operation::new(OperationKind::remove_frontmatter(at("remove.md"), "status")),
        Operation::new(OperationKind::push_frontmatter(
            at("push.md"),
            "tags",
            AuthoredValue::string("project"),
        )),
        Operation::new(OperationKind::pop_frontmatter(
            at("pop.md"),
            "tags",
            AuthoredValue::string("project"),
        )),
        Operation::new(OperationKind::replace_body(path("body.md"), "# New\n")),
        Operation::new(OperationKind::replace_section(
            path("replace.md"),
            "Tasks",
            "fresh\n",
        )),
        Operation::new(OperationKind::append_to_section(
            path("append.md"),
            "Tasks",
            "added",
        )),
        Operation::new(OperationKind::delete_section(path("delete.md"), "Tasks")),
        Operation::new(OperationKind::insert_before_heading(
            path("before.md"),
            "Tasks",
            "above",
        )),
        Operation::new(OperationKind::insert_after_heading(
            path("after.md"),
            "Tasks",
            "below",
        )),
    ]
}

const DOCUMENT_LOCAL_VAULT: &[(&str, &str)] = &[
    ("set.md", "---\nstatus: draft\n---\n# Set\n"),
    ("remove.md", "---\nstatus: draft\n---\n# Remove\n"),
    ("push.md", "---\ntags:\n  - project\n---\n# Push\n"),
    ("pop.md", "---\ntags:\n  - project\n  - other\n---\n# Pop\n"),
    ("body.md", "---\ntitle: Body\n---\n# Old\n"),
    ("replace.md", "# R\n\n## Tasks\n\none\n"),
    ("append.md", "# A\n\n## Tasks\n\none\n"),
    ("delete.md", "# D\n\n## Tasks\n\none\n\n## Keep\n"),
    ("before.md", "# B\n\n## Tasks\n\none\n"),
    ("after.md", "# F\n\n## Tasks\n\none\n"),
];

/// **Every document-local kind previews as its apply applies, and its
/// resolved plan sent again is landed**: the preview answers the plan its
/// operations resolved to, the apply writes that same plan, and the plan sent
/// once more finds every target at its after-state and writes nothing.
#[test]
fn each_document_local_kind_previews_as_it_applies_and_a_re_send_is_found() {
    for operation in one_of_each_document_local_kind() {
        let name = operation.kind.name();
        let mut fixture = Fixture::new(DOCUMENT_LOCAL_VAULT);
        let plan = fixture.plan(vec![operation]);
        assert_eq!(plan.transitions.len(), 1, "{name}");
        let (previewed, _) = fixture
            .preview(plan.clone())
            .unwrap_or_else(|refusal| panic!("{name} previews: {refusal:?}"));
        assert_eq!(previewed, plan, "{name}");
        let landed = applied(fixture.apply(plan.clone()));
        assert_eq!(landed.plan, previewed, "{name}");
        assert_eq!(
            landed.targets.iter().map(|t| t.result).collect::<Vec<_>>(),
            vec![TargetResult::Wrote],
            "{name}"
        );
        fixture.assert_store_is_a_build_from_zero();
        let again = applied(fixture.apply(plan));
        assert_eq!(
            again.targets.iter().map(|t| t.result).collect::<Vec<_>>(),
            vec![TargetResult::Found],
            "{name}"
        );
    }
}

/// **A resolved plan carrying an expected value applies as its conditions
/// were planned, and is found when sent again**: the condition on the
/// document it writes is that document's before-state, and the one on a
/// document it does not write travels as a condition on its content, which
/// another writer's change then breaks.
#[test]
fn a_plan_carrying_expected_values_applies_and_refuses_once_the_observed_document_changes() {
    use norn_wire::{AuthorCondition, AuthoredValue, ExpectedField, WriteTarget};
    let mut fixture = Fixture::new(&[
        ("a.md", "---\ntitle: A\n---\n"),
        ("c.md", "---\nstatus: ready\n---\n"),
    ]);
    let operations = vec![
        Operation::new(OperationKind::set_frontmatter(
            WriteTarget::path(path("a.md")),
            "status",
            AuthoredValue::string("new"),
        ))
        .with_conditions(vec![
            AuthorCondition::expected_value(path("a.md"), "status", ExpectedField::absent()),
            AuthorCondition::expected_value(
                path("c.md"),
                "status",
                ExpectedField::present(AuthoredValue::string("ready")),
            ),
        ]),
    ];
    let plan = fixture.plan(operations.clone());
    assert_eq!(plan.conditions.len(), 1);
    applied(fixture.apply(plan.clone()));
    assert_eq!(
        fixture.read("a.md").as_deref(),
        Some("---\ntitle: A\nstatus: new\n---\n")
    );
    let again = applied(fixture.apply(plan));
    assert_eq!(
        results(&again),
        vec![("a.md".to_string(), TargetResult::Found)]
    );

    let mut fixture = Fixture::new(&[
        ("a.md", "---\ntitle: A\n---\n"),
        ("c.md", "---\nstatus: ready\n---\n"),
    ]);
    let plan = fixture.plan(operations);
    fixture.foreign("c.md", "---\nstatus: held\n---\n");
    let refused = refused(fixture.apply(plan));
    assert!(
        matches!(
            refused.checks.as_slice(),
            [norn_wire::RefusedCheck::ConditionFailed { .. }]
        ),
        "{:?}",
        refused.checks
    );
    assert!(refused.plan.operations.is_empty());
    assert_eq!(refused.unresolved.len(), 1, "{:?}", refused.unresolved);
    assert_eq!(
        fixture.read("a.md").as_deref(),
        Some("---\ntitle: A\n---\n")
    );
}

/// **A resolved plan whose recorded state does not hold its own operation's
/// expected value is not what its operations do**: an expectation edited
/// after planning on the document it writes, or a plan dropping the content
/// condition an expectation on another document travels as, is
/// `request/plan-invalid` naming the document, and nothing is published.
#[test]
fn a_plan_whose_state_does_not_hold_its_expected_value_is_invalid() {
    use norn_wire::{AuthorCondition, AuthoredValue, ExpectedField, WriteTarget};
    let mut fixture = Fixture::new(&[
        ("a.md", "---\ntitle: A\n---\n"),
        ("c.md", "---\nstatus: ready\n---\n"),
    ]);
    let set = Operation::new(OperationKind::set_frontmatter(
        WriteTarget::path(path("a.md")),
        "status",
        AuthoredValue::string("new"),
    ));
    let mut edited = fixture.plan(vec![set.clone().with_conditions(vec![
        AuthorCondition::expected_value(path("a.md"), "status", ExpectedField::absent()),
    ])]);
    edited.operations[0].conditions = vec![AuthorCondition::expected_value(
        path("a.md"),
        "title",
        ExpectedField::present(AuthoredValue::string("B")),
    )];
    assert_eq!(fixture.refuses_disagreeing(edited), vec![path("a.md")]);
    let mut dropped = fixture.plan(vec![set.with_conditions(vec![
        AuthorCondition::expected_value(
            path("c.md"),
            "status",
            ExpectedField::present(AuthoredValue::string("ready")),
        ),
    ])]);
    dropped.conditions.clear();
    assert_eq!(fixture.refuses_disagreeing(dropped), vec![path("c.md")]);
}
