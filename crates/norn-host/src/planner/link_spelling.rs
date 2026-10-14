//! **A link is respelled in its own style, and only to a spelling that reads
//! back.** The new address is the shortest spelling of the link's
//! destination — the file the moved document lands at, the delete's target,
//! or the wikilink rewrite's `new` — the link's syntax and protocol can
//! write: for a bare wikilink
//! the shortest suffix of the destination that names it alone with every
//! target of the plan at its after-state — at least two segments where the
//! link was written path-qualified, so it stays so — written with the
//! document extension exactly where the link was; for a `vault://` wikilink
//! the destination's root path, in that same style; and for a Markdown link the
//! destination's path from the holder's folder, or from the root where the
//! link was written from the root, in the link's own percent-escaping. Each
//! candidate is probed through the same door, from the holder where it stands
//! after the plan, and the first that resolves to exactly the destination is
//! written.

use super::links::wire_family;
use norn_store::{DocumentPath, LinkFact};
use norn_wire::{DOCUMENT_EXTENSION, LinkAddress};

/// Every target spelling `link` could take to name `to` from `holder`,
/// where the plan leaves it, in the order tried by the shared rule: a
/// wikilink's suffixes shortest first, at least two segments where the link
/// was path-qualified, with the extension exactly where it had one; a
/// `vault://` wikilink's root path in that style; or a Markdown link's path
/// from the holder's folder, or the root where written rooted, in the link's
/// percent-escaping. The first candidate resolving to the destination alone
/// is written ([`first_resolving_spelling`]).
pub(crate) fn candidate_spellings(
    holder: &norn_store::DocumentPath,
    link: &LinkFact,
    to: &DocumentPath,
) -> Vec<String> {
    let written_with_extension = names_the_extension(&link.target);
    match LinkAddress::of(
        wire_family(link.family),
        link.protocol.as_deref(),
        &link.target,
    ) {
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
            styled(written_with_extension, with, without)
        }
        LinkAddress::RootedName(_) => {
            let path = to.as_str().to_string();
            let stem = strip_extension(&path).map(str::to_string);
            styled(
                written_with_extension,
                vec![path],
                stem.into_iter().collect(),
            )
        }
        LinkAddress::Relative(_) => {
            norn_store::relative_spelling(holder, to.as_str(), &link.target)
                .into_iter()
                .collect()
        }
        LinkAddress::Rooted(_) => norn_store::rooted_spelling(to.as_str(), &link.target)
            .into_iter()
            .collect(),
        LinkAddress::Elsewhere | LinkAddress::HoldingDocument => Vec::new(),
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
