//! Link health: the findings the store judges about the links a set of
//! documents holds — **broken**, **ambiguous** and **missing anchor**, each a
//! finding about exactly one link, at warning severity ([ADR 0027]).
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
//! written — protocol and anchor included — as its target. It is keyed by
//! exactly the link's keys in the key space the root probes: a suffix key as a
//! class key, and a path key as a path key, so the changeset's discard on
//! either axis reaches it through the key the link reached its documents by.
//!
//! # The work adds links and candidates, and never multiplies them
//!
//! A link's total is the sum over its keys, and its head the merge of its
//! keys' heads in ladder order, cut to [`CANDIDATE_HEAD`]; the keys' classes
//! are disjoint, so the merge is the head of the whole. So **each distinct key
//! is resolved once**, however many of the links carry it: one statement reads
//! every distinct key's head ([`statement::heads_sql`]), and one counts the
//! keys whose head filled ([`statement::totals_sql`]). The links cost their
//! own seeks, and each key its class, so the work is the links plus the
//! candidates they resolve against. Resolving each link against its class, as
//! a read's links column does, would cost the links times the candidates.
//!
//! Five statements judge a set of documents, whatever it holds: the links and
//! their keys, the keys' heads, the filled heads' totals, the anchors of the
//! links naming one document, and the suffixes of the candidates the findings
//! carry. A statement with nothing to read is not run.
//!
//! **A dormant carrier.** Its consuming layer is the re-decision
//! [ADR 0027] rules into the changeset: after every entry of a changeset is
//! written, the store judges the links the changeset reaches and files these
//! findings in the same transaction. That re-decision is not built, so no
//! write reaches this module yet: [`crate::Request::judge_link_health`] is its
//! one door, and the plan seam bars each statement it runs.
//!
//! [ADR 0027]: https://github.com/dbtlr/norn/blob/main/docs/decisions/0027-link-health-rides-the-changeset.md

pub(crate) mod statement;

use std::collections::{BTreeMap, BTreeSet, HashMap};

use norn_db::rusqlite::{Connection, params_from_iter};
use norn_wire::{FindingKind, LinkHealth};

use crate::error::StoreError;
use crate::facts::{
    CANDIDATE_HEAD, CandidateFact, FindingFacts, LinkAnchor, LinkFact, StoredLink, StoredPathOrder,
};
use crate::path::{ClassKey, DocumentPath, PathKey, SuffixKey};
use crate::read::SuffixSpellings;
use crate::request::{ReadWork, Request, stored_link_row, unreadable};
use crate::resolve::AmbiguityIgnore;

/// The message every broken link's finding carries.
const BROKEN: &str = "the link names no document";

/// The message every ambiguous link's finding carries.
const AMBIGUOUS: &str = "the link names more than one document";

/// The message every missing anchor's finding carries.
const MISSING_ANCHOR: &str = "the document the link names holds no place its anchor names";

/// One link under judgment: where it is held, its row, and its keys.
struct Held {
    holder: String,
    id: i64,
    ordinal: u64,
    link: StoredLink,
    /// Each key the link index holds the link under, in the root's key space,
    /// beside its segment count, which a suffix key carries and a path key
    /// does not.
    keys: Vec<(String, Option<u64>)>,
}

/// One document a key names: its row id, its path, and its rung on the
/// resolution ladder.
#[derive(Clone, Debug)]
struct Named {
    id: i64,
    path: String,
    rung: String,
}

/// What one key names: the head of it in ladder order, and how many.
#[derive(Default)]
struct Summary {
    head: Vec<Named>,
    total: u64,
}

/// Every distinct key the judged links hold, in the two key spaces a link is
/// addressed in, each with its summary once it is read.
#[derive(Default)]
struct Keys {
    classes: Vec<(String, u64)>,
    paths: Vec<String>,
    /// Where each key stands: the arm and its index in that arm's list.
    places: BTreeMap<(String, Option<u64>), (i64, usize)>,
    summaries: [Vec<Summary>; 2],
}

impl Keys {
    fn of(links: &[Held]) -> Self {
        let mut keys = Keys::default();
        for (key, segments) in links.iter().flat_map(|held| &held.keys) {
            if keys.places.contains_key(&(key.clone(), *segments)) {
                continue;
            }
            let place = match segments {
                Some(segments) => {
                    keys.classes.push((key.clone(), *segments));
                    (statement::CLASS_ARM, keys.classes.len() - 1)
                }
                None => {
                    keys.paths.push(key.clone());
                    (statement::PATH_ARM, keys.paths.len() - 1)
                }
            };
            keys.places.insert((key.clone(), *segments), place);
        }
        keys.summaries = [
            keys.classes.iter().map(|_| Summary::default()).collect(),
            keys.paths.iter().map(|_| Summary::default()).collect(),
        ];
        keys
    }

    fn is_empty(&self) -> bool {
        self.places.is_empty()
    }

    fn summary_of(&self, key: &str, segments: Option<u64>) -> &Summary {
        let (arm, index) = self.places[&(key.to_string(), segments)];
        &self.summaries[arm as usize][index]
    }
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

/// The link-health findings about every link the documents at `documents`
/// hold, judged against the documents the store holds now under the key space
/// `order` selects, less the places `ignore` keeps out of a class. Each
/// statement runs on `connection` and its steps are added to `work`.
///
/// The findings are in the order of the holding document's path, then the
/// link's ordinal.
pub(crate) fn judge(
    connection: &Connection,
    work: &ReadWork,
    order: StoredPathOrder,
    ignore: &AmbiguityIgnore,
    documents: &[DocumentPath],
) -> Result<Vec<FindingFacts>, StoreError> {
    if documents.is_empty() {
        return Ok(Vec::new());
    }
    let key = SuffixKey::under(order);
    let links = held_links(connection, work, key, documents)?;
    let mut keys = Keys::of(&links);
    resolve_keys(connection, work, key, order, ignore, &mut keys)?;

    let verdicts: Vec<Option<Verdict>> = links.iter().map(|held| verdict(held, &keys)).collect();
    let missing = missing_anchors(connection, work, &links, &verdicts)?;

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
    let suffixes = candidate_suffixes(connection, work, order, ignore, &named)?;

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
        findings.push(finding(held, kind, &head, total, message, &suffixes)?);
    }
    Ok(findings)
}

/// Every link the documents at `documents` hold, with its keys in the key
/// space `key` selects, in the order of the holding path, then the ordinal.
fn held_links(
    connection: &Connection,
    work: &ReadWork,
    key: SuffixKey,
    documents: &[DocumentPath],
) -> Result<Vec<Held>, StoreError> {
    let paths: Vec<&str> = documents.iter().map(DocumentPath::as_str).collect();
    let rows = Request::read_all_on(
        connection,
        work,
        &statement::links_sql(key),
        params_from_iter(statement::links_parameters(&paths)?),
        |row| {
            let link = stored_link_row(row)?;
            let holder: String = row.get(13)?;
            let id: i64 = row.get(14)?;
            let ordinal: i64 = row.get(15)?;
            let key: Option<String> = row.get(16)?;
            let segments: Option<i64> = row.get(17)?;
            Ok(link.and_then(|link| {
                let ordinal = u64::try_from(ordinal)
                    .map_err(|_| unreadable("links.ordinal", &ordinal.to_string()))?;
                let segments = segments
                    .map(|segments| {
                        u64::try_from(segments)
                            .map_err(|_| unreadable("link_keys.segments", &segments.to_string()))
                    })
                    .transpose()?;
                Ok((holder, id, ordinal, link, key.map(|key| (key, segments))))
            }))
        },
        "reading the links a judgment judges",
    )?;
    let mut links: Vec<Held> = Vec::new();
    let mut by_id: HashMap<i64, usize> = HashMap::new();
    for (holder, id, ordinal, link, key) in rows {
        let at = *by_id.entry(id).or_insert_with(|| {
            links.push(Held {
                holder,
                id,
                ordinal,
                link,
                keys: Vec::new(),
            });
            links.len() - 1
        });
        links[at].keys.extend(key);
    }
    links.sort_by(|left, right| (&left.holder, left.ordinal).cmp(&(&right.holder, right.ordinal)));
    Ok(links)
}

/// Read every distinct key's head, and the total of every key whose head
/// filled, into `keys`.
fn resolve_keys(
    connection: &Connection,
    work: &ReadWork,
    key: SuffixKey,
    order: StoredPathOrder,
    ignore: &AmbiguityIgnore,
    keys: &mut Keys,
) -> Result<(), StoreError> {
    if keys.is_empty() {
        return Ok(());
    }
    let classes: Vec<(&str, u64)> = keys
        .classes
        .iter()
        .map(|(key, segments)| (key.as_str(), *segments))
        .collect();
    let paths: Vec<&str> = keys.paths.iter().map(String::as_str).collect();
    let summaries = &mut keys.summaries;
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
        summary(summaries, arm, index)?.head.push(named);
    }

    // A head cut below its bound is the whole of what its key names; only a
    // filled head is counted.
    let mut filled: [Vec<usize>; 2] = [Vec::new(), Vec::new()];
    for (arm, heads) in summaries.iter_mut().enumerate() {
        for (index, summary) in heads.iter_mut().enumerate() {
            summary
                .head
                .sort_by(|left, right| (&left.rung, &left.path).cmp(&(&right.rung, &right.path)));
            summary.total = summary.head.len() as u64;
            if summary.head.len() >= CANDIDATE_HEAD {
                filled[arm].push(index);
            }
        }
    }
    if filled.iter().all(Vec::is_empty) {
        return Ok(());
    }
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
        summary(summaries, arm, index)?.total = total;
    }
    Ok(())
}

/// What resolving `held` against its keys' summaries found, and `None` where
/// its health raises nothing.
fn verdict(held: &Held, keys: &Keys) -> Option<Verdict> {
    let summaries: Vec<&Summary> = held
        .keys
        .iter()
        .map(|(key, segments)| keys.summary_of(key, *segments))
        .collect();
    let total: u64 = summaries.iter().map(|summary| summary.total).sum();
    match LinkHealth::of_address(held.link.address, total) {
        LinkHealth::Broken => Some(Verdict::Broken),
        LinkHealth::Ambiguous => {
            let mut head: Vec<Named> = summaries
                .iter()
                .flat_map(|summary| summary.head.iter().cloned())
                .collect();
            head.sort_by(|left, right| (&left.rung, &left.path).cmp(&(&right.rung, &right.path)));
            head.truncate(CANDIDATE_HEAD);
            Some(Verdict::Ambiguous { head, total })
        }
        LinkHealth::Healthy => summaries
            .iter()
            .find_map(|summary| summary.head.first().cloned())
            .map(Verdict::One),
        _ => None,
    }
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

/// The minimal disambiguating suffix of each candidate `named` holds, by its
/// id, as a read names it ([`SuffixSpellings`]).
fn candidate_suffixes(
    connection: &Connection,
    work: &ReadWork,
    order: StoredPathOrder,
    ignore: &AmbiguityIgnore,
    named: &BTreeMap<i64, &str>,
) -> Result<HashMap<i64, String>, StoreError> {
    if named.is_empty() {
        return Ok(HashMap::new());
    }
    let spellings = SuffixSpellings::new(named, ignore, order)?;
    let (sql, values) = spellings.statement()?;
    let rows = Request::read_all_on(
        connection,
        work,
        &sql,
        params_from_iter(values),
        |row| Ok(Ok((row.get::<_, usize>(0)?, row.get::<_, i64>(1)?))),
        "naming a judgment's candidates by their suffixes",
    )?;
    spellings.suffixes(rows)
}

/// The finding of `kind` about `held`, carrying `head` named by `suffixes`
/// beside `total`.
fn finding(
    held: &Held,
    kind: FindingKind,
    head: &[Named],
    total: u64,
    message: &str,
    suffixes: &HashMap<i64, String>,
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
            Ok(CandidateFact {
                path: DocumentPath::new(&named.path)
                    .map_err(|_| unreadable("documents.path", &named.path))?,
                suffix: suffixes
                    .get(&named.id)
                    .cloned()
                    .unwrap_or_else(|| named.path.clone()),
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
