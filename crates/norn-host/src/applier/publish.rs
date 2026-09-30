//! The publishing phase: every staged target published through the kernel in
//! the contract's order, each verified again, and every publication recorded
//! as an own write the moment it lands.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use norn_fs::{Durability, Publication, ShadowHome};
use norn_wire::{DocumentPath, FileState, ResolvedPlan, TargetResult};

use super::OwnWriteLedger;
use super::observe::Unit;
use super::stage::{Classified, Held, Phase, StagedPlan, classify, discard_all, kernel_hash};
use crate::production::PlanEffect;

/// What publication did, as far as it went.
#[derive(Debug, Default)]
pub(super) struct Progress {
    /// What each path a publication touched now holds, for the changeset:
    /// every landed target, and a respell's first step where only it landed.
    pub(super) effects: Vec<PlanEffect>,
    /// Each landed transition, by index, and whether this apply wrote it.
    pub(super) results: Vec<(usize, TargetResult)>,
    /// The folders the creates made, relative to the vault root.
    pub(super) folders_made: BTreeSet<PathBuf>,
    /// The folders the removals left empty, which were removed.
    pub(super) folders_removed: BTreeSet<PathBuf>,
}

/// Why publication stopped part-way.
#[derive(Debug)]
pub(super) enum Stopped {
    /// A target was changed by another writer after it was staged.
    ForeignEdit {
        path: DocumentPath,
        holds: FileState,
    },
    /// A create's name was taken after it was staged.
    NameTaken { path: DocumentPath },
    /// The vault root was replaced.
    RootReplaced,
    /// The machine failed: a publication, a sync, a shadow gone or changed.
    Io(String),
}

/// What the publishing phase needs of the vault.
pub(super) struct Publisher<'a> {
    pub(super) anchor: &'a Path,
    pub(super) root: norn_fs::Identity,
    pub(super) shadows: &'a ShadowHome,
    pub(super) own_writes: &'a dyn OwnWriteLedger,
}

impl Publisher<'_> {
    /// Publish every target of `staged` in order, stopping at the first that
    /// cannot land durably, and empty the folders the removals left.
    ///
    /// **Any landing not synced stops publication**: a later target may draw
    /// on this one's content, and a source is not replaced or removed until
    /// every target drawing on it durably landed, its folders synced (ADR
    /// 0031). What stops is discarded, and nothing after it publishes.
    pub(super) fn publish(
        &self,
        plan: &ResolvedPlan,
        staged: StagedPlan,
    ) -> (Progress, Option<Stopped>) {
        let mut progress = Progress::default();
        let mut removed: Vec<DocumentPath> = Vec::new();
        // What the force let through is the apply's to report, not publication's.
        let StagedPlan {
            targets,
            stored,
            forced: _,
        } = staged;
        let mut remaining = targets.into_iter();
        while let Some(target) = remaining.next() {
            let unit = target.unit;
            let phase = target.phase;
            let publishing = Publishing {
                plan,
                stored: &stored,
                unit,
            };
            if let Err(stopped) = self.publish_one(&publishing, target.held, &mut progress) {
                discard_all(self.anchor, self.shadows, remaining);
                return (progress, Some(stopped));
            }
            if phase == Phase::Remove {
                removed.extend(
                    unit.transitions()
                        .map(|index| plan.transitions[index].path.clone()),
                );
            }
        }
        self.empty_folders(&removed, &mut progress);
        (progress, None)
    }

    /// Publish or confirm one unit.
    #[allow(clippy::disallowed_methods)] // The one applier: the vault write kernel's one caller.
    fn publish_one(
        &self,
        publishing: &Publishing<'_>,
        held: Held,
        progress: &mut Progress,
    ) -> Result<(), Stopped> {
        let Publishing { plan, unit, .. } = *publishing;
        let written = match unit {
            Unit::One(index) => index,
            Unit::Respell { new, .. } => new,
        };
        let written_path = &plan.transitions[written].path;
        let staged = match held {
            Held::Nothing => {
                progress.results.push((written, TargetResult::Found));
                return Ok(());
            }
            Held::Landed(landed) => {
                let confirmed = norn_fs::confirm_landed(self.anchor, &landed)
                    .map_err(|refusal| stopped(&refusal, written_path))?;
                publishing.landed_whole(TargetResult::Found, progress);
                return durable(confirmed.durability, written_path);
            }
            Held::Staged(staged) => staged,
        };
        match norn_fs::publish(self.anchor, staged, self.shadows)
            .map_err(|refusal| stopped(&refusal, written_path))?
        {
            Publication::Wrote(published) => {
                self.own_writes
                    .published(Path::new(written_path.as_str()), &published);
                progress
                    .folders_made
                    .extend(published.made_folders.iter().cloned());
                publishing.landed_whole(TargetResult::Wrote, progress);
                durable(published.durability, written_path)
            }
            Publication::Found(confirmed) => {
                publishing.landed_whole(TargetResult::Found, progress);
                durable(confirmed.durability, written_path)
            }
            Publication::Interrupted(interrupted) => {
                // The content landed under the old spelling and the rename did
                // not: the ledger records the first step there, and the
                // changeset carries it, or the store goes stale. Only the
                // kernel's respell publishes in two steps, and staging asks
                // for one only for a respell unit, which the recomposition
                // proved is a case-only rename its operations make.
                let Unit::Respell { old, new } = unit else {
                    unreachable!("only a respell publishes in two steps");
                };
                let old_path = &plan.transitions[old].path;
                self.own_writes
                    .published(Path::new(old_path.as_str()), &interrupted.published);
                if let FileState::Present { hash } = &plan.transitions[new].after {
                    progress.effects.push(PlanEffect {
                        path: publishing.stored[old].clone(),
                        holds: Some(kernel_hash(hash)),
                    });
                }
                Err(Stopped::Io(format!(
                    "the case-only rename of `{old_path}` to `{written_path}` published its content and did not rename it: {}",
                    interrupted.cause
                )))
            }
        }
    }

    /// Empty the folders the removals left, from each removed target's own
    /// folder upward.
    ///
    /// **A folder that cannot be removed is left, and not reported.** Folders
    /// are not transitions and every target has landed by now, so the plan is
    /// applied; the report names the folders removed, and the wire has no
    /// place for one that was not, so a failure here reaches the caller only
    /// as a folder missing from that list. A re-send empties it again.
    #[allow(clippy::disallowed_methods)] // The one applier: the vault write kernel's one caller.
    fn empty_folders(&self, removed: &[DocumentPath], progress: &mut Progress) {
        let mut folders: Vec<PathBuf> = removed
            .iter()
            .filter_map(|path| Path::new(path.as_str()).parent())
            .filter(|folder| !folder.as_os_str().is_empty())
            .map(Path::to_path_buf)
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        folders.sort_by_key(|folder| std::cmp::Reverse(folder.components().count()));
        for folder in folders {
            if let Ok(emptied) = norn_fs::remove_empty_folders(self.anchor, self.root, &folder) {
                progress.folders_removed.extend(emptied.removed);
            }
        }
    }
}

/// One unit being published, with what it needs of the staged plan.
struct Publishing<'a> {
    plan: &'a ResolvedPlan,
    /// Each transition's path as the store names it, by index.
    stored: &'a [norn_store::DocumentPath],
    unit: Unit,
}

impl Publishing<'_> {
    /// Record every transition of the unit as landed, with `result`, and
    /// what each path now holds.
    fn landed_whole(&self, result: TargetResult, progress: &mut Progress) {
        for index in self.unit.transitions() {
            let transition = &self.plan.transitions[index];
            progress.results.push((index, result));
            progress.effects.push(PlanEffect {
                path: self.stored[index].clone(),
                holds: match &transition.after {
                    FileState::Present { hash } => Some(kernel_hash(hash)),
                    FileState::Absent {} => None,
                },
            });
        }
    }
}

/// A landing that is not on the disk stops publication, as an I/O failure.
fn durable(durability: Durability, path: &DocumentPath) -> Result<(), Stopped> {
    match durability {
        Durability::Synced => Ok(()),
        Durability::NotSynced(error) => Err(Stopped::Io(format!(
            "`{path}` landed and its folder was not synced: {error}"
        ))),
    }
}

/// Why a kernel refusal about `path` stops publication.
fn stopped(refusal: &norn_fs::Refusal, path: &DocumentPath) -> Stopped {
    match classify(refusal) {
        Classified::Drift(holds) => Stopped::ForeignEdit {
            path: path.clone(),
            holds,
        },
        Classified::NameTaken => Stopped::NameTaken { path: path.clone() },
        Classified::RootReplaced => Stopped::RootReplaced,
        Classified::Io(detail) => Stopped::Io(detail),
    }
}
