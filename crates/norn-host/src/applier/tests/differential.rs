//! A differential over generated vaults: each trial plans a move — of one
//! document, keeping its stem or not, of two where one lands on the path the
//! other vacates, or of a folder — applies it, and judges every link the vault
//! held against the store built from zero over what landed.
//!
//! **What every link must do.** Read where its holder lands, a link resolves
//! after the plan to what it resolved to before, the plan's moves followed —
//! unless the forecast says why the cascade left it as written, or it named a
//! document no move carries and the plan records exactly how its resolution
//! changed. A link the cascade respelled changed its target alone, in its own
//! style. The seeds are fixed, so a failure names a trial that fails again.

use std::collections::{BTreeMap, BTreeSet};

use norn_store::{LinkFact, PathOverlay, ProbedLink};
use norn_wire::{
    AuthoredPlan, FolderPath, LinkAdvisory, Operation, OperationKind, PlanCondition, Resolves,
};

use super::{Fixture, moving, path};
use crate::apply::PlanSnapshot;
use crate::derivation::document_links;
use crate::planner::control::SchemaPlace;
use crate::planner::expand::resolve_expanding;
use crate::planner::links::{EntryKey, LinkIndex, address, entry_key, family_name, wire_family};
use crate::planner::view::TreeView;

/// How many trials run: enough that each kind of move meets bare,
/// path-qualified, `vault://`, relative and rooted links, escaped and not.
const TRIALS: u64 = 60;

/// The kinds of plan a trial makes, one after another.
const KINDS: u64 = 5;

const FOLDERS: &[&str] = &["", "a", "b", "a/b", "c/d/e", "sp ace", "Ü"];
const STEMS: &[&str] = &["n", "m", "note one", "café", "v1.2", "N2"];
const DESTINATION_FOLDERS: &[&str] = &["", "a", "z", "z/y", "b", "new dir", "Ü"];
const DESTINATION_STEMS: &[&str] = &["n", "fresh", "new name", "café", "Zed"];

/// A deterministic stream of choices: SplitMix64.
struct Choices(u64);

impl Choices {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn below(&mut self, bound: usize) -> usize {
        usize::try_from(self.next() % bound as u64).expect("a bound fits")
    }

    fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[self.below(items.len())]
    }

    fn chance(&mut self, percent: u64) -> bool {
        self.next() % 100 < percent
    }
}

fn joined(folder: &str, leaf: &str) -> String {
    if folder.is_empty() {
        leaf.to_string()
    } else {
        format!("{folder}/{leaf}")
    }
}

/// `segment` percent-encoded as a Markdown destination escapes it.
fn escaped(segment: &str) -> String {
    let mut out = String::new();
    for character in segment.chars() {
        if character.is_ascii_alphanumeric() || matches!(character, '-' | '.' | '_' | '~') {
            out.push(character);
        } else {
            let mut buffer = [0; 4];
            for byte in character.encode_utf8(&mut buffer).bytes() {
                out.push_str(&format!("%{byte:02X}"));
            }
        }
    }
    out
}

/// A wikilink naming the document at `to`: by its stem, by two segments, by
/// its path, or from the vault root, with or without the extension.
fn wikilink(choices: &mut Choices, to: &str) -> String {
    let stem_path = to.strip_suffix(".md").expect("a document path");
    let segments: Vec<&str> = stem_path.split('/').collect();
    let mut target = match choices.below(5) {
        0 | 1 => segments[segments.len() - 1].to_string(),
        2 => segments[segments.len() - 2.min(segments.len())..].join("/"),
        3 => stem_path.to_string(),
        _ => format!("vault://{stem_path}"),
    };
    if choices.chance(25) {
        target.push_str(".md");
    }
    let tail = *choices.pick(&["", "#Head", "|alias"]);
    let embed = if choices.chance(15) { "!" } else { "" };
    format!("{embed}[[{target}{tail}]]")
}

/// A Markdown link from `holder` to the file at `to`: relative or rooted,
/// escaped or not.
fn markdown(choices: &mut Choices, holder: &str, to: &str) -> String {
    let from: Vec<&str> = holder.split('/').collect();
    let from = &from[..from.len() - 1];
    let to_segments: Vec<&str> = to.split('/').collect();
    let rooted = choices.chance(20);
    let segments: Vec<String> = if rooted {
        to_segments
            .iter()
            .map(|segment| segment.to_string())
            .collect()
    } else {
        let shared = from
            .iter()
            .zip(&to_segments[..to_segments.len() - 1])
            .take_while(|(left, right)| left == right)
            .count();
        std::iter::repeat_n("..".to_string(), from.len() - shared)
            .chain(
                to_segments[shared..]
                    .iter()
                    .map(|segment| segment.to_string()),
            )
            .collect()
    };
    let escape = choices.chance(50);
    let target = segments
        .iter()
        .map(|segment| {
            if escape && segment != ".." {
                escaped(segment)
            } else {
                segment.clone()
            }
        })
        .collect::<Vec<_>>()
        .join("/");
    let target = if rooted { format!("/{target}") } else { target };
    let tail = *choices.pick(&["", "#Head"]);
    if target.contains(' ') {
        format!("[t](<{target}{tail}>)")
    } else {
        format!("[t]({target}{tail})")
    }
}

/// A vault of a few documents linking each other, an attachment beside them.
fn generated(choices: &mut Choices) -> Vec<(String, String)> {
    let mut paths = BTreeSet::new();
    let count = 4 + choices.below(6);
    while paths.len() < count {
        paths.insert(joined(
            choices.pick(FOLDERS),
            &format!("{}.md", choices.pick(STEMS)),
        ));
    }
    let paths: Vec<String> = paths.into_iter().collect();
    let attachment = joined(choices.pick(FOLDERS), "img.png");
    let mut files = vec![(attachment.clone(), "png".to_string())];
    for holder in &paths {
        let mut text = String::new();
        if choices.chance(25) {
            let named = choices.pick(&paths).clone();
            text.push_str(&format!(
                "---\nrel: \"{}\"\n---\n",
                wikilink(choices, &named)
            ));
        }
        text.push_str("# Head\n\n");
        for _ in 0..1 + choices.below(5) {
            let link = match choices.below(8) {
                0..=2 => {
                    let named = choices.pick(&paths).clone();
                    wikilink(choices, &named)
                }
                3..=5 => {
                    let named = choices.pick(&paths).clone();
                    markdown(choices, holder, &named)
                }
                6 => markdown(choices, holder, &attachment),
                _ => format!("[[#Head]] {}", wikilink(choices, holder)),
            };
            text.push_str(&link);
            text.push_str(if choices.chance(30) { "\n\n" } else { " " });
        }
        text.push('\n');
        files.push((holder.clone(), text));
    }
    files
}

/// The operations one trial plans, by its kind: a document move, a move
/// keeping the document's stem, a move onto the path another move vacates,
/// a folder move, or a move taking the stem of a document another move
/// carries off, the two listed in either order. `None` where the vault
/// offers the kind nothing to do.
fn chosen(choices: &mut Choices, kind: u64, files: &[(String, String)]) -> Option<Vec<Operation>> {
    let documents: Vec<&String> = files
        .iter()
        .map(|(at, _)| at)
        .filter(|at| at.ends_with(".md"))
        .collect();
    let occupied = |at: &str| files.iter().any(|(held, _)| held == at);
    let fresh = |choices: &mut Choices| {
        joined(
            choices.pick(DESTINATION_FOLDERS),
            &format!("{}.md", choices.pick(DESTINATION_STEMS)),
        )
    };
    let from = (*choices.pick(&documents)).clone();
    match kind {
        0 => {
            let to = fresh(choices);
            (!occupied(&to)).then(|| vec![moving(&from, &to)])
        }
        1 => {
            let leaf = from.rsplit('/').next().expect("a leaf");
            let to = joined(choices.pick(DESTINATION_FOLDERS), leaf);
            (!occupied(&to)).then(|| vec![moving(&from, &to)])
        }
        2 => {
            let other = (*choices.pick(&documents)).clone();
            let to = fresh(choices);
            (other != from && !occupied(&to))
                .then(|| vec![moving(&other, &to), moving(&from, &other)])
        }
        4 => {
            let other = (*choices.pick(&documents)).clone();
            let leaf = other.rsplit('/').next().expect("a leaf");
            let taking = joined(choices.pick(DESTINATION_FOLDERS), leaf);
            let to = fresh(choices);
            if other == from || occupied(&taking) || occupied(&to) || taking == to {
                return None;
            }
            let mut pair = vec![moving(&from, &taking), moving(&other, &to)];
            if choices.chance(50) {
                pair.reverse();
            }
            Some(pair)
        }
        _ => {
            let folder = from.rsplit_once('/')?.0.to_string();
            let to = choices.pick(&["q", "q/r", "z/y", "new dir"]).to_string();
            let vacant = files.iter().all(|(at, _)| {
                at.strip_prefix(&format!("{folder}/"))
                    .is_none_or(|rest| !occupied(&format!("{to}/{rest}")))
            });
            vacant.then(|| {
                vec![Operation::new(OperationKind::move_folder(
                    FolderPath::new(&folder).expect("a folder path"),
                    FolderPath::new(&to).expect("a folder path"),
                ))]
            })
        }
    }
}

/// What `link`, held at `holder`, resolves to in the store as it stands.
fn resolution(index: &PlanSnapshot<'_>, holder: &str, link: &LinkFact) -> Resolves {
    let holder = norn_store::DocumentPath::new(holder).expect("a stored path");
    let probe = ProbedLink {
        before_holder: holder.clone(),
        after_holder: holder,
        link: link.clone(),
        written: true,
    };
    let mut resolved = None;
    index
        .changes(&PathOverlay::new(), &[probe], &mut |change| {
            resolved = Some(change.after);
        })
        .expect("the store answers");
    resolved.expect("a written probe is answered")
}

/// `link`'s key, held at `holder`, as the plan records it.
fn key(holder: &str, link: &LinkFact) -> EntryKey {
    (
        holder.to_string(),
        family_name(wire_family(link.family)),
        address(link),
    )
}

/// Run one trial, adding to `failures` each way a link did not do what it
/// must; whether its plan was planned whole and applied.
fn trial(seed: u64, failures: &mut Vec<String>) -> bool {
    let mut choices = Choices(seed);
    let mut files = generated(&mut choices);
    let Some(operations) = chosen(&mut choices, seed % KINDS, &files) else {
        return false;
    };
    // A holder naming every moved document by its bare stem, so two moves
    // whose rewrites meet in one holder — one's new spelling the other's old
    // one — do in every trial.
    let stems: Vec<String> = operations
        .iter()
        .filter_map(|operation| match &operation.kind {
            OperationKind::MoveDocument { from, .. } => Some(format!(
                "[[{}]]",
                norn_store::DocumentPath::new(from.as_str())
                    .expect("a stored path")
                    .stem()
            )),
            _ => None,
        })
        .collect();
    if !stems.is_empty() && !files.iter().any(|(at, _)| at == "stems.md") {
        files.push(("stems.md".to_string(), format!("{}\n", stems.join(" "))));
    }
    let borrowed: Vec<(&str, &str)> = files
        .iter()
        .map(|(at, text)| (at.as_str(), text.as_str()))
        .collect();
    let mut fixture = Fixture::new(&borrowed);
    let tag = format!("seed {seed}, {operations:?}");

    let mut before: BTreeMap<String, Vec<(LinkFact, Resolves)>> = BTreeMap::new();
    let resolved = {
        let links = fixture.links();
        let index = links.index();
        for (holder, text) in files.iter().filter(|(at, _)| at.ends_with(".md")) {
            let held = document_links(text.as_bytes())
                .into_iter()
                .map(|link| {
                    let resolves = resolution(&index, holder, &link);
                    (link, resolves)
                })
                .collect();
            before.insert(holder.clone(), held);
        }
        let view = TreeView::open(&fixture.vault, &fixture.exclusions, &SchemaPlace::default())
            .expect("a vault");
        resolve_expanding(
            AuthoredPlan::new(crate::planner::links::testing::vault(), operations),
            fixture.root_identity(),
            &view,
            &index,
            &index,
            &crate::planner::rule::testing::no_rules(),
        )
        .unwrap_or_else(|failure| panic!("{tag}: planning failed: {failure:?}"))
    };
    if !resolved.unresolved.is_empty() {
        return false;
    }

    let mut moved: BTreeMap<String, String> = BTreeMap::new();
    for operation in &resolved.plan.operations {
        if let OperationKind::MoveDocument { from, to } = &operation.kind {
            moved.insert(from.as_str().to_string(), to.as_str().to_string());
        }
    }
    let landed = |at: &str| moved.get(at).cloned().unwrap_or_else(|| at.to_string());
    let skipped: BTreeSet<EntryKey> = resolved
        .forecast
        .links
        .iter()
        .filter_map(|advisory| match advisory {
            LinkAdvisory::SkippedAmbiguous { link, .. }
            | LinkAdvisory::SkippedUnrepresentable { link, .. }
            | LinkAdvisory::SkippedWouldCorruptFrontmatter { link, .. }
            | LinkAdvisory::SkippedNotRewritable { link, .. } => Some(entry_key(link)),
            _ => None,
        })
        .collect();
    let recorded: BTreeMap<EntryKey, (Resolves, Resolves)> = resolved
        .plan
        .conditions
        .iter()
        .filter_map(|condition| match condition {
            PlanCondition::LinkResolution {
                link,
                before,
                after,
                ..
            } => Some((entry_key(link), (before.clone(), after.clone()))),
            _ => None,
        })
        .collect();

    let outcome = fixture.apply(resolved.plan.clone());
    let crate::applier::ApplyOutcome::Applied(_) = outcome else {
        failures.push(format!("{tag}: the plan did not apply: {outcome:?}"));
        return false;
    };
    fixture.assert_store_is_a_build_from_zero();

    let links = fixture.links();
    let index = links.index();
    for (holder, held) in &before {
        let holder = landed(holder);
        let text = fixture.read(&holder).unwrap_or_default();
        let after = document_links(text.as_bytes());
        if after.len() != held.len() {
            failures.push(format!("{tag}: `{holder}` holds another number of links"));
            continue;
        }
        for ((was, resolved_before), is) in held.iter().zip(&after) {
            let resolves = resolution(&index, &holder, is);
            let followed = match resolved_before {
                Resolves::One { path: named } => Resolves::one(path(&landed(named.as_str()))),
                other => other.clone(),
            };
            let carried = matches!(resolved_before, Resolves::One { path: named }
                if moved.contains_key(named.as_str()));
            let at = key(&holder, is);
            let said = skipped.contains(&at);
            let recorded_as_is =
                recorded.get(&at) == Some(&(resolved_before.clone(), resolves.clone()));
            if resolves != followed && !said && !(recorded_as_is && !carried) {
                failures.push(format!(
                    "{tag}: in `{holder}`, `{}` became `{}`, naming {resolves:?} where it named \
                     {resolved_before:?}",
                    address(was),
                    address(is)
                ));
            }
            if was.target == is.target {
                continue;
            }
            let same_form = was.embed == is.embed
                && was.title == is.title
                && was.anchor == is.anchor
                && was.protocol == is.protocol
                && was.target.to_ascii_lowercase().ends_with(".md")
                    == is.target.to_ascii_lowercase().ends_with(".md")
                && was.target.starts_with('/') == is.target.starts_with('/');
            if !same_form {
                failures.push(format!(
                    "{tag}: in `{holder}`, `{}` was respelled `{}` out of its own style",
                    address(was),
                    address(is)
                ));
            }
        }
    }
    true
}

/// **Every planned move leaves each link naming what it named, or says
/// why.** Over generated vaults, a move of one document, a move keeping its
/// stem, a move onto the path another move vacates, a folder move, and a
/// move taking the stem another move's document leaves each land, and every link resolves after them to the document it resolved to
/// before, where that document landed — respelled in its own style where it
/// had to be — or is advised on as left by the cascade, or names a document
/// no move carried and is recorded with exactly the change it saw.
#[test]
fn every_planned_move_leaves_each_link_naming_what_it_named_or_says_why() {
    let mut failures = Vec::new();
    let mut applied = [0; KINDS as usize];
    for seed in 0..TRIALS {
        if trial(seed, &mut failures) {
            applied[usize::try_from(seed % KINDS).expect("a kind")] += 1;
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    assert!(
        applied.iter().all(|&count| count >= 4),
        "each kind of move lands in several trials: {applied:?}"
    );
}
