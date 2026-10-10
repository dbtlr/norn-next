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
//! set, which changes what the stem counts, so such a stem's bare links may be
//! respelled too. A `vault://` or path-qualified wikilink may be respelled.
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
//! once per candidate state, and each holder with a link that may be
//! respelled read and judged once — a holder outside the batch at its
//! before-state alone, through the view the plan resolves on, which reads it
//! again from memory where its cascade composes it.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use norn_config::schema::{FieldType, RuleWork, VaultSchema};
use norn_fs::NormalizedPath;
use norn_store::{LinkFamily, PathOverlay, ProbedLink};
use norn_wire::{AuthoredValue, DocumentPath, FindingKind, Resolves, SkipReason, SkippedFinding};

use super::{Draft, State};
use crate::applier::standing;
use crate::derivation::{frontmatter_links, written_fields};
use crate::evidence::count_rule_work;
use crate::planner::repair::{Before, Reading, Repairing};

/// A wikilink's address as its frontmatter writes it: its target and its
/// protocol. Two links of one holder written alike resolve alike, so the
/// address names every link a respelling of one would respell.
type Address = (String, Option<String>);

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
    // by every place it may stand at.
    let place = |path: &DocumentPath| vault.place(path);
    let origins: BTreeMap<NormalizedPath, usize> = routes
        .iter()
        .filter_map(|(at, passed)| Some((place(&passed.from)?, *at)))
        .collect();
    let mut documents: BTreeMap<NormalizedPath, usize> = BTreeMap::new();
    for (at, draft) in drafts.iter().enumerate() {
        let Some(compositions) = draft.compositions() else {
            continue;
        };
        if let Some(origin) = place(&compositions.reckoned.start.at) {
            documents.insert(origin, at);
        }
        if let Some((passed, _)) = draft.passed()
            && let Some(lands) = place(&passed.to)
        {
            documents.insert(lands, at);
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
    let mut named: BTreeMap<DocumentPath, BTreeMap<Address, BTreeSet<usize>>> = BTreeMap::new();
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
        // A batch document is named where it stands, wherever the probe
        // held it.
        let holder = DocumentPath::new(change.holder.as_str())
            .expect("a stored document path is a wire document path");
        let holder = match place(&holder).and_then(|held| documents.get(&held)) {
            Some(&at) => drafts[at]
                .compositions()
                .expect("a batch document placed was read")
                .reckoned
                .start
                .at
                .clone(),
            None => holder,
        };
        named
            .entry(holder)
            .or_default()
            .entry((change.link.target.clone(), change.link.protocol.clone()))
            .or_default()
            .insert(*route);
    })?;

    let selector_keys: BTreeSet<&str> = schema
        .rules()
        .flat_map(|rule| rule.selector().frontmatter().map(|(key, _)| key))
        .collect();
    let mut refused = BTreeMap::new();
    for (holder, addresses) in &named {
        let states = match place(holder).and_then(|held| documents.get(&held)) {
            Some(&at) => candidate_states(&drafts[at]),
            None => match vault.before(holder)? {
                Before::Held(bytes) => vec![State::of(
                    holder.clone(),
                    Arc::clone(&bytes),
                    standing(holder, &bytes, repairing.declared, repairing.case),
                )],
                // A holder that does not read holds no field the cascade
                // could respell; its cascade cannot compose either.
                Before::Unread => Vec::new(),
            },
        };
        let judged = ValueRead::of(&states, schema, &selector_keys, repairing);
        for state in &states {
            for (field, address) in field_links(&state.bytes) {
                let Some(routes) = addresses.get(&address) else {
                    continue;
                };
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
                                 of `{holder}`, a field the rules read by value",
                                passed.to
                            ),
                        )
                    });
                }
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
/// stem of each routed document whose origin and destination differ in
/// whether the schema's ambiguity-ignore set keeps them out of a stem's
/// documents, folded as [`stem_of`] folds one.
fn crossing_stems(
    routes: &[(usize, &super::routes::Passed)],
    schema: &VaultSchema,
    repairing: &Repairing<'_>,
) -> BTreeSet<String> {
    let ignored = |path: &DocumentPath| {
        schema
            .ambiguity_ignore()
            .iter()
            .any(|glob| glob.matches(path.as_str(), repairing.case))
    };
    routes
        .iter()
        .filter(|(_, passed)| ignored(&passed.from) != ignored(&passed.to))
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
            for link in frontmatter_links(&state.bytes) {
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

/// Each frontmatter field of the document `bytes` spell, with the address of
/// each wikilink its value holds at any depth.
fn field_links(bytes: &[u8]) -> Vec<(String, Address)> {
    let Some(fields) = written_fields(bytes) else {
        return Vec::new();
    };
    let mut links = Vec::new();
    for (field, value) in fields.entries() {
        each_string(value, &mut |text| {
            for link in norn_text::parse_wikilinks_in_text(text) {
                links.push((field.clone(), (link.target, link.protocol)));
            }
        });
    }
    links
}

/// Hand `each` every string `value` holds, at any depth: what the
/// derivation reads a frontmatter value's wikilinks from.
fn each_string(value: &AuthoredValue, each: &mut dyn FnMut(&str)) {
    match value {
        AuthoredValue::String(text) => each(text),
        AuthoredValue::List(items) => {
            for item in items {
                each_string(item, each);
            }
        }
        AuthoredValue::Map(map) => {
            for (_, item) in map.entries() {
                each_string(item, each);
            }
        }
        AuthoredValue::Null
        | AuthoredValue::Bool(_)
        | AuthoredValue::Integer(_)
        | AuthoredValue::Float(_) => {}
    }
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
        match plan(&misplaced(routed), &repairing, &reading) {
            Ok(planned) => planned,
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
            ("the tags", "", "---\ntags: [\"[[loose/a]]\"]\n---\n"),
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
