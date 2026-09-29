//! The forecast: what a plan does beyond its transitions, which is the
//! folders it makes and removes.

use std::collections::BTreeSet;

use norn_wire::{DocumentPath, FileState, FolderPath, Forecast, Transition};

use super::view::VaultView;

/// The forecast of a plan with `transitions`, over what `view` holds. No
/// target has drifted from a plan resolved from what the vault holds.
pub(crate) fn forecast<V: VaultView>(
    transitions: &[Transition],
    view: &V,
) -> Result<Forecast, V::Error> {
    let creates: Vec<&DocumentPath> = transitions
        .iter()
        .filter(|transition| is_create(transition))
        .map(|transition| &transition.path)
        .collect();
    let removals: BTreeSet<&str> = transitions
        .iter()
        .filter(|transition| is_removal(transition))
        .map(|transition| transition.path.as_str())
        .collect();
    let removed_paths = transitions
        .iter()
        .filter(|transition| is_removal(transition))
        .map(|transition| &transition.path);
    let mut made = BTreeSet::new();
    for path in &creates {
        for folder in folders_above(path) {
            if !made.contains(&folder) && !view.folder_stands(&folder)? {
                made.insert(folder);
            }
        }
    }
    let receiving: BTreeSet<FolderPath> = creates
        .iter()
        .flat_map(|path| folders_above(path))
        .collect();
    let mut candidates: Vec<FolderPath> = removed_paths
        .flat_map(folders_above)
        .filter(|folder| !receiving.contains(folder))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    // Deepest first, so a folder is judged after every folder inside it.
    candidates.sort_by_key(|folder| std::cmp::Reverse(folder.as_str().matches('/').count()));
    let mut removed: BTreeSet<FolderPath> = BTreeSet::new();
    for folder in candidates {
        let emptied = view.folder_entries(&folder)?.iter().all(|name| {
            let inside = format!("{}/{name}", folder.as_str());
            removals.contains(inside.as_str()) || removed.iter().any(|gone| gone.as_str() == inside)
        });
        if emptied {
            removed.insert(folder);
        }
    }
    Ok(Forecast::new(
        Vec::new(),
        made.into_iter().collect(),
        removed.into_iter().collect(),
    ))
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

/// Every folder above `path`, outermost first; none for a document at the
/// vault root.
fn folders_above(path: &DocumentPath) -> Vec<FolderPath> {
    let text = path.as_str();
    text.match_indices('/')
        .map(|(at, _)| FolderPath::new(&text[..at]).expect("a non-empty relative prefix"))
        .collect()
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
