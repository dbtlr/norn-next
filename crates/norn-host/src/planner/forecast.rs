//! The forecast: what a plan does beyond its transitions, which is the
//! folders it makes and removes.

use std::cmp::Reverse;
use std::collections::BTreeSet;
use std::path::Path;

use norn_fs::NormalizedPath;
use norn_wire::{DocumentPath, FileState, FolderPath, Forecast, Transition};

use super::view::VaultView;

/// The forecast of a plan with `transitions`, over what `view` holds. No
/// target has drifted from a plan resolved from what the vault holds.
///
/// Every folder is judged by its identity under the view's one rule, so a
/// folder the tree lists in another case is the folder a transition's path
/// runs through, and is named at the spelling the transition carries.
pub(crate) fn forecast<V: VaultView>(
    transitions: &[Transition],
    view: &V,
) -> Result<Forecast, V::Error> {
    let creates: Vec<&DocumentPath> = transitions
        .iter()
        .filter(|transition| is_create(transition))
        .map(|transition| &transition.path)
        .collect();
    let removals: Vec<&DocumentPath> = transitions
        .iter()
        .filter(|transition| is_removal(transition))
        .map(|transition| &transition.path)
        .collect();
    Ok(Forecast::new(
        Vec::new(),
        folders_made(&creates, view)?,
        folders_removed(&removals, &creates, view)?,
    ))
}

/// Every folder above a created document that does not stand: the folders
/// publication makes just before the create it makes them for.
fn folders_made<V: VaultView>(
    creates: &[&DocumentPath],
    view: &V,
) -> Result<Vec<FolderPath>, V::Error> {
    let mut made = BTreeSet::new();
    let mut judged = BTreeSet::new();
    for path in creates {
        for (identity, folder) in folders_above(path, view) {
            if judged.insert(identity.clone()) && !view.folder_stands(&identity)? {
                made.insert(folder);
            }
        }
    }
    Ok(made.into_iter().collect())
}

/// Every folder above a removed document that the plan leaves empty: every
/// entry it lists is a document the plan removes or a folder it empties, and
/// no create lands inside it. This is what the applier's emptying after the
/// removals takes, deepest first and upward until a folder is not empty.
fn folders_removed<V: VaultView>(
    removals: &[&DocumentPath],
    creates: &[&DocumentPath],
    view: &V,
) -> Result<Vec<FolderPath>, V::Error> {
    let receiving: BTreeSet<NormalizedPath> = creates
        .iter()
        .flat_map(|path| folders_above(path, view))
        .map(|(identity, _)| identity)
        .collect();
    let mut candidates: Vec<(NormalizedPath, FolderPath)> = removals
        .iter()
        .flat_map(|path| folders_above(path, view))
        .filter(|(identity, _)| !receiving.contains(identity))
        .collect::<std::collections::BTreeMap<_, _>>()
        .into_iter()
        .collect();
    // Deepest first, so a folder is judged after every folder inside it.
    candidates.sort_by_key(|(identity, _)| Reverse(identity.as_path().components().count()));
    // Every identity the plan leaves nothing at: its removals, then each
    // folder it empties as that folder is judged.
    let mut gone: BTreeSet<NormalizedPath> = removals
        .iter()
        .filter_map(|path| identity(Path::new(path.as_str()), view))
        .collect();
    let mut removed = BTreeSet::new();
    for (folder, spelled) in candidates {
        let mut emptied = true;
        for name in view.folder_names(&folder)? {
            let inside = identity(&folder.as_path().join(name), view);
            if !inside.is_some_and(|inside| gone.contains(&inside)) {
                emptied = false;
                break;
            }
        }
        if emptied {
            gone.insert(folder);
            removed.insert(spelled);
        }
    }
    Ok(removed.into_iter().collect())
}

/// `path`'s identity under the view's rule.
fn identity<V: VaultView>(path: &Path, view: &V) -> Option<NormalizedPath> {
    view.normalizer().normalize(path).ok()
}

/// Every folder above `path`, with its identity: none for a document at the
/// vault root.
fn folders_above<V: VaultView>(path: &DocumentPath, view: &V) -> Vec<(NormalizedPath, FolderPath)> {
    let Some(document) = identity(Path::new(path.as_str()), view) else {
        return Vec::new();
    };
    document
        .as_path()
        .ancestors()
        .skip(1)
        .filter(|above| !above.as_os_str().is_empty())
        .filter_map(|above| {
            let folder = FolderPath::new(above.to_str()?).ok()?;
            Some((identity(above, view)?, folder))
        })
        .collect()
}

/// Whether a transition puts a document where none stood.
fn is_create(transition: &Transition) -> bool {
    matches!(
        (&transition.before, &transition.after),
        (FileState::Absent {}, FileState::Present { .. })
    )
}

/// Whether a transition takes away a document that stood.
fn is_removal(transition: &Transition) -> bool {
    matches!(
        (&transition.before, &transition.after),
        (FileState::Present { .. }, FileState::Absent {})
    )
}

#[cfg(test)]
mod tests {
    use norn_wire::{DocumentPath, FileState, FolderPath};

    use super::super::compose::content_hash;
    use super::super::view::memory::MemoryVault;
    use super::*;

    fn path(text: &str) -> DocumentPath {
        DocumentPath::new(text).expect("a legal document path")
    }

    fn folders(texts: &[&str]) -> Vec<FolderPath> {
        texts
            .iter()
            .map(|text| FolderPath::new(text).expect("a legal folder path"))
            .collect()
    }

    fn present() -> FileState {
        FileState::present(content_hash(b"content"))
    }

    fn create(at: &str) -> Transition {
        Transition::new(path(at), FileState::absent(), present())
    }

    fn remove(at: &str) -> Transition {
        Transition::new(path(at), present(), FileState::absent())
    }

    fn of(vault: &MemoryVault, transitions: &[Transition]) -> Forecast {
        forecast(transitions, vault).expect("an infallible view")
    }

    #[test]
    fn a_create_makes_every_folder_above_it_that_does_not_stand() {
        let vault = MemoryVault::with(&[("notes/a.md", "a")]);
        let forecast = of(&vault, &[create("notes/2024/may/b.md")]);
        assert_eq!(
            forecast.folders_made,
            folders(&["notes/2024", "notes/2024/may"])
        );
        assert!(forecast.folders_removed.is_empty());
        assert!(forecast.drifted.is_empty());
    }

    #[test]
    fn a_removal_takes_every_folder_above_it_that_it_leaves_empty() {
        let vault = MemoryVault::with(&[("keep.md", "k"), ("old/sub/x.md", "x")]);
        let forecast = of(&vault, &[remove("old/sub/x.md")]);
        assert_eq!(forecast.folders_removed, folders(&["old", "old/sub"]));
        assert!(forecast.folders_made.is_empty());
    }

    #[test]
    fn a_folder_holding_anything_the_plan_does_not_remove_stays() {
        let vault = MemoryVault::with(&[
            ("a/x.md", "x"),
            ("a/y.md", "y"),
            ("b/x.md", "x"),
            ("c/x.md", "x"),
        ])
        .with_empty_folder("b/empty");
        let forecast = of(
            &vault,
            &[remove("a/x.md"), remove("b/x.md"), remove("c/x.md")],
        );
        assert_eq!(forecast.folders_removed, folders(&["c"]));
    }

    #[test]
    fn a_folder_a_create_lands_in_is_not_removed() {
        let vault = MemoryVault::with(&[("old/a.md", "a")]);
        let forecast = of(&vault, &[remove("old/a.md"), create("old/b.md")]);
        assert!(forecast.folders_removed.is_empty());
        assert!(forecast.folders_made.is_empty());
    }
}
