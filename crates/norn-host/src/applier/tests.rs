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
    _scratch: Scratch,
    pub(super) vault: PathBuf,
    data: PathBuf,
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
        Fixture::over(scratch, vault, data)
    }

    /// The fixture over a vault and data directory already on disk, with a
    /// store opened there and built from zero where it is new.
    pub(super) fn over(scratch: Scratch, vault: PathBuf, data: PathBuf) -> Fixture {
        let key = MaintainershipKey::new("test", "vault", "data").expect("a key");
        let shadows = ShadowHome::resolve(&vault, &data.join("tmp"), &key).expect("a shadow home");
        let exclusions = shadow_exclusions(&shadows, &vault);
        let root = norn_fs::path_identity(&vault)
            .expect("the root reads")
            .expect("the root stands");
        let order = order_of(&vault);
        let database = data.join("store.sqlite3");
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
