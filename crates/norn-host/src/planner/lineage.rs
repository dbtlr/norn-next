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

use std::collections::{BTreeMap, BTreeSet};
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
/// **A dormant carrier for the applier.** The planner calls this on the
/// order a resolved plan's operations compose in. Its second consumer is
/// NORN-295's applier, which will call it on a resolved plan's operations in
/// their recorded order so one rule judges both; that applier has not landed
/// on this branch, so the planner is its only caller today.
pub(crate) fn content_cycle(
    operations: &[Operation],
    order: &[usize],
    normalizer: &PathNormalizer,
) -> Option<Vec<usize>> {
    first_cycle(&lineage(operations, order, normalizer), &mut 0)
}

/// The cycle among `drawing` whose moves hold the lowest position, counting
/// in `steps` each time the walk follows a file to the one it draws on.
///
/// **Each file is followed once.** Each file draws on at most one other, so
/// a walk from a file not yet seen follows what it draws on until it reaches
/// a file an earlier walk finished, a file drawing on nothing, or a file on
/// its own path, which closes a cycle. Every file it passed is then finished,
/// so the whole search is linear in the files that draw on another.
fn first_cycle(drawing: &BTreeMap<NormalizedPath, Drawn>, steps: &mut usize) -> Option<Vec<usize>> {
    let mut finished: BTreeSet<&NormalizedPath> = BTreeSet::new();
    let mut cycles: Vec<Vec<usize>> = Vec::new();
    for start in drawing.keys() {
        if finished.contains(start) {
            continue;
        }
        // The walk's path, and each file's place on it.
        let mut path: Vec<&NormalizedPath> = Vec::new();
        let mut on_path: BTreeMap<&NormalizedPath, usize> = BTreeMap::new();
        let mut at = start;
        loop {
            on_path.insert(at, path.len());
            path.push(at);
            let Some(drawn) = drawing.get(at) else {
                break;
            };
            *steps += 1;
            at = &drawn.from;
            if let Some(&closes) = on_path.get(at) {
                cycles.push(carriers(&path[closes..], drawing));
                break;
            }
            if finished.contains(at) {
                break;
            }
        }
        finished.extend(path);
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

#[cfg(test)]
mod tests {
    use norn_fs::CaseSensitivity;
    use norn_wire::DocumentPath;

    use super::*;

    fn moving(from: &str, to: &str) -> Operation {
        Operation::new(OperationKind::move_document(
            DocumentPath::new(from).expect("a legal document path"),
            DocumentPath::new(to).expect("a legal document path"),
        ))
    }

    #[test]
    fn a_long_chain_of_moves_is_walked_once_per_file() {
        // `d3999` moves to a new name, then each document moves into the
        // name the one before it vacated: every file draws on the next, and
        // nothing closes.
        let files = 4000;
        let mut operations = vec![moving(&format!("d{}.md", files - 1), "end.md")];
        operations.extend(
            (1..files)
                .rev()
                .map(|to| moving(&format!("d{}.md", to - 1), &format!("d{to}.md"))),
        );
        let order: Vec<usize> = (0..operations.len()).collect();
        let normalizer = PathNormalizer::for_sensitivity(CaseSensitivity::Sensitive);
        let drawing = lineage(&operations, &order, &normalizer);
        assert_eq!(drawing.len(), files);
        let mut steps = 0;
        assert_eq!(first_cycle(&drawing, &mut steps), None);
        assert!(
            steps <= drawing.len(),
            "the walk followed {steps} steps over {} files",
            drawing.len()
        );
    }
}
