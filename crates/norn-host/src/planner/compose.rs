//! Composition: each target's after-bytes from its before-bytes and the
//! operations touching it.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use norn_fs::NormalizedPath;
use norn_wire::{ContentHash, DocumentPath, FileState, Operation, OperationKind};

use super::view::{Entry, VaultView, document_path, wire_hash};

/// What composing a plan's operations came to.
pub(crate) struct Composition {
    /// One per file an operation touches, by the spelling the plan writes it
    /// at.
    pub(crate) targets: BTreeMap<DocumentPath, ComposedTarget>,
    /// The spelling each identity the plan touches was read at, whose target
    /// carries what the vault held there before the plan.
    pub(crate) read_at: BTreeMap<NormalizedPath, DocumentPath>,
    /// Each operation that did not resolve against the state it met.
    pub(crate) unresolvable: Vec<Unresolvable>,
}

impl Composition {
    /// What the vault held at `identity` before the plan, where the plan
    /// touches it.
    pub(crate) fn before(&self, identity: &NormalizedPath) -> Option<&FileState> {
        let spelling = self.read_at.get(identity)?;
        Some(&self.targets[spelling].before)
    }
}

/// One file's two sides.
pub(crate) struct ComposedTarget {
    /// What the file held before the plan.
    pub(crate) before: FileState,
    /// What it holds after, `None` where it is absent.
    pub(crate) after: Option<Arc<[u8]>>,
}

/// An operation that met a state it cannot act on.
#[derive(Debug, Eq, PartialEq)]
pub(crate) struct Unresolvable {
    /// Its position in the plan's operation list.
    pub(crate) position: usize,
    /// Why, in words.
    pub(crate) detail: String,
}

/// Compose `operations`, taken in `order`, over what `view` holds.
pub(crate) fn compose<V: VaultView>(
    operations: &[Operation],
    order: &[usize],
    view: &V,
) -> Result<Composition, V::Error> {
    let mut vault = Simulated::over(view);
    let mut unresolvable = Vec::new();
    for &position in order {
        if let Err(detail) = vault.apply(&operations[position].kind)? {
            unresolvable.push(Unresolvable { position, detail });
        }
    }
    Ok(Composition {
        targets: vault.targets,
        read_at: vault.read_at,
        unresolvable,
    })
}

/// The files the plan touches, each as it stood before and as it stands so
/// far, over the view the before-states are read from.
///
/// **A file is its identity.** Every name an operation carries is normalized
/// by the view's one rule, so two spellings of one file — `a//b.md` and
/// `a/b.md`, or on a root that folds case `A.md` and `a.md` — are one file,
/// held at the spelling the tree lists. The one act that gives an identity a
/// second spelling is a case-only rename, which writes the file at its new
/// spelling and takes it away at its old one.
struct Simulated<'view, V> {
    view: &'view V,
    targets: BTreeMap<DocumentPath, ComposedTarget>,
    /// The spelling each identity was first read at.
    read_at: BTreeMap<NormalizedPath, DocumentPath>,
    /// The spelling each identity stands at now: where it was read, or where a
    /// case-only rename moved it.
    spelled: BTreeMap<NormalizedPath, DocumentPath>,
    /// How many documents stand so far beneath each folder identity, so
    /// whether a name is a folder the plan makes is one lookup however large
    /// the plan.
    standing_below: BTreeMap<NormalizedPath, usize>,
    /// The spelling each folder identity was first given by a document
    /// standing beneath it: the tree's, or the one an operation made it at.
    /// Publication makes a folder before the create it is made for and
    /// removes none until the end, so a folder keeps that spelling for the
    /// whole plan. The spelling is kept even when a later operation of the
    /// plan removes the document that gave it, so no publication makes the
    /// folder: a second spelling is then refused where it could have stood,
    /// which costs the author a re-plan and never a wrong transition.
    folder_spelled: BTreeMap<NormalizedPath, PathBuf>,
}

/// Why one operation cannot act on the state it met.
type Unresolved = String;

/// Where one name an operation carries leads.
enum Place {
    /// A file the plan composes, at the spelling it is written at.
    File(NormalizedPath, DocumentPath),
    /// A place no document is read from or made at, and why.
    NoFile(Unresolved),
}

impl<'view, V: VaultView> Simulated<'view, V> {
    fn over(view: &'view V) -> Self {
        Simulated {
            view,
            targets: BTreeMap::new(),
            read_at: BTreeMap::new(),
            spelled: BTreeMap::new(),
            standing_below: BTreeMap::new(),
            folder_spelled: BTreeMap::new(),
        }
    }

    /// Where `path` leads, reading its before-state the first time the plan
    /// touches its identity.
    fn place(&mut self, path: &DocumentPath) -> Result<Place, V::Error> {
        let identity = match self.view.normalizer().normalize(Path::new(path.as_str())) {
            Ok(identity) => identity,
            Err(error) => {
                return Ok(Place::NoFile(format!(
                    "`{path}` names no document in the vault: {error}"
                )));
            }
        };
        if let Some(spelling) = self.spelled.get(&identity) {
            return Ok(Place::File(identity, spelling.clone()));
        }
        let (spelling, before, after) = match self.view.entry(&identity)? {
            Entry::Document { at, bytes, hash } => (at, FileState::present(hash), Some(bytes)),
            Entry::Absent { at } => (at, FileState::absent(), None),
            Entry::Folder => {
                return Ok(Place::NoFile(format!(
                    "a folder stands at `{path}`, where a document would be"
                )));
            }
            Entry::Blocked { detail } => return Ok(Place::NoFile(detail)),
        };
        self.targets.insert(
            spelling.clone(),
            ComposedTarget {
                before,
                after: None,
            },
        );
        self.set_after(&spelling, after);
        self.read_at.insert(identity.clone(), spelling.clone());
        self.spelled.insert(identity.clone(), spelling.clone());
        Ok(Place::File(identity, spelling))
    }

    /// The document standing so far at the file `path` leads to, or why none
    /// does.
    fn standing(
        &mut self,
        path: &DocumentPath,
    ) -> Result<Result<DocumentPath, Unresolved>, V::Error> {
        Ok(match self.place(path)? {
            Place::NoFile(detail) => Err(detail),
            Place::File(_, spelling) if self.targets[&spelling].after.is_some() => Ok(spelling),
            Place::File(..) => Err(format!("no document stands at `{path}`")),
        })
    }

    /// The file a document can be put at for `path`, or why none can: the
    /// name holds a document, a folder the plan makes, or lies beneath a
    /// document; or `path` spells a folder above it differently from the tree
    /// or from the operation that made it.
    fn vacant(
        &mut self,
        path: &DocumentPath,
    ) -> Result<Result<DocumentPath, Unresolved>, V::Error> {
        let (identity, spelling) = match self.place(path)? {
            Place::NoFile(detail) => return Ok(Err(detail)),
            Place::File(identity, spelling) => (identity, spelling),
        };
        if self.targets[&spelling].after.is_some() {
            return Ok(Err(format!("a document stands at `{path}`")));
        }
        if let Some(above) = self.document_above(&identity) {
            return Ok(Err(format!(
                "`{path}` lies beneath the document the plan puts at `{above}`"
            )));
        }
        if self
            .standing_below
            .get(&identity)
            .is_some_and(|&count| count > 0)
        {
            return Ok(Err(format!(
                "a folder stands at `{path}`, where the plan puts a document beneath it"
            )));
        }
        if let Some(made) = self.folder_spelled_otherwise(&identity) {
            return Ok(Err(format!(
                "`{path}` runs through the folder the plan puts at `{}`: a document is put at the spelling its folder already has, and a folder's change of case is not planned",
                made.display()
            )));
        }
        if spelling.as_str() != spelled_as_asked(&identity) {
            return Ok(Err(format!(
                "`{path}` is spelled `{spelling}` in the vault: a document is put at the spelling the vault lists, and a folder's change of case is not planned"
            )));
        }
        Ok(Ok(spelling))
    }

    /// The document standing so far at a folder above `identity`.
    fn document_above(&self, identity: &NormalizedPath) -> Option<&DocumentPath> {
        identity
            .as_path()
            .ancestors()
            .skip(1)
            .filter(|above| !above.as_os_str().is_empty())
            .filter_map(|above| self.view.normalizer().normalize(above).ok())
            .filter_map(|above| self.spelled.get(&above))
            .find(|spelling| self.targets[*spelling].after.is_some())
    }

    /// The spelling of a folder above `identity` that this plan has given
    /// another spelling than `identity` asks for.
    fn folder_spelled_otherwise(&self, identity: &NormalizedPath) -> Option<&PathBuf> {
        identity
            .as_path()
            .ancestors()
            .skip(1)
            .filter(|above| !above.as_os_str().is_empty())
            .find_map(|above| {
                let folder = self.view.normalizer().normalize(above).ok()?;
                self.folder_spelled
                    .get(&folder)
                    .filter(|made| made.as_path() != above)
            })
    }

    /// Set what the file at `spelling` holds so far, counting it beneath
    /// every folder above it while a document stands there.
    fn set_after(&mut self, spelling: &DocumentPath, after: Option<Arc<[u8]>>) {
        let target = self.target(spelling);
        let change = match (target.after.is_some(), after.is_some()) {
            (false, true) => Some(true),
            (true, false) => Some(false),
            _ => None,
        };
        target.after = after;
        let Some(arrives) = change else {
            return;
        };
        for above in Path::new(spelling.as_str()).ancestors().skip(1) {
            if above.as_os_str().is_empty() {
                break;
            }
            let Ok(folder) = self.view.normalizer().normalize(above) else {
                continue;
            };
            if arrives {
                self.folder_spelled
                    .entry(folder.clone())
                    .or_insert_with(|| above.to_owned());
            }
            let count = self.standing_below.entry(folder).or_default();
            if arrives {
                *count += 1;
            } else {
                *count -= 1;
            }
        }
    }

    /// Act on `kind`, or say why it cannot act, leaving the state as it was.
    fn apply(&mut self, kind: &OperationKind) -> Result<Result<(), Unresolved>, V::Error> {
        Ok(match kind {
            OperationKind::CreateDocument { path, content } => self.vacant(path)?.map(|spelling| {
                self.set_after(&spelling, Some(Arc::from(content.as_bytes())));
            }),
            OperationKind::StrReplace {
                path,
                old_str,
                new_str,
            } => match self.standing(path)? {
                Err(detail) => Err(detail),
                Ok(spelling) => {
                    let file = self.target(&spelling);
                    let bytes = file.after.as_ref().expect("a document stands");
                    replace_once(bytes, old_str, new_str).map(|replaced| {
                        file.after = Some(replaced);
                    })
                }
            },
            OperationKind::MoveDocument { from, to } => self.move_document(from, to)?,
            OperationKind::DeleteDocument { path } => self.standing(path)?.map(|spelling| {
                self.set_after(&spelling, None);
            }),
        })
    }

    /// Move the document at `from` to `to`.
    ///
    /// **A case-only rename is a move.** On a root that folds case a
    /// destination differing from its source only in case names the source
    /// itself (ADR 0031), so the destination is not an occupied name: the
    /// document is written at the new spelling and taken away at the old, two
    /// transitions the applier publishes as one respell. A destination whose
    /// spelling is the source's own is a move onto itself, which names no
    /// change.
    fn move_document(
        &mut self,
        from: &DocumentPath,
        to: &DocumentPath,
    ) -> Result<Result<(), Unresolved>, V::Error> {
        let source = match self.standing(from)? {
            Ok(source) => source,
            Err(detail) => return Ok(Err(detail)),
        };
        let identity = |path: &DocumentPath| {
            self.view
                .normalizer()
                .normalize(Path::new(path.as_str()))
                .ok()
        };
        let (from_identity, to_identity) = (identity(from), identity(to));
        let destination = if let Some(to_identity) = to_identity
            && Some(&to_identity) == from_identity.as_ref()
        {
            let respelled = spelled_as_asked(&to_identity);
            if respelled == source.as_str() {
                return Ok(Err(format!("`{from}` would be moved onto itself")));
            }
            if Path::new(&respelled).parent() != Path::new(source.as_str()).parent() {
                return Ok(Err(format!(
                    "`{to}` differs from `{source}` in the case of a folder, and a folder's change of case is not planned"
                )));
            }
            let respelled = DocumentPath::new(&respelled).expect("a normalized document path");
            self.targets
                .entry(respelled.clone())
                .or_insert(ComposedTarget {
                    before: FileState::absent(),
                    after: None,
                });
            self.spelled.insert(to_identity, respelled.clone());
            respelled
        } else {
            match self.vacant(to)? {
                Ok(destination) => destination,
                Err(detail) => return Ok(Err(detail)),
            }
        };
        let moved = self.target(&source).after.clone();
        self.set_after(&source, None);
        self.set_after(&destination, moved);
        Ok(Ok(()))
    }

    fn target(&mut self, spelling: &DocumentPath) -> &mut ComposedTarget {
        self.targets
            .get_mut(spelling)
            .expect("a placed file has a target")
    }
}

/// `identity` at the spelling its operation asked for, normalized.
fn spelled_as_asked(identity: &NormalizedPath) -> String {
    document_path(identity.as_path())
        .map(|path| path.as_str().to_string())
        .unwrap_or_default()
}

/// The files an operation touches: a move touches its source and its
/// destination, every other kind the one file it names.
pub(crate) fn touches(kind: &OperationKind) -> impl Iterator<Item = &DocumentPath> {
    let (first, second) = match kind {
        OperationKind::CreateDocument { path, .. }
        | OperationKind::StrReplace { path, .. }
        | OperationKind::DeleteDocument { path } => (path, None),
        OperationKind::MoveDocument { from, to } => (from, Some(to)),
    };
    std::iter::once(first).chain(second)
}

/// `bytes` with the one occurrence of `old` replaced by `new`.
///
/// **Exactly one occurrence, counted at every offset.** An edit's anchor names
/// one place: text found nowhere, or at more than one offset — overlapping
/// offsets included — names none, and so does the empty text, which occurs at
/// every offset. The search is over bytes, so a document that is not UTF-8 is
/// edited where the anchor's bytes occur.
fn replace_once(bytes: &[u8], old: &str, new: &str) -> Result<Arc<[u8]>, Unresolved> {
    let needle = old.as_bytes();
    if needle.is_empty() {
        return Err("an edit's text is empty, so it names no one place".to_string());
    }
    let mut offsets = bytes
        .windows(needle.len())
        .enumerate()
        .filter(|(_, window)| *window == needle)
        .map(|(offset, _)| offset);
    let Some(at) = offsets.next() else {
        return Err(format!("the text `{old}` no longer occurs in the document"));
    };
    if offsets.next().is_some() {
        return Err(format!(
            "the text `{old}` occurs more than once in the document"
        ));
    }
    let mut replaced = Vec::with_capacity(bytes.len() - old.len() + new.len());
    replaced.extend_from_slice(&bytes[..at]);
    replaced.extend_from_slice(new.as_bytes());
    replaced.extend_from_slice(&bytes[at + old.len()..]);
    Ok(Arc::from(replaced))
}

/// The wire's content hash of `bytes`.
pub(crate) fn content_hash(bytes: &[u8]) -> ContentHash {
    wire_hash(norn_fs::ContentHash::of(bytes))
}

#[cfg(test)]
mod tests {
    use super::super::view::memory::MemoryVault;
    use super::*;

    pub(crate) fn path(text: &str) -> DocumentPath {
        DocumentPath::new(text).expect("a legal document path")
    }

    fn after_text(composition: &Composition, at: &str) -> Option<String> {
        let target = composition.targets.get(&path(at)).expect("a target");
        target
            .after
            .as_ref()
            .map(|bytes| String::from_utf8(bytes.to_vec()).expect("utf-8"))
    }

    fn in_order(operations: &[Operation]) -> Vec<usize> {
        (0..operations.len()).collect()
    }

    #[test]
    fn a_create_composes_its_content_over_an_absent_file() {
        let vault = MemoryVault::default();
        let operations = [Operation::new(OperationKind::create_document(
            path("notes/new.md"),
            "hello",
        ))];
        let composition =
            compose(&operations, &in_order(&operations), &vault).expect("an infallible view");
        assert!(composition.unresolvable.is_empty());
        assert_eq!(
            composition.targets[&path("notes/new.md")].before,
            FileState::absent()
        );
        assert_eq!(
            after_text(&composition, "notes/new.md").as_deref(),
            Some("hello")
        );
    }

    #[test]
    fn a_str_replace_replaces_the_one_occurrence_of_its_text() {
        let vault = MemoryVault::with(&[("a.md", "status: draft\nbody\n")]);
        let operations = [Operation::new(OperationKind::str_replace(
            path("a.md"),
            "draft",
            "final",
        ))];
        let composition =
            compose(&operations, &in_order(&operations), &vault).expect("an infallible view");
        assert!(composition.unresolvable.is_empty());
        assert_eq!(
            composition.targets[&path("a.md")].before,
            FileState::present(content_hash(b"status: draft\nbody\n"))
        );
        assert_eq!(
            after_text(&composition, "a.md").as_deref(),
            Some("status: final\nbody\n")
        );
    }

    fn unresolvable_detail(vault: &MemoryVault, operation: Operation) -> String {
        let operations = [operation];
        let composition =
            compose(&operations, &in_order(&operations), vault).expect("an infallible view");
        let [
            Unresolvable {
                position: 0,
                detail,
            },
        ] = &composition.unresolvable[..]
        else {
            panic!("the one operation is unresolvable");
        };
        detail.clone()
    }

    #[test]
    fn a_str_replace_whose_text_is_gone_does_not_resolve() {
        let vault = MemoryVault::with(&[("a.md", "status: final\n")]);
        let detail = unresolvable_detail(
            &vault,
            Operation::new(OperationKind::str_replace(path("a.md"), "draft", "final")),
        );
        assert!(detail.contains("no longer occurs"), "{detail}");
    }

    #[test]
    fn a_str_replace_whose_text_occurs_twice_does_not_resolve() {
        let vault = MemoryVault::with(&[("a.md", "draft and draft\n")]);
        let detail = unresolvable_detail(
            &vault,
            Operation::new(OperationKind::str_replace(path("a.md"), "draft", "final")),
        );
        assert!(detail.contains("more than once"), "{detail}");
    }

    #[test]
    fn overlapping_occurrences_are_more_than_one() {
        let vault = MemoryVault::with(&[("a.md", "aaa")]);
        let detail = unresolvable_detail(
            &vault,
            Operation::new(OperationKind::str_replace(path("a.md"), "aa", "b")),
        );
        assert!(detail.contains("more than once"), "{detail}");
    }

    #[test]
    fn an_empty_text_names_no_place_even_in_an_empty_document() {
        let vault = MemoryVault::with(&[("a.md", "")]);
        let detail = unresolvable_detail(
            &vault,
            Operation::new(OperationKind::str_replace(path("a.md"), "", "text")),
        );
        assert!(detail.contains("empty"), "{detail}");
    }

    #[test]
    fn a_str_replace_on_an_absent_document_does_not_resolve() {
        let vault = MemoryVault::default();
        let detail = unresolvable_detail(
            &vault,
            Operation::new(OperationKind::str_replace(path("a.md"), "x", "y")),
        );
        assert!(detail.contains("no document"), "{detail}");
    }

    #[test]
    fn a_create_over_a_standing_document_does_not_resolve() {
        let vault = MemoryVault::with(&[("a.md", "here")]);
        let detail = unresolvable_detail(
            &vault,
            Operation::new(OperationKind::create_document(path("a.md"), "new")),
        );
        assert!(detail.contains("a document stands"), "{detail}");
    }

    #[test]
    fn a_delete_leaves_its_document_absent() {
        let vault = MemoryVault::with(&[("a.md", "gone")]);
        let operations = [Operation::new(OperationKind::delete_document(path("a.md")))];
        let composition =
            compose(&operations, &in_order(&operations), &vault).expect("an infallible view");
        assert!(composition.unresolvable.is_empty());
        assert_eq!(
            composition.targets[&path("a.md")].before,
            FileState::present(content_hash(b"gone"))
        );
        assert_eq!(after_text(&composition, "a.md"), None);
    }

    #[test]
    fn a_delete_of_an_absent_document_does_not_resolve() {
        let detail = unresolvable_detail(
            &MemoryVault::default(),
            Operation::new(OperationKind::delete_document(path("a.md"))),
        );
        assert!(detail.contains("no document"), "{detail}");
    }

    #[test]
    fn a_move_creates_its_destination_from_its_source_and_removes_the_source() {
        let vault = MemoryVault::with(&[("a.md", "moved")]);
        let operations = [Operation::new(OperationKind::move_document(
            path("a.md"),
            path("archive/a.md"),
        ))];
        let composition =
            compose(&operations, &in_order(&operations), &vault).expect("an infallible view");
        assert!(composition.unresolvable.is_empty());
        assert_eq!(after_text(&composition, "a.md"), None);
        assert_eq!(
            composition.targets[&path("archive/a.md")].before,
            FileState::absent()
        );
        assert_eq!(
            after_text(&composition, "archive/a.md").as_deref(),
            Some("moved")
        );
    }

    #[test]
    fn a_move_onto_a_standing_document_does_not_resolve() {
        let vault = MemoryVault::with(&[("a.md", "a"), ("b.md", "b")]);
        let detail = unresolvable_detail(
            &vault,
            Operation::new(OperationKind::move_document(path("a.md"), path("b.md"))),
        );
        assert!(detail.contains("a document stands"), "{detail}");
    }

    #[test]
    fn a_move_onto_itself_does_not_resolve() {
        let vault = MemoryVault::with(&[("a.md", "a")]);
        let detail = unresolvable_detail(
            &vault,
            Operation::new(OperationKind::move_document(path("a.md"), path("a.md"))),
        );
        assert!(detail.contains("onto itself"), "{detail}");
    }

    #[test]
    fn a_move_from_an_absent_document_does_not_resolve() {
        let detail = unresolvable_detail(
            &MemoryVault::default(),
            Operation::new(OperationKind::move_document(path("a.md"), path("b.md"))),
        );
        assert!(detail.contains("no document"), "{detail}");
    }

    #[test]
    fn operations_on_one_file_compose_in_order() {
        let vault = MemoryVault::with(&[("a.md", "one two")]);
        let operations = [
            Operation::new(OperationKind::str_replace(path("a.md"), "one", "three")),
            Operation::new(OperationKind::move_document(path("a.md"), path("b.md"))),
            Operation::new(OperationKind::str_replace(
                path("b.md"),
                "three two",
                "four",
            )),
        ];
        let composition =
            compose(&operations, &in_order(&operations), &vault).expect("an infallible view");
        assert!(composition.unresolvable.is_empty());
        assert_eq!(after_text(&composition, "a.md"), None);
        assert_eq!(after_text(&composition, "b.md").as_deref(), Some("four"));
    }

    #[test]
    fn an_operation_that_does_not_resolve_leaves_the_state_it_met() {
        let vault = MemoryVault::with(&[("a.md", "one")]);
        let operations = [
            Operation::new(OperationKind::str_replace(path("a.md"), "absent", "x")),
            Operation::new(OperationKind::str_replace(path("a.md"), "one", "two")),
        ];
        let composition =
            compose(&operations, &in_order(&operations), &vault).expect("an infallible view");
        assert_eq!(composition.unresolvable.len(), 1);
        assert_eq!(after_text(&composition, "a.md").as_deref(), Some("two"));
    }

    #[test]
    fn a_path_in_a_second_spelling_is_the_file_its_one_spelling_names() {
        let vault = MemoryVault::with(&[("a/b.md", "b")]);
        for spelling in ["a//b.md", "./a/b.md", "a/./b.md", "a/b.md/"] {
            let operations = [Operation::new(OperationKind::delete_document(path(
                spelling,
            )))];
            let composition =
                compose(&operations, &in_order(&operations), &vault).expect("an infallible view");
            assert!(composition.unresolvable.is_empty(), "{spelling}");
            assert_eq!(
                composition.targets.keys().collect::<Vec<_>>(),
                vec![&path("a/b.md")],
                "{spelling}"
            );
        }
    }

    #[test]
    fn a_path_climbing_out_through_a_parent_name_does_not_resolve() {
        let vault = MemoryVault::with(&[("a/b.md", "b")]);
        let detail = unresolvable_detail(
            &vault,
            Operation::new(OperationKind::delete_document(path("x/../a/b.md"))),
        );
        assert!(detail.contains("names no document"), "{detail}");
    }

    #[test]
    fn two_spellings_of_one_file_compose_as_one_target() {
        let vault = MemoryVault::with(&[("a/b.md", "one two")]);
        let operations = [
            Operation::new(OperationKind::str_replace(path("a/b.md"), "one", "1")),
            Operation::new(OperationKind::str_replace(path("a//b.md"), "two", "2")),
        ];
        let composition =
            compose(&operations, &in_order(&operations), &vault).expect("an infallible view");
        assert!(composition.unresolvable.is_empty());
        assert_eq!(composition.targets.len(), 1);
        assert_eq!(after_text(&composition, "a/b.md").as_deref(), Some("1 2"));
    }
}
