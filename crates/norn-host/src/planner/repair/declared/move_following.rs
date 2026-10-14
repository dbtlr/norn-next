//! Move-following composition: one derived rewrite per holder, syntax and
//! old address, after the holder's ordinary fixes and bare-link fixes, at
//! the place the batch's settled routes leave it.
//!
//! **Each addition is judged on the bytes the earlier fixes left.** No link
//! of the old address remaining makes no fix, leaving the finding for
//! `held_links` to settle. No spelling reaching the followed document
//! uniquely, or a text-layer refusal, is `link_not_rewritable` with its
//! reason; an addition the judge refuses is `judge_would_refuse`. An admitted
//! rewrite below the threshold is `below_threshold` with its operation.

use std::collections::BTreeMap;

use super::{Composed, Document, Made, Reckoned, State, cited, skip, verdict};
use crate::planner::links::{EntryKey, address, entry_key, wire_family};
use crate::planner::repair::broken_links::{Asked, Follow, Follows};
use norn_wire::{
    AuthorCondition, Confidence, LinkKey, LinkRewrite, Operation, OperationKind, PlanCondition,
    Resolves, SkipReason,
};

impl Reckoned<'_> {
    /// Compose the unique follows of this document onto `composed`, after
    /// its ordinary and bare-link fixes, as `composing` judges each addition
    /// and the batch's `resolutions` answer its candidate addresses. A group
    /// whose old links are gone makes no fix; an unwritable follow or a
    /// refused addition skips with its reason, and a rewrite below the
    /// threshold skips carrying its proposed operation.
    pub(super) fn follow_fixes(
        &self,
        composing: &Document<'_>,
        composed: &mut Composed,
        follows: &Follows,
        asked: &Asked,
        resolutions: &BTreeMap<EntryKey, Resolves>,
    ) {
        for (at, row) in self.rows.iter().enumerate() {
            if let Some(Err(skipped)) = follows.get(&row.id) {
                composed.decided[at] = Some(skipped.clone());
            }
        }
        let mut groups: BTreeMap<_, Vec<(usize, &Follow)>> = BTreeMap::new();
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
                for (at, _) in &group {
                    composed.decided[*at] = Some(
                        skip(&self.rows[*at], SkipReason::LinkNotRewritable).with_note(format!(
                            "no spelling resolves uniquely to `{}` from `{}`",
                            follow.to, composed.state.at
                        )),
                    );
                }
                continue;
            };
            let old = LinkKey::new(
                composed.state.at.clone(),
                follow.link.syntax,
                &follow.link.address,
            );
            let new = LinkKey::new(composed.state.at.clone(), follow.link.syntax, &to);
            let rewrite = LinkRewrite::new(
                composed.state.at.clone(),
                follow.link.syntax,
                &follow.link.address,
                &to,
            );
            let rewritten = match crate::planner::edit::rewritten(&composed.state.bytes, [&rewrite])
            {
                Ok(rewritten) => rewritten,
                Err(reason) => {
                    for (at, _) in &group {
                        composed.decided[*at] = Some(
                            skip(&self.rows[*at], SkipReason::LinkNotRewritable).with_note(
                                format!("the text layer refuses the rewrite: {reason:?}"),
                            ),
                        );
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
                for (at, _) in &group {
                    composed.decided[*at] = Some(
                        skip(&self.rows[*at], SkipReason::LinkNotRewritable).with_note(format!(
                            "the text layer refuses the rewrite: {:?}",
                            skipped.reason
                        )),
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
            if !result.introduced.is_empty() {
                for (at, _) in &group {
                    composed.decided[*at] =
                        Some(skip(&self.rows[*at], SkipReason::JudgeWouldRefuse));
                }
                continue;
            }
            let operation = Operation::new(OperationKind::rewrite_link(
                composed.state.at.clone(),
                follow.link.syntax,
                &follow.link.address,
                to,
            ))
            .with_conditions(vec![AuthorCondition::content_hash(
                follow.to.clone(),
                follow.hash.clone(),
            )]);
            if !composing.admits(Confidence::Derived) {
                for (at, _) in &group {
                    composed.decided[*at] = Some(
                        skip(&self.rows[*at], SkipReason::BelowThreshold)
                            .with_proposed(operation.clone()),
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
                    .map(|(at, _)| cited(&self.rows[*at], Confidence::Derived, Vec::new()))
                    .collect(),
                conditions,
            });
            composed.state = State::of(composed.state.at.clone(), rewritten.bytes, result.standing);
        }
    }
}
