//! A plan's content lineage: which file's before-state each target's content
//! is drawn from, and the content cycle that lineage refuses.
//!
//! **Why a cycle is refused.** ADR 0031 publishes no target over content a
//! later publication needs: a source is not replaced or removed until every
//! target drawing on it has durably landed, because a re-send restages each
//! target from the before-states, and content held only in a shadow is lost
//! to the sweep. A target whose content is drawn from another target's
//! before-state must therefore land before that other target publishes. Where
//! those requirements close a cycle — two documents exchanging places, or a
//! rotation among several — no publication order keeps every source standing,
//! and the plan is refused at planning as a content cycle.
//!
//! **Which check catches what.** [`super::order`] refuses a cycle among the
//! operations' own order: a move waits for what vacates its destination only
//! where a document stands there at planning, so a direct exchange `[a→b,
//! b→a]` cannot be ordered at all and is refused there, before anything
//! composes. An exchange routed through a name nothing stands at, such as
//! `[a→t, b→a, t→b]`, orders and composes; its net transitions still draw
//! on each other, and [`content_cycle`] refuses it here. The two cannot be
//! one check, since composition needs the order the first one settles.

use std::collections::BTreeMap;
use std::path::Path;

use norn_fs::{NormalizedPath, PathNormalizer};
use norn_wire::{Operation, OperationKind};

/// Where the content a file holds at the end of a plan was drawn from.
#[derive(Clone, Debug, Eq, PartialEq)]
struct Drawn {
    /// The file whose before-state the content is.
    from: NormalizedPath,
    /// The positions of the moves that carried it there, in the order they
    /// compose.
    moves: Vec<usize>,
}

/// The positions of the moves closing a content cycle among `operations`,
/// taken in `order`, or `None` where their targets draw on no cycle.
///
/// **What a target draws on.** Only a move carries content between files: a
/// file no operation has touched holds its own before-state, a create writes
/// content its operation carries, a removal leaves nothing, an edit changes
/// the content a file already holds, and a move takes its source's content to
/// its destination and leaves the source holding nothing. Following
/// `operations` at the positions `order` names, in that order, gives each
/// file whose content at the end is another file's before-state. A case-only
/// rename, whose two spellings `normalizer` names as one file, keeps its
/// file's content where it was; a name `normalizer` refuses is no file and
/// carries nothing.
///
/// **The cycle.** Each such file must land before the file it draws on is
/// replaced or removed, and a file drawing on its own before-state waits for
/// nothing. A cycle among those requirements is reported as the moves that
/// carry its content, each file's moves in the order they compose, starting
/// from the file whose moves hold the lowest position.
///
/// The planner calls this on the order a resolved plan's operations compose
/// in; the applier calls it on a resolved plan's operations in their
/// recorded order, so one rule judges both.
pub(crate) fn content_cycle(
    operations: &[Operation],
    order: &[usize],
    normalizer: &PathNormalizer,
) -> Option<Vec<usize>> {
    let drawing = lineage(operations, order, normalizer);
    // Each file draws on at most one other, so a cycle is found by walking
    // from each file along what it draws on until the walk meets itself.
    let mut cycles: Vec<Vec<usize>> = Vec::new();
    for start in drawing.keys() {
        let mut walked: Vec<&NormalizedPath> = vec![start];
        let mut at = start;
        while let Some(drawn) = drawing.get(at) {
            at = &drawn.from;
            if at == start {
                cycles.push(carriers(&walked, &drawing));
                break;
            }
            if walked.contains(&at) {
                break;
            }
            walked.push(at);
        }
    }
    cycles.into_iter().min()
}

/// The moves carrying a cycle's content, from the file whose moves hold the
/// lowest position.
fn carriers(cycle: &[&NormalizedPath], drawing: &BTreeMap<NormalizedPath, Drawn>) -> Vec<usize> {
    let lowest = |file: &&NormalizedPath| drawing[*file].moves.iter().min().copied();
    let first = (0..cycle.len())
        .min_by_key(|&index| lowest(&cycle[index]))
        .unwrap_or(0);
    cycle[first..]
        .iter()
        .chain(&cycle[..first])
        .flat_map(|file| drawing[*file].moves.iter().copied())
        .collect()
}

/// Each file whose content at the end of the plan is another file's
/// before-state, and where that content was drawn from.
fn lineage(
    operations: &[Operation],
    order: &[usize],
    normalizer: &PathNormalizer,
) -> BTreeMap<NormalizedPath, Drawn> {
    let identity = |path: &str| normalizer.normalize(Path::new(path)).ok();
    // Every file an operation touched, and what it holds so far: `None`
    // where it holds nothing drawn from a before-state.
    let mut holding: BTreeMap<NormalizedPath, Option<Drawn>> = BTreeMap::new();
    let held =
        |holding: &BTreeMap<NormalizedPath, Option<Drawn>>, file: &NormalizedPath| match holding
            .get(file)
        {
            Some(drawn) => drawn.clone(),
            None => Some(Drawn {
                from: file.clone(),
                moves: Vec::new(),
            }),
        };
    for &position in order {
        match &operations[position].kind {
            OperationKind::CreateDocument { path, .. } | OperationKind::DeleteDocument { path } => {
                if let Some(file) = identity(path.as_str()) {
                    holding.insert(file, None);
                }
            }
            OperationKind::StrReplace { .. } => {}
            OperationKind::MoveDocument { from, to } => {
                let (Some(from), Some(to)) = (identity(from.as_str()), identity(to.as_str()))
                else {
                    continue;
                };
                if from == to {
                    continue;
                }
                let carried = held(&holding, &from).map(|mut drawn| {
                    drawn.moves.push(position);
                    drawn
                });
                holding.insert(from, None);
                holding.insert(to, carried);
            }
        }
    }
    holding
        .into_iter()
        .filter_map(|(file, drawn)| drawn.filter(|drawn| drawn.from != file).map(|d| (file, d)))
        .collect()
}
