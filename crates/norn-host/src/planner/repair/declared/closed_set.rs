//! Closed-set fixes: the member a value outside a closed set is replaced by.
//!
//! **Two levels answer a selected `field/not-one-of` finding.** The declared
//! candidates are the members the contributing rules' synonyms for the
//! offending value map it to. A rule that declares a `one_of` on the field
//! but no synonym for the value proposes nothing. The value is compared with a
//! synonym's written value by the field's typed equality, as the judge
//! compares an element ([`VaultSchema::equality_key`]): `4` and `4.0` are one
//! number under a `number` field, and `Play` and `#play` one tag under a tag
//! fold. The candidates agree when they name one member by the same equality.
//! The derived answer is the one member of the combined closed set that
//! differs from the value only by case and white space ([`derived::Folds`]),
//! where writing it changes the address of no wikilink the value holds
//! ([`derived::recases_a_link`]): a fold that would re-case a link is
//! [`SkipReason::NoDeclaredFix`], noted, and no derived answer beside a
//! declared one.
//!
//! **The strongest level present decides.** A declared candidate decides even
//! where the combined constraint or the judge refuses it: the derived answer
//! never replaces the owner's. Where a declared fix is made and the derived
//! answer differs, the citation notes it. Declared candidates that disagree
//! are [`SkipReason::Tie`], with every member and the rule proposing it, the
//! derived answer noted; a declared member the judge refuses, such as one
//! outside the combined closed set the co-selecting rules narrow the field
//! to, is [`SkipReason::JudgeWouldRefuse`], with every candidate, the derived
//! answer among them. With no declared candidate the derived answer decides:
//! one member is written, several are [`SkipReason::Tie`] with those members,
//! and none is [`SkipReason::NoDeclaredFix`], with the offending value. A
//! derived level the request's threshold does not admit decides nothing.
//!
//! **A field's offending values are fixed together** ([`elements`]): a
//! scalar field's one finding is a `set_frontmatter` of the member, and a list
//! field's findings compose into one `set_frontmatter` of the whole list.

use norn_config::schema::VaultSchema;
use norn_wire::{AuthoredValue, Confidence, SkipReason, SkippedCandidates, ValueCandidate};

use super::derived::{self, FoldCache, Folded};
use super::elements::{Offending, Proposed, Replacement, same_member, same_value};
use super::{Document, Proposal, candidates, head, skip, value_candidates};

/// What the closed set of the field `field` makes of the offending element
/// `offending`, folds served from `folds`.
pub(super) fn propose(
    document: &Document<'_>,
    field: &str,
    folds: &mut FoldCache,
    offending: &Offending<'_>,
) -> Proposed {
    let schema = document.schema();
    let row = offending.row;
    let rules: Vec<String> = offending.held.rules.iter().cloned().collect();
    let declared = proposals(schema, field, &rules, offending.text);
    let folded = folds
        .of(schema, field, &offending.held.rules)
        .answer(offending.text);
    let derived_admitted = document.admits(Confidence::Derived);
    // A fold rewrites only a string, and never re-cases a link.
    let (folded, recasing) = match folded {
        _ if !offending.string => (Folded::None, None),
        Folded::One(member) if derived::recases_a_link(offending.text, &member) => {
            (Folded::None, Some(member))
        }
        folded => (folded, None),
    };

    let Some((first, rest)) = declared.split_first() else {
        if let Some(member) = recasing {
            return Proposed::Skipped(Box::new(crate::planner::repair::skipped(row).with_note(
                format!(
                    "a case and white space fold would write `{member}`, which would re-case a \
                     link, and a fold never does"
                ),
            )));
        }
        return match folded {
            Folded::One(member) => Proposed::Replace(Replacement {
                candidates: derived_candidates(std::slice::from_ref(&member)),
                member: AuthoredValue::string(member),
                level: Confidence::Derived,
                notes: Vec::new(),
            }),
            Folded::Many(members) if derived_admitted => Proposed::Skipped(Box::new(
                skip(row, SkipReason::Tie).with_candidates(derived_candidates(&members)),
            )),
            Folded::Many(_) | Folded::None => {
                Proposed::Skipped(Box::new(crate::planner::repair::skipped(row)))
            }
        };
    };
    // The derived answer a declared fix is weighed against: the one member
    // the fold proves, where its level is pooled.
    let derived = match folded {
        Folded::One(member) if derived_admitted => Some(member),
        _ => None,
    };
    if rest
        .iter()
        .any(|proposal| !same_member(schema, field, &proposal.value, &first.value))
    {
        let tie = skip(row, SkipReason::Tie).with_candidates(candidates(&declared));
        return Proposed::Skipped(Box::new(match derived {
            Some(member) => tie.with_note(differing(&member)),
            None => tie,
        }));
    }
    let differs = derived
        .filter(|member| !same_member(schema, field, &first.value, &AuthoredValue::string(member)));
    Proposed::Replace(Replacement {
        member: first.value.clone(),
        level: Confidence::Declared,
        candidates: pooled(&declared, differs.as_slice()),
        notes: differs.iter().map(|member| differing(member)).collect(),
    })
}

/// The note a declared fix carries, or a declared tie, for a derived answer
/// that differs from it.
fn differing(derived: &str) -> String {
    format!("the schema's synonym decides; a case and white space fold would write `{derived}`")
}

/// The candidates a skip carries when the derived members `members` are the
/// ones in question: proposed by no named rule.
fn derived_candidates(members: &[String]) -> SkippedCandidates {
    pooled(&[], members)
}

/// Every declared proposal, with the rule proposing it, and then the derived
/// members `derived`, proposed by no named rule.
fn pooled(declared: &[Proposal], derived: &[String]) -> SkippedCandidates {
    value_candidates(
        declared
            .iter()
            .map(|proposal| {
                ValueCandidate::new(head(&proposal.value)).by_rule(proposal.rule.as_str())
            })
            .chain(
                derived
                    .iter()
                    .map(|member| ValueCandidate::new(norn_store::value_head(member))),
            ),
    )
}

/// The member each of the rules `contributing` to the field maps `offending`
/// to, in rule name order.
fn proposals(
    schema: &VaultSchema,
    field: &str,
    contributing: &[String],
    offending: &str,
) -> Vec<Proposal> {
    let mut proposals = Vec::new();
    for name in contributing {
        let Some(rule) = schema.rule(name) else {
            continue;
        };
        for (_, set) in rule.one_of().filter(|(declared, _)| *declared == field) {
            for (written, member) in set.synonym_values() {
                if same_value(schema, field, written, offending) {
                    proposals.push(Proposal {
                        value: member.clone(),
                        rule: name.clone(),
                        clocked: false,
                    });
                }
            }
        }
    }
    proposals
}
