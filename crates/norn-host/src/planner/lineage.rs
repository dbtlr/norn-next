//! A plan's content lineage: which file's before-state each target's content
//! is drawn from, and the content cycle that lineage refuses.
//!
//! **Why a cycle is refused.** ADR 0032 publishes no target over content a
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
use norn_store::TargetNaming;
use norn_wire::{Backlinks, LinkFamily, Operation, OperationId, OperationKind};

use super::compose::touches;
use super::links::rewrite_destination;

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
    /// acted on was drawn from: where it stood in the order for an edit, and
    /// at the end of the plan for an authored link rewrite.
    edited: BTreeMap<usize, NormalizedPath>,
    /// Each file whose before-state ends the plan at another file, and that
    /// file: [`Self::at_end`] read the other way.
    carried: BTreeMap<NormalizedPath, NormalizedPath>,
    /// Every delete of the plan, in the order they compose, whatever the
    /// content it removes was drawn from.
    removals: Vec<Removal>,
    /// Each file whose before-state a delete of the plan removes, wherever
    /// the plan's moves carried it first, and that delete's place in
    /// [`Self::removals`].
    removed: BTreeMap<NormalizedPath, usize>,
    /// Each wikilink rewrite of the plan, in the order it composes.
    retargets: Vec<Retarget>,
    /// Each authored link rewrite of the plan, in the order it composes.
    relinks: Vec<Relink>,
}

/// An authored link rewrite of the plan: the links it respells, named by
/// the document holding them where the plan leaves it, their syntax and the
/// address they are written with.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Relink {
    /// The rewrite's position.
    pub(crate) position: usize,
    /// The document holding the links, where the plan leaves it.
    pub(crate) holder: NormalizedPath,
    /// The links' syntax.
    pub(crate) syntax: LinkFamily,
    /// The address the links are written with.
    pub(crate) from: String,
}

/// A wikilink rewrite of the plan: what it retargets the wikilinks naming,
/// and what it retargets them to.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Retarget {
    /// The rewrite's position.
    pub(crate) position: usize,
    /// The identifier other operations require it by, where it carries one.
    pub(crate) id: Option<OperationId>,
    /// The address of what the retargeted wikilinks name before the plan.
    pub(crate) old: String,
    /// The address of the document they name after it.
    pub(crate) new: String,
}

/// A delete of the plan, and what it says of the links naming the document
/// it removes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Removal {
    /// The delete's position.
    pub(crate) position: usize,
    /// The delete's link choice: forbidding the links naming its document,
    /// rewriting them, or leaving them broken.
    pub(crate) backlinks: Backlinks,
}

impl Removal {
    /// The address the delete rewrites the links naming its document to,
    /// where it rewrites them.
    pub(crate) fn rewrite_to(&self) -> Option<&str> {
        match &self.backlinks {
            Backlinks::RewrittenTo(target) => Some(target.address()),
            Backlinks::Forbidden | Backlinks::LeftBroken => None,
        }
    }

    /// **The one rule a delete's link choice is held to** where the plan
    /// leaves the vault, with `named` whether a backlink of the document it
    /// removes is held there — a link that itself resolved before the plan
    /// to exactly that document, a link a rewrite writes judged by the text
    /// it had and never by its new spelling — and `target` what its
    /// `rewrite_to` names there: a delete forbidding the links naming its
    /// document is kept where no link names it; one rewriting them, where
    /// its `rewrite_to` names exactly one document at a path a link can be
    /// respelled toward ([`rewrite_destination`]) — never the removed one,
    /// which stands nowhere after the plan, though another may stand at its
    /// path; one leaving them broken, always.
    ///
    /// Planning resolves every delete by this rule, leaving one it does not
    /// keep unresolved, and the applier refuses a resolved plan holding one
    /// by it again; neither reads anything more than the links and the
    /// target it judges. Planning reads the backlinks before any cascade
    /// writes a link and judges each delete once the plan composes with its
    /// cascades, and the change set reads each link a rewrite writes as the
    /// text that rewrite matched, from where its holder's content stood, so
    /// the two read the same backlinks: one a cascade respells — away from
    /// the document, or from where its moved holder lands — stays one, while
    /// one an authored link rewrite or a wikilink rewrite respells, whose
    /// author said what it names, is none.
    pub(crate) fn kept_by(
        &self,
        named: bool,
        target: Option<&TargetNaming>,
        normalizer: &PathNormalizer,
    ) -> bool {
        match &self.backlinks {
            Backlinks::Forbidden => !named,
            Backlinks::RewrittenTo(_) => {
                target.is_some_and(|target| rewrite_destination(target, normalizer).is_some())
            }
            Backlinks::LeftBroken => true,
        }
    }
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
        let mut rewritten: Vec<(usize, NormalizedPath)> = Vec::new();
        for &position in order {
            let kind = &operations[position].kind;
            match kind {
                OperationKind::CreateDocument { path, .. } => {
                    if let Some(file) = identity(path.as_str()) {
                        lineage.at_end.insert(file, None);
                    }
                }
                // Dormant carrier for NORN-298's planner: a creation by rule
                // names no path until planning expands it, one for one, into
                // a `create_document`, as a folder move expands into document
                // moves before anything composes (`super::expand`). The
                // call graph does not give this arm a path to follow yet
                // because that expansion has not landed, and composition
                // leaves the operation unresolved (`super::compose`).
                OperationKind::CreateByRule { .. } => {}
                // Every delete is a removal, so its link choice is read
                // though the content it removes is one the plan created; only
                // one removing a before-state can have links naming it.
                OperationKind::DeleteDocument { path, backlinks } => {
                    if let Some(file) = identity(path.as_str()) {
                        if let Some(drawn) = lineage.source(&file) {
                            lineage.removed.insert(drawn.from, lineage.removals.len());
                        }
                        lineage.removals.push(Removal {
                            position,
                            backlinks: backlinks.clone(),
                        });
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
                // A wikilink rewrite draws on nothing and edits nothing
                // itself: its cascade's rewrites, which planning generates
                // from what it names here, are edits in place of their
                // holders where the plan leaves them.
                OperationKind::RewriteWikilink { old, new } => {
                    lineage.retargets.push(Retarget {
                        position,
                        id: operations[position].id.clone(),
                        old: old.address().to_string(),
                        new: new.address().to_string(),
                    });
                }
                // An authored link rewrite is an edit in place of the
                // document it names where the plan leaves it, composing after
                // every operation with its holder's batch, so it acts on what
                // that document holds at the end of the plan: its source is
                // read once the walk is done.
                OperationKind::RewriteLink {
                    path, syntax, from, ..
                } => {
                    if let Some(file) = identity(path.as_str()) {
                        lineage.relinks.push(Relink {
                            position,
                            holder: file.clone(),
                            syntax: *syntax,
                            from: from.clone(),
                        });
                        rewritten.push((position, file));
                    }
                }
                // A folder move arrives expanded into the moves it makes.
                OperationKind::MoveFolder { .. } => {}
            }
        }
        for (position, file) in rewritten {
            if let Some(source) = lineage.source(&file) {
                lineage.edited.insert(position, source.from);
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

    /// The file the document standing at `from` before the plan stands at
    /// after it: where the plan's moves carry it, `from` itself where it
    /// stays — edited in place, or moved away and back — and `None` where no
    /// file holds it at the end of the plan.
    ///
    /// **Which document a name meant.** A name read before the plan and one
    /// read after it name one document where the first's document lands
    /// where the second names, so a wikilink rewrite reads here whether its
    /// two ends name one document across the plan's moves.
    pub(crate) fn landing(&self, from: &NormalizedPath) -> Option<NormalizedPath> {
        if let Some((to, _)) = self.carried_to(from) {
            return Some(to.clone());
        }
        self.source(from)
            .is_some_and(|drawn| drawn.from == *from)
            .then(|| from.clone())
    }

    /// The delete that removes the document standing at `from` before the
    /// plan — at `from`, or wherever the plan's moves carried it first — and
    /// `None` where no delete of the plan removes it.
    ///
    /// **What a delete's backlinks are.** A link that named the document
    /// before the plan names nothing of it after, so the delete reads here
    /// which of the links the store's resolution door reaches are its own,
    /// and what it rewrites them to.
    pub(crate) fn removed_by(&self, from: &NormalizedPath) -> Option<&Removal> {
        self.removed.get(from).map(|&at| &self.removals[at])
    }

    /// Every delete of the plan, in the order they compose — one removing a
    /// document the plan itself created among them.
    pub(crate) fn removals(&self) -> impl Iterator<Item = &Removal> + '_ {
        self.removals.iter()
    }

    /// Every wikilink rewrite of the plan, in the order it composes.
    pub(crate) fn retargets(&self) -> impl Iterator<Item = &Retarget> + '_ {
        self.retargets.iter()
    }

    /// Every authored link rewrite of the plan, in the order it composes.
    pub(crate) fn relinks(&self) -> impl Iterator<Item = &Relink> + '_ {
        self.relinks.iter()
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
    use norn_wire::{DocumentPath, ResolutionTarget, Resolves};

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

    /// **A rewriting delete is kept only where its `rewrite_to` names one
    /// document at a path a link can be respelled toward.** One document at
    /// a path both the vault's rule and the store's grammar read keeps it; a
    /// path either refuses — one the vault's rule does not read, or one
    /// whose leaf the store's grammar reduces to no stem — does not, nor
    /// does naming no document, as planning leaves each unresolved.
    #[test]
    fn a_rewriting_delete_is_kept_only_where_its_target_is_a_path_links_can_name() {
        let normalizer = PathNormalizer::for_sensitivity(CaseSensitivity::Sensitive);
        let removal = Removal {
            position: 0,
            backlinks: Backlinks::RewrittenTo(ResolutionTarget::new("c").expect("a target")),
        };
        let naming = |after: Resolves| TargetNaming::new(Resolves::none(), after, None);
        let one = |at: &str| Resolves::one(DocumentPath::new(at).expect("a document path"));
        assert!(removal.kept_by(false, Some(&naming(one("x/c.md"))), &normalizer));
        for unreadable in ["../c.md", "x/...md"] {
            assert!(
                !removal.kept_by(false, Some(&naming(one(unreadable))), &normalizer),
                "{unreadable}"
            );
        }
        assert!(!removal.kept_by(false, Some(&naming(Resolves::none())), &normalizer));
        assert!(!removal.kept_by(false, None, &normalizer));
    }
}
