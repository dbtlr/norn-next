//! The one applier: a resolved plan in, checked and staged whole, published
//! through `norn-fs`'s kernel in the contract's order, committed to the store
//! as one changeset, and answered as a typed outcome.
//!
//! **What the applier owns.** Invariant 4 names one plan vocabulary and one
//! applier; this is that applier, and every write — a verb's one operation, a
//! caller's plan, a repair — reaches the vault through it once it is resolved
//! ([ADR 0037]). The flow is four phases, one module each:
//!
//! - [`observe`] — what each target holds now, at the exact spelling the plan
//!   writes: its before-state, its after-state (landed, whichever writer put
//!   it there), or neither (drift). A case-only rename's two transitions are
//!   one unit, published as the kernel's respell.
//! - [`stage`] — the check-and-stage phase. The root identity, every target's
//!   path and state, every condition, the plan's operations run again from its
//!   before-states through the planner's own ordering and composition
//!   ([`recompose`]) — which must give back exactly the plan's transitions, so
//!   a plan whose transitions say anything its operations do not is refused
//!   before anything is staged — every schema violation a composed result
//!   introduces ([`schema`]), which a forced plan lets through and lists on its
//!   forecast and applied report, judged against the document its content came from
//!   (the planner's one [`lineage`](crate::planner::lineage), followed in the
//!   plan's recorded order, which also refuses a content cycle), and a shadow
//!   for every written target, a create included.
//!   A refusal here discards every shadow and publishes nothing.
//! - [`publish`] — creates, then replaces, then removals, each verified again
//!   by the kernel, with every folder a removal left empty removed after
//!   them; a source is never replaced or removed until every target drawing
//!   on it durably landed. Every publication is recorded in the own-write
//!   ledger the moment it lands.
//! - the changeset — what publication left, committed as one changeset marked
//!   composed through the heal's own derivation
//!   ([`crate::production::commit_plan_changeset`]); exactly the landed
//!   subset when publication stopped part-way. A control file a plan writes
//!   is no document, so the changeset derives nothing for it: the vault takes
//!   a control file into service by a reload, never through the store.
//!
//! **Landed is judged by content, not by author.** A target whose content
//! already matches its after-state is landed whoever put it there, a
//! hand-edited after-state included, so a plan every one of whose targets
//! already holds its after-state answers applied, every target found, and
//! writes nothing (see [`recompose`]). The one exception is a target whose
//! after-state equals its before-state: it is recomposed from the bytes it
//! holds, so an edit made to change nothing there is refused.
//!
//! A refusal answers with a fresh plan ([`refresh`]); a plan whose own shape
//! is wrong answers `request/plan-invalid` with no plan — operations in a
//! cycle or with a broken identifier, or transitions that are not what the
//! operations do, which name every file they disagree at ([`recompose`]) and
//! are a client's fault to fix by previewing again, never vault drift; a root
//! that is not the plan's answers with no plan; an interruption names what
//! landed; an I/O failure before anything landed wrote nothing ([`outcome`]).
//!
//! **Memory.** From staging to publication the applier holds the plan — its
//! operations and one fixed-size transition per target — and one fixed-size
//! staged record per target: no handle, and no byte of any file. The
//! observed and composed bytes are held only while staging, as planning holds
//! them, and the changeset reads each landed document back when it commits.
//! A document a move carries byte for byte — by the planner's one rule,
//! `crate::planner::lineage::Carried` — is observed streamed ([`observe`]),
//! recomposed unread, and staged as the write kernel's streamed copy of its
//! source, a create's or a refilled name's replace. **No copy once
//! indexed**: its links are read from the index where the index derived
//! them from the bytes the plan carries; where the index does not vouch for
//! them, the file standing at the plan's hash — the source, or a re-send's
//! landed destination — is read whole once, one copy, as planning reads it
//! where its index lags. A re-sent plan must finish over a vault whose index
//! has not taken in what the vault holds (ADR 0037), so a check never
//! refuses a plan for what the index has not yet seen.
//!
//! **Who applies here.** The apply job, which takes the entry's claim,
//! derives the facts delivered by then, plans an authored plan through the
//! planner, and hands the resolved plan here with the entry's vault root,
//! root identity, shadow home, own-write ledger and store, and a question it
//! asks just before the first publication: whether its leg still stands. Yes
//! records the mark from which an apply dropped unanswered may have landed
//! targets; no is a teardown, and the applier removes every shadow and
//! publishes nothing.
//!
//! [ADR 0037]: https://github.com/dbtlr/norn/blob/main/docs/decisions/0037-a-plan-refuses-exactly-the-violations-it-introduces.md

mod observe;
mod outcome;
mod place;
mod publish;
mod recompose;
mod refresh;
mod schema;
mod shape;
mod stage;

use std::cell::RefCell;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use norn_fs::{OwnWrites, Published, ShadowHome};
use norn_store::{IncrementProvenance, Store};
use norn_wire::{
    AppliedTarget, ChangesetOutcome, DocumentPath, FolderPath, Forecast, InterruptionCause,
    RefusedCheck, ResolvedPlan, RootIdentity, SchemaViolation, TargetResult,
};

pub(crate) use outcome::{Applied, ApplyOutcome, Interrupted};

use crate::derivation::Declared;
use crate::planner::control::{SchemaPlace, role_at};
use crate::planner::forecast::forecast;
use crate::planner::view::{TreeView, VaultView};
use crate::production::{PlanEffect, commit_plan_changeset, pinned_declaration};
#[cfg(feature = "induced-failure")]
pub(crate) use observe::copied_sources;
use place::Ground;
use publish::{Progress, Publisher, Stopped};
use schema::Citations;
pub(crate) use schema::{Held, Verdict, verdict};
pub(crate) use stage::Links;
use stage::Stop;

/// Where a publication is recorded so the watcher's echo of it is known as
/// the applier's own.
///
/// **Only a publication is recorded**, at the moment it lands: never a
/// landing a re-send merely confirmed, which caused no event (see
/// [`OwnWrites::published`]).
pub(crate) trait OwnWriteLedger {
    /// Record that `path`, relative to the vault root, was published as
    /// `published` says.
    fn published(&self, path: &Path, published: &Published);
}

impl OwnWriteLedger for OwnWrites {
    fn published(&self, path: &Path, published: &Published) {
        // A plan's path is a vault-relative document path, which the ledger
        // always normalizes; a refusal would be a path outside the vault.
        let recorded = OwnWrites::published(self, path, published);
        debug_assert!(recorded.is_ok(), "a plan's path is inside its vault");
    }
}

/// The one applier, over one vault.
///
/// Everything it acts on is handed to it: the entry's vault root as it is
/// spelled, the root's identity as the entry proved it, the roots its walk
/// does not enter, its shadow home and its own-write recorder. It holds no
/// state between applies.
pub(crate) struct Applier<'a> {
    /// The vault root, as it is spelled.
    pub(crate) anchor: &'a Path,
    /// The `(device, inode)` of the vault root.
    pub(crate) root: norn_fs::Identity,
    /// The roots the vault's walk does not enter.
    pub(crate) exclusions: &'a [PathBuf],
    /// Where the vault schema the registration reads lives, which a schema
    /// write lands at (ADR 0034).
    pub(crate) schema: &'a SchemaPlace,
    /// Where written targets are staged.
    pub(crate) shadows: &'a ShadowHome,
    /// Where each publication is recorded.
    pub(crate) own_writes: &'a dyn OwnWriteLedger,
    /// Asked once, after every target is staged and just before the first
    /// is published, whether publication may begin: the apply job's check
    /// that its leg still stands, which records the progress mark — from
    /// which an apply dropped unanswered may have landed targets — where it
    /// does. An answer of no is a teardown the apply stops at: every shadow
    /// is removed and nothing is published.
    pub(crate) publishing: &'a dyn Fn() -> bool,
    /// Where the plan's resolution change set is computed again: the apply
    /// job's one snapshot, taken after its intake the first time a read asks
    /// for it. Released before a changeset commits, so no read snapshot is
    /// held over the store's write; a refusal publication met before anything
    /// landed is answered while it is still held, or on one taken then, since
    /// nothing has committed since the job's intake.
    pub(crate) links: Links<'a>,
}

impl Applier<'_> {
    /// Apply `plan`, committing what lands to `store`.
    ///
    /// **The store is lent in a cell, not held**, because the job's one read
    /// handle may be minted off it while the applier runs: `links` mints it
    /// the first time a read asks — the check of a plan whose planning read
    /// no links, or the fresh plan a refusal resolves — and gives it back
    /// before the changeset commits. The applier borrows the store only for
    /// the statement that reads its pin and for the commit, and no link is
    /// read across either.
    pub(crate) fn apply(&self, plan: ResolvedPlan, store: &RefCell<&mut Store>) -> ApplyOutcome {
        let found = self.root_identity();
        if plan.root != found {
            return ApplyOutcome::RootChanged {
                expected: plan.root,
                found,
                healing: Vec::new(),
            };
        }
        let view = match TreeView::open(self.anchor, self.exclusions, self.schema) {
            Ok(view) => view,
            Err(error) => return write_failed(plan, error.to_string(), Vec::new()),
        };
        let declared = match pinned_declaration(&mut store.borrow_mut()) {
            Ok(declared) => declared,
            Err(failure) => return write_failed(plan, format!("{failure:?}"), Vec::new()),
        };
        let mut citations = Citations::default();
        let mut staged = match stage::check_and_stage(
            &self.ground(),
            self.shadows,
            &plan,
            &view,
            &declared,
            self.links,
            &mut citations,
        ) {
            Ok(staged) => staged,
            Err(Stop::Refused(checks)) => return self.refuse(plan, &declared, checks, citations),
            Err(Stop::Invalid(fault)) => return ApplyOutcome::Invalid(fault),
            Err(Stop::RootReplaced) => return self.root_replaced(plan),
            Err(Stop::Failed(detail)) => return write_failed(plan, detail, Vec::new()),
            Err(Stop::Unread(refusal)) => {
                return ApplyOutcome::Unread {
                    refusal,
                    healing: Vec::new(),
                };
            }
        };
        drop(view);
        let forced = std::mem::take(&mut staged.forced);
        if !(self.publishing)() {
            stage::discard_all(&self.ground(), self.shadows, &plan, staged.targets);
            return ApplyOutcome::StoodDown;
        }
        let publisher = Publisher {
            ground: self.ground(),
            shadows: self.shadows,
            own_writes: self.own_writes,
        };
        let (progress, stopped) = publisher.publish(&plan, staged);
        self.answer(
            plan,
            progress,
            stopped,
            (forced, citations),
            &declared,
            store,
        )
    }

    /// The outcome of a publication that ran as far as `progress`, whose
    /// force let `forced` through citing `citations`, under the declaration
    /// `declared`.
    fn answer(
        &self,
        plan: ResolvedPlan,
        progress: Progress,
        stopped: Option<Stopped>,
        (forced, citations): (Vec<SchemaViolation>, Citations),
        declared: &Declared,
        store: &RefCell<&mut Store>,
    ) -> ApplyOutcome {
        // A control file is no document, so what a plan writing one left is
        // no document the changeset derives: the reload that takes it into
        // service is the host's, never a row of the store.
        let effects: Vec<PlanEffect> = progress
            .effects
            .iter()
            .filter(|effect| role_at(effect.path.as_str()).is_none())
            .cloned()
            .collect();
        let changeset = if effects.is_empty() {
            ChangesetOutcome::Committed
        } else {
            // Nothing reads the plan's links once a target landed, so the
            // snapshot they were read on is given back before the changeset
            // commits over it.
            self.links.release();
            match commit_plan_changeset(
                &mut store.borrow_mut(),
                self.anchor,
                self.exclusions,
                &effects,
                IncrementProvenance::Composed,
            ) {
                Ok(_) => ChangesetOutcome::Committed,
                Err(_) => ChangesetOutcome::Healing,
            }
        };
        // Each transition's result, indexed once, so both lists below are
        // linear in the plan; a transition published twice keeps its first.
        let mut result_of: Vec<Option<TargetResult>> = vec![None; plan.transitions.len()];
        for &(index, result) in &progress.results {
            result_of[index].get_or_insert(result);
        }
        let landed: Vec<DocumentPath> = plan
            .transitions
            .iter()
            .zip(&result_of)
            .filter(|(_, result)| result.is_some())
            .map(|(transition, _)| transition.path.clone())
            .collect();
        let Some(stopped) = stopped else {
            let targets = plan
                .transitions
                .iter()
                .zip(&result_of)
                .filter_map(|(transition, result)| {
                    Some(AppliedTarget::new(transition.path.clone(), (*result)?))
                })
                .collect();
            return ApplyOutcome::Applied(Applied {
                plan,
                changeset,
                targets,
                folders_made: folder_paths(&progress.folders_made),
                folders_removed: folder_paths(&progress.folders_removed),
                forced,
                citations,
            });
        };
        if progress.published {
            let cause = match stopped {
                Stopped::ForeignEdit { path, .. } => InterruptionCause::foreign_edit(path),
                Stopped::NameTaken { path } => InterruptionCause::name_taken(path),
                Stopped::RootReplaced => {
                    InterruptionCause::io_failure("the vault root was replaced during publication")
                }
                Stopped::Io(detail) => InterruptionCause::io_failure(detail),
            };
            // What the force let through in a target that did not land is
            // not written; a re-send that lands it lists it then.
            let forced = forced
                .into_iter()
                .filter(|violation| landed.contains(&violation.path))
                .collect();
            return ApplyOutcome::Interrupted(Box::new(Interrupted {
                plan,
                landed,
                cause,
                forced,
                citations,
                changeset,
            }));
        }
        let healing = if changeset == ChangesetOutcome::Healing {
            plan.transitions
                .iter()
                .map(|transition| transition.path.clone())
                .collect()
        } else {
            Vec::new()
        };
        let outcome = match stopped {
            Stopped::ForeignEdit { path, holds } => self.refuse(
                plan,
                declared,
                vec![RefusedCheck::drifted(path, holds)],
                citations,
            ),
            Stopped::NameTaken { path } => self.refuse(
                plan,
                declared,
                vec![RefusedCheck::name_taken(path)],
                citations,
            ),
            Stopped::RootReplaced => self.root_replaced(plan),
            Stopped::Io(detail) => write_failed(plan, detail, Vec::new()),
        };
        outcome.with_confirmed_progress(landed, healing)
    }

    /// Refuse `plan` for `checks`, answering with a fresh plan judged under
    /// `declared`, every violation in the answer citing its rules through
    /// `citations`.
    fn refuse(
        &self,
        plan: ResolvedPlan,
        declared: &Declared,
        checks: Vec<RefusedCheck>,
        citations: Citations,
    ) -> ApplyOutcome {
        match TreeView::open(self.anchor, self.exclusions, self.schema) {
            Ok(view) => {
                refresh::refuse_and_refresh(plan, &view, declared, checks, self.links, citations)
            }
            Err(error) => write_failed(plan, error.to_string(), Vec::new()),
        }
    }

    /// The answer when the root the plan was staged under is no longer the
    /// directory the root's spelling names.
    fn root_replaced(&self, plan: ResolvedPlan) -> ApplyOutcome {
        root_replaced(self.anchor, plan)
    }

    /// The ground the plan's targets land on.
    fn ground(&self) -> Ground<'_> {
        Ground {
            vault: self.anchor,
            root: self.root,
            schema: self.schema,
        }
    }

    /// The vault's root identity, as the wire spells it.
    fn root_identity(&self) -> RootIdentity {
        RootIdentity::from_device_and_inode(self.root.dev, self.root.ino)
    }
}

/// Preview the resolved `plan` over the vault at `anchor`: judge it as an
/// apply would, reading the vault and writing nothing.
///
/// **The judgment is the apply's own**: the root identity, then every check
/// [`stage::check`] runs — each target at its before- or after-state, every
/// condition, the operations recomposed from the before-states, the schema —
/// then the write kernel's own judgment of every written target in the order
/// it publishes, as staging would meet it ([`stage::judge`]): its descent to
/// the target through no link and what stands at the name, staging nothing.
/// Where an apply would go on to stage a shadow, the answer is the same plan
/// with its forecast from what the vault holds, the advisories on its links
/// read off its resolution change set computed again through `links`; where
/// it would not, the answer is the outcome an apply of the plan over the same
/// files ends in — a refusal with its fresh plan, a fault in the plan's
/// shape, a root replaced, or a vault that could not be read, which an apply
/// answers as a write that failed before anything landed. So what a caller
/// previewed is what applies, and a plan an interruption left part-landed
/// previews as itself.
pub(crate) fn preview(
    plan: ResolvedPlan,
    anchor: &Path,
    root: norn_fs::Identity,
    exclusions: &[PathBuf],
    schema: &SchemaPlace,
    declared: &Declared,
    links: Links<'_>,
) -> Result<(ResolvedPlan, Forecast), Box<ApplyOutcome>> {
    let found = RootIdentity::from_device_and_inode(root.dev, root.ino);
    if plan.root != found {
        return Err(Box::new(ApplyOutcome::RootChanged {
            expected: plan.root,
            found,
            healing: Vec::new(),
        }));
    }
    let view = match TreeView::open(anchor, exclusions, schema) {
        Ok(view) => view,
        Err(error) => return Err(Box::new(write_failed(plan, error.to_string(), Vec::new()))),
    };
    let ground = Ground {
        vault: anchor,
        root,
        schema,
    };
    let mut citations = Citations::default();
    let stop = match stage::check(&plan, &view, declared, links, &mut citations) {
        Ok(checked) => match stage::judge(&ground, &plan, view.normalizer(), &checked) {
            Ok(()) => {
                let cited = citations.cited_by(&checked.forced);
                return match forecast(&plan.transitions, &view) {
                    Ok(forecast) => Ok((
                        plan,
                        forecast
                            .with_forced(checked.forced, cited)
                            .with_links(checked.links),
                    )),
                    Err(error) => Err(Box::new(write_failed(plan, error.to_string(), Vec::new()))),
                };
            }
            Err(stop) => stop,
        },
        Err(unfit) => Stop::from(unfit),
    };
    Err(Box::new(match stop {
        Stop::Refused(checks) => {
            refresh::refuse_and_refresh(plan, &view, declared, checks, links, citations)
        }
        Stop::Invalid(fault) => ApplyOutcome::Invalid(fault),
        Stop::RootReplaced => root_replaced(anchor, plan),
        Stop::Failed(detail) => write_failed(plan, detail, Vec::new()),
        Stop::Unread(refusal) => ApplyOutcome::Unread {
            refusal,
            healing: Vec::new(),
        },
    }))
}

/// The answer when the root at `anchor` the plan was judged under is no
/// longer the directory the root's spelling names.
fn root_replaced(anchor: &Path, plan: ResolvedPlan) -> ApplyOutcome {
    match norn_fs::path_identity(anchor) {
        Ok(Some(now)) => ApplyOutcome::RootChanged {
            expected: plan.root,
            found: RootIdentity::from_device_and_inode(now.dev, now.ino),
            healing: Vec::new(),
        },
        _ => write_failed(plan, "the vault root was replaced".to_string(), Vec::new()),
    }
}

fn write_failed(plan: ResolvedPlan, detail: String, landed: Vec<DocumentPath>) -> ApplyOutcome {
    ApplyOutcome::WriteFailed {
        plan,
        detail,
        landed,
        healing: Vec::new(),
    }
}

/// Folders as the wire names them.
fn folder_paths(folders: &BTreeSet<PathBuf>) -> Vec<FolderPath> {
    folders
        .iter()
        .filter_map(|folder| FolderPath::new(folder.to_str()?).ok())
        .collect()
}

#[cfg(all(test, feature = "induced-failure"))]
mod induced;
#[cfg(test)]
mod tests;
