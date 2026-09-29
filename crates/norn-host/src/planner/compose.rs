//! Composition: each target's after-bytes from its before-bytes and the
//! operations touching it.

use std::collections::BTreeMap;
use std::sync::Arc;

use norn_wire::{ContentHash, DocumentPath, FileState, Operation, OperationKind};

use super::view::VaultView;

/// What composing a plan's operations came to.
pub(crate) struct Composition {
    /// One per file an operation touches.
    pub(crate) targets: BTreeMap<DocumentPath, ComposedTarget>,
    /// Each operation that did not resolve against the state it met.
    pub(crate) unresolvable: Vec<Unresolvable>,
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
        targets: vault.into_targets(),
        unresolvable,
    })
}

/// The files the plan touches, each as it stood before and as it stands so
/// far, over the view the before-states are read from.
struct Simulated<'view, V> {
    view: &'view V,
    files: BTreeMap<DocumentPath, ComposedTarget>,
}

/// Why one operation cannot act on the state it met.
type Unresolved = String;

impl<'view, V: VaultView> Simulated<'view, V> {
    fn over(view: &'view V) -> Self {
        Simulated {
            view,
            files: BTreeMap::new(),
        }
    }

    /// The file at `path` as it stands so far, reading its before-state the
    /// first time the plan touches it.
    fn file(&mut self, path: &DocumentPath) -> Result<&mut ComposedTarget, V::Error> {
        if !self.files.contains_key(path) {
            let bytes: Option<Arc<[u8]>> = self.view.file(path)?.map(Arc::from);
            let before = match &bytes {
                Some(bytes) => FileState::present(content_hash(bytes)),
                None => FileState::absent(),
            };
            self.files.insert(
                path.clone(),
                ComposedTarget {
                    before,
                    after: bytes,
                },
            );
        }
        Ok(self.files.get_mut(path).expect("inserted above"))
    }

    /// Act on `kind`, or say why it cannot act, leaving the state as it was.
    fn apply(&mut self, kind: &OperationKind) -> Result<Result<(), Unresolved>, V::Error> {
        Ok(match kind {
            OperationKind::CreateDocument { path, content } => {
                let file = self.file(path)?;
                if file.after.is_some() {
                    Err(format!(
                        "something stands at `{path}`, where the document would be created"
                    ))
                } else {
                    file.after = Some(Arc::from(content.as_bytes()));
                    Ok(())
                }
            }
            OperationKind::StrReplace {
                path,
                old_str,
                new_str,
            } => {
                let file = self.file(path)?;
                match &file.after {
                    None => Err(format!("no document stands at `{path}` to edit")),
                    Some(bytes) => replace_once(bytes, old_str, new_str).map(|replaced| {
                        file.after = Some(replaced);
                    }),
                }
            }
            OperationKind::MoveDocument { from, to } => {
                let Some(moved) = self.file(from)?.after.clone() else {
                    return Ok(Err(format!("no document stands at `{from}` to move")));
                };
                let destination = self.file(to)?;
                if destination.after.is_some() {
                    Err(format!(
                        "something stands at `{to}`, where the document would be moved"
                    ))
                } else {
                    destination.after = Some(moved);
                    self.file(from)?.after = None;
                    Ok(())
                }
            }
            OperationKind::DeleteDocument { path } => {
                let file = self.file(path)?;
                if file.after.take().is_some() {
                    Ok(())
                } else {
                    Err(format!("no document stands at `{path}` to delete"))
                }
            }
        })
    }

    fn into_targets(self) -> BTreeMap<DocumentPath, ComposedTarget> {
        self.files
    }
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

/// The wire's content hash of `bytes`: the filesystem layer's SHA-256, spelled
/// with its algorithm.
pub(crate) fn content_hash(bytes: &[u8]) -> ContentHash {
    ContentHash::new(format!(
        "sha256:{}",
        norn_fs::ContentHash::of(bytes).to_hex()
    ))
    .expect("a SHA-256 digest spells a content hash")
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
        assert!(detail.contains("something stands"), "{detail}");
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
        assert!(detail.contains("something stands"), "{detail}");
    }

    #[test]
    fn a_move_onto_itself_does_not_resolve() {
        let vault = MemoryVault::with(&[("a.md", "a")]);
        let detail = unresolvable_detail(
            &vault,
            Operation::new(OperationKind::move_document(path("a.md"), path("a.md"))),
        );
        assert!(detail.contains("something stands"), "{detail}");
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
}
