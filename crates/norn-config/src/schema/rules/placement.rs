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

use std::collections::HashMap;
use std::fmt;
use std::sync::{PoisonError, RwLock};

use norn_wire::{CaseFold, Pattern, sets_share_a_document_path};

use super::super::VaultSchema;
use super::{Rule, RuleWork};

/// The most the allowed paths of rules that may select one document together
/// may weigh: the product of their weights, `2^18`.
///
/// A rule's weight is the sum over its `allowed_paths` globs of each glob's
/// length in characters plus one, which bounds the states of the automaton
/// the globs make together, so the product bounds the states the walk
/// deciding whether co-selecting rules leave a document any path visits —
/// the walk schema read takes over each rule's allowed paths alone and over
/// the rules a statically unavoidable conflict groups, and that rule
/// judgment takes over the two or more placement rules selecting a
/// misplaced document, once per set of them ([`VaultSchema::judge`]). The
/// schema refuses at read any neighbourhood of rules whose weight could pass
/// it.
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
/// each rule stating allowed paths alone and of each group of rules that
/// always select together, and rule judgment asks it of the two or more
/// placement rules selecting a misplaced document, through the schema's memo
/// of verdicts ([`PlacementVerdicts`]).
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

/// The placement verdicts a schema has reached: for each set of rules stating
/// allowed paths, by name, and each fold, whether their allowed paths share a
/// document path.
///
/// **A memo, not part of the model.** The verdict is a function of the set and
/// the fold alone, which the schema's bytes fix, so the memo changes no answer
/// and is no part of the model's identity: two schemas compare equal whatever
/// either has memoized, and a clone starts empty. Rule judgment fills it the
/// first time a misplaced document's set is decided, and every later document
/// selected by the same set reads the verdict instead of walking, so a
/// derivation pays the walk once per set rather than once per document.
///
/// **It is safe under concurrent readers.** Judgments on several threads read
/// and fill it through a lock; two that miss one set together each walk it and
/// write the same verdict. A lock a panicking holder poisoned still holds
/// verdicts that were whole when written, so it is read through the poison.
///
/// It holds at most one entry per set of placement rules some judged document
/// was selected by, which no document count bounds tighter than the vault's
/// own: every entry is the names of a set and one bit.
#[derive(Default)]
pub(in crate::schema) struct PlacementVerdicts {
    held: RwLock<HashMap<(CaseFold, Vec<String>), bool>>,
}

impl Clone for PlacementVerdicts {
    fn clone(&self) -> Self {
        PlacementVerdicts::default()
    }
}

impl PartialEq for PlacementVerdicts {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

impl Eq for PlacementVerdicts {}

impl fmt::Debug for PlacementVerdicts {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let held = self.held.read().unwrap_or_else(PoisonError::into_inner);
        formatter
            .debug_struct("PlacementVerdicts")
            .field("held", &held.len())
            .finish()
    }
}

impl VaultSchema {
    /// Whether the allowed paths of `rules`, every one stating some, share a
    /// document path under `case`: the memoized verdict where one was
    /// reached, and the walk ([`share_a_path`]) where none was, tallied in
    /// `work` either way.
    pub(super) fn placement_shared(
        &self,
        rules: &[&Rule],
        case: CaseFold,
        work: &mut RuleWork,
    ) -> bool {
        let key = (
            case,
            rules
                .iter()
                .map(|rule| rule.name.clone())
                .collect::<Vec<_>>(),
        );
        let held = self
            .placement_verdicts
            .held
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&key)
            .copied();
        if let Some(shared) = held {
            work.placement_verdicts_reused += 1;
            return shared;
        }
        work.placement_walks += 1;
        work.placement_weight = work
            .placement_weight
            .saturating_add(product_weight(rules.iter().copied()));
        let shared = share_a_path(rules, case);
        self.placement_verdicts
            .held
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(key, shared);
        shared
    }
}
