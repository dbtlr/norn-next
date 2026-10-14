//! Move-following composition: one derived rewrite per holder, syntax and
//! old address, after the holder's ordinary fixes and bare-link fixes, at
//! the place the batch's settled routes leave it. The written address is
//! conditioned there; the replaced address where the finding read it.
//!
//! **Each addition is judged on the bytes the earlier fixes left.** No link
//! of the old address remaining makes no fix, leaving the finding for
//! `held_links` to settle. No spelling reaching the followed document
//! uniquely, or a text-layer refusal, is `link_not_rewritable` with its
//! reason. An addition bringing in missing required fields is
//! `brings_in_required_fields` with their defaults; any other refusal is
//! `judge_would_refuse`, including selecting a rule reading by value a link
//! a settled route may respell. An admitted rewrite below the threshold is
//! `below_threshold` with its operation.
//!
//! **Similarity answers use the same composition at suggested confidence.**
//! Their citations and below-threshold proposals carry the score note, and
//! both addresses carry the same conditions as a follow. Only a follow
//! carries a content-hash author condition. A suggestion below threshold
//! carries its score note.

use std::collections::BTreeMap;

use super::{
    Composed, Document, Made, Reckoned, Respellable, State, cited, respells, skip, verdict,
};
use crate::planner::links::{EntryKey, address, entry_key, skip_advisory, wire_family};
use crate::planner::repair::broken_links::{Answer, Answers, Asked, Evidence};
use norn_wire::{
    AuthorCondition, LinkKey, LinkRewrite, Operation, OperationKind, PlanCondition, Resolves,
    SkipReason,
};

impl Reckoned<'_> {
    /// Compose the single broken-link answers of this document onto `composed`, after
    /// its ordinary and bare-link fixes, as `composing` judges each addition
    /// and the batch's `resolutions` answer its candidate addresses. A group
    /// whose old links are gone makes no fix; an unwritable follow or a
    /// refused addition skips with its reason, and a rewrite below the
    /// threshold skips carrying its proposed operation.
    pub(super) fn broken_link_fixes(
        &self,
        composing: &Document<'_>,
        composed: &mut Composed,
        follows: &Answers,
        asked: &Asked,
        resolutions: &BTreeMap<EntryKey, Resolves>,
        respellable: Option<&Respellable>,
    ) {
        for (at, row) in self.rows.iter().enumerate() {
            if let Some(Err(skipped)) = follows.get(&row.id) {
                composed.decided[at] = Some(skipped.clone());
            }
        }
        let mut groups: BTreeMap<_, Vec<(usize, &Answer)>> = BTreeMap::new();
        for (at, row) in self.rows.iter().enumerate() {
            if let Some(Ok(follow)) = follows.get(&row.id) {
                let link = LinkKey::new(
                    composed.state.at.clone(),
                    follow.link.syntax,
                    &follow.link.address,
                );
                groups
                    .entry(entry_key(&link))
                    .or_default()
                    .push((at, follow));
            }
        }
        for (key, group) in groups {
            let (_, follow) = group[0];
            let candidates = &asked[&key];
            let spelling = crate::planner::link_spelling::first_resolving_spelling(
                candidates.iter().map(|link| link.address.clone()).collect(),
                |candidate| {
                    let link =
                        LinkKey::new(composed.state.at.clone(), follow.link.syntax, candidate);
                    resolutions.get(&entry_key(&link)) == Some(&Resolves::one(follow.to.clone()))
                },
            );
            let Some(to) = spelling else {
                for (at, answer) in &group {
                    composed.decided[*at] = Some(answer.skipped(
                        skip(&self.rows[*at], SkipReason::LinkNotRewritable).with_note(format!(
                            "no spelling resolves uniquely to `{}` from `{}`",
                            follow.to, composed.state.at
                        )),
                    ));
                }
                continue;
            };
            let old = follow.link.clone();
            let new = LinkKey::new(composed.state.at.clone(), follow.link.syntax, &to);
            let rewrite = LinkRewrite::new(
                composed.state.at.clone(),
                follow.link.syntax,
                &follow.link.address,
                &to,
            );
            let rewritten =
                match crate::planner::edit::rewritten(&composed.state.bytes, [&rewrite]) {
                    Ok(rewritten) => rewritten,
                    Err(reason) => {
                        for (at, answer) in &group {
                            composed.decided[*at] = Some(answer.skipped(
                                skip(&self.rows[*at], SkipReason::LinkNotRewritable).with_note(
                                    format!("the text layer refuses the rewrite: {reason}"),
                                ),
                            ));
                        }
                        continue;
                    }
                };
            if !rewritten.links.iter().any(|link| {
                wire_family(link.family) == follow.link.syntax
                    && address(link) == follow.link.address
            }) {
                continue;
            }
            if let Some(skipped) = rewritten.skipped.first() {
                let reason = skip_advisory(old.clone(), skipped.reason).as_str();
                for (at, answer) in &group {
                    composed.decided[*at] = Some(
                        answer.skipped(
                            skip(&self.rows[*at], SkipReason::LinkNotRewritable)
                                .with_note(format!("the text layer refuses the rewrite: {reason}")),
                        ),
                    );
                }
                continue;
            }
            let result = verdict(
                &composing.before,
                &composed.state.standing,
                &composed.state.at,
                &rewritten.bytes,
                composing.repairing.declared,
                composing.repairing.case,
            );
            let state = State::of(composed.state.at.clone(), rewritten.bytes, result.standing);
            if !result.introduced.is_empty() {
                for (at, answer) in &group {
                    composed.decided[*at] = composing
                        .introducing(&state, &result.introduced, &self.rows[*at], None)
                        .err()
                        .map(|skipped| answer.skipped(*skipped));
                }
                continue;
            }
            if let Some((field, (from, to))) = respellable.and_then(|links| {
                respells::reads_a_respellable_link(&state, links, composing.repairing)
            }) {
                let note = format!(
                    "the link brings `{}` under a rule reading `{field}` by value, whose link the \
                     move of `{from}` to `{to}` would respell",
                    state.at
                );
                for (at, answer) in &group {
                    composed.decided[*at] = Some(answer.skipped(
                        skip(&self.rows[*at], SkipReason::JudgeWouldRefuse).with_note(note.clone()),
                    ));
                }
                continue;
            }
            let operation = Operation::new(OperationKind::rewrite_link(
                composed.state.at.clone(),
                follow.link.syntax,
                &follow.link.address,
                to,
            ));
            let operation = match &follow.evidence {
                Evidence::Moved(hash) => {
                    operation.with_conditions(vec![AuthorCondition::content_hash(
                        follow.before.clone(),
                        hash.clone(),
                    )])
                }
                Evidence::Similar { .. } => operation,
            };
            if !composing.admits(follow.confidence()) {
                for (at, answer) in &group {
                    composed.decided[*at] = Some(
                        answer.skipped(
                            skip(&self.rows[*at], SkipReason::BelowThreshold)
                                .with_proposed(operation.clone()),
                        ),
                    );
                }
                continue;
            }
            let conditions = [old, new]
                .into_iter()
                .map(|link| {
                    let after = resolutions[&entry_key(&link)].clone();
                    PlanCondition::address_resolution(link, after)
                })
                .collect();
            for (at, _) in &group {
                composed.decided[*at] = None;
            }
            composed.made.push(Made::Followed {
                operation,
                cited: group
                    .iter()
                    .map(|(at, _)| cited(&self.rows[*at], follow.confidence(), follow.notes()))
                    .collect(),
                conditions,
            });
            composed.state = state;
        }
    }
}
