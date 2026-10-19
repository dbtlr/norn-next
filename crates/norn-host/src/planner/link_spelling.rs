//! **A link keeps its authored style and takes the first unique spelling.**
//! The destination is where a moved document lands, a delete's target, a
//! wikilink rewrite's `new`, or the document a move-following fix follows.
//! This module orders candidate spellings and picks the first the caller's
//! resolution answer reaches uniquely. A suffix-addressed link, in either
//! family (ADR 0040), keeps its suffix style and extension, a Markdown one
//! its percent-escaping too; a `vault://` wikilink keeps its root style and
//! extension; a Markdown path keeps its relative or rooted style and
//! percent-escaping ([`candidate_spellings`]). A wikilink written as a
//! relative or rooted path is given no spelling yet, so a change that would
//! respell one skips it.
//!
//! **The callers probe the candidates.** The cascade and repair ask the
//! link index over the plan's after-state, repair through the applier's
//! `resolved_addresses` probe, and supply those answers here.

use super::links::wire_family;
use norn_store::{DocumentPath, LinkFact};
use norn_wire::{DOCUMENT_EXTENSION, LinkAddress, LinkFamily};

/// Every target spelling `link` could take to name `to` from `holder`,
/// where the plan leaves it, in the order tried by the shared rule: a
/// suffix-addressed link's suffixes shortest first, at least two segments
/// where the link was path-qualified, with the extension exactly where it
/// had one, a Markdown destination's in its percent-escaping; a `vault://`
/// wikilink's root path in that style; or a Markdown link's path from the
/// holder's folder, or the root where written rooted or under `vault://`,
/// in the link's percent-escaping. The first candidate resolving to the
/// destination alone is written ([`first_resolving_spelling`]).
pub(crate) fn candidate_spellings(
    holder: &norn_store::DocumentPath,
    link: &LinkFact,
    to: &DocumentPath,
) -> Vec<String> {
    let family = wire_family(link.family);
    match LinkAddress::of(family, link.protocol.as_deref(), &link.target) {
        LinkAddress::Suffix(target) => {
            let least = if target.contains('/') {
                2.min(to.depth())
            } else {
                1
            };
            let leaf = to.as_str().rsplit('/').next().unwrap_or_default();
            let (with, without): (Vec<String>, Vec<String>) = to
                .suffix_spellings()
                .filter(|spelling| spelling.split('/').count() >= least)
                .partition(|spelling| spelling.ends_with(leaf) && leaf != to.stem());
            let spellings = styled(names_the_extension(&target), with, without);
            if family == LinkFamily::Markdown {
                spellings
                    .iter()
                    .map(|spelling| norn_store::suffix_spelling(spelling, &link.target))
                    .collect()
            } else {
                spellings
            }
        }
        LinkAddress::RootedName(target) if family == LinkFamily::Wikilink => {
            let path = to.as_str().to_string();
            let stem = strip_extension(&path).map(str::to_string);
            styled(
                names_the_extension(&target),
                vec![path],
                stem.into_iter().collect(),
            )
        }
        LinkAddress::Relative(_) if family == LinkFamily::Markdown => {
            norn_store::relative_spelling(holder, to.as_str(), &link.target)
                .into_iter()
                .collect()
        }
        LinkAddress::Rooted(_) | LinkAddress::RootedName(_) if family == LinkFamily::Markdown => {
            norn_store::rooted_spelling(to.as_str(), &link.target)
                .into_iter()
                .collect()
        }
        LinkAddress::Relative(_)
        | LinkAddress::Rooted(_)
        | LinkAddress::RootedName(_)
        | LinkAddress::Elsewhere
        | LinkAddress::Unnameable(_)
        | LinkAddress::HoldingDocument => Vec::new(),
    }
}

/// **The first candidate resolving uniquely to the destination wins.**
/// The caller supplies answers from its one batched index judgment.
pub(crate) fn first_resolving_spelling(
    candidates: Vec<String>,
    mut reaches: impl FnMut(&str) -> bool,
) -> Option<String> {
    candidates.into_iter().find(|candidate| reaches(candidate))
}

/// The spellings in the style the link was written in: `with` the document
/// extension where it was written with it, else `without`. The other style
/// is never a fallback — a link whose own style spells nothing that reads
/// back is unrepresentable, not given an extension its author did not write
/// or stripped of one they did.
fn styled(with_extension: bool, with: Vec<String>, without: Vec<String>) -> Vec<String> {
    if with_extension { with } else { without }
}

/// Whether `target`'s last segment carries the document extension, in any
/// ASCII case: how a wikilink written with its extension is told apart.
fn names_the_extension(target: &str) -> bool {
    strip_extension(target).is_some()
}

/// `path` without the document extension its last segment carries, in any
/// ASCII case; `None` where it carries none.
fn strip_extension(path: &str) -> Option<&str> {
    let leaf = path.rsplit('/').next().unwrap_or(path);
    let dot = leaf.rfind('.').filter(|&dot| dot > 0)?;
    leaf[dot + 1..]
        .eq_ignore_ascii_case(DOCUMENT_EXTENSION)
        .then(|| &path[..path.len() - (leaf.len() - dot)])
}
