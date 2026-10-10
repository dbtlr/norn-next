//! The respell check: which routes' link cascades could change how another
//! document is judged.
//!
//! **A cascade changes a judgment only where it respells a frontmatter
//! wikilink in a field a rule reads by value.** The schema judge reads a
//! document's path, its frontmatter and its body's tags. A cascade changes no
//! path but the moved document's, which its route's own judgment reads where
//! it lands ([`super::routes`]); a link rewrite in a body never adds or removes
//! a tag, since the text layer refuses to respell a link where the rewrite
//! would leave a tag around it (`norn_text`'s `RewriteSkip::Unrepresentable`);
//! and a frontmatter Markdown link is no link at all, the derivation reading
//! only a frontmatter's wikilinks (`crate::derivation::frontmatter_links`).
//! What is left is a frontmatter wikilink whose spelling changes, which
//! changes a judgment only where a rule or a field declaration reads the
//! field's value, not merely its presence.
//!
//! **A route skips as [`SkipReason::RespellsAJudgedLink`] where its cascade
//! may respell a frontmatter wikilink in a field its holder's rules read by
//! value**, the note naming the holder and the field. Every other route's
//! effect on other documents is judgment-neutral, so no holder is judged
//! with a cascade and no cascade is generated here: the plan's one resolution
//! writes them all.
//!
//! **"May respell" is decided before the plan, locally.** A link follows a
//! move where it resolves before the plan to exactly the moved document
//! (`crate::planner::cascade`), so a frontmatter wikilink may be respelled by
//! a route where it resolves before the plan to exactly the route's document,
//! read through the store's resolution door in one batched read over every
//! route the batch proposes, the routed documents' own frontmatter links
//! probed beside the links the store holds. A bare single-segment wikilink
//! keeps naming the document, since a route keeps the file name and so every
//! stem's documents stay where they are counted; except where a route of the
//! batch carries a document of that stem across the schema's ambiguity-ignore
//! set — as the store's own rule reads the set, so a document a bare link
//! names on one side of its route and not the other — which changes what the
//! stem counts, so such a stem's bare links may be respelled too. A
//! `vault://` or path-qualified wikilink may be respelled. A link the text
//! layer cannot place — a flow list's item, an escaped scalar — has no bytes
//! a rewrite can write over, so every cascade leaves it as written, advised
//! on as a Layer 4 move advises on it, and it is never respelled. A placed
//! link no spelling of the destination reads back for is still counted as
//! one that may be respelled: which spelling reads back is the resolution's
//! to find, and counting it keeps the check local.
//!
//! **"Read by value" is read on every state the holder may end in**: a
//! field is read by value where it is a `match.frontmatter` key of any rule,
//! the tags field, or a field declared a type other than text or link — a
//! respelled `[[x]]` is a new spelling a type mismatch names — or where, at
//! any of the holder's candidate states, the combined constraint of the rules
//! selecting it holds a closed set, a length limit or a forbiddance on the
//! field (a rules conflict on a field is one of these), or the state holds a
//! finding on the field. A holder's candidate states are its before-state;
//! its fixes composed where it stands, where the batch makes fixes to it; and
//! its route's move and fixes composed where it lands, where its route passes
//! its own judgment. Judging the union makes each route's answer independent
//! of whether its holder's own route is made, so routes need no order here.
//!
//! **The cost is linear in the links under the routed documents' keys**: one
//! batched read of the index, the routed documents' frontmatter links probed
//! once per candidate state, each holder outside the batch with a link that
//! may be respelled read once at its before-state, through the view the plan
//! resolves on — which reads it again from memory where its cascade composes
//! it — and judged once only where a frontmatter field of it holds such a
//! link. The store keeps no record of whether a link stands in a frontmatter
//! or a body, so a holder is read to tell; one whose links stand in its body
//! alone is never judged.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use norn_config::schema::{FieldType, RuleWork, VaultSchema};
use norn_fs::NormalizedPath;
use norn_store::{AmbiguityIgnore, LinkFamily, PathOverlay, ProbedLink, StoredPathOrder};
use norn_wire::{CaseFold, DocumentPath, FindingKind, Resolves, SkipReason, SkippedFinding};

use super::{Draft, State};
use crate::applier::standing;
use crate::derivation::{frontmatter_field_links, written_fields};
use crate::evidence::count_rule_work;
use crate::planner::repair::{Before, Reading, Repairing};

/// A wikilink's address as its frontmatter writes it: its target and its
/// protocol. Two links of one holder written alike resolve alike, so the
/// address names every link a respelling of one would respell.
type Address = (String, Option<String>);

/// A document holding a link a route may respell: a batch document, by its
/// draft's position, judged at every state it may end in; or one outside the
/// batch, by its path, judged where it stands.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum Holder {
    Batch(usize),
    Outside(DocumentPath),
}

/// The routes of `drafts` whose link cascade may respell a frontmatter
/// wikilink in a field a rule reads by value, each with its skip, by the
/// draft's position; read through `vault` under `repairing`. The index is
/// not read where no route passes its own judgment.
pub(in crate::planner::repair) fn respelling<R: Reading>(
    drafts: &[Draft<'_>],
    repairing: &Repairing<'_>,
    vault: &R,
) -> Result<BTreeMap<usize, SkippedFinding>, R::Error> {
    #[cfg(test)]
    if BLIND.with(std::cell::Cell::get) {
        return Ok(BTreeMap::new());
    }
    let routes: Vec<(usize, &super::routes::Passed)> = drafts
        .iter()
        .enumerate()
        .filter_map(|(at, draft)| draft.passed().map(|(passed, _)| (at, passed)))
        .collect();
    if routes.is_empty() {
        return Ok(BTreeMap::new());
    }
    let schema = repairing.declared.schema();
    // Each route by the place its document leaves; and each batch document
    // by every place it may stand at, two routes landing at one place each
    // naming it.
    let place = |path: &DocumentPath| vault.place(path);
    let origins: BTreeMap<NormalizedPath, usize> = routes
        .iter()
        .filter_map(|(at, passed)| Some((place(&passed.from)?, *at)))
        .collect();
    let mut documents: BTreeMap<NormalizedPath, BTreeSet<usize>> = BTreeMap::new();
    for (at, draft) in drafts.iter().enumerate() {
        let Some(compositions) = draft.compositions() else {
            continue;
        };
        let lands = draft.passed().and_then(|(passed, _)| place(&passed.to));
        for stands in [place(&compositions.reckoned.start.at), lands]
            .into_iter()
            .flatten()
        {
            documents.entry(stands).or_default().insert(at);
        }
    }
    let crossing = crossing_stems(&routes, schema, repairing);

    let mut overlay = PathOverlay::new();
    for (_, passed) in &routes {
        overlay =
            overlay
                .with(stored(&passed.from), true, false)
                .with(stored(&passed.to), false, true);
    }
    let probed = probes(drafts);

    // The routes each holder's links name, by the address the link writes.
    let mut named: BTreeMap<Holder, BTreeMap<Address, BTreeSet<usize>>> = BTreeMap::new();
    vault.links(&overlay, &probed, &mut |change| {
        if change.link.family != LinkFamily::Wikilink {
            return;
        }
        let Resolves::One { path } = &change.before else {
            return;
        };
        let Some(route) = place(path).and_then(|named| origins.get(&named)) else {
            return;
        };
        if !may_respell(&change.link, &crossing) {
            return;
        }
        // A batch document is named by its draft, wherever the probe held
        // it; a place two routes land at names each.
        let holder = DocumentPath::new(change.holder.as_str())
            .expect("a stored document path is a wire document path");
        let holders: Vec<Holder> = match place(&holder).and_then(|held| documents.get(&held)) {
            Some(batch) => batch.iter().copied().map(Holder::Batch).collect(),
            None => vec![Holder::Outside(holder)],
        };
        let address = (change.link.target.clone(), change.link.protocol.clone());
        for holder in holders {
            named
                .entry(holder)
                .or_default()
                .entry(address.clone())
                .or_default()
                .insert(*route);
        }
    })?;

    let selector_keys: BTreeSet<&str> = schema
        .rules()
        .flat_map(|rule| rule.selector().frontmatter().map(|(key, _)| key))
        .collect();
    let mut refused = BTreeMap::new();
    for (holder, addresses) in &named {
        // The fields each state the holder may end in holds a link the
        // routes may respell in; a holder holding none — its links in its
        // body, or written where the text layer cannot respell them — is
        // never judged.
        let (at, states) = match holder {
            Holder::Batch(at) => {
                let states = candidate_states(&drafts[*at]);
                (states[0].at.clone(), states)
            }
            Holder::Outside(holder) => match vault.before(holder)? {
                Before::Held(bytes) => {
                    if respelled(&bytes, addresses).next().is_none() {
                        continue;
                    }
                    let standing = standing(holder, &bytes, repairing.declared, repairing.case);
                    (
                        holder.clone(),
                        vec![State::of(holder.clone(), bytes, standing)],
                    )
                }
                // A holder that does not read holds no field the cascade
                // could respell; its cascade cannot compose either.
                Before::Unread => continue,
            },
        };
        let fields: Vec<(String, &BTreeSet<usize>)> = states
            .iter()
            .flat_map(|state| respelled(&state.bytes, addresses).collect::<Vec<_>>())
            .collect();
        if fields.is_empty() {
            continue;
        }
        let judged = ValueRead::of(&states, schema, &selector_keys, repairing);
        for (field, routes) in fields {
            if !judged.reads(&field) {
                continue;
            }
            for &route in routes {
                refused.entry(route).or_insert_with(|| {
                    let (passed, _) = drafts[route]
                        .passed()
                        .expect("a route the index names passed its own judgment");
                    passed.skipped(
                        SkipReason::RespellsAJudgedLink,
                        format!(
                            "moving the document to `{}` would respell its link in `{field}` \
                             of `{at}`, a field the rules read by value",
                            passed.to
                        ),
                    )
                });
            }
        }
    }
    Ok(refused)
}

#[cfg(test)]
thread_local! {
    /// Whether the respell check judges no route, as a fault in its local
    /// judgment would: what the host's backstop is proven against.
    static BLIND: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// `run`, with the respell check judging no route on this thread: a fault
/// injected into the planning's local judgment, which the host's judgment of
/// the whole plan must catch (`crate::apply`'s guard).
#[cfg(test)]
pub(crate) fn blinded<T>(run: impl FnOnce() -> T) -> T {
    BLIND.with(|blind| blind.set(true));
    let ran = run();
    BLIND.with(|blind| blind.set(false));
    ran
}

/// The stems whose bare wikilinks a route of `routes` may respell: the file
/// stem of each routed document a bare link names at its origin and not at
/// its destination, or the other way about, as the store's own
/// ambiguity-ignore rule reads the schema's set
/// ([`AmbiguityIgnore::admits`] for a one-segment target), folded as
/// [`stem_of`] folds one.
fn crossing_stems(
    routes: &[(usize, &super::routes::Passed)],
    schema: &VaultSchema,
    repairing: &Repairing<'_>,
) -> BTreeSet<String> {
    if schema.ambiguity_ignore().is_empty() {
        return BTreeSet::new();
    }
    let ignore = AmbiguityIgnore::new(schema.ambiguity_ignore().iter().cloned());
    let order = match repairing.case {
        CaseFold::Exact => StoredPathOrder::Sensitive,
        CaseFold::Ascii => StoredPathOrder::AsciiCaseInsensitive,
    };
    let named_bare = |path: &DocumentPath| ignore.admits(path.as_str(), 1, order);
    routes
        .iter()
        .filter(|(_, passed)| named_bare(&passed.from) != named_bare(&passed.to))
        .filter_map(|(_, passed)| passed.to.as_str().rsplit('/').next().map(stem_of))
        .collect()
}

/// `name`, a file name or a bare wikilink's target, as one stem's documents
/// are compared here: lower-cased, the document extension dropped. A wider
/// fold than the store's only ever counts more links as possibly respelled.
fn stem_of(name: &str) -> String {
    let folded = name.to_lowercase();
    match folded.strip_suffix(".md") {
        Some(stem) => stem.to_string(),
        None => folded,
    }
}

/// Whether a cascade may respell `link`, a wikilink resolving before the
/// plan to exactly a routed document: any but a bare single-segment one,
/// and a bare one whose stem a route carries across the ambiguity-ignore
/// set, `crossing`.
fn may_respell(link: &norn_store::LinkFact, crossing: &BTreeSet<String>) -> bool {
    link.protocol.is_some()
        || link.target.contains('/')
        || crossing.contains(&stem_of(&link.target))
}

/// Every frontmatter wikilink each batch document holds at a state it may
/// end in, probed from where it stands before the plan, and held where its
/// route lands where it has one.
fn probes(drafts: &[Draft<'_>]) -> Vec<ProbedLink> {
    let mut probed = Vec::new();
    for draft in drafts {
        let Some(compositions) = draft.compositions() else {
            continue;
        };
        let before_holder = stored(&compositions.reckoned.start.at);
        let after_holder = match draft.passed() {
            Some((passed, _)) => stored(&passed.to),
            None => before_holder.clone(),
        };
        let mut seen = BTreeSet::new();
        for state in candidate_states(draft) {
            for (_, link) in frontmatter_field_links(&state.bytes) {
                if seen.insert((link.target.clone(), link.protocol.clone(), link.embed)) {
                    probed.push(ProbedLink {
                        before_holder: before_holder.clone(),
                        after_holder: after_holder.clone(),
                        link,
                        written: false,
                    });
                }
            }
        }
    }
    probed
}

/// The states the document `draft` holds may end in: its before-state, its
/// fixes composed where it stands, and its route's move and fixes composed
/// where it lands, where it has one that passed its own judgment; each once.
fn candidate_states(draft: &Draft<'_>) -> Vec<State> {
    let Some(compositions) = draft.compositions() else {
        return Vec::new();
    };
    let mut states = vec![compositions.reckoned.start.clone()];
    for composed in [
        Some(&compositions.origin),
        compositions.destination.as_ref(),
    ]
    .into_iter()
    .flatten()
    {
        let state = &composed.state;
        if !states
            .iter()
            .any(|seen| seen.at == state.at && Arc::ptr_eq(&seen.bytes, &state.bytes))
        {
            states.push(state.clone());
        }
    }
    states
}

/// Each frontmatter field of the document `bytes` spell holding a wikilink
/// of one of `addresses` the text layer can respell, with the routes that
/// may respell it; a field once per such link. A link the text layer cannot
/// place — no bytes a rewrite can write over — is left as written by every
/// cascade ([`norn_text::RewriteSkip::Unplaced`]), so it is never respelled.
fn respelled<'a>(
    bytes: &[u8],
    addresses: &'a BTreeMap<Address, BTreeSet<usize>>,
) -> impl Iterator<Item = (String, &'a BTreeSet<usize>)> {
    frontmatter_field_links(bytes)
        .into_iter()
        .filter(|(_, link)| link.span.is_some())
        .filter_map(|(field, link)| Some((field, addresses.get(&(link.target, link.protocol))?)))
}

/// The fields a holder's rules and field declarations read by value at any
/// of its candidate states (see the [module](self)).
struct ValueRead<'s> {
    schema: &'s VaultSchema,
    selector_keys: &'s BTreeSet<&'s str>,
    /// The fields a closed set, a length limit or a forbiddance constrains,
    /// or a finding stands on, at some candidate state.
    judged: BTreeSet<String>,
}

impl<'s> ValueRead<'s> {
    /// What the rules selecting each of `states` read by value, each state's
    /// selection tallied on the rule counters.
    fn of(
        states: &[State],
        schema: &'s VaultSchema,
        selector_keys: &'s BTreeSet<&'s str>,
        repairing: &Repairing<'_>,
    ) -> Self {
        let mut judged = BTreeSet::new();
        for state in states {
            let frontmatter = written_fields(&state.bytes).unwrap_or_default();
            let mut work = RuleWork::default();
            let selecting =
                schema.selecting_rules(state.at.as_str(), &frontmatter, repairing.case, &mut work);
            count_rule_work(work);
            for (field, constraint) in schema.combined(&selecting).fields() {
                if constraint.one_of().is_some()
                    || constraint.max_length().is_some()
                    || constraint.is_forbidden()
                {
                    judged.insert(field.to_string());
                }
            }
            // An undeclared tag's finding names the tag, not a field: the
            // tags field is read by value whatever it holds.
            judged.extend(
                state
                    .holds
                    .iter()
                    .filter(|held| held.kind != FindingKind::UndeclaredTag)
                    .filter_map(|held| held.field.clone()),
            );
        }
        ValueRead {
            schema,
            selector_keys,
            judged,
        }
    }

    /// Whether `field` is read by value.
    fn reads(&self, field: &str) -> bool {
        self.selector_keys.contains(field)
            || field == norn_text::TAGS_FIELD
            || !matches!(
                self.schema.declared_type(field),
                FieldType::Text | FieldType::Link
            )
            || self.judged.contains(field)
    }
}

/// `path` as the store names it.
fn stored(path: &DocumentPath) -> norn_store::DocumentPath {
    norn_store::DocumentPath::from(path)
}

#[cfg(test)]
#[allow(clippy::disallowed_methods)] // Harness scaffolding: the tree each case plans over.
mod tests {
    use norn_config::schema::{LocalTimestamp, NotALocalTimestamp};
    use norn_testkit::scratch::Scratch;
    use norn_wire::{CandidateHead, FindingRow, RuleSet, Severity};

    use super::*;
    use crate::clock::OneReading;
    use crate::derivation::Declared;
    use crate::planner::control::SchemaPlace;
    use crate::planner::links::testing::TreeStore;
    use crate::planner::repair::{Over, Planned, plan};
    use crate::planner::view::TreeView;

    /// The rule set every misplaced row cites.
    const RULE_SET: u64 = 1;

    /// The rows of a misplaced finding of each of `routed`, in path order,
    /// numbered from 1, citing [`RULE_SET`].
    fn misplaced(routed: &[&str]) -> Vec<FindingRow> {
        let mut paths = routed.to_vec();
        paths.sort_unstable();
        paths
            .into_iter()
            .enumerate()
            .map(|(at, path)| {
                FindingRow::new(
                    at as u64 + 1,
                    FindingKind::Misplaced,
                    Severity::Warning,
                    DocumentPath::new(path).expect("a document path"),
                    None,
                    None,
                    CandidateHead::new([], 0).expect("an empty head"),
                    None,
                    "a finding",
                    1,
                )
                .citing(RULE_SET)
            })
            .collect()
    }

    /// The repair of the misplaced documents `routed` among `documents`,
    /// written to a tree on disk whose links a store derived, under `schema`.
    fn planned(schema: &str, documents: &[(&str, &str)], routed: &[&str]) -> Planned {
        tallied(schema, documents, routed).0
    }

    /// [`planned`], with the rule work the planning alone paid: the tree's
    /// derivation is not counted.
    fn tallied(
        schema: &str,
        documents: &[(&str, &str)],
        routed: &[&str],
    ) -> (Planned, norn_config::schema::RuleWork) {
        let scratch = Scratch::new("repair-respells");
        for (path, text) in documents {
            let at = scratch.join(path);
            std::fs::create_dir_all(at.parent().expect("a parent")).expect("a folder");
            std::fs::write(at, text).expect("a document");
        }
        let declared = Declared::pinned(
            norn_config::schema::VaultSchema::parse(schema.as_bytes()).expect("a schema"),
            "a fingerprint",
        );
        let rule_sets = [RuleSet::new(
            RULE_SET,
            declared
                .schema()
                .rules()
                .map(|rule| rule.name().to_string()),
        )
        .expect("a rule set")];
        let clock = || -> Result<LocalTimestamp, NotALocalTimestamp> {
            panic!("no route here reads the clock")
        };
        let one = OneReading::of(&clock);
        let repairing = Repairing {
            declared: &declared,
            case: norn_wire::CaseFold::Exact,
            clock: &one,
            rule_sets: &rule_sets,
        };
        let view = TreeView::open(scratch.root(), &[], &SchemaPlace::default()).expect("a view");
        let store = TreeStore::over(scratch.root());
        let index = store.index();
        let reading = Over {
            view: &view,
            index: &index,
        };
        let rows = misplaced(routed);
        let (planned, paid) = crate::evidence::rule_work_of(|| plan(&rows, &repairing, &reading));
        match planned {
            Ok(planned) => (planned, paid),
            Err(_) => panic!("the tree reads"),
        }
    }

    /// The reason each skip of `planned` gives, with its note.
    fn skips(planned: &Planned) -> Vec<(u64, SkipReason, String)> {
        planned
            .skipped
            .iter()
            .map(|skipped| {
                (
                    skipped.finding,
                    skipped.reason,
                    skipped.note.clone().unwrap_or_default(),
                )
            })
            .collect()
    }

    /// The paths each move of `planned` moves.
    fn moved(planned: &Planned) -> Vec<String> {
        planned
            .operations
            .iter()
            .filter_map(|operation| match &operation.kind {
                norn_wire::OperationKind::MoveDocument { from, .. } => {
                    Some(from.as_str().to_string())
                }
                _ => None,
            })
            .collect()
    }

    /// A rule routing a task into `tasks/`, and `extra` after it.
    fn routing(extra: &str) -> String {
        format!(
            "version: 1\nrules:\n  tasks:\n    match: {{frontmatter: {{type: task}}}}\n    allowed_paths: {{paths: ['tasks/**'], route: 'tasks/'}}\n{extra}"
        )
    }

    /// The task the cases route.
    const TASK: (&str, &str) = ("loose/a.md", "---\ntype: task\n---\n# A\n");

    /// **A route whose cascade may respell a frontmatter wikilink in a field
    /// a rule reads by value skips as respelling a judged link, naming the
    /// holder and the field** — whichever way the field is read: a selector
    /// key, a closed set, a length limit, a forbiddance, the tags, a type
    /// other than text or link, or a finding standing on it.
    #[test]
    fn a_route_respelling_a_frontmatter_link_in_a_field_read_by_value_skips() {
        let cases: [(&str, &str, &str); 7] = [
            (
                "a selector key",
                "  linked:\n    match: {frontmatter: {up: '[[loose/a]]'}}\n    required:\n      up:\n",
                "---\nup: \"[[loose/a]]\"\n---\n",
            ),
            (
                "a closed set",
                "  hubs:\n    match: {frontmatter: {type: hub}}\n    one_of:\n      up: {values: ['[[loose/a]]']}\n",
                "---\ntype: hub\nup: \"[[loose/a]]\"\n---\n",
            ),
            (
                "a length limit",
                "  hubs:\n    match: {frontmatter: {type: hub}}\n    max_length:\n      up: 40\n",
                "---\ntype: hub\nup: \"[[loose/a]]\"\n---\n",
            ),
            (
                "a forbiddance",
                "  hubs:\n    match: {frontmatter: {type: hub}}\n    forbidden:\n      up:\n",
                "---\ntype: hub\nup: \"[[loose/a]]\"\n---\n",
            ),
            ("the tags", "", "---\ntags:\n  - \"[[loose/a]]\"\n---\n"),
            (
                "a declared type",
                "fields:\n  up: {type: number}\n",
                "---\nup: \"[[loose/a]]\"\n---\n",
            ),
            (
                "a standing finding",
                "fields:\n  up: {type: text, shape: list}\n",
                "---\nup: \"[[loose/a]]\"\n---\n",
            ),
        ];
        for (read, extra, holder) in cases {
            let schema = if extra.starts_with("fields") {
                format!(
                    "version: 1\n{extra}rules:\n  tasks:\n    match: {{frontmatter: {{type: task}}}}\n    allowed_paths: {{paths: ['tasks/**'], route: 'tasks/'}}\n"
                )
            } else {
                routing(extra)
            };

            let planned = planned(&schema, &[TASK, ("h.md", holder)], &["loose/a.md"]);

            assert!(planned.operations.is_empty(), "{read}: {planned:?}");
            let [(finding, reason, note)] = skips(&planned)
                .try_into()
                .unwrap_or_else(|skips| panic!("{read}: one skip: {skips:?}"));
            assert_eq!(
                (finding, reason),
                (1, SkipReason::RespellsAJudgedLink),
                "{read}"
            );
            let field = if read == "the tags" { "tags" } else { "up" };
            assert!(
                note.contains("`h.md`") && note.contains(&format!("`{field}`")),
                "{read}: {note}"
            );
        }
    }

    /// The rule closing a hub's `up` over the links it may hold.
    const CLOSED_UP: &str = "  hubs:\n    match: {frontmatter: {type: hub}}\n    one_of:\n      up: {values: ['[[a]]', '[[loose/a]]', '[[tasks/a]]']}\n";

    /// **A bare-stem frontmatter backlink in a field read by value does not
    /// skip the route**: the route keeps the file name, so `[[a]]` names the
    /// moved document wherever it lands and is not respelled.
    #[test]
    fn a_bare_stem_backlink_in_a_field_read_by_value_does_not_skip() {
        let planned = planned(
            &routing(CLOSED_UP),
            &[TASK, ("h.md", "---\ntype: hub\nup: \"[[a]]\"\n---\n")],
            &["loose/a.md"],
        );

        assert_eq!(moved(&planned), ["loose/a.md"]);
        assert!(planned.skipped.is_empty(), "{planned:?}");
    }

    /// **A bare stem the route carries across the ambiguity-ignore set may be
    /// respelled, so it skips**: `archive/` is kept out of every stem's
    /// documents, so `[[a]]` would name nothing where the route lands.
    #[test]
    fn a_bare_stem_carried_across_the_ambiguity_ignore_set_skips() {
        let schema = format!(
            "version: 1\npaths:\n  ambiguity_ignore: ['archive/**']\nrules:\n  tasks:\n    match: {{frontmatter: {{type: task}}}}\n    allowed_paths: {{paths: ['archive/**'], route: 'archive/'}}\n{CLOSED_UP}"
        );

        let planned = planned(
            &schema,
            &[TASK, ("h.md", "---\ntype: hub\nup: \"[[a]]\"\n---\n")],
            &["loose/a.md"],
        );

        assert!(planned.operations.is_empty(), "{planned:?}");
        assert_eq!(
            planned
                .skipped
                .iter()
                .map(|skip| skip.reason)
                .collect::<Vec<_>>(),
            [SkipReason::RespellsAJudgedLink]
        );
    }

    /// **Whether a route carries a stem across the ambiguity-ignore set is
    /// the store's own rule**: a glob naming a folder keeps everything
    /// beneath it out of a stem's documents, a glob naming a folder one
    /// level down keeps out what stands deeper, and a document at the root
    /// is its whole place, so a bare link names it whatever glob matches it.
    /// Each route here carries `[[a]]`'s document out of what the stem
    /// counts, so the hub's closed `up` would no longer name it.
    #[test]
    fn a_bare_stem_crossing_the_ambiguity_ignore_set_is_read_by_the_stores_rule() {
        let cases: [(&str, &str, &str, (&str, &str)); 3] = [
            ("a folder's own glob", "archive", "archive/", TASK),
            ("a one-level glob", "archive/*", "archive/old/", TASK),
            (
                "a root document",
                "**/a.md",
                "tasks/",
                ("a.md", "---\ntype: task\n---\n# A\n"),
            ),
        ];
        for (case, ignored, route, task) in cases {
            let schema = format!(
                "version: 1\npaths:\n  ambiguity_ignore: ['{ignored}']\nrules:\n  tasks:\n    match: {{frontmatter: {{type: task}}}}\n    allowed_paths: {{paths: ['{route}**'], route: '{route}'}}\n{CLOSED_UP}"
            );

            let planned = planned(
                &schema,
                &[task, ("h.md", "---\ntype: hub\nup: \"[[a]]\"\n---\n")],
                &[task.0],
            );

            assert!(planned.operations.is_empty(), "{case}: {planned:?}");
            assert_eq!(
                planned
                    .skipped
                    .iter()
                    .map(|skip| skip.reason)
                    .collect::<Vec<_>>(),
                [SkipReason::RespellsAJudgedLink],
                "{case}"
            );
        }
    }

    /// The rule limiting a hub's `up`, which reads it by value.
    const LIMITED_UP: &str =
        "  hubs:\n    match: {frontmatter: {type: hub}}\n    max_length:\n      up: 40\n";

    /// **A `vault://` wikilink may be respelled however few segments it
    /// spells**: `[[vault://a]]` names the root document `a.md` by its path,
    /// not by its stem, so the route moving it would respell the hub's
    /// limited `up`.
    #[test]
    fn a_vault_link_to_a_root_document_may_be_respelled() {
        let planned = planned(
            &routing(LIMITED_UP),
            &[
                ("a.md", "---\ntype: task\n---\n# A\n"),
                ("h.md", "---\ntype: hub\nup: \"[[vault://a]]\"\n---\n"),
            ],
            &["a.md"],
        );

        assert!(planned.operations.is_empty(), "{planned:?}");
        assert_eq!(
            planned
                .skipped
                .iter()
                .map(|skip| skip.reason)
                .collect::<Vec<_>>(),
            [SkipReason::RespellsAJudgedLink]
        );
    }

    /// **A field declared a type other than text or link is read by value
    /// even where no finding stands on it**: a `tags`-typed field holds
    /// `[[loose/a]]` as a tag name, which the judge accepts, and a
    /// respelling would write another name.
    #[test]
    fn a_field_typed_other_than_text_or_link_is_read_by_value_with_no_finding_on_it() {
        let schema = "version: 1\nfields:\n  labels: {type: tags}\nrules:\n  tasks:\n    match: {frontmatter: {type: task}}\n    allowed_paths: {paths: ['tasks/**'], route: 'tasks/'}\n";
        let holder = "---\nlabels:\n  - \"[[loose/a]]\"\n---\n";

        let planned = planned(schema, &[TASK, ("h.md", holder)], &["loose/a.md"]);

        assert!(planned.operations.is_empty(), "{planned:?}");
        let [(_, reason, note)] = skips(&planned)
            .try_into()
            .unwrap_or_else(|skips| panic!("one skip: {skips:?}"));
        assert_eq!(reason, SkipReason::RespellsAJudgedLink);
        assert!(note.contains("`labels`"), "{note}");
    }

    /// **A frontmatter link the text layer cannot place is never respelled,
    /// so it never holds a route back**, wherever it stands: a flow list's
    /// item has no bytes a rewrite can write over, so the cascade leaves it
    /// as written and the forecast advises on it, as a Layer 4 move's does.
    #[test]
    fn a_frontmatter_link_the_text_layer_cannot_place_never_skips_the_route() {
        let planned = planned(
            &routing(CLOSED_UP),
            &[
                TASK,
                ("h.md", "---\ntype: hub\nup: [\"[[loose/a]]\"]\n---\n"),
            ],
            &["loose/a.md"],
        );

        assert_eq!(moved(&planned), ["loose/a.md"]);
        assert!(planned.skipped.is_empty(), "{planned:?}");
    }

    /// **Two routes landing at one place are each judged on their own
    /// document's states**: `x/a.md` and `y/a.md` both route to `tasks/a.md`,
    /// and `x/a.md`'s own path-qualified link in its limited `up` would be
    /// respelled past the limit by its move, so its route skips, and
    /// `y/a.md`'s, no longer taken, moves.
    #[test]
    fn two_routes_landing_at_one_place_are_each_judged_on_their_own_documents_states() {
        let schema = routing(
            "  ups:\n    match: {frontmatter: {type: task}}\n    max_length:\n      up: 9\n",
        );

        let planned = planned(
            &schema,
            &[
                ("x/a.md", "---\ntype: task\nup: \"[[x/a]]\"\n---\n"),
                ("y/a.md", "---\ntype: task\n---\n"),
            ],
            &["x/a.md", "y/a.md"],
        );

        assert_eq!(moved(&planned), ["y/a.md"]);
        let [(finding, reason, note)] = skips(&planned)
            .try_into()
            .unwrap_or_else(|skips| panic!("one skip: {skips:?}"));
        assert_eq!((finding, reason), (1, SkipReason::RespellsAJudgedLink));
        assert!(note.contains("`x/a.md`") && note.contains("`up`"), "{note}");
    }

    /// **A holder whose links a route may respell only in its body is never
    /// judged**: the hub's rules close its `up`, but its link to the routed
    /// document is in its body, so the plan pays no rule work for the hub —
    /// exactly what it pays with no hub at all.
    #[test]
    fn a_body_only_holder_is_never_judged() {
        let schema = routing(CLOSED_UP);
        let alone = tallied(&schema, &[TASK], &["loose/a.md"]);

        let held = tallied(
            &schema,
            &[
                TASK,
                (
                    "h.md",
                    "---\ntype: hub\nup: \"[[a]]\"\n---\nSee [[loose/a]].\n",
                ),
            ],
            &["loose/a.md"],
        );

        assert_eq!(moved(&held.0), ["loose/a.md"]);
        assert_eq!(held.1, alone.1);
    }

    /// **A batch holder whose links to a routed document stand in its body
    /// alone is never judged either**: `h.md` is in the batch, its own route
    /// read and composed though skipped where it lands, which lacks the
    /// `owner` a rule there requires, so the store hands back its body link
    /// to `loose/a.md` as a batch document's; the plan pays the rule work it
    /// pays where `h.md` links nothing.
    #[test]
    fn a_batch_holder_whose_backlinks_stand_in_its_body_alone_is_never_judged() {
        let schema = routing(
            "  hubs:\n    match: {frontmatter: {type: hub}}\n    allowed_paths: {paths: ['shelf/**'], route: 'shelf/'}\n  shelved:\n    match: {path: 'shelf/**'}\n    required:\n      owner:\n",
        );
        let routed = ["h.md", "loose/a.md"];
        let tally = |hub: &str| {
            let (planned, paid) = tallied(&schema, &[TASK, ("h.md", hub)], &routed);
            assert_eq!(moved(&planned), ["loose/a.md"]);
            assert_eq!(
                skips(&planned)
                    .into_iter()
                    .map(|(finding, reason, _)| (finding, reason))
                    .collect::<Vec<_>>(),
                [(1, SkipReason::BringsInRequiredFields)]
            );
            paid
        };

        let unlinked = tally("---\ntype: hub\n---\n# Hub\n");
        let linked = tally("---\ntype: hub\n---\n# Hub\nSee [[loose/a]].\n");

        assert_eq!(linked, unlinked);
    }

    /// **A body-only backlink never skips the route**, however the holder's
    /// rules read its fields, and **a frontmatter backlink in a field no
    /// rule reads by value does not either**.
    #[test]
    fn a_body_backlink_or_one_in_a_field_no_rule_reads_by_value_never_skips() {
        let planned = planned(
            &routing(CLOSED_UP),
            &[
                TASK,
                (
                    "h.md",
                    "---\ntype: hub\nup: \"[[a]]\"\nsee: \"[[loose/a]]\"\n---\nUp: [[loose/a]] and [a](loose/a.md)\n",
                ),
            ],
            &["loose/a.md"],
        );

        assert_eq!(moved(&planned), ["loose/a.md"]);
        assert!(planned.skipped.is_empty(), "{planned:?}");
    }

    /// **A holder its own route moves is judged at every state it may end
    /// in**: `h.md` holds `up: [[loose/a]]`, which no rule reads where it
    /// stands, but its route takes it into `shelf/`, where a rule closes
    /// `up`, so the route moving `loose/a.md` skips whether or not `h.md`'s
    /// own route is made; `h.md`'s does not, since nothing respells a link
    /// to it.
    #[test]
    fn a_holder_its_own_route_moves_is_judged_by_every_state_it_may_end_in() {
        let schema = routing(
            "  hubs:\n    match: {frontmatter: {type: hub}}\n    allowed_paths: {paths: ['shelf/**'], route: 'shelf/'}\n  shelved:\n    match: {path: 'shelf/**'}\n    one_of:\n      up: {values: ['[[loose/a]]', '[[tasks/a]]']}\n",
        );

        let planned = planned(
            &schema,
            &[
                TASK,
                ("h/h.md", "---\ntype: hub\nup: \"[[loose/a]]\"\n---\n"),
            ],
            &["loose/a.md", "h/h.md"],
        );

        assert_eq!(moved(&planned), ["h/h.md"]);
        let [(finding, reason, note)] = skips(&planned)
            .try_into()
            .unwrap_or_else(|skips| panic!("one skip: {skips:?}"));
        assert_eq!((finding, reason), (2, SkipReason::RespellsAJudgedLink));
        assert!(note.contains("`h/h.md`") && note.contains("`up`"), "{note}");
    }

    /// **A routed document's own frontmatter link to another routed document
    /// is a holder's link too**: `b.md`'s closed `up` names `a.md`, and both
    /// are routed, so `a.md`'s route skips and `b.md`'s moves.
    #[test]
    fn a_routed_documents_own_frontmatter_link_to_another_routed_document_is_judged() {
        let schema = routing(
            "  ups:\n    match: {frontmatter: {type: task}}\n    one_of:\n      up: {values: ['[[loose/a]]', '[[tasks/a]]']}\n",
        );

        let planned = planned(
            &schema,
            &[
                ("loose/a.md", "---\ntype: task\nup: \"[[loose/a]]\"\n---\n"),
                ("loose/b.md", "---\ntype: task\nup: \"[[loose/a]]\"\n---\n"),
            ],
            &["loose/a.md", "loose/b.md"],
        );

        assert_eq!(moved(&planned), ["loose/b.md"]);
        assert_eq!(
            planned
                .skipped
                .iter()
                .map(|skip| (skip.finding, skip.reason))
                .collect::<Vec<_>>(),
            [(1, SkipReason::RespellsAJudgedLink)]
        );
    }

    /// **A body link's respelling never changes the body's tags**: the text
    /// layer proves every rewrite by reading it back — the same links,
    /// headings, tags and code, only the rewritten targets changed — and
    /// leaves a link it cannot so respell as written
    /// (`RewriteSkip::Unrepresentable`). The respell check rests on it: a
    /// cascade judged neutral for a document's body is neutral for the
    /// schema, which reads a body for its tags alone. Each case respells a
    /// link beside a tag to targets that would hide one, reveal one or write
    /// one, and the tags read back as they were.
    #[test]
    fn a_body_links_respelling_never_changes_the_bodys_tags() {
        use norn_text::{AddressRewrite, Document, LinkFamily};

        let bodies = [
            "see [[a]] and #tag here -->\n",
            "x <!-- [[a]] #hidden -->\n",
            "[[a]] #tag `code` \">\n",
            "see [x](a) #tag\n",
            "#first [[a]]#second\n",
        ];
        let targets = [
            "<!--", "-->", "<x y=\"", "b #new", "#new", "b`", "tasks/a", "b\\",
        ];
        let tags = |text: &str| -> Vec<String> {
            Document::parse(text)
                .tags()
                .into_iter()
                .map(|tag| tag.name)
                .collect()
        };
        for body in bodies {
            for family in [LinkFamily::Wikilink, LinkFamily::Markdown] {
                for to in targets {
                    let rewritten = Document::parse(body)
                        .rewrite_links(&[AddressRewrite::new(family, "a", to)]);
                    assert_eq!(
                        tags(&rewritten.text),
                        tags(body),
                        "{body:?} respelled to {to:?} as {:?}",
                        rewritten.text
                    );
                }
            }
        }
    }
}
