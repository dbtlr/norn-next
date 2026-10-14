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

use std::collections::BTreeMap;

use super::Reading;
use crate::planner::links::{EntryKey, address, entry_key, probe_of, spelled};
use norn_wire::{
    ContentHash, DocumentPath, FindingKind, FindingRow, LinkKey, SkipReason, SkippedFinding,
};

/// A broken link's unique answer: the live document holding the one
/// tombstone's recorded bytes, with the address and hash the fix rests on.
pub(super) struct Follow {
    /// The finding's link, held where its document stood before any route.
    pub(super) link: LinkKey,
    /// The live holder's before-state path, where its hash is observed.
    pub(super) before: DocumentPath,
    /// The one live hash holder, mapped to where the settled routes land it.
    pub(super) to: DocumentPath,
    /// The tombstone's recorded hash, carried as an author condition.
    pub(super) hash: ContentHash,
}

/// The selected broken findings with a follow or a move-following skip,
/// keyed by finding id; no entry where no tombstone was found.
pub(super) type Follows = BTreeMap<u64, Result<Follow, SkippedFinding>>;
/// The candidate addresses prepared for the batch's one resolution probe,
/// keyed by the holder where it lands, syntax and old address. Candidate
/// probes use that landed holder; the replaced address's probe uses the
/// holder where the finding read it.
pub(super) type Asked = BTreeMap<EntryKey, Vec<LinkKey>>;

/// The follows and skips of the selected broken findings `rows`, reading
/// each stored link of `links` through `vault`: the tombstones its address
/// names, then the live holders of the one tombstone's recorded hash. The
/// selection narrows the findings, never the documents a follow may name.
pub(super) fn read<R: Reading>(
    rows: &[FindingRow],
    links: &BTreeMap<u64, norn_store::FindingLink>,
    vault: &R,
) -> Result<Follows, R::Error> {
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
            Ok(Follow {
                link,
                before: to.clone(),
                to: to.clone(),
                hash: ContentHash::new(format!("sha256:{hash}")).expect("a recorded content hash"),
            }),
        );
    }
    Ok(follows)
}

/// Add to `probes` every candidate spelling of `follow` and its replaced
/// address, and record the candidates in `asked`. Candidates are probed
/// from where `holder` lands; the replaced address from where the finding
/// read it ([`Follow::link`]). One holder, syntax and old address is prepared
/// once, however many selected findings name it.
pub(super) fn ask(
    follow: &Follow,
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
