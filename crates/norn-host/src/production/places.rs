//! The spellings of one rendered place, read where a scoped leg meets one.
//!
//! A rendering names a place and not an identity, and several spellings can
//! render onto one place. The first spelling finding a flush records at a place
//! replaces every spelling finding standing there, so a leg that files only the
//! spelling it was told about takes the findings of the others; and a leg told
//! that a spelling left has nothing it can conclude about the place until it
//! knows whether any other spelling still renders there. This is how a scoped
//! leg answers both: it reads every spelling of the place, files each, and
//! ends the place's spelling findings where it finds none.
//!
//! **What a reading costs is the folders along the place's path.** The
//! segments above the first marked one are spelled as the vault spells them
//! ([`DocumentPath::unrendered_ancestor`]), a segment with no marker below it
//! is one name, and each marked segment lists one folder per candidate — so a
//! reading never walks a subtree, and a wide sibling folder is read by nothing.
//! A job lists each folder once and keeps its marked names by the segment they
//! render to, so a reading looks its candidates up rather than scanning the
//! folder: many refused names in one folder cost one listing and one lookup
//! per place, never one comparison per name per place.

use std::ffi::OsString;
use std::ops::ControlFlow;

use norn_store::{RENDERED_MARKER, rendered_folder_name};

use super::*;

/// One vault spelling rendering onto a place, and why it names no document.
pub(super) struct Spelling {
    pub(super) path: PathBuf,
    pub(super) quarantine: Quarantine,
}

/// The places one job has read and the folders those readings listed.
///
/// **A place is read once per job.** Every spelling of it is filed by the
/// first reading, and a second would file each of them again beside the first,
/// since only the first finding a flush records at a place replaces what stood
/// there.
#[derive(Default)]
pub(super) struct PlaceReadings {
    read: BTreeSet<DocumentPath>,
    /// Per folder listed, the names in it whose rendering carries the marker —
    /// the only names a marked segment can be — or `None` where the folder
    /// could not be listed. Unmarked names are never kept, so what this holds
    /// follows the vault's refused spellings rather than its width.
    folders: BTreeMap<PathBuf, Option<MarkedNames>>,
}

/// One folder's marked names, keyed by the segment each renders to: once as a
/// folder above a place, once as the place's own leaf. The two differ only for
/// a name whose stem the grammar refuses, which only a leaf reads.
#[derive(Default)]
struct MarkedNames {
    as_folder: BTreeMap<String, Vec<OsString>>,
    as_leaf: BTreeMap<String, Vec<OsString>>,
}

impl MarkedNames {
    fn rendering_to(&self, segment: &str, leaf: bool) -> &[OsString] {
        let names = if leaf { &self.as_leaf } else { &self.as_folder };
        names.get(segment).map_or(&[], Vec::as_slice)
    }
}

impl PlaceReadings {
    /// Whether `place` is one this job has not read yet, marking it read.
    pub(super) fn first_reading(&mut self, place: &DocumentPath) -> bool {
        self.read.insert(place.clone())
    }

    /// Every spelling in the vault that renders onto `place` and names a
    /// Markdown file the vault walk would quarantine, or `None` where it found
    /// none and a folder on the way hid some.
    ///
    /// **A hidden folder is passed over as the vault heal passes over it.**
    /// A heal files every spelling it reaches at the place, and the first of
    /// them replaces what stood there, so a spelling beneath a folder it could
    /// not list or search is stated nowhere — as in a build from zero, which
    /// marks that folder instead. Where the reading reaches no spelling at all
    /// and a folder hid some, it concludes nothing, as a heal that files
    /// nothing at the place takes nothing there: the place keeps what stands
    /// until the folder reads again.
    pub(super) fn spellings(
        &mut self,
        vault: &norn_fs::Vault,
        place: &DocumentPath,
    ) -> Result<Option<Vec<Spelling>>, JobFailure> {
        let segments: Vec<&str> = place.as_str().split('/').collect();
        let ancestor = place.unrendered_ancestor();
        let spelled = if ancestor.is_empty() {
            0
        } else {
            ancestor.split('/').count()
        };
        let mut candidates = vec![PathBuf::from(ancestor)];
        let mut hidden = false;
        for (index, segment) in segments.iter().enumerate().skip(spelled) {
            let leaf = index + 1 == segments.len();
            let mut next = Vec::new();
            for folder in &candidates {
                // A segment without the marker is one name, spelled as written:
                // the reach at the leaf is what says whether it is there.
                if !segment.contains(RENDERED_MARKER) {
                    next.push(folder.join(segment));
                    continue;
                }
                let Some(names) = self.marked_names(vault, folder) else {
                    hidden = true;
                    continue;
                };
                // A rendering is its folders' renderings above its leaf's, so a
                // name under a folder that renders onto the place's ancestors
                // renders onto the place exactly when its own segment does.
                for name in names.rendering_to(segment, leaf) {
                    count_place_candidate();
                    next.push(folder.join(name));
                }
            }
            candidates = next;
        }
        let mut spellings = Vec::new();
        for candidate in candidates {
            // The reach decides by spelling alone and opens nothing, which is
            // all a spelling finding is concluded by: an excluded root, a shadow
            // or a link on the way is a name the vault walk never yields, and a
            // name that is not a regular file is no document either.
            let at = match vault.reach_or_unreadable(&candidate).map_err(effect)? {
                Ok(norn_fs::Reach::Stands {
                    kind: norn_fs::PathKind::RegularFile,
                    at,
                }) => at,
                Ok(_) => continue,
                // A folder on the way that cannot be listed hides what is under
                // it, as it does from the vault walk.
                Err(unreadable) if unreadable.kind() == norn_fs::UnreadKind::Directory => {
                    hidden = true;
                    continue;
                }
                // A name whose own stat failed is still a spelling standing
                // there, which the vault walk's revisit files the same way —
                // unless the walk cannot enter the folder holding it, which a
                // folder that lists and refuses a search is: the walk marks
                // that folder and reads no name beneath it.
                Err(unreadable) => {
                    let parent = candidate.parent().unwrap_or(Path::new(""));
                    if !parent.as_os_str().is_empty()
                        && let Ok(norn_fs::WalkFact::Unreadable(folder)) = vault.fact(parent)
                        && folder.kind() == norn_fs::UnreadKind::Directory
                    {
                        hidden = true;
                        continue;
                    }
                    unreadable.path().clone()
                }
            };
            let path = at.as_path();
            if !is_markdown(path) {
                continue;
            }
            if let Err(quarantine) = document_path(path) {
                spellings.push(Spelling {
                    path: path.to_owned(),
                    quarantine,
                });
            }
        }
        Ok((!spellings.is_empty() || !hidden).then_some(spellings))
    }

    /// The names in `folder` whose rendering carries the marker, listed once
    /// per job; `None` where the folder could not be listed. A folder the walk
    /// reaches no folder at holds no name.
    fn marked_names(&mut self, vault: &norn_fs::Vault, folder: &Path) -> Option<&MarkedNames> {
        if !self.folders.contains_key(folder) {
            let mut marked = MarkedNames::default();
            let listed = vault.visit_folder_names(folder, |name| {
                let as_folder = rendered_folder_name(name);
                let as_leaf = DocumentPath::rendered(Path::new(name)).as_str().to_owned();
                if as_folder.contains(RENDERED_MARKER) {
                    marked
                        .as_folder
                        .entry(as_folder)
                        .or_default()
                        .push(name.to_owned());
                }
                if as_leaf.contains(RENDERED_MARKER) {
                    marked
                        .as_leaf
                        .entry(as_leaf)
                        .or_default()
                        .push(name.to_owned());
                }
                ControlFlow::Continue(())
            });
            let names = match listed {
                Ok(_) => Some(marked),
                // A listing that fails is a folder this reading cannot see
                // into, which leaves the place for the next walk that can.
                Err(_) => None,
            };
            self.folders.insert(folder.to_owned(), names);
        }
        self.folders[folder].as_ref()
    }
}

#[cfg(test)]
thread_local! {
    /// The candidate names this thread's place readings looked up, which a
    /// case reads to hold a reading to the names that render onto its place.
    static PLACE_CANDIDATES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
fn count_place_candidate() {
    PLACE_CANDIDATES.with(|seen| seen.set(seen.get() + 1));
}

#[cfg(not(test))]
fn count_place_candidate() {}

/// The candidate names this thread's place readings have looked up.
#[cfg(test)]
pub(super) fn place_candidates() -> usize {
    PLACE_CANDIDATES.with(std::cell::Cell::get)
}

/// Re-derive the spelling findings a refused spelling's places hold: its own
/// place, where the name could be or have been a document, and the places
/// beneath it, where it could be or have been a folder no stored path sits
/// under.
///
/// **Every arm of a scoped leg that meets a refused spelling ends here**,
/// whatever it found at the path. A report does not say what the path was
/// before — a file that became a folder leaves its own place behind, and a
/// folder that became a file, or lost what it held, leaves the places
/// beneath — so both are read, and each costs nothing where the spelling could
/// not have been that.
pub(super) fn read_places_of_a_refused_spelling(
    pending: &mut Pending<'_>,
    vault: &norn_fs::Vault,
    path: &Path,
    page_size: usize,
    order: StoredPathOrder,
) -> Result<(), JobFailure> {
    let addressed = path
        .to_str()
        .is_some_and(|spelling| DirectoryPrefix::new(spelling).is_ok());
    // A folder whose spelling a prefix admits addresses the stored paths
    // beneath it, and the scope the caller pruned and registered is what
    // concludes the places there.
    if !addressed {
        read_places_beneath(pending, vault, path, page_size, order)?;
    }
    if is_markdown(path) && document_path(path).is_err() {
        pending.read_place(vault, &DocumentPath::rendered(path))?;
    }
    Ok(())
}

/// Re-derive the spelling findings at every place the documents under a
/// refused folder rendered onto.
///
/// The folder's own spelling addresses no range of stored paths, since no
/// path carrying it is storable, so the places are found from the findings
/// the store holds under the folder's rendering: one ordered pass over that
/// range, which costs the findings standing there rather than the vault. Each
/// place is read for the spellings still rendering onto it, so a colliding
/// sibling folder's documents keep their findings.
fn read_places_beneath(
    pending: &mut Pending<'_>,
    vault: &norn_fs::Vault,
    folder: &Path,
    page_size: usize,
    order: StoredPathOrder,
) -> Result<(), JobFailure> {
    let rendered: Vec<String> = folder
        .components()
        .map(|segment| rendered_folder_name(segment.as_os_str()))
        .collect();
    let Ok(prefix) = DirectoryPrefix::new(&rendered.join("/")) else {
        return Ok(());
    };
    let mut after: Option<DocumentPath> = None;
    loop {
        let page = pending
            .store
            .begin_request()
            .finding_subjects_without_rows_after(
                SubjectScope::Under(&prefix),
                Decided::BySpelling.kinds(),
                after.as_ref(),
                page_size,
                order,
            )
            .map_err(store_effect)?;
        let Some(last) = page.last() else { break };
        after = Some(last.clone());
        for place in &page {
            pending.read_place(vault, place)?;
        }
    }
    Ok(())
}
