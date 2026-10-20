//! Derived rooted repairs and move-following: the derived answer to a selected broken link whose
//! removed target's recorded bytes identify one live document.
//!
//! **A link's address decides which tombstones it reads.** A suffix-addressed
//! link of either family reads by suffix; a `./`, `../` or `/` link of either
//! family reads the exact path it names from its holder. A `vault://` link
//! reads both root paths its name can reduce to, as live resolution does. The deaths are pooled,
//! distinct by path and in path order, stopping at two. Quarantine is
//! excluded, and ambiguity-ignore never narrows these reads.
//!
//! **One tombstone and one live hash holder give a follow.** Two tombstones
//! skip as `several_tombstones`; one without a recorded hash skips as
//! `no_recorded_hash`. No live document holding that hash is `no_holder`;
//! two are `several_holders`. The tombstone and holder reads stop at two. The
//! `no_recorded_hash` and `no_holder` notes name the tombstone's path; each
//! several note names the first two paths and says "at least two", with no
//! candidate head. No tombstone, or an address no move read applies to, goes
//! to the similarity read. Every move skip keeps the value its finding judged. The
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
//! **Without a tombstone, similarity suggests the single best leaf stem.**
//! The query takes the protocol-stripped address's last `/` segment, decodes
//! it for Markdown and drops the document extension in any ASCII case.
//! A failed decoding stays written. At least three folded Unicode scalars are admitted;
//! shorter queries skip `too_short` without reading the index. Jaccard over
//! padded folded trigram sets uses [`norn_store::similarity::CUTOFF`]. Equal best scores
//! skip `tie`; a needed posting past cap 256 skips `too_common`. The fold is
//! only a suggestion's, never a resolution rule (ADR 0039). The finding's
//! holder is excluded before scoring, so no suggestion names its own document.
//! With no tombstone and no candidate reaching the cutoff, `no_declared_fix`
//! still stands.
//!
//! **A suggestion shares a follow's spelling and composition.** It proposes
//! a `suggested` rewrite with the score note, guarding the written and
//! replaced addresses without a candidate content-hash author condition.
//! A unique follow never consults similarity, even when composition skips it.
//! A tombstone records a deleted target, so a move-following skip stands
//! with its own note and no similarity outcome, and the link stays broken.
//! Quarantine tombstones are not read and count as none. A suggestion below
//! threshold carries its operation and score note.

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
    /// A moved hash, a resolving vault scheme, or a suggestion's score note.
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
    VaultScheme,
    Similar { note: String },
}

impl Answer {
    pub(super) fn confidence(&self) -> norn_wire::Confidence {
        match self.evidence {
            Evidence::Moved(_) | Evidence::VaultScheme => norn_wire::Confidence::Derived,
            Evidence::Similar { .. } => norn_wire::Confidence::Suggested,
        }
    }

    pub(super) fn notes(&self) -> Vec<String> {
        match &self.evidence {
            Evidence::Moved(_) | Evidence::VaultScheme => Vec::new(),
            Evidence::Similar { note } => vec![note.clone()],
        }
    }

    /// A suggestion below the threshold retains the score on its proposal.
    pub(super) fn skipped(&self, mut skipped: SkippedFinding) -> SkippedFinding {
        if skipped.reason == SkipReason::BelowThreshold
            && let Evidence::Similar { note } = &self.evidence
        {
            skipped.note = Some(note.clone());
        }
        skipped
    }
}

/// The floor is measured after the suggestion's fold, in Unicode scalars.
fn admits_stem(stem: &str) -> bool {
    norn_store::similarity::fold(stem).chars().count() >= 3
}

/// Read resolving vault-scheme fixes and move-following, then similarity
/// only for broken links without a tombstone.
pub(super) fn read<R: Reading>(
    rows: &[FindingRow],
    links: &BTreeMap<u64, norn_store::FindingLink>,
    vault: &R,
) -> Result<Answers, R::Error> {
    use norn_store::StemMatches;
    use norn_wire::SkippedCandidates;
    let mut answers = read_moves(rows, links, vault)?;
    for row in rows
        .iter()
        .filter(|row| row.kind == FindingKind::VaultScheme)
    {
        let Some(found) = links.get(&row.id) else {
            continue;
        };
        let [target] = row.head.candidates() else {
            continue;
        };
        answers.insert(
            row.id,
            Ok(Answer {
                link: LinkKey::new(
                    row.path.clone(),
                    crate::planner::links::wire_family(found.family),
                    &found.address,
                ),
                before: target.path.clone(),
                to: target.path.clone(),
                evidence: Evidence::VaultScheme,
            }),
        );
    }
    let mut queried = BTreeMap::new();
    for row in rows.iter().filter(|row| row.kind == FindingKind::Broken) {
        if answers.contains_key(&row.id) {
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
        let stem = norn_store::query_leaf(&link);
        let stem = stem.as_str();
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
            let retrieved = match queried.entry(stem.to_string()) {
                std::collections::btree_map::Entry::Occupied(entry) => entry.into_mut(),
                std::collections::btree_map::Entry::Vacant(entry) => {
                    entry.insert(vault.similar_stems(stem)?)
                }
            };
            match &retrieved.matches {
                StemMatches::TooCommon { trigram } => Err(query_skip(
                    SkipReason::TooCommon,
                    format!(
                        "trigram `{trigram}` exceeds the postings cap {}",
                        norn_store::similarity::POSTINGS_CAP
                    ),
                )),
                StemMatches::Candidates(candidates) => {
                    let mut best = Vec::new();
                    let mut score = norn_store::similarity::jaccard_counts(0, 1, 0);
                    for candidate in candidates
                        .iter()
                        .filter(|candidate| candidate.path.as_str() != row.path.as_str())
                    {
                        let candidate_score = norn_store::similarity::jaccard_counts(
                            candidate.shared,
                            retrieved.query_trigrams,
                            candidate.candidate_trigrams,
                        );
                        if !candidate_score.reaches_cutoff() {
                            continue;
                        }
                        match candidate_score.cmp(&score) {
                            std::cmp::Ordering::Greater => {
                                best.clear();
                                score = candidate_score;
                                best.push(candidate);
                            }
                            std::cmp::Ordering::Equal => best.push(candidate),
                            std::cmp::Ordering::Less => {}
                        }
                    }
                    let note = format!(
                        "similarity {score} (trigram, cutoff {})",
                        norn_store::similarity::CUTOFF
                    );
                    if best.is_empty() {
                        continue;
                    }
                    if best.len() == 1 {
                        let to = DocumentPath::new(best[0].path.as_str())
                            .expect("a candidate document path");
                        Ok(Answer {
                            link,
                            before: to.clone(),
                            to,
                            evidence: Evidence::Similar { note },
                        })
                    } else {
                        best.sort_by(|a, b| a.path.cmp(&b.path));
                        let total = best.len() as u64;
                        let paths = best
                            .into_iter()
                            .take(norn_store::CANDIDATE_HEAD)
                            .map(|candidate| {
                                DocumentPath::new(candidate.path.as_str())
                                    .expect("a candidate document path")
                            })
                            .collect::<Vec<_>>();
                        let head = vault.candidate_head(&paths, total)?;
                        Err(super::skip(row, SkipReason::Tie)
                            .with_candidates(SkippedCandidates::documents(head))
                            .with_note(format!("tie at {note}")))
                    }
                }
            }
        };
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
    let mut probe = probe_of(&landed).expect("a followed link has a stored syntax");
    if matches!(follow.evidence, Evidence::VaultScheme) {
        probe.link.protocol = None;
        probe.link.target = format!("/{}", probe.link.target.trim_start_matches('/'));
    }
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
    for candidate in &candidates {
        probes.extend(
            crate::planner::link_spelling::spelling_variants(candidate.syntax, &candidate.address)
                .into_iter()
                .map(|address| LinkKey::new(candidate.holder.clone(), candidate.syntax, address)),
        );
    }
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
