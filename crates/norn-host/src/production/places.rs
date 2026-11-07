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

/// What one reading of a place found at every spelling rendering onto it.
pub(super) struct PlaceReading {
    /// The Markdown files the vault walk would quarantine there.
    pub(super) spellings: Vec<Spelling>,
    /// Whether a folder on the way hid some spellings, as it hides them from
    /// the vault walk.
    pub(super) hidden: bool,
    /// Whether some spelling there is a folder the vault walk cannot list,
    /// which a build from zero marks at the place.
    pub(super) unlistable: bool,
}

/// The place the paths beneath a folder render under: each of its segments
/// rendered as a folder, which is how every path under it renders them.
pub(super) fn rendered_folder(folder: &Path) -> String {
    folder
        .components()
        .map(|segment| rendered_folder_name(segment.as_os_str()))
        .collect::<Vec<String>>()
        .join("/")
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
        let reading = self.read(vault, place, false)?;
        Ok((!reading.spellings.is_empty() || !reading.hidden).then_some(reading.spellings))
    }

    /// Every spelling in the vault that renders onto `place`, read as
    /// [`PlaceReadings::spellings`] reads them, and — where `folders` asks —
    /// whether any of them is a folder the vault walk cannot list.
    ///
    /// **A folder's mark is filed at its own place**, which is a rendering
    /// wherever the document grammar refuses its name, so the folders rendering
    /// onto a place are its spellings on the content side exactly as the files
    /// are on the spelling side. Asking costs one listing per folder found
    /// there, which only a reading that concludes a mark asks for.
    pub(super) fn read(
        &mut self,
        vault: &norn_fs::Vault,
        place: &DocumentPath,
        folders: bool,
    ) -> Result<PlaceReading, JobFailure> {
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
        let mut unlistable = false;
        for candidate in candidates {
            let at = match reach_spelling(vault, &candidate)? {
                Reached::Stands(at) => at,
                Reached::Hidden => {
                    hidden = true;
                    continue;
                }
                Reached::Folder(at) => {
                    // The vault walk's own answer about the folder, which is
                    // what a build from zero marks it by.
                    if folders
                        && !unlistable
                        && let norn_fs::WalkFact::Unreadable(fact) =
                            vault.fact(at.as_path()).map_err(effect)?
                    {
                        unlistable = fact.kind() == norn_fs::UnreadKind::Directory;
                    }
                    continue;
                }
                Reached::Absent => continue,
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
        Ok(PlaceReading {
            spellings,
            hidden,
            unlistable,
        })
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

/// The places a drain of deferred spellings has read, and those of them a
/// folder hid every spelling of, whose deferred spellings stand as the walk
/// read them.
#[derive(Default)]
pub(super) struct DeferredReadings {
    pub(super) readings: PlaceReadings,
    pub(super) withheld: BTreeSet<DocumentPath>,
}

/// What the vault holds at one spelling a reading names.
pub(super) enum Reached {
    /// A regular file stands there, at the spelling its folder lists.
    Stands(norn_fs::NormalizedPath),
    /// A folder stands there, which names no document and can carry a mark.
    Folder(norn_fs::NormalizedPath),
    /// A folder on the way hides it, as it hides it from the vault walk.
    Hidden,
    /// No file the vault walk would yield stands there.
    Absent,
}

/// Reach one spelling by its name alone, opening nothing, which is all a
/// spelling finding is concluded by: an excluded root, a shadow or a link on
/// the way is a name the vault walk never yields, and a name that is not a
/// regular file is no document either.
pub(super) fn reach_spelling(
    vault: &norn_fs::Vault,
    spelling: &Path,
) -> Result<Reached, JobFailure> {
    Ok(match vault.reach_or_unreadable(spelling).map_err(effect)? {
        Ok(norn_fs::Reach::Stands {
            kind: norn_fs::PathKind::RegularFile,
            at,
        }) => Reached::Stands(at),
        Ok(norn_fs::Reach::Stands {
            kind: norn_fs::PathKind::Directory,
            at,
        }) => Reached::Folder(at),
        Ok(_) => Reached::Absent,
        // A folder on the way that cannot be listed hides what is under it, as
        // it does from the vault walk.
        Err(unreadable) if unreadable.kind() == norn_fs::UnreadKind::Directory => Reached::Hidden,
        // A name whose own stat failed is still a spelling standing there,
        // which the vault walk's revisit files the same way — unless the walk
        // cannot enter the folder holding it, which a folder that lists and
        // refuses a search is: the walk marks that folder and reads no name
        // beneath it.
        Err(unreadable) => {
            let parent = spelling.parent().unwrap_or(Path::new(""));
            if !parent.as_os_str().is_empty()
                && let Ok(norn_fs::WalkFact::Unreadable(folder)) = vault.fact(parent)
                && folder.kind() == norn_fs::UnreadKind::Directory
            {
                Reached::Hidden
            } else {
                Reached::Stands(unreadable.path().clone())
            }
        }
    })
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

/// Re-derive the place-scoped findings at the rendered places a reported
/// spelling stands at or holds: its own place, where the name could be or have
/// been a refused document or a marked folder, and the places beneath it, where
/// it could be or have been a folder.
///
/// **A place is read wherever no scope the caller registered can conclude
/// it**, which is wherever it carries the marker: a place is named by every
/// spelling rendering onto it, so only the vault walk reaches its unrendered
/// ancestor. A refused spelling's places always do, and so do the places of an
/// admitted name that carries the marker as written.
///
/// **Every arm of a scoped leg ends here**, whatever it found at the path. A
/// report does not say what the path was before — a file that became a folder
/// leaves its own place behind, and a folder that became a file, or lost what
/// it held, leaves the places beneath — so both are read, and each costs
/// nothing where the spelling could not have been that. A name that is no
/// refused Markdown file has no spelling finding at its place, so its own
/// place is read only where a mark stands there — one keyed probe — and an
/// admitted name is the one spelling of a place without the marker, so the
/// reading finds whether a folder the walk cannot list still stands there.
pub(super) fn read_rendered_places(
    pending: &mut Pending<'_>,
    vault: &norn_fs::Vault,
    path: &Path,
    page_size: usize,
    order: StoredPathOrder,
) -> Result<(), JobFailure> {
    // A folder whose rendering is its own spelling holds places the scope the
    // caller pruned and registered concludes.
    if rendered_folder(path).contains(RENDERED_MARKER) {
        read_places_beneath(pending, vault, path, page_size, order)?;
    }
    let place = DocumentPath::rendered(path);
    let refused_document = is_markdown(path) && document_path(path).is_err();
    if refused_document || pending.marked(&place)? {
        pending.read_place(vault, &place)?;
    }
    Ok(())
}

/// Re-derive the place-scoped findings at every place the documents and
/// folders under a folder whose rendering carries the marker rendered onto.
///
/// The places are found from the findings the store holds under the folder's
/// rendering — a refused folder's spelling addresses no range of stored paths
/// at all, and an admitted one's range holds rows rather than the places other
/// spellings render onto: one ordered pass over that
/// range, which costs the findings standing there rather than the vault. Each
/// place is read for the spellings still rendering onto it, so a colliding
/// sibling folder's documents keep their findings and a folder beneath that
/// still cannot be listed keeps its mark.
fn read_places_beneath(
    pending: &mut Pending<'_>,
    vault: &norn_fs::Vault,
    folder: &Path,
    page_size: usize,
    order: StoredPathOrder,
) -> Result<(), JobFailure> {
    let Ok(prefix) = DirectoryPrefix::new(&rendered_folder(folder)) else {
        return Ok(());
    };
    let kinds: Vec<FindingKind> = Decided::BySpelling
        .kinds()
        .iter()
        .copied()
        .chain([FindingKind::UnreadableDirectory])
        .collect();
    let mut after: Option<DocumentPath> = None;
    loop {
        let page = pending
            .store
            .begin_request()
            .finding_subjects_without_rows_after(
                SubjectScope::Under(&prefix),
                &kinds,
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
