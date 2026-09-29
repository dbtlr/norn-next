//! Where each target's content comes from: the before-state of the file a
//! chain of moves carried it from, or nothing where an operation wrote it.
//!
//! **Only a move carries content between files.** An edit changes the content
//! a file already holds, a create writes content its operation carries, and a
//! removal leaves nothing, so following the moves in the plan's recorded
//! order names, for every file, whose before-state its content at the end of
//! the plan was drawn from. The applier reads that three ways: a document
//! whose before-state is gone can no longer give content to a target that has
//! not landed (drift, see [`super::observe`]); a target drawing on a
//! before-state this apply cannot see is not recomposed byte for byte
//! ([`super::recompose`]); and a written target's schema is judged against
//! the document its content came from ([`super::schema`]).

use std::collections::BTreeMap;

use norn_fs::{NormalizedPath, PathNormalizer};
use norn_wire::{OperationKind, ResolvedPlan};

use super::observe::identity;

/// Where a file's content came from.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct Source {
    /// The file whose before-state the content was drawn from.
    pub(super) from: NormalizedPath,
    /// The positions of the moves that carried it, in order.
    pub(super) moves: Vec<usize>,
}

/// The source of every file's content, at the end of the plan and where each
/// edit acted.
#[derive(Debug, Default)]
pub(super) struct Lineage {
    /// Each file an operation touches, and the source of what it holds at
    /// the end of the plan: `None` where an operation wrote it or it holds
    /// nothing.
    at_end: BTreeMap<NormalizedPath, Option<Source>>,
    /// Each edit's position, and the file whose before-state the content it
    /// acted on was drawn from.
    edited: BTreeMap<usize, NormalizedPath>,
}

impl Lineage {
    /// Follow `plan`'s operations in its recorded order.
    ///
    /// A file no operation has touched yet holds its own before-state. A name
    /// with no identity under `normalizer` is no file, and is left out: the
    /// recomposition refuses the operation naming it.
    pub(super) fn of(plan: &ResolvedPlan, normalizer: &PathNormalizer) -> Lineage {
        let mut lineage = Lineage::default();
        for (position, operation) in plan.operations.iter().enumerate() {
            match &operation.kind {
                OperationKind::CreateDocument { path, .. }
                | OperationKind::DeleteDocument { path } => {
                    if let Some(file) = identity(normalizer, path.as_str()) {
                        lineage.at_end.insert(file, None);
                    }
                }
                OperationKind::StrReplace { path, .. } => {
                    if let Some(file) = identity(normalizer, path.as_str())
                        && let Some(source) = lineage.holding(&file)
                    {
                        lineage.edited.insert(position, source.from);
                    }
                }
                OperationKind::MoveDocument { from, to } => {
                    let (Some(from), Some(to)) = (
                        identity(normalizer, from.as_str()),
                        identity(normalizer, to.as_str()),
                    ) else {
                        continue;
                    };
                    // A case-only rename keeps its file, and its content.
                    if from == to {
                        continue;
                    }
                    let carried = lineage.holding(&from).map(|mut source| {
                        source.moves.push(position);
                        source
                    });
                    lineage.at_end.insert(from, None);
                    lineage.at_end.insert(to, carried);
                }
            }
        }
        lineage
    }

    /// The source of what `file` holds at the end of the plan, where its
    /// content was drawn from a before-state.
    pub(super) fn source(&self, file: &NormalizedPath) -> Option<Source> {
        self.holding(file)
    }

    /// The file whose before-state the edit at `position` acted on, where it
    /// acted on one.
    pub(super) fn edited(&self, position: usize) -> Option<&NormalizedPath> {
        self.edited.get(&position)
    }

    fn holding(&self, file: &NormalizedPath) -> Option<Source> {
        match self.at_end.get(file) {
            Some(source) => source.clone(),
            None => Some(Source {
                from: file.clone(),
                moves: Vec::new(),
            }),
        }
    }
}
