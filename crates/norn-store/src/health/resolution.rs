//! The resolution change set's store half: what each link a plan reaches
//! resolves to with every target of the plan at its before-state, and with
//! every target at its after-state, judged on one read snapshot.
//!
//! # The vault a plan sees is the store's, with its targets overlaid
//!
//! A plan names the files it writes, each with whether a document stands
//! there before it and after it ([`PathOverlay`]). Every other document is
//! read from the store as it stands. So the two vaults a link is resolved
//! against are the store's documents less every target, with the targets
//! present before added back for the first and the targets present after for
//! the second. **Every target is overlaid both ways**, whatever the store
//! holds at its path: a re-send whose landed targets the store has already
//! taken in reads the same two vaults as the first send, so a plan's own
//! progress never changes what it records (ADR 0031). A target is matched by
//! its path key in the store's key space, so on a root that folds case the
//! store's spelling of a target is the target.
//!
//! **A target changes where its presence does, or where the plan replaces the
//! document standing there** ([`PathOverlay::replacing`]): one taken away and
//! another put at its path — the path a move vacates refilled by another
//! move or a create. A link resolving there resolves to the same path on
//! both sides, but not to the same document, so it is reached as a link
//! whose target changes is.
//!
//! # Which links are judged
//!
//! Two arms, each over links the plan bounds:
//!
//! - **The probed links** ([`ProbedLink`]): the links the documents the plan
//!   writes hold at its after-state, which the caller reads from the bytes
//!   it composed, since the store holds their before-state. Each is held at
//!   its after-state's holder and was read, before the plan, from its
//!   holder's lineage source: where a move carried the document's content
//!   from. One is judged where the plan writes its text, where the two
//!   holders give it different keys — a relative path from a moved
//!   document — or where one of its keys names a target the plan
//!   changes.
//! - **The links the store holds under a changed key**: every key that could
//!   name a target the plan changes ([`crate::link`]'s
//!   `keys_naming`), each read by an equality seek of the link index — a
//!   suffix key as a path key is, since the index holds both as written. A
//!   link held by a target is the first arm's, or, where the target is gone
//!   after the plan, no link the plan leaves. A link held under several
//!   changed keys is judged by the least of them, as the changeset's
//!   re-decision decides which pass owns a link, so nothing remembers which
//!   links were judged.
//!
//! A link reached by neither arm keeps its resolution: no key it is held
//! under names a target the plan changes, and its holder stands where it
//! stood.
//!
//! # What a key names, read once
//!
//! A link's resolution is the sum over its keys of what each names, the
//! keys' classes being disjoint ([`super`]): none, one document, or several.
//! So each distinct key a chunk of links holds is resolved once — the head of
//! what the store holds under it, through the link-health judgment's own
//! statement ([`super::statement::heads_sql`]), cut at two more rows than the
//! number of targets it could name — and the targets are added in memory:
//! the store's rows that are targets are dropped, and the targets present on
//! the side being resolved are counted in. Two rows left over tell several
//! from one, so no key's whole class is ever counted.
//!
//! **One limit stands on that bound.** The head is read in ladder order with
//! the places the ambiguity-ignore set keeps out dropped as the read meets
//! them, so a class whose head stands behind many ignored members steps
//! through each before the cut is reached: what such a key costs grows with
//! the ignored members ahead of its head. The statement is link health's own,
//! and the changeset's re-decision pays the same; NORN-320 bounds it.
//!
//! # The caller filters
//!
//! Every judged link is handed back ([`LinkChange`]), whether or not its
//! resolution moved: which of them a plan records, and what it advises, is
//! the planner's to decide.
//!
//! # A target is named over the same two vaults
//!
//! What a request's target names on each side of a plan — a delete's
//! `rewrite_to`, which must name one document where the plan leaves the
//! vault — is read through the same keys a wikilink written with it is held
//! under, and resolved by the same heads ([`Snapshot::target_naming`]). Where
//! it names several documents after the plan, the head of them is what a
//! store built at the after-state would report: the stored members of its
//! classes less every target, read in ladder order a bound past the targets
//! they could hold, counted where that read fills, and merged in that order
//! with the targets standing after; each candidate is named by the first of
//! its suffix spellings that names it alone after the plan, read the same
//! way. Only that refusal path reads past the bound a link's judgment reads.

use std::collections::{BTreeMap, BTreeSet};

use norn_db::rusqlite::types::Value;
use norn_wire::{Candidate, CandidateHead, LinkAddressKind, Resolves};

use super::run::{OnSnapshot, ResolutionStatement, Runner};
use super::{Held, Key, LINK_HEALTH_CHUNK, Pages, occupied_keys, statement};
use crate::error::StoreError;
use crate::facts::{CANDIDATE_HEAD, LinkFact, StoredPathOrder};
use crate::fields::ContentModel;
use crate::link::{self, address_kind, keys_naming, link_keys, suffix_keys};
use crate::path::{DocumentPath, SuffixKey};
use crate::read::{Lookups, PageRefusal, ReadStatement, wire_path};
use crate::request::unreadable;
use crate::resolve::AmbiguityIgnore;
use crate::store::Snapshot;

/// The files a plan writes, each with whether a document stands there before
/// the plan and after it: the overlay its two vaults are read through.
///
/// A path listed twice is overlaid as listed last.
#[derive(Clone, Debug, Default)]
pub struct PathOverlay {
    targets: Vec<Overlaid>,
}

/// One file a plan writes, and whether a document stands there on each side.
#[derive(Clone, Debug)]
struct Overlaid {
    path: DocumentPath,
    before: bool,
    after: bool,
    /// The document standing there after the plan is not the one standing
    /// there before it.
    replaced: bool,
}

impl Overlaid {
    /// Whether the plan changes what stands at this file: a document where
    /// none stood, none where one did, or another document where one did.
    fn changes(&self) -> bool {
        self.before != self.after || self.replaced
    }
}

impl PathOverlay {
    /// The overlay of a plan writing no file.
    pub fn new() -> Self {
        Self::default()
    }

    /// The same overlay, with a document standing at `path` before the plan
    /// where `before` says so, and after it where `after` does.
    #[must_use]
    pub fn with(mut self, path: DocumentPath, before: bool, after: bool) -> Self {
        self.targets.retain(|held| held.path != path);
        self.targets.push(Overlaid {
            path,
            before,
            after,
            replaced: false,
        });
        self
    }

    /// The same overlay, with a document standing at `path` on both sides of
    /// the plan, but not the same one: the plan takes the document standing
    /// there away and puts another there. Every link the store holds under a
    /// key that could name `path` is judged, as for a target whose presence
    /// changes, and is said to have had what it names move under it.
    #[must_use]
    pub fn replacing(mut self, path: DocumentPath) -> Self {
        self.targets.retain(|held| held.path != path);
        self.targets.push(Overlaid {
            path,
            before: true,
            after: true,
            replaced: true,
        });
        self
    }

    /// Whether some file the overlay names holds a document on one side and
    /// not the other: the one thing that moves a link whose holder and text
    /// stay as they were.
    pub fn changes_presence(&self) -> bool {
        self.targets.iter().any(|held| held.before != held.after)
    }
}

/// One link a document the plan writes holds at the plan's after-state, read
/// by the caller from the bytes it composed.
#[derive(Clone, Debug)]
pub struct ProbedLink {
    /// Where the document holding the link stood before the plan: its
    /// lineage source — where a move carried its content from — or, for a
    /// document no move carried, its own path. A relative path, and an empty
    /// address, are read from here before the plan.
    pub before_holder: DocumentPath,
    /// Where the document holding the link stands after the plan.
    pub after_holder: DocumentPath,
    /// The link as the after-state writes it.
    pub link: LinkFact,
    /// Whether the plan writes this link's text, so it is judged and handed
    /// back whatever it resolves to.
    pub written: bool,
}

/// One link a plan reaches, and what it resolves to on each side.
#[derive(Clone, Debug)]
pub struct LinkChange {
    /// The document holding the link, where it stands after the plan.
    pub holder: DocumentPath,
    /// The link as the after-state holds it.
    pub link: LinkFact,
    /// How the link's address stands to judging it, as the store holds
    /// beside every link: what a resolution to no document means for its
    /// health ([`norn_wire::LinkHealth::of_address`]).
    pub address: LinkAddressKind,
    /// What it resolves to from its before-holder, with every target at its
    /// before-state, in the vocabulary a plan records it in.
    pub before: Resolves,
    /// What it resolves to from its holder, with every target at its
    /// after-state.
    pub after: Resolves,
    /// Whether the plan writes the link's text ([`ProbedLink::written`]).
    pub written: bool,
    /// Whether what the link names moved under it: its two holders give it
    /// different keys, or one of its keys names a target the plan changes —
    /// its presence, or the document standing there — and the
    /// ambiguity-ignore set lets it in. An ambiguous link
    /// resolving to several documents on both sides moved exactly where this
    /// holds.
    pub members_moved: bool,
    /// Each of the plan's targets standing before it that the link's keys
    /// name from its before-holder, the ambiguity-ignore set letting it in,
    /// once each in key order: which of the documents the plan writes the
    /// link could name before the plan. A link resolving to several
    /// documents names one of these only where it is among them, which is
    /// how a caller tells whether an ambiguous link could name a document
    /// the plan moves.
    pub before_targets: Vec<DocumentPath>,
}

/// What one judgment of a plan's links cost, beside the statements its
/// snapshot counted.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ResolutionWork {
    /// Every statement the judgment ran on its snapshot, by name, in the
    /// order it ran them: the record a bar reads what was run from, beside
    /// the steps [`Snapshot::counters`] counted. The read of the pinned
    /// declaration that precedes the judgment is a read's own, not one of
    /// these.
    pub ran: Vec<ResolutionStatement>,
    /// Links judged, each once however many ways the plan reached it.
    pub links_evaluated: u64,
    /// Key resolutions: once per distinct key across a run of consecutive
    /// chunks holding it.
    pub keys_resolved: u64,
    /// Rows the key resolutions read, each a document a key names, at most
    /// two more than the targets the key could name.
    pub head_rows: u64,
}

/// What one target names on each side of a plan, read as a wikilink written
/// with it is.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub struct TargetNaming {
    /// What it names with every target of the plan at its before-state.
    pub before: Resolves,
    /// What it names with every target at its after-state.
    pub after: Resolves,
    /// Where it names several documents after the plan, the head of them in
    /// the resolution ladder's order, each named by its minimal
    /// disambiguating suffix after the plan, beside how many there are;
    /// `None` otherwise.
    pub candidates: Option<CandidateHead>,
}

impl TargetNaming {
    /// A target naming `before` before the plan and `after` after it, headed
    /// by `candidates` where it names several after.
    pub const fn new(before: Resolves, after: Resolves, candidates: Option<CandidateHead>) -> Self {
        TargetNaming {
            before,
            after,
            candidates,
        }
    }
}

impl Snapshot {
    /// Judge every link the plan `overlay` describes reaches, handing each to
    /// `each` with what it resolves to before the plan and after it: the
    /// links of `probed`, and the links this snapshot holds under a key that
    /// could name a target the plan changes. A class's members
    /// are read less the places `declared`'s ambiguity-ignore set keeps out,
    /// as every surface reads one.
    ///
    /// Refused where `declared` is not the declaration this snapshot pins.
    ///
    /// **The work is the links the plan reaches plus the candidates they
    /// resolve against**: the changed keys are asked about
    /// 256 at a time in one statement, and only a key
    /// some link is held under is read; its links are read a chunk at a time,
    /// and each distinct key a chunk holds is resolved once, its head cut at
    /// two more rows than the targets it could name. Nothing counts a class.
    /// The one limit: a head is read past every member of its class the
    /// ambiguity-ignore set keeps out ahead of it, so a class with many such
    /// members costs them (NORN-320).
    pub fn resolution_changes(
        &self,
        overlay: &PathOverlay,
        probed: &[ProbedLink],
        declared: &ContentModel,
        mut each: impl FnMut(LinkChange),
    ) -> Result<ResolutionWork, PageRefusal> {
        let mut lookups = Lookups::default();
        self.declaration_pinned(declared, &mut lookups)?;
        let mut judging = Judging::new(
            self.path_order(),
            declared.ambiguity_ignore(),
            overlay,
            OnSnapshot::new(self),
        );
        judging.run(probed, &mut each).map_err(PageRefusal::from)?;
        Ok(judging.finished())
    }

    /// What the suffix address `address` names over the plan `overlay`, on
    /// each side of it, read as a wikilink written with it is, a class's
    /// members less the places `declared`'s ambiguity-ignore set keeps out;
    /// and what reading it cost.
    ///
    /// Refused where `declared` is not the declaration this snapshot pins.
    ///
    /// **The work is the address's keys, and where it names several
    /// documents after the plan, its head and their suffixes**: the keys are
    /// resolved as a link's are, two rows past the targets each could name;
    /// only where the target names several after the plan is each key's
    /// head read [`CANDIDATE_HEAD`] rows past those targets, counted where
    /// it fills, and each candidate's suffix spellings resolved the same way
    /// until one names it alone.
    pub fn target_naming(
        &self,
        overlay: &PathOverlay,
        address: &str,
        declared: &ContentModel,
    ) -> Result<(TargetNaming, ResolutionWork), PageRefusal> {
        let mut lookups = Lookups::default();
        self.declaration_pinned(declared, &mut lookups)?;
        let mut judging = Judging::new(
            self.path_order(),
            declared.ambiguity_ignore(),
            overlay,
            OnSnapshot::new(self),
        );
        let naming = judging.name(address).map_err(PageRefusal::from)?;
        Ok((naming, judging.finished()))
    }
}

impl Judging<'_, OnSnapshot<'_>> {
    /// What the judgment cost, the statements its snapshot ran named in the
    /// order they ran.
    fn finished(self) -> ResolutionWork {
        let ran = self
            .runner
            .record
            .into_inner()
            .into_iter()
            .filter_map(|ran| match ran.statement {
                ReadStatement::Resolution(statement) => Some(statement),
                _ => None,
            })
            .collect();
        ResolutionWork { ran, ..self.work }
    }
}

/// One link under judgment: where it is held on each side, as it is written,
/// and its keys on each side.
struct Judged {
    holder: DocumentPath,
    link: LinkFact,
    address: LinkAddressKind,
    before: Vec<Key>,
    after: Vec<Key>,
    written: bool,
}

/// What the store holds under one key, less the targets: the paths of at
/// most two of the documents it names that the plan does not write, in
/// ladder order. Which targets the key names is read off the overlay.
#[derive(Debug, Default)]
struct KeyHeld {
    stored: Vec<String>,
}

/// One judgment's state: the overlay read into the store's key space, and the
/// keys resolved so far.
struct Judging<'a, R> {
    order: StoredPathOrder,
    key: SuffixKey,
    ignore: &'a AmbiguityIgnore,
    overlay: &'a PathOverlay,
    runner: R,
    /// Each target's path key.
    target_keys: BTreeSet<String>,
    /// Each key that could name a target, and the targets it could name.
    naming: BTreeMap<String, Vec<usize>>,
    /// Each key that could name a target the plan changes.
    changed: BTreeSet<String>,
    resolved: BTreeMap<Key, KeyHeld>,
    work: ResolutionWork,
}

impl<'a, R: Runner> Judging<'a, R> {
    fn new(
        order: StoredPathOrder,
        ignore: &'a AmbiguityIgnore,
        overlay: &'a PathOverlay,
        runner: R,
    ) -> Self {
        let key = SuffixKey::under(order);
        let mut naming: BTreeMap<String, Vec<usize>> = BTreeMap::new();
        let mut changed = BTreeSet::new();
        for (at, target) in overlay.targets.iter().enumerate() {
            for named in keys_naming(&target.path, key) {
                if target.changes() {
                    changed.insert(named.clone());
                }
                naming.entry(named).or_default().push(at);
            }
        }
        Judging {
            order,
            key,
            ignore,
            overlay,
            runner,
            target_keys: overlay
                .targets
                .iter()
                .map(|target| target.path.path_key_in(key).as_str().to_string())
                .collect(),
            naming,
            changed,
            resolved: BTreeMap::new(),
            work: ResolutionWork::default(),
        }
    }

    /// Judge the probed links, then the links the store holds under each
    /// changed key, a chunk at a time.
    fn run(
        &mut self,
        probed: &[ProbedLink],
        each: &mut impl FnMut(LinkChange),
    ) -> Result<(), StoreError> {
        let mut chunk: Vec<Judged> = Vec::new();
        for probe in probed {
            let before = self.keys_of(&probe.link, &probe.before_holder);
            let after = self.keys_of(&probe.link, &probe.after_holder);
            if !probe.written && before == after && !self.meets_a_change(&after) {
                continue;
            }
            chunk.push(Judged {
                holder: probe.after_holder.clone(),
                link: probe.link.clone(),
                address: address_kind(&probe.link),
                before,
                after,
                written: probe.written,
            });
            if chunk.len() == LINK_HEALTH_CHUNK {
                self.judge(std::mem::take(&mut chunk), each)?;
            }
        }
        self.judge(chunk, each)?;

        let changed: Vec<String> = self.changed.iter().cloned().collect();
        for keys in changed.chunks(LINK_HEALTH_CHUNK) {
            let listed: Vec<&str> = keys.iter().map(String::as_str).collect();
            let occupied = occupied_keys(&self.runner, self.key, &[], &listed)?;
            for key in occupied.paths.into_iter().map(|at| listed[at]) {
                let mut pages = Pages::by_key(key);
                while let Some(links) = pages.next(&self.runner, self.key)? {
                    let mut chunk = Vec::with_capacity(links.len());
                    for held in links {
                        if let Some(judged) = self.owned_by(key, held)? {
                            chunk.push(judged);
                        }
                    }
                    self.judge(chunk, each)?;
                }
            }
        }
        Ok(())
    }

    /// `held`, read under the changed key `key`, as a link to judge — or
    /// `None` where its holder is a target, or a lesser changed key it is
    /// held under owns it.
    fn owned_by(&self, key: &str, held: Held) -> Result<Option<Judged>, StoreError> {
        let holder = DocumentPath::new(&held.holder)
            .map_err(|_| unreadable("documents.path", &held.holder))?;
        if self
            .target_keys
            .contains(holder.path_key_in(self.key).as_str())
        {
            return Ok(None);
        }
        if held
            .keys
            .iter()
            .any(|(text, _)| text.as_str() < key && self.changed.contains(text))
        {
            return Ok(None);
        }
        Ok(Some(Judged {
            holder,
            link: held.link.fact,
            address: held.link.address,
            before: held.keys.clone(),
            after: held.keys,
            written: false,
        }))
    }

    /// The keys `link`, held at `holder`, is read through, in the store's
    /// key space, each once.
    fn keys_of(&self, link: &LinkFact, holder: &DocumentPath) -> Vec<Key> {
        self.in_space(link_keys(link, holder))
    }

    /// `keys` in the store's key space, each once.
    fn in_space(&self, keys: Vec<link::LinkKey>) -> Vec<Key> {
        let mut keys: Vec<Key> = keys
            .into_iter()
            .map(|key| {
                let text = match self.key {
                    SuffixKey::Raw => key.key,
                    SuffixKey::Folded => key.folded_key,
                };
                (text, key.segments)
            })
            .collect();
        keys.sort();
        keys.dedup();
        keys
    }

    /// What the suffix address `address` names on each side of the plan,
    /// and the head of what it names after where that is several.
    fn name(&mut self, address: &str) -> Result<TargetNaming, StoreError> {
        let keys = self.in_space(suffix_keys(address));
        self.resolve(&keys.iter().cloned().collect())?;
        self.work.links_evaluated += 1;
        let before = self.resolution(&keys, |target| target.before)?;
        let after = self.resolution(&keys, |target| target.after)?;
        let candidates = match after {
            Resolves::Several {} => Some(self.head_after(&keys)?),
            _ => None,
        };
        Ok(TargetNaming {
            before,
            after,
            candidates,
        })
    }

    /// The head of what `keys` name with every target at its after-state, as
    /// a store built there would read it: the stored members of each key
    /// less the targets, read [`CANDIDATE_HEAD`] rows past the targets the
    /// key could hold and counted where that read fills, merged in ladder
    /// order with the targets standing after; each candidate named by its
    /// minimal suffix after the plan ([`Self::minimal_suffix`]).
    fn head_after(&mut self, keys: &[Key]) -> Result<CandidateHead, StoreError> {
        let bound = |judging: &Self, key: &Key| {
            CANDIDATE_HEAD + judging.naming.get(&key.0).map_or(0, Vec::len)
        };
        let bounds: Vec<(&Key, usize)> = keys.iter().map(|key| (key, bound(self, key))).collect();
        let heads = self.heads(&bounds)?;
        let mut named: Vec<(String, String)> = Vec::new();
        let mut total = 0u64;
        let mut filled: Vec<&Key> = Vec::new();
        for (key, bound) in &bounds {
            let rows = heads.get(*key).map(Vec::as_slice).unwrap_or_default();
            if rows.len() < *bound {
                total += rows.iter().filter(|(_, path)| !self.targets(path)).count() as u64;
            } else {
                filled.push(key);
            }
            named.extend(rows.iter().filter(|(_, path)| !self.targets(path)).cloned());
            for target in self.members(key).filter(|target| target.after) {
                total += 1;
                let rung = match key.1 {
                    Some(_) => match self.key {
                        SuffixKey::Raw => target.path.suffix_key().to_string(),
                        SuffixKey::Folded => target.path.folded_suffix_key().to_string(),
                    },
                    None => key.0.clone(),
                };
                named.push((rung, target.path.as_str().to_string()));
            }
        }
        if !filled.is_empty() {
            total += self.stored_past_the_targets(&filled)?;
        }
        named.sort_by(|left, right| {
            crate::resolve::ladder_cmp((&left.0, &left.1), (&right.0, &right.1))
        });
        named.truncate(CANDIDATE_HEAD);
        let mut candidates = Vec::with_capacity(named.len());
        for (_, path) in named {
            let suffix = self.minimal_suffix(&path)?;
            candidates.push(Candidate::new(wire_path(&path)?, suffix));
        }
        CandidateHead::new(candidates, total).map_err(|problem| StoreError::Damaged {
            what: format!("a target's head outgrew its count: {problem}"),
        })
    }

    /// How many documents the store holds under each of `filled`, keys whose
    /// head filled its bound, at a path no target of the plan stands at: each
    /// key's count, less the targets it could name that the store holds a
    /// document at, which the overlay names instead.
    fn stored_past_the_targets(&mut self, filled: &[&Key]) -> Result<u64, StoreError> {
        let totals = self.totals(filled)?;
        // Which of the targets the filled keys could name the store holds a
        // document at, read by their path keys, one row each at most.
        let mut held_at: Vec<Key> = Vec::new();
        for key in filled {
            for target in self.members(key) {
                let at = (target.path.path_key_in(self.key).as_str().to_string(), None);
                if !held_at.contains(&at) {
                    held_at.push(at);
                }
            }
        }
        let bounds: Vec<(&Key, usize)> = held_at.iter().map(|key| (key, 1)).collect();
        let held: BTreeSet<Key> = self
            .heads(&bounds)?
            .into_iter()
            .filter(|(_, rows)| !rows.is_empty())
            .map(|(key, _)| key)
            .collect();
        let mut stored = 0u64;
        for key in filled {
            let standing = self
                .members(key)
                .filter(|target| {
                    held.contains(&(target.path.path_key_in(self.key).as_str().to_string(), None))
                })
                .count() as u64;
            stored += totals
                .get(*key)
                .copied()
                .unwrap_or_default()
                .saturating_sub(standing);
        }
        Ok(stored)
    }

    /// The first of `path`'s suffix spellings that names the document there
    /// alone after the plan, or the path itself where none does.
    fn minimal_suffix(&mut self, path: &str) -> Result<String, StoreError> {
        let document = DocumentPath::new(path).map_err(|_| unreadable("documents.path", path))?;
        for spelling in document.suffix_spellings() {
            let keys = self.in_space(suffix_keys(&spelling));
            self.resolve(&keys.iter().cloned().collect())?;
            if let Resolves::One { path: one } = self.resolution(&keys, |target| target.after)?
                && one.as_str() == path
            {
                return Ok(spelling);
            }
        }
        Ok(path.to_string())
    }

    /// Whether a target of the plan stands at the stored `path`.
    fn targets(&self, path: &str) -> bool {
        DocumentPath::new(path)
            .is_ok_and(|at| self.target_keys.contains(at.path_key_in(self.key).as_str()))
    }

    /// Whether one of `keys` could name a target the plan changes.
    fn meets_a_change(&self, keys: &[Key]) -> bool {
        keys.iter().any(|(text, _)| self.changed.contains(text))
    }

    /// Resolve every key `chunk` holds that is not yet resolved, hand each of
    /// its links to `each`, and keep only the keys it held.
    fn judge(
        &mut self,
        chunk: Vec<Judged>,
        each: &mut impl FnMut(LinkChange),
    ) -> Result<(), StoreError> {
        if chunk.is_empty() {
            return Ok(());
        }
        let held: BTreeSet<Key> = chunk
            .iter()
            .flat_map(|judged| judged.before.iter().chain(&judged.after))
            .cloned()
            .collect();
        self.resolve(&held)?;
        for judged in chunk {
            self.work.links_evaluated += 1;
            let before = self.resolution(&judged.before, |target| target.before)?;
            let after = self.resolution(&judged.after, |target| target.after)?;
            let members_moved = judged.before != judged.after
                || judged
                    .before
                    .iter()
                    .chain(&judged.after)
                    .any(|key| self.members(key).any(Overlaid::changes));
            let mut before_targets: Vec<DocumentPath> = Vec::new();
            for target in judged.before.iter().flat_map(|key| self.members(key)) {
                if target.before && !before_targets.contains(&target.path) {
                    before_targets.push(target.path.clone());
                }
            }
            each(LinkChange {
                holder: judged.holder,
                link: judged.link,
                address: judged.address,
                before,
                after,
                written: judged.written,
                members_moved,
                before_targets,
            });
        }
        // What the next chunk may reuse is what this one held, so what is
        // kept is two chunks' keys at most, however many links the plan
        // reaches.
        self.resolved.retain(|key, _| held.contains(key));
        Ok(())
    }

    /// The targets `key` names, the ambiguity-ignore set letting each in.
    fn members(&self, key: &Key) -> impl Iterator<Item = &Overlaid> {
        let (text, segments) = key;
        self.naming
            .get(text)
            .into_iter()
            .flatten()
            .map(|at| &self.overlay.targets[*at])
            .filter(move |target| {
                segments.is_none_or(|segments| {
                    self.ignore.admits(
                        target.path.as_str(),
                        usize::try_from(segments).unwrap_or(usize::MAX),
                        self.order,
                    )
                })
            })
    }

    /// What a link held under `keys` resolves to, on the side `present`
    /// reads each target's presence from.
    fn resolution(
        &self,
        keys: &[Key],
        present: impl Fn(&Overlaid) -> bool,
    ) -> Result<Resolves, StoreError> {
        let mut total = 0usize;
        let mut one: Option<&str> = None;
        for key in keys {
            let held = &self.resolved[key];
            let targets: Vec<&Overlaid> = self.members(key).filter(|t| present(t)).collect();
            total += held.stored.len() + targets.len();
            if total > 1 {
                return Ok(Resolves::several());
            }
            one = one
                .or_else(|| held.stored.first().map(String::as_str))
                .or_else(|| targets.first().map(|target| target.path.as_str()));
        }
        Ok(match one {
            Some(path) if total == 1 => Resolves::one(wire_path(path)?),
            _ => Resolves::none(),
        })
    }

    /// Resolve every key of `keys` not yet resolved: the head of what the
    /// store holds under it, cut at two more rows than the targets it could
    /// name, less those targets.
    ///
    /// The keys are read in one statement per head bound, and a key naming
    /// no target — almost every key — is cut at two.
    fn resolve(&mut self, keys: &BTreeSet<Key>) -> Result<(), StoreError> {
        let bounds: Vec<(&Key, usize)> = keys
            .iter()
            .filter(|key| !self.resolved.contains_key(*key))
            .map(|key| (key, self.naming.get(&key.0).map_or(0, Vec::len) + 2))
            .collect();
        for (key, head) in self.heads(&bounds)? {
            let stored = head
                .into_iter()
                .map(|(_, path)| path)
                .filter(|path| !self.targets(path))
                .take(2)
                .collect();
            self.work.keys_resolved += 1;
            self.resolved.insert(key, KeyHeld { stored });
        }
        Ok(())
    }

    /// The head of what the store holds under each key `bounds` lists, cut
    /// at the bound beside it, each row its rung and its path in ladder
    /// order — the targets' paths among them. Every key listed has an entry.
    ///
    /// The keys are read in one statement per head bound.
    fn heads(
        &mut self,
        bounds: &[(&Key, usize)],
    ) -> Result<BTreeMap<Key, Vec<(String, String)>>, StoreError> {
        let mut by_bound: BTreeMap<usize, Vec<&Key>> = BTreeMap::new();
        for (key, bound) in bounds {
            by_bound.entry(*bound).or_default().push(key);
        }
        let mut heads: BTreeMap<Key, Vec<(String, String)>> = BTreeMap::new();
        for (bound, listed) in by_bound {
            let Arms {
                classes,
                paths,
                keys: arms,
            } = arms(&listed);
            let values: Vec<Value> =
                statement::keys_parameters(&classes, &paths, self.ignore, self.order, Some(bound))?;
            let rows = self.runner.read_all(
                ResolutionStatement::Heads,
                statement::heads_sql(self.key),
                values,
                |row| {
                    Ok(Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, usize>(1)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                    )))
                },
                "reading what a plan's link keys name",
            )?;
            self.work.head_rows += rows.len() as u64;
            for key in arms.iter().flatten() {
                heads.entry((*key).clone()).or_default();
            }
            for (arm, at, path, rung) in rows {
                let key = asked(&arms, arm, at, "a key read")?;
                heads.entry(key.clone()).or_default().push((rung, path));
            }
        }
        for head in heads.values_mut() {
            head.sort_by(|left, right| {
                crate::resolve::ladder_cmp((&left.0, &left.1), (&right.0, &right.1))
            });
        }
        Ok(heads)
    }

    /// How many documents the store holds under each of `keys`, the places
    /// the ambiguity-ignore set keeps out left out, in one statement.
    fn totals(&mut self, keys: &[&Key]) -> Result<BTreeMap<Key, u64>, StoreError> {
        let Arms {
            classes,
            paths,
            keys: arms,
        } = arms(keys);
        let values: Vec<Value> =
            statement::keys_parameters(&classes, &paths, self.ignore, self.order, None)?;
        let rows = self.runner.read_all(
            ResolutionStatement::Totals,
            statement::totals_sql(self.key),
            values,
            |row| {
                Ok(Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, usize>(1)?,
                    row.get::<_, u64>(2)?,
                )))
            },
            "counting what a target's keys name",
        )?;
        let mut totals = BTreeMap::new();
        for (arm, at, total) in rows {
            totals.insert(asked(&arms, arm, at, "a count")?.clone(), total);
        }
        Ok(totals)
    }
}

/// Keys split into the two arms the key statements walk: the suffix keys
/// beside their segment counts, the path keys, and each arm's keys in the
/// order its list names them.
struct Arms<'k> {
    classes: Vec<(&'k str, u64)>,
    paths: Vec<&'k str>,
    keys: [Vec<&'k Key>; 2],
}

/// `keys` split into the two arms the key statements walk.
fn arms<'k>(keys: &[&'k Key]) -> Arms<'k> {
    let mut classes: Vec<(&str, u64)> = Vec::new();
    let mut paths: Vec<&str> = Vec::new();
    let mut arms: [Vec<&Key>; 2] = [Vec::new(), Vec::new()];
    for key in keys {
        match key {
            (text, Some(segments)) => {
                classes.push((text, *segments));
                arms[0].push(key);
            }
            (text, None) => {
                paths.push(text);
                arms[1].push(key);
            }
        }
    }
    Arms {
        classes,
        paths,
        keys: arms,
    }
}

/// The key a row of a key statement answers, by its arm and its place in
/// that arm's list; damage where the statement answered a key it was not
/// asked, `what` naming the statement.
fn asked<'k>(
    arms: &[Vec<&'k Key>; 2],
    arm: i64,
    at: usize,
    what: &str,
) -> Result<&'k Key, StoreError> {
    usize::try_from(arm)
        .ok()
        .and_then(|arm| arms.get(arm))
        .and_then(|keys| keys.get(at))
        .copied()
        .ok_or_else(|| StoreError::Damaged {
            what: format!("{what} answered a key it was not asked, {arm}:{at}"),
        })
}
