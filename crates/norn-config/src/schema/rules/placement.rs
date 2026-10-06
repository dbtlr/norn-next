//! Placement: whether the allowed paths of rules that select one document
//! leave it any path, and the ceiling that keeps deciding so bounded.
//!
//! A document's combined placement constraint is met only at a path every
//! contributing rule's `allowed_paths` admits, so the rules conflict where
//! their glob sets share no document path. Deciding that is the product of
//! the sets' glob automata and the document grammar's
//! ([`norn_wire::sets_share_a_document_path`]), whose states number at most a
//! constant times the product of the sets' weights. A rule's **weight**
//! ([`Rule::placement_weight`]) is the sum over its globs of each glob's
//! length in characters plus one; the product over the rules that select one
//! document together is what [`PLACEMENT_CEILING`] bounds.

use norn_wire::{CaseFold, Pattern, sets_share_a_document_path};

use super::Rule;

/// The most the allowed paths of rules that may select one document together
/// may weigh: the product of their weights, `2^18`.
///
/// A rule's weight is the sum over its `allowed_paths` globs of each glob's
/// length in characters plus one, which bounds the states of the automaton
/// the globs make together, so the product bounds the states the walk
/// deciding whether co-selecting rules leave a document any path visits —
/// the walk schema read takes over the rules a statically unavoidable
/// conflict groups, and that the rule findings (NORN-358) take over the rules
/// selecting each document. The schema refuses at read any neighbourhood of
/// rules whose weight could pass it.
pub const PLACEMENT_CEILING: u64 = 1 << 18;

/// The weight of `globs`: the sum over them of each glob's length in
/// characters plus one, saturating rather than wrapping.
pub(super) fn weight(globs: &[Pattern]) -> u64 {
    globs.iter().fold(0u64, |sum, glob| {
        let length = u64::try_from(glob.as_str().chars().count()).unwrap_or(u64::MAX);
        sum.saturating_add(length.saturating_add(1))
    })
}

/// The product of the weights of those of `rules` stating allowed paths,
/// saturating rather than wrapping.
pub(super) fn product_weight<'a>(rules: impl IntoIterator<Item = &'a Rule>) -> u64 {
    rules
        .into_iter()
        .filter(|rule| rule.allowed_paths.is_some())
        .fold(1u64, |product, rule| {
            product.saturating_mul(rule.placement_weight())
        })
}

/// Whether some document path is admitted by the allowed paths of every rule
/// in `rules` that states any; true where none does. `case` says how literal
/// letters compare. A path no document could stand at — `shared`, or one
/// holding a `..` segment — is no witness.
///
/// The walk is exact and its cost is a constant times the product of the
/// rules' weights, which a caller holds under [`PLACEMENT_CEILING`] before
/// asking: schema read refuses every neighbourhood that could pass it. It is
/// the one procedure deciding placement emptiness: schema read asks it of
/// each group of rules that always select together, and the rule findings
/// (NORN-358) ask it of the rules selecting each document.
pub(super) fn share_a_path(rules: &[&Rule], case: CaseFold) -> bool {
    let sets: Vec<&[Pattern]> = rules
        .iter()
        .filter_map(|rule| rule.allowed_paths.as_ref())
        .map(|allowed| allowed.paths.as_slice())
        .collect();
    debug_assert!(
        product_weight(rules.iter().copied()) <= PLACEMENT_CEILING,
        "a placement walk past the ceiling schema read holds every neighbourhood under"
    );
    sets.is_empty() || sets_share_a_document_path(&sets, case)
}
