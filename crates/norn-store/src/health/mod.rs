//! Link health: the findings the store judges about a selection of links —
//! **broken**, **ambiguous** and **missing anchor**, each a finding about
//! exactly one link, at warning severity ([ADR 0027]).
//!
//! # The rules are the ones every surface reads a link's health by
//!
//! A link's health is [`LinkHealth::of_address`] over the address kind stored
//! beside it and how many documents its keys name now — the same judgment a
//! links column reads through [`LinkHealth::of_link`]. What a link names is
//! read through the keys the link index holds it under
//! ([`crate::link::link_keys`]), each class less the places the vault's
//! ambiguity-ignore globs keep out of it, exactly as a read resolves a link.
//!
//! - A link addressed elsewhere, and an attachment resolving to no document,
//!   raise nothing.
//! - An eligible link resolving to no document is broken, with an empty head.
//! - A link resolving to two or more is ambiguous: a head of at most
//!   [`CANDIDATE_HEAD`] of them in the resolution ladder's order, each named by
//!   its minimal disambiguating suffix, beside the exact total.
//! - A link resolving to one document that carries an anchor that document
//!   does not hold ([`crate::anchor`]) has a missing anchor; its head is that
//!   one document, out of one.
//!
//! An embed is a link, and is judged as one. A finding stands at the holding
//! document's path, at the link's ordinal and span, and carries the link as
//! written — protocol and anchor included — as its target.
//!
//! # A finding is keyed by its link's keys and its candidates' naming classes
//!
//! A finding's keys are its link's keys, in the key space the root probes — a
//! suffix key as a class key, and a path key as a path key — and the naming
//! classes of every candidate it carries: the classes the candidate's minimal
//! disambiguating suffix is read from ([`SuffixSpellings::naming_classes`]).
//! What a finding says moves with the documents its link's keys name, and with
//! the documents those names are read against, so a change to either reaches
//! it: the changeset's discard on either axis ranges over exactly these keys.
//! `[p](x/t.md#Nope)` is keyed by the path `x/t.md`, and by the class `t/` its
//! candidate is named `t` in, so writing `y/t.md` re-decides it, its
//! candidate named `x/t` now.
//!
//! # The work adds links and candidates, and never multiplies them
//!
//! A link's total is the sum over its keys, and its head the merge of its
//! keys' heads in ladder order, cut to [`CANDIDATE_HEAD`]; the keys' classes
//! are disjoint, so the merge is the head of the whole. So **each distinct key
//! is resolved once**, however many of the links carry it: one statement reads
//! every distinct key's head ([`statement::heads_sql`]), and one counts the
//! keys whose head filled ([`statement::totals_sql`]). What a key names is
//! kept ([`KeySummaries`]), so a judgment taken in chunks of links resolves a
//! key the first chunk resolved in no later one; the changeset's re-decision
//! keeps a key's head only while the chunks holding it run on, and reads the
//! head again where a chunk that does not hold it came between, while a
//! filled key's total is kept and counted once per changeset ([`redecide`]).
//! The links cost their
//! own seeks, and each key its class, so the work is the links plus the
//! candidates they resolve against. Resolving each link against its class, as
//! a read's links column does, would cost the links times the candidates.
//!
//! # Which links are judged is the selection's, and what is found is not
//!
//! The links are selected apart from their judgment ([`LinkSelection`]): by
//! the documents holding them, by a class their suffix keys fall in, or by
//! the path key they spell — the three ways the changeset reaches a link.
//! Each selection is one statement ([`statement::links_sql`]) reading the same
//! rows, a link and every key it is held under, so a link is judged alike
//! whichever selection reached it.
//!
//! Five statements judge a selection, whatever it holds: the one reading the
//! links and their keys, the keys' heads, the filled heads' totals, the
//! anchors of the links naming one document, and the suffixes of the
//! candidates the findings carry. A statement with nothing to read is not run.
//!
//! # The changeset re-decides what it reaches
//!
//! After every entry of a changeset is written, the store judges the links
//! the changeset reaches and files these findings in the same transaction
//! ([`redecide`], [ADR 0027]): the links its written documents hold, read a
//! chunk at a time off the generation the write stamped them with; then, for
//! each class it changed, the findings standing under the class, discarded a
//! chunk at a time with the links they were about that no key of the class
//! reaches, and the links the class selects; then the links each path key it
//! changed selects, read a chunk at a time too. A class or a path key is
//! walked only where something is held under it: the classes and paths are
//! asked about a chunk at a time first ([`statement::occupied_sql`]), so a
//! mass delete's classes and paths that no link and no finding is held under
//! cost a share of one statement each, and no walk.
//! [`crate::Request::judge_selected_links`] is the door that judges
//! a selection and files nothing, and the plan seam bars each statement
//! either runs.
//!
//! [ADR 0027]: https://github.com/dbtlr/norn/blob/main/docs/decisions/0027-link-health-rides-the-changeset.md

pub(crate) mod statement;

use statement::{After, Selected};

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet, HashMap};

use norn_db::rusqlite::types::Value;
use norn_db::rusqlite::{Connection, Transaction, params_from_iter};
use norn_wire::{FindingKind, LinkHealth};

use crate::error::StoreError;
use crate::facts::{
    CANDIDATE_HEAD, CandidateFact, FindingFacts, LinkAnchor, LinkFact, StoredLink, StoredPathOrder,
};
use crate::path::{ClassKey, DocumentPath, PathKey, SuffixKey};
use crate::read::SuffixSpellings;
use crate::request::{self, ReadWork, Request, stored_link_row, unreadable};
use crate::resolve::{self, AmbiguityIgnore};

/// The kinds the store judges and files itself, and no caller records or
/// discards.
pub(crate) const LINK_HEALTH_KINDS: [FindingKind; 3] = [
    FindingKind::Broken,
    FindingKind::Ambiguous,
    FindingKind::MissingAnchor,
];

/// The message every broken link's finding carries.
const BROKEN: &str = "the link names no document";

/// The message every ambiguous link's finding carries.
const AMBIGUOUS: &str = "the link names more than one document";

/// The message every missing anchor's finding carries.
const MISSING_ANCHOR: &str = "the document the link names holds no place its anchor names";

/// Which links a judgment judges.
///
/// The re-decision [ADR 0027] rules into the changeset reaches its links three
/// ways, and each is a selection here: the links a written document holds, the
/// suffix-addressed links whose keys fall in the class of a changed path, and
/// the path-addressed links whose key is a changed path. Each is read by an
/// index seek — [`crate::ExplainedStatement::LinkHealthLinks`],
/// [`crate::ExplainedStatement::LinkHealthClassLinks`] and
/// [`crate::ExplainedStatement::LinkHealthPathLinks`] — and whichever way a
/// link is reached it is judged alike: the selection decides which links are
/// read, and never what is found about them.
///
/// A key is spelled in the key space the store's path order selects, as the
/// changeset names its classes and paths
/// ([`crate::IncrementOutcome::affected_classes`],
/// [`crate::IncrementOutcome::affected_paths`]).
///
/// [ADR 0027]: https://github.com/dbtlr/norn/blob/main/docs/decisions/0027-link-health-rides-the-changeset.md
#[derive(Clone, Copy, Debug)]
pub enum LinkSelection<'a> {
    /// Every link the documents at these paths hold. A path named twice holds
    /// its links once, and a path no document stands at holds none.
    Documents(&'a [DocumentPath]),
    /// Every link held under a suffix key inside this class: every
    /// suffix-addressed link that could name a document the class holds.
    Class(&'a ClassKey),
    /// Every link held under exactly this path key: every path-addressed link
    /// spelling this path.
    Path(&'a PathKey),
}

/// One key a link is held under, in the root's key space, beside its segment
/// count, which a suffix key carries and a path key does not.
type Key = (String, Option<u64>);

/// One link under judgment: where it is held, its row, and its keys.
struct Held {
    holder: String,
    /// The write generation the holding document was last written at.
    generation: i64,
    id: i64,
    ordinal: u64,
    link: StoredLink,
    /// Each key the link index holds the link under, each once.
    keys: Vec<Key>,
}

/// One document a key names: its row id, its path, and its rung on the
/// resolution ladder.
#[derive(Clone, Debug)]
struct Named {
    id: i64,
    path: String,
    rung: String,
}

impl Named {
    /// How `self` stands against `other` on the resolution ladder.
    fn ladder(&self, other: &Named) -> Ordering {
        resolve::ladder_cmp((&self.rung, &self.path), (&other.rung, &other.path))
    }
}

/// What one key names: the head of it in ladder order, and how many.
#[derive(Debug, Default)]
struct Summary {
    head: Vec<Named>,
    total: u64,
}

/// What each key a judgment has resolved names, so a judgment taken in chunks
/// resolves a key once across a run of consecutive chunks that hold it, and
/// counts a filled key's total once however many runs hold it ([ADR 0027]'s
/// fifth condition) — and how each candidate its findings carried is named,
/// so a candidate is named once while its key is held. A set nothing evicts
/// from, as the public judgment door's, resolves each distinct key once.
///
/// A summary is what the store held when it was read, under the store's key
/// space and the ignore set the judgment was taken under. One set is valid
/// only while no document fact and no link fact moves — the read that filled
/// it and every judgment that reuses it see the same documents, links,
/// headings and blocks, under the same declaration — and a judgment after a
/// write that moves one takes a new one. Findings are no input to a
/// judgment, so filing them between chunks leaves a set valid, as the
/// changeset's re-decision does. It holds one summary per distinct key it has
/// resolved and not forgotten, each a head of at most [`CANDIDATE_HEAD`]
/// documents, and a name for at most those heads' documents, so the caller
/// bounds its size by the keys it chooses to resolve against it: the
/// re-decision forgets, after each chunk it files, every key that chunk's
/// links do not hold. What forgetting leaves is the exact total of each key
/// whose head filled, one key and one count each, so a key resolved again
/// reads its bounded head and never counts what it names twice.
///
/// [ADR 0027]: https://github.com/dbtlr/norn/blob/main/docs/decisions/0027-link-health-rides-the-changeset.md
#[derive(Debug, Default)]
pub struct KeySummaries {
    summaries: BTreeMap<Key, Summary>,
    /// How each candidate a finding carried is named, by its document's row
    /// id: at most the heads of the keys resolved.
    names: HashMap<i64, CandidateName>,
    /// The exact total of each key resolved whose head filled, which
    /// forgetting the key leaves: a key resolved again reads its head alone,
    /// so what a key names is counted once per set.
    totals: HashMap<Key, u64>,
    resolved: u64,
    candidates: u64,
}

impl KeySummaries {
    /// How many key resolutions the judgments taken against this set have
    /// made: one per run of consecutive chunks holding a key, and so one per
    /// distinct key where nothing is evicted between chunks.
    pub fn keys_resolved(&self) -> u64 {
        self.resolved
    }

    /// How many candidates resolving those keys read: each resolution's head,
    /// and each filled key's whole total the one time it is counted.
    pub fn candidates_read(&self) -> u64 {
        self.candidates
    }

    /// What `key` names, refused where no judgment against this set resolved
    /// it.
    fn of(&self, key: &Key) -> Result<&Summary, StoreError> {
        self.summaries.get(key).ok_or_else(|| StoreError::Damaged {
            what: format!("a judgment read a key it did not resolve, {key:?}"),
        })
    }

    /// Forget every key no link in `links` holds, and the names of the
    /// candidates their heads held. A key forgotten is resolved again if a
    /// later judgment holds it, so forgetting one costs work only where a
    /// later chunk holds it and the chunk before that one does not.
    fn keep_only_keys_of(&mut self, links: &[Held]) {
        let held: BTreeSet<&Key> = links.iter().flat_map(|held| &held.keys).collect();
        let forgotten: Vec<Key> = self
            .summaries
            .keys()
            .filter(|key| !held.contains(key))
            .cloned()
            .collect();
        for key in forgotten {
            self.forget(&key);
        }
    }

    /// Forget `key`, and the names of the candidates its head held.
    fn forget(&mut self, key: &Key) {
        if let Some(summary) = self.summaries.remove(key) {
            for named in summary.head {
                self.names.remove(&named.id);
            }
        }
    }
}

/// How a finding names one candidate: its minimal disambiguating suffix, and
/// the classes that suffix is read from ([`SuffixSpellings::naming_classes`]),
/// which the finding is keyed by beside its link's own keys.
#[derive(Debug)]
struct CandidateName {
    suffix: String,
    classes: BTreeSet<ClassKey>,
}

/// The summary at `index` of the arm `arm` among `summaries`, refused where a
/// statement answered a key it was not asked.
fn summary(
    summaries: &mut [Vec<Summary>; 2],
    arm: i64,
    index: usize,
) -> Result<&mut Summary, StoreError> {
    usize::try_from(arm)
        .ok()
        .and_then(|arm| summaries.get_mut(arm))
        .and_then(|summaries| summaries.get_mut(index))
        .ok_or_else(|| StoreError::Damaged {
            what: format!("a key read answered a key it was not asked, {arm}:{index}"),
        })
}

/// What resolving one link found, before its anchor and its candidates'
/// suffixes are read.
enum Verdict {
    Broken,
    Ambiguous {
        head: Vec<Named>,
        total: u64,
    },
    /// The link names exactly this document, and is judged further only where
    /// it carries an anchor.
    One(Named),
}

/// The link-health findings about every link `selection` selects, judged
/// against the documents the store holds now under the key space `order`
/// selects, less the places `ignore` keeps out of a class. Each key the links
/// hold that `summaries` does not yet hold is resolved and kept there, so a
/// judgment taken in chunks resolves each key once. Each statement runs on
/// `connection` and its steps are added to `work`.
///
/// A class or a path key is read a page of [`LINK_HEALTH_CHUNK`] links at a
/// time, each page judged before the next is read. The findings are in the
/// order of the holding document's path, then the link's ordinal.
pub(crate) fn judge(
    connection: &Connection,
    work: &ReadWork,
    order: StoredPathOrder,
    ignore: &AmbiguityIgnore,
    selection: LinkSelection<'_>,
    summaries: &mut KeySummaries,
) -> Result<Vec<FindingFacts>, StoreError> {
    let key = SuffixKey::under(order);
    let mut findings = Vec::new();
    match selection {
        LinkSelection::Documents([]) => {}
        LinkSelection::Documents(documents) => {
            // Each document once, so a path named twice reads its links once.
            let paths: BTreeSet<&str> = documents.iter().map(DocumentPath::as_str).collect();
            let values =
                statement::documents_parameters(&paths.into_iter().collect::<Vec<&str>>())?;
            let (links, _) = read_links(connection, work, key, Selected::Documents, values)?;
            findings = judge_links(connection, work, order, ignore, &links, summaries)?;
        }
        LinkSelection::Class(_) | LinkSelection::Path(_) => {
            let mut pages = Pages::new(selection);
            while let Some(links) = pages.next(connection, work, key)? {
                findings.extend(judge_links(
                    connection, work, order, ignore, &links, summaries,
                )?);
            }
        }
    }
    findings.sort_by(|left, right| {
        (left.path.as_str(), left.ordinal).cmp(&(right.path.as_str(), right.ordinal))
    });
    Ok(findings)
}

/// How many links one page of a paged selection holds at most: the chunk a
/// re-decision reads, judges and files before it reads the next, which is
/// what bounds the links, keys and findings it holds at once.
pub(crate) const LINK_HEALTH_CHUNK: usize = 256;

/// A paged selection, read a page of [`LINK_HEALTH_CHUNK`] links at a time.
struct Pages<'a> {
    selected: Selected,
    by: PagedBy<'a>,
    /// Where the next page resumes, or `None` once a page came back short.
    after: Option<After>,
}

/// What a paged selection selects by.
enum PagedBy<'a> {
    Written(i64),
    Class(&'a ClassKey),
    Path(&'a PathKey),
}

impl<'a> Pages<'a> {
    /// The pages of a class or a path key's links.
    fn new(selection: LinkSelection<'a>) -> Self {
        let (by, after) = match selection {
            LinkSelection::Class(class) => (PagedBy::Class(class), After::class_start(class)),
            LinkSelection::Path(path) => (PagedBy::Path(path), After::path_start(path)),
            LinkSelection::Documents(_) => unreachable!("a documents selection is read whole"),
        };
        Pages {
            selected: Selected::of(selection),
            by,
            after: Some(after),
        }
    }

    /// The pages of the links of every document the write stamped with
    /// `generation` wrote.
    fn written(generation: i64) -> Self {
        Pages {
            selected: Selected::Written,
            by: PagedBy::Written(generation),
            after: Some(After::written_start()),
        }
    }

    /// The next page, or `None` once the selection is read through.
    fn next(
        &mut self,
        connection: &Connection,
        work: &ReadWork,
        key: SuffixKey,
    ) -> Result<Option<Vec<Held>>, StoreError> {
        let Some(after) = self.after.take() else {
            return Ok(None);
        };
        let values = match self.by {
            PagedBy::Written(generation) => {
                statement::written_parameters(generation, &after, LINK_HEALTH_CHUNK)
            }
            PagedBy::Class(class) => statement::class_parameters(class, &after, LINK_HEALTH_CHUNK),
            PagedBy::Path(path) => statement::path_parameters(path, &after, LINK_HEALTH_CHUNK),
        };
        let (links, driven) = read_links(connection, work, key, self.selected, values)?;
        // A page whose driver read as many rows as its bound may have one
        // after it; a shorter one is the last.
        self.after = driven
            .last()
            .filter(|_| driven.len() == LINK_HEALTH_CHUNK)
            .cloned();
        Ok((!links.is_empty()).then_some(links))
    }
}

/// What one changeset's link-health re-decision did.
#[derive(Debug, Default)]
pub(crate) struct Redecided {
    /// Links judged, each once however many ways the changeset reached it.
    pub(crate) links: u64,
    /// Keys resolved: each key once across a run of consecutive chunks
    /// holding it, and once more for each later run.
    pub(crate) keys_resolved: u64,
    /// Candidates the resolution read: each resolution's head, and each
    /// filled key's whole total the one time it is counted.
    pub(crate) candidates_read: u64,
    /// Findings filed.
    pub(crate) findings: u64,
    /// Findings discarded from under the classes the changeset changed.
    pub(crate) discarded: u64,
}

/// Re-decide the link health of every link the changeset that stamped
/// `generation` reaches, inside its `transaction`, and file the findings:
/// [ADR 0027]'s re-decided set, which is
///
/// 1. every link a document the changeset wrote holds — those stamped with
///    `generation`;
/// 2. for each of `classes`, the classes of the paths it wrote or killed, in
///    order: every link a finding standing under the class was about, and
///    every suffix-addressed link whose keys fall in the class;
/// 3. every path-addressed link whose key is one of `paths`, the paths it
///    wrote or killed.
///
/// **The second arm is where a class's findings are discarded.** Each class
/// first reads the findings standing under it that the changeset did not
/// file, a chunk at a time, discards them, and judges each link one of them
/// was about that holds no suffix key in any of `classes`: a finding keyed
/// under the class only through a candidate's naming class, whose link no key
/// of the class reaches. Such a link holds no key in `paths` either, since
/// the path discard took every finding keyed by one before the re-decision
/// began. The discard is known from the rows it takes, a chunk at a time, so
/// nothing past a chunk is held.
///
/// **Every finding a re-decided link held is gone before it is judged**, so
/// its new finding never meets the old one: the subject discard took every
/// finding at a written document's path; a finding carries every key its link
/// is held under, so the path discard took the finding of every link the
/// third arm reaches; and a finding a link the second arm reaches under one
/// of its keys was keyed under that key's class, which the class's own
/// findings pass, or an earlier class's, discarded first.
///
/// **Each link is judged once.** A link the changeset reaches more than one
/// way is judged by the first arm that reaches it and skipped by the rest,
/// and which arm is first is a predicate over the link's own facts: a link
/// whose document the changeset stamped belongs to the first arm; a link
/// holding a suffix key in an affected class belongs to the least such
/// class's keyed pass; a link a finding under a class was about and no such
/// key reaches belongs to the findings pass that discarded that finding, which
/// is the only pass that ever reads it; and a link the third arm reaches under
/// one key belongs to the least of its affected paths. So nothing is
/// remembered between chunks but the key summaries, and the arms hold no set
/// of the links they judged.
///
/// Each arm is read a chunk of [`LINK_HEALTH_CHUNK`] links at a time, judged
/// and filed before the next is read, against one set of key summaries
/// ([`KeySummaries`]) passed from chunk to chunk. A class's two passes and a path key's pass each run only
/// where [`occupied`] finds something they would read, so a key nothing is
/// held under costs a share of one statement and no pass.
///
/// **What is kept between chunks is the keys the last chunk held.** After a
/// chunk is filed, every summary no link in it holds is forgotten, in every
/// arm alike, so what is held at once is a chunk and the keys of two chunks
/// at most, never the re-decided set. A link holds a key in each class its
/// reductions name and one per path its rooted name spells, and those other
/// classes and paths fall outside the walk under way, so no walk order tells
/// when one of them is passed; the chunk that held it is what does. A key
/// held by consecutive chunks is resolved once across them — a hub class's
/// one key, and a key every page of its walk carries — and a key a later
/// chunk holds after a chunk that did not is resolved again: its head, at
/// most [`CANDIDATE_HEAD`] rows, is read once more. A filled key's exact
/// total outlives its head, so what a key names is counted once per
/// changeset and the work stays the links plus the candidates: the totals
/// kept are one per distinct filled key the re-decided links hold.
///
/// [ADR 0027]: https://github.com/dbtlr/norn/blob/main/docs/decisions/0027-link-health-rides-the-changeset.md
pub(crate) fn redecide(
    transaction: &Transaction<'_>,
    work: &ReadWork,
    order: StoredPathOrder,
    ignore: &AmbiguityIgnore,
    generation: i64,
    classes: &BTreeSet<ClassKey>,
    paths: &BTreeSet<PathKey>,
) -> Result<Redecided, StoreError> {
    let key = SuffixKey::under(order);
    let mut summaries = KeySummaries::default();
    let mut redecided = Redecided::default();
    let mut file = |links: Vec<Held>, summaries: &mut KeySummaries| -> Result<(), StoreError> {
        if links.is_empty() {
            return Ok(());
        }
        redecided.links += links.len() as u64;
        for finding in judge_links(transaction, work, order, ignore, &links, summaries)? {
            request::write_finding(transaction, &finding)?;
            redecided.findings += 1;
        }
        // What the next chunk may reuse is what this one held: a key it
        // holds too is resolved once between them, and a key it does not
        // hold is resolved again, so what is kept is two chunks' keys at
        // most, whatever the changeset re-decides.
        summaries.keep_only_keys_of(&links);
        // The point a re-decision can be torn at: a chunk filed, the
        // transaction open, nothing committed. A build without the
        // `induced-failure` feature carries no check here.
        #[cfg(feature = "induced-failure")]
        crate::faults::abort_if_the_redecision_is_torn();
        Ok(())
    };

    let mut written = Pages::written(generation);
    while let Some(links) = written.next(transaction, work, key)? {
        file(links, &mut summaries)?;
    }
    // A pass runs only over a key it could read something under, a chunk of
    // the keys asked about at a time.
    let listed: Vec<&ClassKey> = classes.iter().collect();
    for chunk in listed.chunks(LINK_HEALTH_CHUNK) {
        let occupied = occupied(transaction, work, key, chunk, &[])?;
        for (at, class) in chunk.iter().copied().enumerate() {
            // The findings standing under the class go first, where any
            // stands, a page at a time, and each link one of them was about is
            // judged here where no arm reaches it by a key: the finding was
            // keyed under the class only by a candidate's name.
            let mut after = After::class_start(class);
            while occupied.class_findings.contains(&at) {
                let page = Request::read_all_on(
                    transaction,
                    work,
                    &statement::class_findings_sql(),
                    params_from_iter(statement::class_findings_parameters(
                        class,
                        &after,
                        generation,
                        LINK_HEALTH_CHUNK,
                    )),
                    |row| {
                        Ok(Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, i64>(1)?,
                            row.get::<_, Option<i64>>(2)?,
                        )))
                    },
                    "reading the findings standing under a class",
                )?;
                let last = page.len() < LINK_HEALTH_CHUNK;
                let Some((text, finding, _)) = page.last() else {
                    break;
                };
                after = After {
                    text: text.clone(),
                    first: *finding,
                    second: 0,
                };
                let findings: BTreeSet<i64> = page.iter().map(|(_, finding, _)| *finding).collect();
                let links: BTreeSet<i64> = page.iter().filter_map(|(_, _, link)| *link).collect();
                redecided.discarded += discard(transaction, &findings)?;
                let mut links = read_links_by_id(transaction, work, key, &links)?;
                links.retain(|held| !reached_by_a_class(held, classes));
                file(links, &mut summaries)?;
                if last {
                    break;
                }
            }

            if !occupied.class_links.contains(&at) {
                continue;
            }
            let mut pages = Pages::new(LinkSelection::Class(class));
            while let Some(mut links) = pages.next(transaction, work, key)? {
                links.retain(|held| {
                    held.generation != generation
                        && !held.keys.iter().any(|(text, segments)| {
                            segments.is_some()
                                && class_of(text).is_some_and(|other| {
                                    other < class.as_str() && classes_hold(classes, other)
                                })
                        })
                });
                file(links, &mut summaries)?;
            }
        }
    }
    let listed: Vec<&PathKey> = paths.iter().collect();
    for chunk in listed.chunks(LINK_HEALTH_CHUNK) {
        let occupied = occupied(transaction, work, key, &[], chunk)?;
        for path in occupied.paths.into_iter().map(|at| chunk[at]) {
            let mut pages = Pages::new(LinkSelection::Path(path));
            while let Some(mut links) = pages.next(transaction, work, key)? {
                links.retain(|held| {
                    held.generation != generation
                        && !held.keys.iter().any(|(text, segments)| {
                            segments.is_none()
                                && text.as_str() < path.as_str()
                                && PathKey::new(text).is_ok_and(|other| paths.contains(&other))
                        })
                });
                file(links, &mut summaries)?;
            }
        }
    }
    redecided.keys_resolved = summaries.keys_resolved();
    redecided.candidates_read = summaries.candidates_read();
    Ok(redecided)
}

/// Which of the keys a re-decision was handed its passes could reach
/// anything under: positions in the lists [`occupied`] was asked about.
#[derive(Debug, Default)]
struct Occupied {
    /// The classes a suffix key the root probes falls in.
    class_links: BTreeSet<usize>,
    /// The classes a finding is held under.
    class_findings: BTreeSet<usize>,
    /// The path keys a link is held under.
    paths: BTreeSet<usize>,
}

/// Which of `classes` and of `paths` a re-decision's passes could reach
/// anything under ([`statement::occupied_sql`]).
///
/// Most classes and paths a mass delete names are ones no link and no finding
/// is held under, and their passes would each run a statement to read
/// nothing. So the re-decision asks about its keys [`LINK_HEALTH_CHUNK`] at a
/// time, one statement a chunk, and runs a pass only over a key that pass
/// would read something under: a key nothing is held under costs its share
/// of that statement and no statement of its own. Passing a pass over changes
/// nothing another pass decides, since each such decision asks whether a link
/// holds a key under a class or at a path, and no link holds one under a key
/// whose pass is passed over.
fn occupied(
    connection: &Connection,
    work: &ReadWork,
    key: SuffixKey,
    classes: &[&ClassKey],
    paths: &[&PathKey],
) -> Result<Occupied, StoreError> {
    let rows = Request::read_all_on(
        connection,
        work,
        &statement::occupied_sql(key),
        params_from_iter(statement::occupied_parameters(classes, paths)?),
        |row| Ok(Ok((row.get::<_, i64>(0)?, row.get::<_, usize>(1)?))),
        "asking which keys a re-decision reaches anything under",
    )?;
    let mut occupied = Occupied::default();
    for (arm, at) in rows {
        let held = match arm {
            statement::CLASS_ARM => Some((&mut occupied.class_links, classes.len())),
            statement::FINDINGS_ARM => Some((&mut occupied.class_findings, classes.len())),
            statement::PATH_ARM => Some((&mut occupied.paths, paths.len())),
            _ => None,
        };
        match held {
            Some((held, listed)) if at < listed => held.insert(at),
            _ => {
                return Err(StoreError::Damaged {
                    what: format!("an occupancy read answered a key it was not asked, {arm}:{at}"),
                });
            }
        };
    }
    Ok(occupied)
}

/// Whether `held` holds a suffix key in one of `classes`: a link the class arm
/// reaches by its own key.
fn reached_by_a_class(held: &Held, classes: &BTreeSet<ClassKey>) -> bool {
    held.keys.iter().any(|(text, segments)| {
        segments.is_some() && class_of(text).is_some_and(|class| classes_hold(classes, class))
    })
}

/// Discard the findings whose row ids `findings` holds, and report how many
/// went.
fn discard(transaction: &Transaction<'_>, findings: &BTreeSet<i64>) -> Result<u64, StoreError> {
    if findings.is_empty() {
        return Ok(0);
    }
    let ids: Vec<i64> = findings.iter().copied().collect();
    let values = statement::ids_parameters(&ids)?;
    let discarded = transaction
        .prepare_cached(&statement::discard_sql())
        .and_then(|mut discard| discard.execute(params_from_iter(values)))
        .map_err(|error| crate::error::sql("discarding the findings under a class", error))?;
    Ok(discarded as u64)
}

/// The links whose row ids `links` holds, read as [`read_links`] reads them.
fn read_links_by_id(
    connection: &Connection,
    work: &ReadWork,
    key: SuffixKey,
    links: &BTreeSet<i64>,
) -> Result<Vec<Held>, StoreError> {
    if links.is_empty() {
        return Ok(Vec::new());
    }
    let ids: Vec<i64> = links.iter().copied().collect();
    let (links, _) = read_links(
        connection,
        work,
        key,
        Selected::Links,
        statement::ids_parameters(&ids)?,
    )?;
    Ok(links)
}

/// The class a suffix key falls in: its first segment, separator included,
/// which is the key of the class every document whose stem that segment spells
/// is in.
fn class_of(key: &str) -> Option<&str> {
    key.find('/').map(|at| &key[..=at])
}

/// Whether `classes` holds the class spelled `class`.
fn classes_hold(classes: &BTreeSet<ClassKey>, class: &str) -> bool {
    ClassKey::new(class).is_ok_and(|class| classes.contains(&class))
}

/// The findings about `links`, judged as [`judge`] states, resolving each key
/// `summaries` does not yet hold. The findings are in the order `links` is.
fn judge_links(
    connection: &Connection,
    work: &ReadWork,
    order: StoredPathOrder,
    ignore: &AmbiguityIgnore,
    links: &[Held],
    summaries: &mut KeySummaries,
) -> Result<Vec<FindingFacts>, StoreError> {
    if links.is_empty() {
        return Ok(Vec::new());
    }
    let key = SuffixKey::under(order);
    resolve_keys(connection, work, key, order, ignore, links, summaries)?;

    let verdicts = links
        .iter()
        .map(|held| verdict(held, summaries))
        .collect::<Result<Vec<Option<Verdict>>, StoreError>>()?;
    let missing = missing_anchors(connection, work, links, &verdicts)?;

    let mut named: BTreeMap<i64, &str> = BTreeMap::new();
    for (at, verdict) in verdicts.iter().enumerate() {
        match verdict {
            Some(Verdict::Ambiguous { head, .. }) => {
                named.extend(head.iter().map(|one| (one.id, one.path.as_str())));
            }
            Some(Verdict::One(one)) if missing.contains(&at) => {
                named.insert(one.id, one.path.as_str());
            }
            _ => {}
        }
    }
    // Each candidate is named once per set of summaries: a candidate an
    // earlier chunk named keeps the name it was named by.
    named.retain(|id, _| !summaries.names.contains_key(id));
    let named = candidate_names(connection, work, order, ignore, &named)?;
    summaries.names.extend(named);
    let names = &summaries.names;

    let mut findings = Vec::new();
    for (at, (held, verdict)) in links.iter().zip(verdicts).enumerate() {
        let (kind, head, total, message) = match verdict {
            Some(Verdict::Broken) => (FindingKind::Broken, Vec::new(), 0, BROKEN),
            Some(Verdict::Ambiguous { head, total }) => {
                (FindingKind::Ambiguous, head, total, AMBIGUOUS)
            }
            Some(Verdict::One(one)) if missing.contains(&at) => {
                (FindingKind::MissingAnchor, vec![one], 1, MISSING_ANCHOR)
            }
            Some(Verdict::One(_)) | None => continue,
        };
        findings.push(finding(held, kind, &head, total, message, names)?);
    }
    Ok(findings)
}

/// The links one read of [`statement::links_sql`] in the shape `selected`
/// reaches, bound to `values`, each with its keys in the key space `key`
/// selects, in the order of the holding path, then the ordinal; and the
/// driver rows a paged read reached, in order, the last of which a page
/// resumes past.
fn read_links(
    connection: &Connection,
    work: &ReadWork,
    key: SuffixKey,
    selected: Selected,
    values: Vec<Value>,
) -> Result<(Vec<Held>, BTreeSet<After>), StoreError> {
    let rows = Request::read_all_on(
        connection,
        work,
        &statement::links_sql(key, selected),
        params_from_iter(values),
        |row| {
            let link = stored_link_row(row)?;
            let holder: String = row.get(13)?;
            let id: i64 = row.get(14)?;
            let ordinal: i64 = row.get(15)?;
            let key: Option<String> = row.get(16)?;
            let segments: Option<i64> = row.get(17)?;
            let generation: i64 = row.get(18)?;
            let after = match row.get::<_, Option<String>>(19)? {
                Some(text) => Some(After {
                    text,
                    first: row.get(20)?,
                    second: row.get(21)?,
                }),
                None => None,
            };
            Ok(link.and_then(|link| {
                let ordinal = u64::try_from(ordinal)
                    .map_err(|_| unreadable("links.ordinal", &ordinal.to_string()))?;
                let segments = segments
                    .map(|segments| {
                        u64::try_from(segments)
                            .map_err(|_| unreadable("link_keys.segments", &segments.to_string()))
                    })
                    .transpose()?;
                Ok((
                    holder,
                    generation,
                    id,
                    ordinal,
                    link,
                    key.map(|key| (key, segments)),
                    after,
                ))
            }))
        },
        "reading the links a judgment judges",
    )?;
    let mut links: Vec<Held> = Vec::new();
    let mut by_id: HashMap<i64, usize> = HashMap::new();
    let mut driven: BTreeSet<After> = BTreeSet::new();
    for (holder, generation, id, ordinal, link, key, after) in rows {
        driven.extend(after);
        let at = *by_id.entry(id).or_insert_with(|| {
            links.push(Held {
                holder,
                generation,
                id,
                ordinal,
                link,
                keys: Vec::new(),
            });
            links.len() - 1
        });
        let keys = &mut links[at].keys;
        keys.extend(key.filter(|key| !keys.contains(key)));
    }
    links.sort_by(|left, right| (&left.holder, left.ordinal).cmp(&(&right.holder, right.ordinal)));
    Ok((links, driven))
}

/// Resolve every distinct key `links` hold that `summaries` does not hold
/// yet: read each one's head, and the total of each whose head filled, and
/// keep them in `summaries`.
fn resolve_keys(
    connection: &Connection,
    work: &ReadWork,
    key: SuffixKey,
    order: StoredPathOrder,
    ignore: &AmbiguityIgnore,
    links: &[Held],
    summaries: &mut KeySummaries,
) -> Result<(), StoreError> {
    let unresolved: BTreeSet<&Key> = links
        .iter()
        .flat_map(|held| &held.keys)
        .filter(|key| !summaries.summaries.contains_key(*key))
        .collect();
    if unresolved.is_empty() {
        return Ok(());
    }
    // The keys in the order the two arms list them, each arm's list the
    // statements walk.
    let mut listed: [Vec<&Key>; 2] = [Vec::new(), Vec::new()];
    let mut classes: Vec<(&str, u64)> = Vec::new();
    let mut paths: Vec<&str> = Vec::new();
    for listed_key in unresolved {
        match listed_key {
            (text, Some(segments)) => {
                classes.push((text, *segments));
                listed[0].push(listed_key);
            }
            (text, None) => {
                paths.push(text);
                listed[1].push(listed_key);
            }
        }
    }
    let mut read: [Vec<Summary>; 2] = [
        classes.iter().map(|_| Summary::default()).collect(),
        paths.iter().map(|_| Summary::default()).collect(),
    ];
    let heads = Request::read_all_on(
        connection,
        work,
        &statement::heads_sql(key),
        params_from_iter(statement::keys_parameters(
            &classes,
            &paths,
            ignore,
            order,
            Some(CANDIDATE_HEAD),
        )?),
        |row| {
            Ok(Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, usize>(1)?,
                Named {
                    id: row.get(2)?,
                    path: row.get(3)?,
                    rung: row.get(4)?,
                },
            )))
        },
        "reading what a judgment's keys name",
    )?;
    for (arm, index, named) in heads {
        summary(&mut read, arm, index)?.head.push(named);
    }

    // A head cut below its bound is the whole of what its key names; only a
    // filled head is counted, and only where no earlier resolution counted it.
    let mut filled: [Vec<usize>; 2] = [Vec::new(), Vec::new()];
    for (arm, heads) in read.iter_mut().enumerate() {
        for (index, summary) in heads.iter_mut().enumerate() {
            summary.head.sort_by(Named::ladder);
            summary.total = summary.head.len() as u64;
            if summary.head.len() < CANDIDATE_HEAD {
                continue;
            }
            match summaries.totals.get(listed[arm][index]) {
                Some(total) => summary.total = *total,
                None => filled[arm].push(index),
            }
        }
    }
    if filled.iter().any(|filled| !filled.is_empty()) {
        let classes: Vec<(&str, u64)> = filled[0].iter().map(|at| classes[*at]).collect();
        let paths: Vec<&str> = filled[1].iter().map(|at| paths[*at]).collect();
        let totals = Request::read_all_on(
            connection,
            work,
            &statement::totals_sql(key),
            params_from_iter(statement::keys_parameters(
                &classes, &paths, ignore, order, None,
            )?),
            |row| {
                Ok(Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, usize>(1)?,
                    row.get::<_, u64>(2)?,
                )))
            },
            "counting what a judgment's keys name",
        )?;
        for (arm, at, total) in totals {
            let index = usize::try_from(arm)
                .ok()
                .and_then(|arm| filled.get(arm))
                .and_then(|filled| filled.get(at))
                .copied()
                .ok_or_else(|| StoreError::Damaged {
                    what: format!("a count answered a key it was not asked, {arm}:{at}"),
                })?;
            summary(&mut read, arm, index)?.total = total;
        }
    }

    for ((keys, read), counted) in listed.into_iter().zip(read).zip(filled) {
        for (index, (key, summary)) in keys.into_iter().zip(read).enumerate() {
            summaries.resolved += 1;
            // Each arm's counted positions were listed in order.
            if counted.binary_search(&index).is_ok() {
                summaries.candidates += summary.total;
                summaries.totals.insert(key.clone(), summary.total);
            } else {
                summaries.candidates += summary.head.len() as u64;
            }
            summaries.summaries.insert(key.clone(), summary);
        }
    }
    Ok(())
}

/// What resolving `held` against its keys' summaries found, and `None` where
/// its health raises nothing.
fn verdict(held: &Held, summaries: &KeySummaries) -> Result<Option<Verdict>, StoreError> {
    let read = held
        .keys
        .iter()
        .map(|key| summaries.of(key))
        .collect::<Result<Vec<&Summary>, StoreError>>()?;
    let total: u64 = read.iter().map(|summary| summary.total).sum();
    Ok(match LinkHealth::of_address(held.link.address, total) {
        LinkHealth::Broken => Some(Verdict::Broken),
        LinkHealth::Ambiguous => {
            let mut head: Vec<Named> = read
                .iter()
                .flat_map(|summary| summary.head.iter().cloned())
                .collect();
            head.sort_by(Named::ladder);
            head.truncate(CANDIDATE_HEAD);
            Some(Verdict::Ambiguous { head, total })
        }
        LinkHealth::Healthy => read
            .iter()
            .find_map(|summary| summary.head.first().cloned())
            .map(Verdict::One),
        LinkHealth::NotJudged => None,
        // The wire's health is non-exhaustive, so a health it adds compiles
        // here; it stops the judgment rather than raising nothing.
        unknown => unreachable!("a link health the judgment has no finding for: {unknown:?}"),
    })
}

/// The positions in `links` of every link naming one document that carries an
/// anchor the document does not hold.
fn missing_anchors(
    connection: &Connection,
    work: &ReadWork,
    links: &[Held],
    verdicts: &[Option<Verdict>],
) -> Result<BTreeSet<usize>, StoreError> {
    let (positions, pairs): (Vec<usize>, Vec<(i64, i64)>) = links
        .iter()
        .zip(verdicts)
        .enumerate()
        .filter_map(|(at, (held, verdict))| match verdict {
            Some(Verdict::One(one)) if held.link.fact.anchor.is_some() => {
                Some((at, (held.id, one.id)))
            }
            _ => None,
        })
        .unzip();
    if pairs.is_empty() {
        return Ok(BTreeSet::new());
    }
    let held = Request::read_all_on(
        connection,
        work,
        &statement::anchors_sql(),
        params_from_iter(statement::anchors_parameters(&pairs)?),
        |row| Ok(Ok((row.get::<_, usize>(0)?, row.get::<_, bool>(1)?))),
        "judging the anchors of the links naming one document",
    )?;
    held.into_iter()
        .filter(|(_, held)| !held)
        .map(|(pair, _)| {
            positions
                .get(pair)
                .copied()
                .ok_or_else(|| StoreError::Damaged {
                    what: format!("an anchor read answered a link it was not asked, {pair}"),
                })
        })
        .collect()
}

/// How each candidate `named` holds is named, by its id: its minimal
/// disambiguating suffix as a read names it ([`SuffixSpellings`]) — its path
/// where no suffix names it alone — and the classes that suffix is read from,
/// both off the one set of spellings, so the classes a finding is keyed by
/// are the ones its candidates' names were read in.
fn candidate_names(
    connection: &Connection,
    work: &ReadWork,
    order: StoredPathOrder,
    ignore: &AmbiguityIgnore,
    named: &BTreeMap<i64, &str>,
) -> Result<HashMap<i64, CandidateName>, StoreError> {
    if named.is_empty() {
        return Ok(HashMap::new());
    }
    let spellings = SuffixSpellings::new(named, ignore, order)?;
    let mut classes = spellings.naming_classes();
    let (sql, values) = spellings.statement()?;
    let rows = Request::read_all_on(
        connection,
        work,
        &sql,
        params_from_iter(values),
        |row| Ok(Ok((row.get::<_, usize>(0)?, row.get::<_, i64>(1)?))),
        "naming a judgment's candidates by their suffixes",
    )?;
    let mut suffixes = spellings.suffixes(rows)?;
    Ok(named
        .iter()
        .map(|(id, path)| {
            let name = CandidateName {
                suffix: suffixes.remove(id).unwrap_or_else(|| (*path).to_string()),
                classes: classes.remove(id).unwrap_or_default(),
            };
            (*id, name)
        })
        .collect())
}

/// The finding of `kind` about `held`, carrying `head` named by `names`
/// beside `total`.
///
/// It is keyed by its link's keys, each in its own key space, and by the
/// classes its candidates' names are read from: a change in any of them can
/// move what the finding says, so the changeset's discard reaches it through
/// any of them.
fn finding(
    held: &Held,
    kind: FindingKind,
    head: &[Named],
    total: u64,
    message: &str,
    names: &HashMap<i64, CandidateName>,
) -> Result<FindingFacts, StoreError> {
    let mut class_keys = BTreeSet::new();
    let mut path_keys = BTreeSet::new();
    for (key, segments) in &held.keys {
        match segments {
            Some(_) => {
                class_keys
                    .insert(ClassKey::new(key).map_err(|_| unreadable("link_keys.key", key))?);
            }
            None => {
                path_keys.insert(PathKey::new(key).map_err(|_| unreadable("link_keys.key", key))?);
            }
        }
    }
    let candidates = head
        .iter()
        .map(|named| {
            let name = names.get(&named.id).ok_or_else(|| StoreError::Damaged {
                what: format!("a judgment named no candidate `{}`", named.path),
            })?;
            class_keys.extend(name.classes.iter().cloned());
            Ok(CandidateFact {
                path: DocumentPath::new(&named.path)
                    .map_err(|_| unreadable("documents.path", &named.path))?,
                suffix: name.suffix.clone(),
            })
        })
        .collect::<Result<Vec<CandidateFact>, StoreError>>()?;
    Ok(FindingFacts {
        kind,
        severity: kind.default_severity(),
        path: DocumentPath::new(&held.holder)
            .map_err(|_| unreadable("documents.path", &held.holder))?,
        class_keys,
        path_keys,
        target: Some(written(&held.link.fact)),
        span: Some(held.link.fact.span),
        ordinal: Some(held.ordinal),
        candidates,
        candidates_total: total,
        message: message.to_string(),
        detail: None,
    })
}

/// The link as written: its protocol, its target, and its anchor.
fn written(link: &LinkFact) -> String {
    let protocol = link
        .protocol
        .as_deref()
        .map(|protocol| format!("{protocol}://"))
        .unwrap_or_default();
    let anchor = match &link.anchor {
        Some(LinkAnchor::Heading { written, .. }) => format!("#{written}"),
        Some(LinkAnchor::Block { id }) => format!("#^{id}"),
        None => String::new(),
    };
    format!("{protocol}{}{anchor}", link.target)
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::{Change, ContentModel, DocumentFacts, IncrementProvenance, Provenance, Store};

    /// **A mass delete no link reaches runs a statement per chunk of its keys,
    /// never one per key.** Six hundred documents, none linked and none named
    /// by a finding, are killed in one changeset. Its re-decision reads the
    /// links the changeset wrote, then asks about its six hundred classes and
    /// six hundred paths [`LINK_HEALTH_CHUNK`] at a time, and runs no pass:
    /// one statement, then three for the classes and three for the paths.
    #[test]
    fn a_mass_delete_no_link_reaches_runs_a_statement_per_chunk_of_its_keys() {
        const DEATHS: usize = 600;
        let root = norn_testkit::scratch::Scratch::new("norn-store-redecide-mass-delete");
        let mut store = Store::open_throwaway(
            root.join("store.sqlite3"),
            StoredPathOrder::Sensitive,
            crate::DerivationVersion::new(1),
        )
        .expect("opening a store");
        let declared = ContentModel::under("mass-delete");
        let at = |index: usize| DocumentPath::new(&format!("d/{index:05}.md")).expect("a path");
        let mut request = store.begin_request();
        request
            .pin_vault_schema(b"mass-delete", "mass-delete")
            .expect("pinning a schema");
        request
            .apply_increment(
                IncrementProvenance::Derived,
                (0..DEATHS).map(|index| {
                    Change::Upsert(DocumentFacts::new(
                        at(index),
                        format!("{index}"),
                        "alpha\n",
                        6,
                    ))
                }),
                &[],
                &declared,
            )
            .expect("writing the documents");

        let before = request.read_statements();
        request
            .apply_increment(
                IncrementProvenance::Derived,
                (0..DEATHS).map(|index| Change::Death {
                    path: at(index),
                    provenance: Provenance::WatcherRemoval,
                }),
                &[],
                &declared,
            )
            .expect("the mass delete");
        let chunks = DEATHS.div_ceil(LINK_HEALTH_CHUNK) as u64;
        assert_eq!(
            request.read_statements() - before,
            1 + 2 * chunks,
            "a re-decision over {DEATHS} classes and {DEATHS} paths nothing is held under is \
             the written links' read and one occupancy read per chunk of {LINK_HEALTH_CHUNK} \
             keys"
        );
    }
}
