//! Move-following: the derived answer to a selected broken link whose
//! removed target's recorded bytes identify one live document.
//!
//! **A link's grammar decides which tombstones it reads.** A suffix wikilink
//! reads by suffix; a relative or rooted Markdown link reads the exact path
//! it names from its holder. A `vault://` wikilink reads both root paths its
//! name can reduce to, as live resolution does. The deaths are pooled,
//! distinct by path and in path order, stopping at two. Quarantine is
//! excluded, and ambiguity-ignore never narrows these reads.
//!
//! **One tombstone and one live hash holder give a follow.** Two tombstones
//! skip as `several_tombstones`; one without a recorded hash skips as
//! `no_recorded_hash`. No live document holding that hash is `no_holder`;
//! two are `several_holders`. The tombstone and holder reads stop at two. The
//! `no_recorded_hash` and `no_holder` notes name the tombstone's path; each
//! several note names the first two paths and says "at least two", with no
//! candidate head. No tombstone, or an address no read applies to, keeps
//! `no_declared_fix`. Every skip keeps the value its finding judged. The
//! followed document need not lie within the repair's selection.
//!
//! **A follow is spelled over the batch's settled routes and composed last.**
//! A followed document a route of the batch moves is spelled toward where
//! the route lands it.
//! All candidates and replaced addresses share the applier's resolution
//! probe. The fix records the written address resolving to the followed
//! document from where the holder lands, and the replaced address's
//! after-state resolution from where the finding read it, and carries
//! the tombstone's hash as an author condition on that document. A spelling
//! that never reaches it uniquely, or a text-layer refusal, skips as
//! `link_not_rewritable`, with that reason in its note.
//!
//! **Two declared limits bound the evidence a follow uses.** A rebuild
//! keeps no tombstones. A norn move whose cascade rewrote the moved
//! document's own links changed its hash, so it skips `no_holder`.
//!
//! **Without a unique follow, similarity suggests the single best leaf stem.**
//! The query removes the protocol, takes the last `/` segment and drops a
//! trailing `.md`. At least three folded Unicode scalars are admitted;
//! shorter queries skip `too_short` without reading the index. Jaccard over
//! padded folded trigram sets has a fixed cutoff of 0.5. Equal best scores
//! skip `tie`; a needed posting past cap 256 skips `too_common`. The fold is
//! only a suggestion's, never a resolution rule (ADR 0039). With no tombstone
//! and no candidate reaching the cutoff, `no_declared_fix` still stands.
//!
//! **A suggestion shares a follow's spelling and composition.** It proposes
//! a `suggested` rewrite with the score note, guarding the written and
//! replaced addresses without a candidate content-hash author condition.
//! A unique follow outranks similarity even when composition later skips it.
//! When both fail, the move skip's reason and value stand, and its note
//! appends `; similarity: <outcome>`. A suggestion below threshold carries
//! its operation and score note, followed by any move skip's reason and note.

use std::collections::BTreeMap;

use super::Reading;
use crate::planner::links::{EntryKey, address, entry_key, probe_of, spelled};
use norn_wire::{
    ContentHash, DocumentPath, FindingKind, FindingRow, LinkKey, SkipReason, SkippedFinding,
};

/// A broken link's single destination, with the evidence its rewrite rests on.
pub(super) struct Answer {
    /// The finding's link, held where its document stood before any route.
    pub(super) link: LinkKey,
    /// The destination's before-state path, where a moved hash is observed.
    pub(super) before: DocumentPath,
    /// The destination, mapped to where the settled routes land it.
    pub(super) to: DocumentPath,
    /// A moved hash or a suggestion's score and any failed move decision.
    pub(super) evidence: Evidence,
}

/// Selected broken findings with an answer or skip, keyed by finding id;
/// no entry when neither a tombstone nor a qualifying similarity answer exists.
pub(super) type Answers = BTreeMap<u64, Result<Answer, SkippedFinding>>;
/// The candidate addresses prepared for the batch's one resolution probe,
/// keyed by the holder where it lands, syntax and old address. Candidate
/// probes use that landed holder; the replaced address's probe uses the
/// holder where the finding read it.
pub(super) type Asked = BTreeMap<EntryKey, Vec<LinkKey>>;

/// The follows and skips of the selected broken findings `rows`, reading
/// each stored link of `links` through `vault`: the tombstones its address
/// names, then the live holders of the one tombstone's recorded hash. The
/// selection narrows the findings, never the documents a follow may name.
fn read_moves<R: Reading>(
    rows: &[FindingRow],
    links: &BTreeMap<u64, norn_store::FindingLink>,
    vault: &R,
) -> Result<Answers, R::Error> {
    let mut follows = BTreeMap::new();
    for row in rows.iter().filter(|row| row.kind == FindingKind::Broken) {
        let Some(found) = links.get(&row.id) else {
            continue;
        };
        let link = LinkKey::new(
            row.path.clone(),
            crate::planner::links::wire_family(found.family),
            &found.address,
        );
        let tombstones = vault.tombstones(&link)?;
        let [tombstone] = tombstones.as_slice() else {
            if let [first, second] = tombstones.as_slice() {
                follows.insert(
                    row.id,
                    Err(
                        super::skip(row, SkipReason::SeveralTombstones).with_note(format!(
                            "at least two tombstones: `{}` and `{}`",
                            first.path.as_str(),
                            second.path.as_str()
                        )),
                    ),
                );
            }
            continue;
        };
        let Some(hash) = &tombstone.last_content_hash else {
            follows.insert(
                row.id,
                Err(
                    super::skip(row, SkipReason::NoRecordedHash).with_note(format!(
                        "`{}` recorded no content hash",
                        tombstone.path.as_str()
                    )),
                ),
            );
            continue;
        };
        let holders = vault.live_holders(hash)?;
        let [to] = holders.as_slice() else {
            if holders.is_empty() {
                follows.insert(
                    row.id,
                    Err(super::skip(row, SkipReason::NoHolder).with_note(format!(
                        "no live document holds the hash recorded at `{}`",
                        tombstone.path.as_str()
                    ))),
                );
            } else if let [first, second] = holders.as_slice() {
                follows.insert(
                    row.id,
                    Err(
                        super::skip(row, SkipReason::SeveralHolders).with_note(format!(
                            "at least two live holders: `{first}` and `{second}`"
                        )),
                    ),
                );
            }
            continue;
        };
        follows.insert(
            row.id,
            Ok(Answer {
                link,
                before: to.clone(),
                to: to.clone(),
                evidence: Evidence::Moved(
                    ContentHash::new(format!("sha256:{hash}")).expect("a recorded content hash"),
                ),
            }),
        );
    }
    Ok(follows)
}

/// The evidence that determines confidence and author conditions of a link answer.
pub(super) enum Evidence {
    Moved(ContentHash),
    Similar {
        note: String,
        following_skip: Option<Box<SkippedFinding>>,
    },
}

impl Answer {
    pub(super) fn confidence(&self) -> norn_wire::Confidence {
        match self.evidence {
            Evidence::Moved(_) => norn_wire::Confidence::Derived,
            Evidence::Similar { .. } => norn_wire::Confidence::Suggested,
        }
    }

    pub(super) fn notes(&self) -> Vec<String> {
        match &self.evidence {
            Evidence::Moved(_) => Vec::new(),
            Evidence::Similar { note, .. } => vec![note.clone()],
        }
    }

    /// Preserve a failed move's decision when its weaker answer cannot compose.
    pub(super) fn skipped(&self, mut skipped: SkippedFinding) -> SkippedFinding {
        if let Evidence::Similar {
            note,
            following_skip,
        } = &self.evidence
        {
            if skipped.reason == norn_wire::SkipReason::BelowThreshold {
                skipped.note = Some(match following_skip {
                    Some(failed) => format!(
                        "{note}; move-following: {}: {}",
                        reason_name(failed.reason),
                        failed.note.as_deref().unwrap_or_default()
                    ),
                    None => note.clone(),
                });
            } else if let Some(failed) = following_skip {
                return both_fail(
                    failed.as_ref().clone(),
                    &format!(
                        "{}: {}",
                        reason_name(skipped.reason),
                        skipped.note.as_deref().unwrap_or_default()
                    ),
                );
            }
        }
        skipped
    }
}

/// The floor is measured after the suggestion's fold, in Unicode scalars.
fn admits_stem(stem: &str) -> bool {
    norn_store::similarity::fold(stem).chars().count() >= 3
}

fn reason_name(reason: SkipReason) -> String {
    serde_json::to_value(reason)
        .expect("a skip reason serializes")
        .as_str()
        .expect("a skip reason serializes as a string")
        .to_owned()
}

fn both_fail(mut failed: SkippedFinding, outcome: &str) -> SkippedFinding {
    failed.note = Some(format!(
        "{}; similarity: {outcome}",
        failed.note.as_deref().unwrap_or_default()
    ));
    failed
}

/// Read move-following first, then suggest only for findings with no unique follow.
pub(super) fn read<R: Reading>(
    rows: &[FindingRow],
    links: &BTreeMap<u64, norn_store::FindingLink>,
    vault: &R,
) -> Result<Answers, R::Error> {
    use norn_store::StemMatches;
    use norn_wire::{Candidate, CandidateHead, SkippedCandidates};
    let mut answers = read_moves(rows, links, vault)?;
    for row in rows.iter().filter(|row| row.kind == FindingKind::Broken) {
        if answers.get(&row.id).is_some_and(Result::is_ok) {
            continue;
        }
        let Some(found) = links.get(&row.id) else {
            continue;
        };
        let link = LinkKey::new(
            row.path.clone(),
            crate::planner::links::wire_family(found.family),
            &found.address,
        );
        let (_, target) = link.protocol_and_stem();
        let leaf = target.rsplit('/').next().expect("a leaf segment");
        let stem = leaf.strip_suffix(".md").unwrap_or(leaf);
        let following_skip = answers.remove(&row.id).and_then(Result::err);
        let query_skip = |reason, note: String| {
            super::skip(row, reason)
                .with_value(norn_wire::ValueHead::of(
                    stem,
                    crate::planner::compose::content_hash(stem.as_bytes()),
                ))
                .with_note(note)
        };
        let answer = if !admits_stem(stem) {
            Err(query_skip(
                SkipReason::TooShort,
                "the shortest admitted query is 3 characters".into(),
            ))
        } else {
            let retrieved = vault.similar_stems(stem)?;
            match retrieved.matches {
                StemMatches::TooCommon { trigram } => Err(query_skip(
                    SkipReason::TooCommon,
                    format!(
                        "trigram `{trigram}` exceeds the postings cap {}",
                        norn_store::similarity::POSTINGS_CAP
                    ),
                )),
                StemMatches::Candidates(candidates) => {
                    let mut best = Vec::new();
                    let mut score = (0, 1);
                    for candidate in candidates {
                        let numerator = candidate.shared;
                        let denominator =
                            retrieved.query_trigrams + candidate.candidate_trigrams - numerator;
                        if 2 * numerator < denominator {
                            continue;
                        }
                        match (numerator * score.1).cmp(&(score.0 * denominator)) {
                            std::cmp::Ordering::Greater => {
                                best.clear();
                                score = (numerator, denominator);
                                best.push(candidate);
                            }
                            std::cmp::Ordering::Equal => best.push(candidate),
                            std::cmp::Ordering::Less => {}
                        }
                    }
                    let note = format!(
                        "similarity {:.2} (trigram, cutoff 0.50)",
                        score.0 as f64 / score.1 as f64
                    );
                    if best.is_empty() {
                        if let Some(failed) = following_skip {
                            answers.insert(
                                row.id,
                                Err(both_fail(failed, "no stem reaches the cutoff 0.50")),
                            );
                        }
                        continue;
                    }
                    if best.len() == 1 {
                        let to = DocumentPath::new(best[0].path.as_str())
                            .expect("a candidate document path");
                        Ok(Answer {
                            link,
                            before: to.clone(),
                            to,
                            evidence: Evidence::Similar {
                                note,
                                following_skip: following_skip.clone().map(Box::new),
                            },
                        })
                    } else {
                        best.sort_by(|a, b| a.path.cmp(&b.path));
                        let total = best.len() as u64;
                        let head = CandidateHead::new(
                            best.into_iter().map(|candidate| {
                                let path = DocumentPath::new(candidate.path.as_str())
                                    .expect("a candidate document path");
                                let suffix = path
                                    .as_str()
                                    .strip_suffix(".md")
                                    .unwrap_or(path.as_str())
                                    .to_string();
                                Candidate::new(path, suffix)
                            }),
                            total,
                        )
                        .expect("a candidate head");
                        Err(super::skip(row, SkipReason::Tie)
                            .with_candidates(SkippedCandidates::documents(head))
                            .with_note(format!("tie at {note}")))
                    }
                }
            }
        };
        let answer = answer.map_err(|skipped| match &following_skip {
            Some(failed) => both_fail(
                failed.clone(),
                skipped.note.as_deref().expect("a similarity skip note"),
            ),
            None => skipped,
        });
        answers.insert(row.id, answer);
    }
    Ok(answers)
}

/// Add to `probes` every candidate spelling of `follow` and its replaced
/// address, and record the candidates in `asked`. Candidates are probed
/// from where `holder` lands; the replaced address from where the finding
/// read it ([`Answer::link`]). One holder, syntax and old address is prepared
/// once, however many selected findings name it.
pub(super) fn ask(
    follow: &Answer,
    holder: &DocumentPath,
    asked: &mut Asked,
    probes: &mut Vec<LinkKey>,
) {
    let landed = LinkKey::new(holder.clone(), follow.link.syntax, &follow.link.address);
    let key = entry_key(&landed);
    if asked.contains_key(&key) {
        return;
    }
    let probe = probe_of(&landed).expect("a followed link has a stored syntax");
    let candidates: Vec<_> = crate::planner::link_spelling::candidate_spellings(
        &probe.after_holder,
        &probe.link,
        &crate::planner::links::stored_path(&follow.to),
    )
    .into_iter()
    .map(|candidate| {
        LinkKey::new(
            holder.clone(),
            landed.syntax,
            address(&spelled(&probe.link, &candidate)),
        )
    })
    .collect();
    probes.push(follow.link.clone());
    probes.extend(candidates.iter().cloned());
    asked.insert(key, candidates);
}

#[cfg(test)]
mod tests {
    /// **Every admitted stem length requires at least one shared trigram across distinct, repeated and folded shapes.**
    #[test]
    fn every_admitted_stem_length_requires_at_least_one_shared_trigram() {
        use norn_store::similarity::{required_shared, trigrams};
        for length in 3..=128 {
            let distinct: String = (0..length)
                .map(|n| char::from_u32(0x4e00 + n).unwrap())
                .collect();
            let repeated = "a".repeat(length as usize);
            let mixed_nfd: String = (0..length)
                .map(|n| if n % 2 == 0 { "E\u{301}" } else { "a" })
                .collect();
            for stem in [distinct, repeated, mixed_nfd] {
                assert!(super::admits_stem(&stem), "length {length}: {stem}");
                assert!(required_shared(trigrams(&stem).len()) >= 1);
            }
        }
    }
}
