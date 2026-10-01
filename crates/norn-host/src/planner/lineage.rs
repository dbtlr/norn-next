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
//! and the plan is refused at planning as a content cycle. The applier holds
//! a resolved plan to the same rule over its recorded order, since a caller
//! can send one planning never produced.
//!
//! **Which check catches what.** [`super::order`] refuses a cycle among the
//! operations' own order: a move waits for what vacates its destination only
//! where a document stands there at planning, so a direct exchange `[a→b,
//! b→a]` cannot be ordered at all and is refused there, before anything
//! composes. An exchange routed through a name nothing stands at, such as
//! `[a→t, b→a, t→b]`, orders and composes; its net transitions still draw
//! on each other, and [`Lineage::content_cycle`] refuses it here. The two cannot be
//! one check, since composition needs the order the first one settles.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use norn_fs::{NormalizedPath, PathNormalizer};
use norn_wire::{Operation, OperationKind};

use super::compose::touches;

/// Where the content a file holds was drawn from.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Drawn {
    /// The file whose before-state the content is.
    pub(crate) from: NormalizedPath,
    /// The positions of the moves that carried it there, in the order they
    /// compose.
    pub(crate) moves: Vec<usize>,
}

/// Which file's before-state every file's content is drawn from, at the end
/// of a plan and where each of its edits acted.
///
/// **What a target draws on.** Only a move carries content between files: a
/// file no operation has touched holds its own before-state, a create writes
/// content its operation carries, a removal leaves nothing, an edit changes
/// the content a file already holds, and a move takes its source's content to
/// its destination and leaves the source holding nothing. A case-only rename,
/// whose two spellings the normalizer names as one file, keeps its file's
/// content where it was; a name the normalizer refuses is no file and carries
/// nothing.
///
/// **One derivation, two consumers.** The planner reads it for the content
/// cycle alone, on the order a resolved plan's operations compose in. The
/// applier reads it on a resolved plan's operations in their recorded order,
/// for the same content cycle and for what else follows content through
/// moves: drift of a source a target that has not landed draws on, the
/// schema baseline each result is judged against, and the stand-in for a
/// before-state an apply cannot see.
#[derive(Debug, Default)]
pub(crate) struct Lineage {
    /// Each file an operation touches, and the source of what it holds at
    /// the end of the plan: `None` where an operation wrote it or it holds
    /// nothing.
    at_end: BTreeMap<NormalizedPath, Option<Drawn>>,
    /// Each edit's position, and the file whose before-state the content it
    /// acted on was drawn from.
    edited: BTreeMap<usize, NormalizedPath>,
    /// Each file whose before-state ends the plan at another file, and that
    /// file: [`Self::at_end`] read the other way.
    carried: BTreeMap<NormalizedPath, NormalizedPath>,
    /// Each file whose before-state a delete of the plan removes, wherever
    /// the plan's moves carried it first, and that delete's position.
    removed: BTreeMap<NormalizedPath, usize>,
}

impl Lineage {
    /// Follow `operations` at the positions `order` names, in that order,
    /// reading every name through `normalizer`.
    pub(crate) fn of(
        operations: &[Operation],
        order: &[usize],
        normalizer: &PathNormalizer,
    ) -> Lineage {
        let identity = |path: &str| normalizer.normalize(Path::new(path)).ok();
        let mut lineage = Lineage::default();
        for &position in order {
            let kind = &operations[position].kind;
            match kind {
                OperationKind::CreateDocument { path, .. } => {
                    if let Some(file) = identity(path.as_str()) {
                        lineage.at_end.insert(file, None);
                    }
                }
                OperationKind::DeleteDocument { path, .. } => {
                    if let Some(file) = identity(path.as_str()) {
                        if let Some(drawn) = lineage.source(&file) {
                            lineage.removed.insert(drawn.from, position);
                        }
                        lineage.at_end.insert(file, None);
                    }
                }
                // An edit in place: a `str_replace` or a document-local kind,
                // each touching the one document it names.
                OperationKind::StrReplace { .. }
                | OperationKind::SetFrontmatter { .. }
                | OperationKind::RemoveFrontmatter { .. }
                | OperationKind::PushFrontmatter { .. }
                | OperationKind::PopFrontmatter { .. }
                | OperationKind::ReplaceBody { .. }
                | OperationKind::ReplaceSection { .. }
                | OperationKind::AppendToSection { .. }
                | OperationKind::DeleteSection { .. }
                | OperationKind::InsertBeforeHeading { .. }
                | OperationKind::InsertAfterHeading { .. } => {
                    if let Some(path) = touches(kind).next()
                        && let Some(file) = identity(path.as_str())
                        && let Some(source) = lineage.source(&file)
                    {
                        lineage.edited.insert(position, source.from);
                    }
                }
                OperationKind::MoveDocument { from, to } => {
                    let (Some(from), Some(to)) = (identity(from.as_str()), identity(to.as_str()))
                    else {
                        continue;
                    };
                    if from == to {
                        continue;
                    }
                    let carried = lineage.source(&from).map(|mut drawn| {
                        drawn.moves.push(position);
                        drawn
                    });
                    lineage.at_end.insert(from, None);
                    lineage.at_end.insert(to, carried);
                }
                // None of these draws content from a before-state. A folder
                // move arrives expanded into the moves it makes, and a link
                // rewrite — an authored one or a cascade's — is an edit in
                // place, drawing on nothing. NORN-297: a wikilink rewrite is
                // not planned yet; a hand-built resolved plan can still carry
                // one this far, and recompose refuses it.
                OperationKind::MoveFolder { .. }
                | OperationKind::RewriteLink { .. }
                | OperationKind::RewriteWikilink { .. } => {}
            }
        }
        lineage.carried = lineage
            .drawing()
            .map(|(file, drawn)| (drawn.from.clone(), file.clone()))
            .collect();
        lineage
    }

    /// Where the document standing at `from` before the plan ends it, where
    /// the plan's moves carry it to another file, and how it got there; `None`
    /// where it stays, or no file holds its content at the end of the plan.
    ///
    /// **What a link naming it follows.** A link that named the document
    /// before a move names it after only where it still resolves to that
    /// file, so a link cascade reads here which file each moved document's
    /// links must name, and which move carried it there.
    pub(crate) fn carried_to(&self, from: &NormalizedPath) -> Option<(&NormalizedPath, &Drawn)> {
        let file = self.carried.get(from)?;
        let drawn = self.at_end.get(file)?.as_ref()?;
        Some((file, drawn))
    }

    /// The position of the delete that removes the document standing at
    /// `from` before the plan — at `from`, or wherever the plan's moves
    /// carried it first — and `None` where no delete of the plan removes it.
    ///
    /// **What a delete's backlinks are.** A link that named the document
    /// before the plan names nothing of it after, so the delete reads here
    /// which of the links the store's resolution door reaches are its own.
    pub(crate) fn removed_by(&self, from: &NormalizedPath) -> Option<usize> {
        self.removed.get(from).copied()
    }

    /// The position of every delete of the plan that removes a document
    /// standing before it.
    pub(crate) fn removals(&self) -> impl Iterator<Item = usize> + '_ {
        self.removed.values().copied()
    }

    /// The source of what `file` holds at the end of the plan, where its
    /// content was drawn from a before-state; a file no operation touches
    /// draws on its own.
    pub(crate) fn source(&self, file: &NormalizedPath) -> Option<Drawn> {
        match self.at_end.get(file) {
            Some(drawn) => drawn.clone(),
            None => Some(Drawn {
                from: file.clone(),
                moves: Vec::new(),
            }),
        }
    }

    /// The file whose before-state the edit at `position` acted on, where it
    /// acted on one.
    pub(crate) fn edited(&self, position: usize) -> Option<&NormalizedPath> {
        self.edited.get(&position)
    }

    /// Each file whose content at the end of the plan is another file's
    /// before-state, and where that content was drawn from.
    pub(crate) fn drawing(&self) -> impl Iterator<Item = (&NormalizedPath, &Drawn)> {
        self.at_end.iter().filter_map(|(file, drawn)| {
            drawn
                .as_ref()
                .filter(|drawn| drawn.from != *file)
                .map(|drawn| (file, drawn))
        })
    }

    /// The positions of the moves closing a content cycle, or `None` where
    /// no target draws on a cycle.
    ///
    /// Each file drawing on another must land before the file it draws on is
    /// replaced or removed, and a file drawing on its own before-state waits
    /// for nothing. A cycle among those requirements is reported as the
    /// moves that carry its content, each file's moves in the order they
    /// compose, starting from the file whose moves hold the lowest position.
    pub(crate) fn content_cycle(&self) -> Option<Vec<usize>> {
        first_cycle(&self.drawing_map(), &mut 0)
    }

    fn drawing_map(&self) -> BTreeMap<&NormalizedPath, &Drawn> {
        self.drawing().collect()
    }
}

/// The cycle among `drawing` whose moves hold the lowest position, counting
/// in `steps` each time the walk follows a file to the one it draws on.
///
/// **Each file is followed once.** Each file draws on at most one other, so
/// a walk from a file not yet seen follows what it draws on until it reaches
/// a file an earlier walk finished, a file drawing on nothing, or a file on
/// its own path, which closes a cycle. Every file it passed is then finished,
/// so the whole search is linear in the files that draw on another.
fn first_cycle(
    drawing: &BTreeMap<&NormalizedPath, &Drawn>,
    steps: &mut usize,
) -> Option<Vec<usize>> {
    let mut finished: BTreeSet<&NormalizedPath> = BTreeSet::new();
    let mut cycles: Vec<Vec<usize>> = Vec::new();
    for &start in drawing.keys() {
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
fn carriers(cycle: &[&NormalizedPath], drawing: &BTreeMap<&NormalizedPath, &Drawn>) -> Vec<usize> {
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
        let lineage = Lineage::of(&operations, &order, &normalizer);
        let drawing = lineage.drawing_map();
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
