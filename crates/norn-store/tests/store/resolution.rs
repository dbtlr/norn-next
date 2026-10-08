//! The resolution change set's store half: what each link a plan reaches
//! resolves to before the plan and after it, read on one snapshot with the
//! plan's targets overlaid on the documents the store holds.
//!
//! Every case runs over a root that tells spellings apart and over one that
//! folds ASCII case, and the documents are derived from Markdown through the
//! text layer, as the host derives them.

use norn_store::{
    ContentModel, LinkChange, LinkFact, PathOverlay, PlanSide, ProbedLink, Provenance,
    ResolutionStatement, ResolutionWork, Snapshot, Store, StoredPathOrder, TargetNaming,
};
use std::collections::{BTreeMap, BTreeSet};

use norn_wire::{
    CandidateHead, Column, FindParams, LinkHealth, Pattern, Predicate, ResolutionTarget, Resolves,
    Unsatisfied, VaultAddress, VaultName,
};

use crate::common::{Scratch, path};
use crate::health::derived;

use StoredPathOrder::{AsciiCaseInsensitive as Folding, Sensitive};

/// The fingerprint the suite's schema is pinned under.
const SCHEMA: &str = "resolution-schema";

/// The declaration every plan here is judged under: `archive/**` kept out of
/// every class a target that does not name it opens.
fn declared() -> ContentModel {
    ContentModel::under(SCHEMA)
        .declare_ambiguity_ignore(Pattern::parse("archive/**").expect("a glob"))
}

/// One judged link as a case reads it: the holder after the plan, the
/// address as written, and what it resolves to before and after.
type Judged = (String, String, String, String);

/// A store over a root proven to have one case behaviour, its schema pinned.
struct Vault {
    _scratch: Scratch,
    store: Store,
    order: StoredPathOrder,
}

impl Vault {
    fn new(label: &str, order: StoredPathOrder) -> Self {
        let scratch = Scratch::new(label);
        let mut store = scratch.open_under(order);
        store
            .begin_request()
            .pin_vault_schema(SCHEMA.as_bytes(), SCHEMA)
            .expect("pinning the suite's schema");
        Vault {
            _scratch: scratch,
            store,
            order,
        }
    }

    /// Write each `(path, body)` in one changeset.
    fn write(&mut self, documents: &[(&str, &str)]) {
        let mut request = self.store.begin_request();
        request
            .apply_increment(
                norn_store::IncrementProvenance::Derived,
                documents
                    .iter()
                    .map(|(at, body)| norn_store::Change::Upsert(derived(at, body))),
                &[],
                &declared(),
            )
            .unwrap_or_else(|refusal| panic!("{:?}: a changeset: {refusal}", self.order));
    }

    /// Kill each document at `paths` in one changeset.
    fn kill(&mut self, paths: &[&str]) {
        let mut request = self.store.begin_request();
        request
            .apply_increment(
                norn_store::IncrementProvenance::Derived,
                paths.iter().map(|at| norn_store::Change::Death {
                    path: path(at),
                    provenance: Provenance::WatcherRemoval,
                }),
                &[],
                &declared(),
            )
            .unwrap_or_else(|refusal| panic!("{:?}: a changeset: {refusal}", self.order));
    }

    fn snapshot(&self) -> Snapshot {
        std::sync::Arc::new(self.store.open_reader().reader.expect("a reader"))
            .try_take()
            .expect("a handle nothing is reading holds its connection")
            .establish()
            .expect("a snapshot")
    }

    /// Every link the plan `overlay` with `probed` reaches, in the order the
    /// store hands them back, and what the judgment cost.
    fn judge(&self, overlay: &PathOverlay, probed: &[ProbedLink]) -> (Vec<Judged>, ResolutionWork) {
        let snapshot = self.snapshot();
        let mut judged = Vec::new();
        let work = snapshot
            .resolution_changes(overlay, probed, &declared(), |change| {
                judged.push(read(&change));
            })
            .unwrap_or_else(|refusal| panic!("{:?}: a judgment: {refusal}", self.order));
        (judged, work)
    }
}

/// `change` as a case reads it.
fn read(change: &LinkChange) -> Judged {
    let protocol = change
        .link
        .protocol
        .as_deref()
        .map(|protocol| format!("{protocol}://"))
        .unwrap_or_default();
    (
        change.holder.as_str().to_string(),
        format!("{protocol}{}", change.link.target),
        resolved(&change.before),
        resolved(&change.after),
    )
}

fn resolved(resolved: &Resolves) -> String {
    match resolved {
        Resolves::None {} => "none".to_string(),
        Resolves::One { path } => format!("one:{}", path.as_str()),
        Resolves::Several {} => "several".to_string(),
    }
}

fn judged(holder: &str, address: &str, before: &str, after: &str) -> Judged {
    (
        holder.to_string(),
        address.to_string(),
        before.to_string(),
        after.to_string(),
    )
}

/// The one link `body` writes, as the text layer reads it.
fn link(body: &str) -> LinkFact {
    let mut links = derived("probe.md", body).links;
    assert_eq!(links.len(), 1, "`{body}` writes one link");
    links.remove(0)
}

/// `body`'s one link, held at `after` after the plan and read from `before`
/// before it.
fn probed(before: &str, after: &str, body: &str, written: bool) -> ProbedLink {
    ProbedLink {
        before_holder: path(before),
        after_holder: path(after),
        link: link(body),
        written,
    }
}

/// A plan creating each of `created` and removing each of `removed`.
fn overlay(created: &[&str], removed: &[&str]) -> PathOverlay {
    let overlay = created.iter().fold(PathOverlay::new(), |overlay, at| {
        overlay.with(path(at), false, true)
    });
    removed
        .iter()
        .fold(overlay, |overlay, at| overlay.with(path(at), true, false))
}

fn both_orders(label: &str, case: impl Fn(Vault)) {
    for order in [Sensitive, Folding] {
        case(Vault::new(&format!("{label}-{order:?}"), order));
    }
}

// ---- base cases ----

/// **A create makes a broken link name one document, and a delete breaks
/// it.** The link is held by a document the plan does not write, so it is
/// reached through the store's link index by the key the target is named
/// under; a link nothing in the plan names is not handed back.
#[test]
fn a_create_and_a_delete_move_a_link_between_none_and_one() {
    both_orders("resolution-create-delete", |mut vault| {
        vault.write(&[("b.md", "[[a]] and [[zzz]]\n")]);
        let (created, _) = vault.judge(&overlay(&["a.md"], &[]), &[]);
        assert_eq!(created, [judged("b.md", "a", "none", "one:a.md")]);

        vault.write(&[("a.md", "alpha\n")]);
        let (removed, _) = vault.judge(&overlay(&[], &["a.md"]), &[]);
        assert_eq!(removed, [judged("b.md", "a", "one:a.md", "none")]);
    });
}

/// **A second document of the same stem makes a link ambiguous**, and the
/// one it named before is named by its path.
#[test]
fn a_second_document_of_a_stem_makes_a_link_ambiguous() {
    both_orders("resolution-second-stem", |mut vault| {
        vault.write(&[("x/a.md", "alpha\n"), ("b.md", "[[a]]\n")]);
        let (judged_links, _) = vault.judge(&overlay(&["y/a.md"], &[]), &[]);
        assert_eq!(judged_links, [judged("b.md", "a", "one:x/a.md", "several")]);
    });
}

/// **A link names the targets its keys could name before the plan**: an
/// ambiguous `[[a]]` names the moved `x/a.md` among the plan's targets, as
/// the stored `y/a.md` is not one, while a link whose keys name no target
/// names none. The ambiguity-ignore set keeps a target it keeps out of a
/// class out of this list too.
#[test]
fn a_link_names_the_targets_it_could_name_before_the_plan() {
    both_orders("resolution-before-targets", |mut vault| {
        vault.write(&[
            ("x/a.md", "alpha\n"),
            ("y/a.md", "alpha\n"),
            ("archive/a.md", "alpha\n"),
            ("b.md", "[[a]]\n"),
        ]);
        let moving = PathOverlay::new()
            .with(path("x/a.md"), true, false)
            .with(path("z/a.md"), false, true)
            .with(path("archive/a.md"), true, false);
        let snapshot = vault.snapshot();
        let mut named = Vec::new();
        snapshot
            .resolution_changes(&moving, &[], &declared(), |change| {
                named.push((
                    change.link.target.clone(),
                    change
                        .before_targets
                        .iter()
                        .map(|target| target.as_str().to_string())
                        .collect::<Vec<_>>(),
                ));
            })
            .expect("a judgment");
        assert_eq!(named, [("a".to_string(), vec!["x/a.md".to_string()])]);
    });
}

/// **A place the ambiguity-ignore set keeps out of a class stays out of it on
/// either side.** Creating `archive/a.md` leaves `[[a]]` naming `x/a.md`
/// alone, while `[[archive/a]]`, which names the ignored place, now resolves
/// to it.
#[test]
fn an_ignored_place_a_plan_adds_joins_only_the_classes_that_name_it() {
    both_orders("resolution-ignored", |mut vault| {
        vault.write(&[("x/a.md", "alpha\n"), ("b.md", "[[a]]\n\n[[archive/a]]\n")]);
        let (judged_links, _) = vault.judge(&overlay(&["archive/a.md"], &[]), &[]);
        assert_eq!(
            judged_links,
            [
                judged("b.md", "a", "one:x/a.md", "one:x/a.md"),
                judged("b.md", "archive/a", "none", "one:archive/a.md"),
            ]
        );
    });
}

/// **A `vault://` wikilink names the root path it spells**, never a deeper
/// document of the same stem.
#[test]
fn a_rooted_wikilink_names_the_root_path_it_spells() {
    both_orders("resolution-rooted", |mut vault| {
        vault.write(&[
            ("deep/notes/a.md", "alpha\n"),
            ("b.md", "[[vault://notes/a]]\n"),
        ]);
        let (judged_links, _) = vault.judge(&overlay(&["notes/a.md"], &[]), &[]);
        assert_eq!(
            judged_links,
            [judged("b.md", "vault://notes/a", "none", "one:notes/a.md")]
        );
    });
}

/// **A relative link in a moved document is read from where it stood before
/// the plan.** `[t](a.md)` in `x/b.md` names `x/a.md`; moved to `y/b.md` it
/// names `y/a.md`, which does not stand, so the move breaks it — which a
/// reading from the new place on both sides would hide.
#[test]
fn a_relative_link_in_a_moved_document_is_read_from_its_source() {
    both_orders("resolution-moved-relative", |mut vault| {
        vault.write(&[("x/a.md", "alpha\n"), ("x/b.md", "[t](a.md)\n")]);
        let plan = overlay(&["y/b.md"], &["x/b.md"]);
        let (judged_links, _) =
            vault.judge(&plan, &[probed("x/b.md", "y/b.md", "[t](a.md)\n", false)]);
        assert_eq!(
            judged_links,
            [judged("y/b.md", "a.md", "one:x/a.md", "none")]
        );
    });
}

/// **An anchor-only link names its holder wherever that stands**, so a moved
/// document's self-link names the source before and the destination after.
#[test]
fn an_anchor_only_link_in_a_moved_document_follows_its_holder() {
    both_orders("resolution-moved-self", |mut vault| {
        vault.write(&[("x/b.md", "# h\n\n[[#h]]\n")]);
        let plan = overlay(&["y/b.md"], &["x/b.md"]);
        let (judged_links, _) =
            vault.judge(&plan, &[probed("x/b.md", "y/b.md", "[[#h]]\n", false)]);
        assert_eq!(
            judged_links,
            [judged("y/b.md", "", "one:x/b.md", "one:y/b.md")]
        );
    });
}

/// **A link whose text the plan writes is handed back whatever it resolves
/// to**, and one the plan neither writes nor moves under is not judged.
#[test]
fn a_written_link_is_handed_back_and_an_untouched_one_is_not() {
    both_orders("resolution-written", |mut vault| {
        vault.write(&[("a.md", "alpha\n"), ("b.md", "[[a]]\n")]);
        let plan = PathOverlay::new().with(path("b.md"), true, true);
        let (written, _) = vault.judge(&plan, &[probed("b.md", "b.md", "[[a]]\n", true)]);
        assert_eq!(written, [judged("b.md", "a", "one:a.md", "one:a.md")]);
        let (untouched, work) = vault.judge(&plan, &[probed("b.md", "b.md", "[[a]]\n", false)]);
        assert_eq!(untouched, []);
        assert_eq!(work, ResolutionWork::default());
    });
}

/// **A document replaced at a path its plan keeps filled is a change**: a
/// plan that takes `a.md`'s document away and puts another there changes
/// what `[[a]]` names though the path it resolves to is the same, so the
/// link is handed back, its two sides alike, and what it names is said to
/// have moved under it. Overlaid as standing on both sides and no more, the
/// same path reaches nothing.
#[test]
fn a_document_replaced_at_its_path_reaches_the_links_naming_it() {
    both_orders("resolution-replaced", |mut vault| {
        vault.write(&[("a.md", "alpha\n"), ("h.md", "[[a]] [t](a.md)\n")]);
        let kept = PathOverlay::new().with(path("a.md"), true, true);
        let (untouched, _) = vault.judge(&kept, &[]);
        assert_eq!(untouched, []);

        let replaced = PathOverlay::new().replacing(path("a.md"));
        let snapshot = vault.snapshot();
        let mut reached = Vec::new();
        snapshot
            .resolution_changes(&replaced, &[], &declared(), |change| {
                reached.push((read(&change), change.members_moved));
            })
            .unwrap_or_else(|refusal| panic!("a judgment: {refusal}"));
        reached.sort_by(|left, right| left.0.cmp(&right.0));
        assert_eq!(
            reached,
            [
                (judged("h.md", "a", "one:a.md", "one:a.md"), true),
                (judged("h.md", "a.md", "one:a.md", "one:a.md"), true),
            ]
        );
    });
}

// ---- links a plan reaches without changing what they name ----

/// **A document a plan reaches hands back every link naming it, however
/// spelled**, though nothing changes there: a bare, a path-qualified, an
/// extended and a `vault://` wikilink, and a Markdown link, each naming
/// `notes/a.md` alike on both sides. A link naming another document is not
/// reached.
#[test]
fn a_document_a_plan_reaches_hands_back_every_link_naming_it() {
    both_orders("resolution-reached-document", |mut vault| {
        vault.write(&[
            ("notes/a.md", "alpha\n"),
            ("b.md", "beta\n"),
            (
                "h.md",
                "[[a]] [[notes/a]] [[a.md]] [[vault://notes/a]] [t](notes/a.md) [[b]]\n",
            ),
        ]);
        let reaching = PathOverlay::new().reaching(path("notes/a.md"));
        let (mut reached, _) = vault.judge(&reaching, &[]);
        reached.sort();
        let named = "one:notes/a.md";
        assert_eq!(
            reached,
            [
                judged("h.md", "a", named, named),
                judged("h.md", "a.md", named, named),
                judged("h.md", "notes/a", named, named),
                judged("h.md", "notes/a.md", named, named),
                judged("h.md", "vault://notes/a", named, named),
            ]
        );
    });
}

/// **An ambiguous link a reached document could answer names it among the
/// documents it could name before the plan**, as it names a target the plan
/// moves, so a caller can tell it could name the reached document; nothing
/// moved under it.
#[test]
fn an_ambiguous_link_names_the_reached_document_it_could_name() {
    both_orders("resolution-reached-ambiguous", |mut vault| {
        vault.write(&[
            ("x/a.md", "alpha\n"),
            ("y/a.md", "alpha\n"),
            ("h.md", "[[a]]\n"),
        ]);
        let reaching = PathOverlay::new().reaching(path("x/a.md"));
        let snapshot = vault.snapshot();
        let mut named = Vec::new();
        snapshot
            .resolution_changes(&reaching, &[], &declared(), |change| {
                named.push((
                    read(&change),
                    change.members_moved,
                    change
                        .before_targets
                        .iter()
                        .map(|target| target.as_str().to_string())
                        .collect::<Vec<_>>(),
                ));
            })
            .expect("a judgment");
        assert_eq!(
            named,
            [(
                judged("h.md", "a", "several", "several"),
                false,
                vec!["x/a.md".to_string()]
            )]
        );
    });
}

/// Every link `overlay` reaches, each with the places among those the plan
/// reaches that it could name before the plan
/// ([`LinkChange::before_targets`]), sorted.
fn could_name(vault: &Vault, overlay: &PathOverlay) -> Vec<(String, Vec<String>)> {
    let snapshot = vault.snapshot();
    let mut named = Vec::new();
    snapshot
        .resolution_changes(overlay, &[], &declared(), |change| {
            named.push((
                address_of(&change),
                change
                    .before_targets
                    .iter()
                    .map(|target| target.as_str().to_string())
                    .collect(),
            ));
        })
        .unwrap_or_else(|refusal| panic!("{:?}: a judgment: {refusal}", vault.order));
    named.sort();
    named
}

/// `change`'s link's address as written, its protocol prefix included.
fn address_of(change: &LinkChange) -> String {
    match &change.link.protocol {
        Some(protocol) => format!("{protocol}://{}", change.link.target),
        None => change.link.target.clone(),
    }
}

/// **A place no document stands at names every link that would resolve to
/// a document standing there**, as the root reads it. With nothing at `Old
/// Note.md`, `[[Old Note]]`, `[[Old Note.md]]`, `[[vault://Old Note]]` and
/// the Markdown link to that path could each name a document there, and
/// `[[old note]]` too only where the root folds ASCII case; `[[sub/Old
/// Note]]` names a deeper place and is never reached. `[[v1]]` could never
/// name a document at `v1.2.md`, which `[[v1.2]]` could.
#[test]
fn a_place_no_document_stands_at_names_the_links_that_would_resolve_there() {
    both_orders("resolution-reached-place", |mut vault| {
        vault.write(&[(
            "h.md",
            "[[Old Note]] [[old note]] [[Old Note.md]] [[sub/Old Note]] [[vault://Old Note]] [t](Old%20Note.md) [[v1]] [[v1.2]]\n",
        )]);
        let place = |at: &str| {
            could_name(&vault, &PathOverlay::new().reaching(path(at)))
                .into_iter()
                .filter(|(_, places)| places.iter().any(|named| named == at))
                .map(|(address, _)| address)
                .collect::<Vec<_>>()
        };
        let mut note = vec![
            "Old Note",
            "Old Note.md",
            "Old%20Note.md",
            "vault://Old Note",
        ];
        if vault.order == Folding {
            note.push("old note");
        }
        note.sort_unstable();
        assert_eq!(place("Old Note.md"), note, "{:?}", vault.order);
        assert_eq!(place("v1.2.md"), ["v1.2"], "{:?}", vault.order);
    });
}

/// **A link reached two ways is handed back once**: `[[a]]` is held under a
/// key the plan's create changes and under one that could name the place
/// the plan reaches.
#[test]
fn a_link_reached_by_a_change_and_by_a_place_is_judged_once() {
    both_orders("resolution-reached-twice", |mut vault| {
        vault.write(&[("h.md", "[[a]]\n")]);
        let plan = overlay(&["a.md"], &[]).reaching(path("x/a.md"));
        let (reached, _) = vault.judge(&plan, &[]);
        assert_eq!(reached, [judged("h.md", "a", "none", "one:a.md")]);
    });
}

// ---- robustness ----

/// **A plan's own progress never changes what it records.** Once the store
/// has taken in a create the plan made, and a removal it made, the same
/// overlay reads the same two vaults: every target is overlaid both ways.
#[test]
fn a_store_holding_landed_targets_answers_as_before_they_landed() {
    both_orders("resolution-landed", |mut vault| {
        vault.write(&[("old/a.md", "alpha\n"), ("b.md", "[[a]]\n\n[[n]]\n")]);
        let plan = overlay(&["n.md"], &["old/a.md"]);
        let (first, _) = vault.judge(&plan, &[]);
        assert_eq!(
            first,
            [
                judged("b.md", "a", "one:old/a.md", "none"),
                judged("b.md", "n", "none", "one:n.md"),
            ]
        );
        vault.write(&[("n.md", "landed\n")]);
        vault.kill(&["old/a.md"]);
        let (again, _) = vault.judge(&plan, &[]);
        assert_eq!(again, first);
    });
}

/// **A landed target heading its class still leaves two stored documents
/// to read.** The store has taken in `n.md`, a create the plan made, which
/// heads the class `[[n]]` reads, beside `a/n.md` and `z/n.md`. The head is
/// read two rows past the one target the key names, so with the target
/// dropped two documents are left, and the link resolved to several before
/// the plan as after it.
#[test]
fn a_landed_target_heading_a_class_leaves_two_stored_documents_to_read() {
    both_orders("resolution-landed-head", |mut vault| {
        vault.write(&[
            ("n.md", "landed\n"),
            ("a/n.md", "a\n"),
            ("z/n.md", "z\n"),
            ("b.md", "[[n]]\n"),
        ]);
        let (judged_links, _) = vault.judge(&overlay(&["n.md"], &[]), &[]);
        assert_eq!(judged_links, [judged("b.md", "n", "several", "several")]);
    });
}

/// **On a root that folds case, the store's spelling of a target is the
/// target.** The store holds `Notes/A.md`; a plan removing it under the
/// spelling `notes/a.md` leaves no stored row standing in for it.
#[test]
fn a_target_spelled_in_another_case_on_a_folding_root_is_the_stored_document() {
    let mut vault = Vault::new("resolution-folding-respell", Folding);
    vault.write(&[("Notes/A.md", "alpha\n"), ("b.md", "[[a]]\n")]);
    let (judged_links, _) = vault.judge(&overlay(&[], &["notes/a.md"]), &[]);
    assert_eq!(
        judged_links,
        [judged("b.md", "a", "one:notes/a.md", "none")]
    );
}

/// **A link held under two changed keys is judged once.** `[[v1.2]]` is held
/// under the classes of both of its reductions; a plan creating a document
/// in each reaches it twice and hands it back once.
#[test]
fn a_link_held_under_two_changed_keys_is_judged_once() {
    both_orders("resolution-least-key", |mut vault| {
        vault.write(&[("b.md", "[[v1.2]]\n")]);
        let (judged_links, work) = vault.judge(&overlay(&["v1.2.md", "v1.md"], &[]), &[]);
        assert_eq!(judged_links, [judged("b.md", "v1.2", "none", "several")]);
        assert_eq!(work.links_evaluated, 1);
    });
}

/// **A mass delete no link reaches costs a statement per chunk of the keys it
/// changes, and no statement per key.** Six hundred documents, none linked,
/// are removed by one plan: the declaration's pin is read once, and the three
/// keys each document is named by are asked about 256 at a time, and nothing
/// else runs.
#[test]
fn a_link_free_mass_delete_costs_a_statement_per_chunk_of_its_keys() {
    const DEATHS: usize = 600;
    let mut vault = Vault::new("resolution-mass-delete", Sensitive);
    let paths: Vec<String> = (0..DEATHS).map(|at| format!("d/{at:05}.md")).collect();
    let documents: Vec<(&str, &str)> = paths.iter().map(|at| (at.as_str(), "alpha\n")).collect();
    vault.write(&documents);
    let plan = paths.iter().fold(PathOverlay::new(), |overlay, at| {
        overlay.with(path(at), true, false)
    });

    let snapshot = vault.snapshot();
    let before = snapshot.counters().statements_executed();
    let work = snapshot
        .resolution_changes(&plan, &[], &declared(), |change| {
            panic!("no link reaches the deleted documents: {change:?}")
        })
        .expect("a judgment");
    let keys = (DEATHS * 3) as u64;
    assert_eq!(
        snapshot.counters().statements_executed() - before,
        1 + keys.div_ceil(256),
        "the pin's read and one occupancy read per 256 of the {keys} keys"
    );
    assert_eq!(
        work,
        ResolutionWork {
            ran: vec![ResolutionStatement::Occupied; keys.div_ceil(256) as usize],
            ..ResolutionWork::default()
        }
    );
    assert_eq!(snapshot.counters().full_scan_steps(), 0);
}

/// **The keys a hub's in-links share are resolved once, and the head read
/// for each is bounded by the targets it could name.** Twenty documents link
/// `[[hub]]`; removing the hub judges twenty links against one key, whose
/// head reads the hub and nothing past the bound.
#[test]
fn a_hubs_in_links_resolve_one_key_once() {
    both_orders("resolution-hub", |mut vault| {
        let holders: Vec<String> = (0..20).map(|at| format!("in/{at:02}.md")).collect();
        let mut documents: Vec<(&str, &str)> = holders
            .iter()
            .map(|at| (at.as_str(), "[[hub]]\n"))
            .collect();
        documents.push(("hub.md", "the hub\n"));
        vault.write(&documents);
        let (judged_links, work) = vault.judge(&overlay(&[], &["hub.md"]), &[]);
        assert_eq!(judged_links.len(), 20);
        assert!(
            judged_links
                .iter()
                .all(|(_, _, before, after)| before == "one:hub.md" && after == "none")
        );
        assert_eq!(work.links_evaluated, 20);
        assert_eq!(work.keys_resolved, 1);
        assert_eq!(work.head_rows, 1);
        assert_eq!(
            work.ran,
            [
                ResolutionStatement::Occupied,
                ResolutionStatement::KeyLinks,
                ResolutionStatement::Heads,
            ],
            "one occupancy read, one page of the hub key's links, one head read"
        );
    });
}

/// What judging a plan that creates `new/index.md` cost over `vault` holding
/// `members` documents `{place}/aNNNN/index.md` beside `zz/index.md` and
/// `holder.md` linking `[[index]]`: the links it judged, its work, and the
/// steps its snapshot took. Under `archive/**` a `place` the root's order
/// matches to `archive` is kept out of the class `index` opens, and any other
/// is admitted to it.
fn create_beside(
    mut vault: Vault,
    place: &str,
    members: usize,
) -> (Vec<Judged>, ResolutionWork, u64) {
    let paths: Vec<String> = (0..members)
        .map(|at| format!("{place}/a{at:04}/index.md"))
        .collect();
    let mut documents: Vec<(&str, &str)> =
        paths.iter().map(|at| (at.as_str(), "index\n")).collect();
    documents.push(("zz/index.md", "index\n"));
    documents.push(("holder.md", "[[index]]\n"));
    vault.write(&documents);

    let snapshot = vault.snapshot();
    let before = snapshot.counters().vm_steps();
    let mut judged_links = Vec::new();
    let work = snapshot
        .resolution_changes(
            &overlay(&["new/index.md"], &[]),
            &[],
            &declared(),
            |change| {
                judged_links.push(read(&change));
            },
        )
        .expect("a judgment");
    assert_eq!(snapshot.counters().full_scan_steps(), 0);
    (judged_links, work, snapshot.counters().vm_steps() - before)
}

/// [`create_beside`] over twenty and over two thousand members at `place`,
/// each on a fresh store under `order`.
fn create_beside_few_and_many(
    order: StoredPathOrder,
    place: &str,
) -> [(Vec<Judged>, ResolutionWork, u64); 2] {
    [20, 2000].map(|members| {
        create_beside(
            Vault::new(
                &format!("resolution-beside-{order:?}-{place}-{members}"),
                order,
            ),
            place,
            members,
        )
    })
}

/// **A key's head costs the members its class admits, not the ones it keeps
/// out.** Two thousand documents `archive/aNNNN/index.md` under the ignored
/// `archive/**` sort ahead of `zz/index.md`, the one member the class `index`
/// admits, and a plan creating `new/index.md` judges `[[index]]` in the same
/// steps beside them as beside twenty: one link, one key, one head row, read
/// by a seek of the admitted members alone. On a root that folds case
/// `Archive/aNNNN/index.md` is the same ignored place, kept out by the same
/// stored count.
#[test]
fn a_heads_work_does_not_follow_the_ignored_members_of_its_class() {
    for (order, place) in [
        (Sensitive, "archive"),
        (Folding, "archive"),
        (Folding, "Archive"),
    ] {
        let at = format!("{order:?} {place}");
        let [(few, few_work, few_steps), (many, many_work, many_steps)] =
            create_beside_few_and_many(order, place);
        let expected = vec![judged("holder.md", "index", "one:zz/index.md", "several")];
        assert_eq!(few, expected, "{at}");
        assert_eq!(many, expected, "{at}");
        assert_eq!(few_work.links_evaluated, 1, "{at}");
        assert_eq!(few_work.keys_resolved, 1, "{at}");
        assert_eq!(few_work.head_rows, 1, "{at}");
        assert_eq!(many_work, few_work, "{at}");
        assert_eq!(
            many_steps, few_steps,
            "{at}: a hundred times the ignored members moved the judgment's steps"
        );
    }
}

/// **A key's head costs its bound, not the members its class admits.** Two
/// thousand documents `x/aNNNN/index.md` the class `index` admits are judged
/// in the same steps as twenty: each admitting count's run of the class is
/// cut at the head's bound before the runs are merged, so no read walks a
/// run past the rows the head can hand back.
#[test]
fn a_heads_work_does_not_follow_the_members_its_class_admits() {
    for order in [Sensitive, Folding] {
        let [(few, few_work, few_steps), (many, many_work, many_steps)] =
            create_beside_few_and_many(order, "x");
        let expected = vec![judged("holder.md", "index", "several", "several")];
        assert_eq!(few, expected, "{order:?}");
        assert_eq!(many, expected, "{order:?}");
        assert_eq!(few_work.keys_resolved, 1, "{order:?}");
        assert_eq!(few_work.head_rows, 3, "{order:?}");
        assert_eq!(many_work, few_work, "{order:?}");
        assert_eq!(
            many_steps, few_steps,
            "{order:?}: a hundred times the admitted members moved the judgment's steps"
        );
    }
}

/// **A head merged across admitting counts is cut to its bound.**
/// `[[archive/index]]` admits four documents `?/archive/index.md` under no
/// ignored place, of count one, and the ignored `archive/index.md`, of count
/// two. Each count's run is read up to the bound, so their union holds four
/// rows; the head hands back the three its bound allows, two past the one
/// target the plan creates.
#[test]
fn a_head_merged_across_admitting_counts_is_cut_to_its_bound() {
    both_orders("resolution-two-counts", |mut vault| {
        vault.write(&[
            ("p/archive/index.md", "p\n"),
            ("q/archive/index.md", "q\n"),
            ("r/archive/index.md", "r\n"),
            ("s/archive/index.md", "s\n"),
            ("archive/index.md", "ignored\n"),
            ("holder.md", "[[archive/index]]\n"),
        ]);
        let (judged_links, work) = vault.judge(&overlay(&["new/archive/index.md"], &[]), &[]);
        assert_eq!(
            judged_links,
            [judged("holder.md", "archive/index", "several", "several")]
        );
        assert_eq!(work.keys_resolved, 1);
        assert_eq!(work.head_rows, 3);
    });
}

/// **A declaration the snapshot does not pin is refused**, as every read
/// builder refuses one.
#[test]
fn a_declaration_the_snapshot_does_not_pin_is_refused() {
    let vault = Vault::new("resolution-unpinned", Sensitive);
    let refused = vault.snapshot().resolution_changes(
        &overlay(&["a.md"], &[]),
        &[],
        &ContentModel::under("another-schema"),
        |_| {},
    );
    assert!(
        matches!(
            refused,
            Err(norn_store::PageRefusal::DeclarationNotPinned { .. })
        ),
        "{refused:?}"
    );
}

// ---- a target named over a plan ----

/// What `address` names over the plan `overlay` on `vault`, as the door reads
/// it, headed where it names several after the plan.
fn named(vault: &Vault, overlay: &PathOverlay, address: &str) -> TargetNaming {
    named_headed(vault, overlay, address, PlanSide::After)
}

/// What `address` names over the plan `overlay` on `vault`, headed where it
/// names several on the side `headed`.
fn named_headed(
    vault: &Vault,
    overlay: &PathOverlay,
    address: &str,
    headed: PlanSide,
) -> TargetNaming {
    vault
        .snapshot()
        .target_naming(overlay, address, headed, &declared())
        .unwrap_or_else(|refusal| panic!("{:?}: naming `{address}`: {refusal}", vault.order))
        .0
}

fn wire(text: &str) -> norn_wire::DocumentPath {
    norn_wire::DocumentPath::new(text).expect("a document path")
}

/// What the store's own reads say `address` names on `vault`: every document
/// a find's `resolves` part matches, and, where it matches several, the head
/// a links-to part reports of them.
fn read_naming(vault: &Vault, address: &str) -> (Resolves, Option<CandidateHead>) {
    let target = ResolutionTarget::new(address).expect("a target");
    let find = |predicate: Predicate| {
        vault
            .snapshot()
            .find(
                &FindParams::new(VaultAddress::name(VaultName::new("notes").expect("a name")))
                    .with_predicates([predicate])
                    .with_limit(500),
                &declared(),
            )
            .expect("a find")
    };
    let resolved = find(Predicate::resolves(target.clone()));
    match &resolved.rows[..] {
        [] => (Resolves::none(), None),
        [one] => (Resolves::one(one.path.clone()), None),
        _ => {
            let reported = find(Predicate::links_to(target));
            let [Unsatisfied::LinksToAmbiguous { candidates, .. }] = &reported.unsatisfied[..]
            else {
                panic!("`{address}` names several, and its links-to part reported {reported:?}");
            };
            (Resolves::several(), Some(candidates.clone()))
        }
    }
}

/// **A target is named on each side of a plan**: a document the plan removes
/// is named before and not after, one it creates after and not before, and
/// one it leaves alone on both.
#[test]
fn a_target_is_named_on_each_side_of_a_plan() {
    both_orders("target-naming-sides", |mut vault| {
        vault.write(&[("x/a.md", "a\n"), ("b.md", "b\n")]);
        let plan = overlay(&["y/c.md"], &["x/a.md"]);
        assert_eq!(
            named(&vault, &plan, "a"),
            TargetNaming::new(Resolves::one(wire("x/a.md")), Resolves::none(), None)
        );
        assert_eq!(
            named(&vault, &plan, "b"),
            TargetNaming::new(
                Resolves::one(wire("b.md")),
                Resolves::one(wire("b.md")),
                None
            )
        );
        assert_eq!(
            named(&vault, &plan, "c"),
            TargetNaming::new(Resolves::none(), Resolves::one(wire("y/c.md")), None)
        );
        assert_eq!(
            named(&vault, &plan, "zzz"),
            TargetNaming::new(Resolves::none(), Resolves::none(), None)
        );
    });
}

/// **A target naming several documents after a plan carries the head of
/// them, as a store built at the plan's after-state reads it**: the documents
/// in the resolution ladder's order, each named by its minimal disambiguating
/// suffix where the plan leaves the vault, beside their exact total — a
/// stored member the plan removes left out, one it creates merged in at its
/// rung, an ignored place kept out, and a head that fills counted past it.
#[test]
fn a_target_naming_several_after_a_plan_is_headed_as_the_store_built_there_reads_it() {
    let stored: Vec<(String, String)> = (0..7)
        .map(|at| (format!("f{at}/c.md"), "c\n".to_string()))
        .chain([
            ("x/a.md".to_string(), "a\n".to_string()),
            ("y/a.md".to_string(), "a\n".to_string()),
            ("archive/a.md".to_string(), "a\n".to_string()),
            ("p/q/d.md".to_string(), "d\n".to_string()),
        ])
        .collect();
    let plans: [(&[&str], &[&str], &[&str]); 3] = [
        (&["z/a.md"], &["y/a.md"], &["a", "x/a", "archive/a"]),
        (
            &["e/c.md", "f9/c.md"],
            &["f0/c.md", "f3/c.md"],
            &["c", "f1/c"],
        ),
        (&["r/q/d.md"], &[], &["d", "q/d"]),
    ];
    for order in [Sensitive, Folding] {
        for (created, removed, addresses) in plans {
            let label = format!(
                "target-naming-head-{order:?}-{}",
                created[0].replace('/', "-")
            );
            let mut was = Vault::new(&format!("{label}-before"), order);
            let documents: Vec<(&str, &str)> = stored
                .iter()
                .map(|(at, body)| (at.as_str(), body.as_str()))
                .collect();
            was.write(&documents);
            let mut is = Vault::new(&format!("{label}-after"), order);
            let after: Vec<(&str, &str)> = documents
                .iter()
                .filter(|(at, _)| !removed.contains(at))
                .copied()
                .chain(created.iter().map(|at| (*at, "new\n")))
                .collect();
            is.write(&after);
            let plan = overlay(created, removed);
            for address in addresses {
                let naming = named(&was, &plan, address);
                let (after, candidates) = read_naming(&is, address);
                assert_eq!(
                    (naming.after, naming.candidates),
                    (after, candidates),
                    "{order:?}: `{address}` over {created:?} created and {removed:?} removed"
                );
                assert_eq!(
                    naming.before,
                    read_naming(&was, address).0,
                    "{order:?}: `{address}` before the plan"
                );
            }
        }
    }
}

/// **A target naming several documents before a plan is headed there when
/// asked**, as the store standing before the plan reads it: the plan
/// removing `y/a.md` leaves `a` naming one document after it, so a head of
/// the after-state is never read, while the head before it is the stored
/// one's.
#[test]
fn a_target_naming_several_before_a_plan_is_headed_as_the_store_reads_it_then() {
    both_orders("target-naming-head-before", |mut vault| {
        vault.write(&[("x/a.md", "a\n"), ("y/a.md", "a\n"), ("b.md", "b\n")]);
        let plan = overlay(&[], &["y/a.md"]);
        let (before, candidates) = read_naming(&vault, "a");
        assert_eq!(
            named_headed(&vault, &plan, "a", PlanSide::Before),
            TargetNaming::new(before, Resolves::one(wire("x/a.md")), candidates)
        );
        assert_eq!(
            named(&vault, &plan, "a"),
            TargetNaming::new(Resolves::several(), Resolves::one(wire("x/a.md")), None)
        );
    });
}

// ---- the store's own reads as the oracle ----

/// A deterministic draw, so every run judges the same vaults and plans.
struct Draw(u64);

impl Draw {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        self.0 >> 33
    }

    fn pick<'a, T>(&mut self, from: &'a [T]) -> &'a T {
        &from[(self.next() as usize) % from.len()]
    }
}

/// Where drawn documents stand: nested, ignored by the declaration, and
/// spelled in another case.
const FOLDERS: &[&str] = &["", "x/", "y/", "x/z/", "archive/", "X/"];
/// The stems drawn documents are named by, a dotted one among them.
const STEMS: &[&str] = &["a", "b", "v1", "v1.2", "A", "c"];
/// The links a drawn document holds: suffix, rooted, relative, attachment
/// and empty addresses, in both syntaxes.
const LINKS: &[&str] = &[
    "[[a]]",
    "[[x/a]]",
    "[[z/a]]",
    "[[vault://x/a]]",
    "[t](a.md)",
    "[t](../a.md)",
    "[t](z/b.md)",
    "[[#h]]",
    "[t](#h)",
    "[[v1.2]]",
    "[[v1]]",
    "[[archive/a]]",
    "[[A]]",
    "[[b.md]]",
    "[[c]]",
];

/// The targets each drawn plan names, as a delete's `rewrite_to` would.
const TARGETS: &[&str] = &[
    "a",
    "x/a",
    "z/a",
    "v1.2",
    "v1",
    "A",
    "c",
    "archive/a",
    "b.md",
];

/// Every link the document at `at` holds, keyed by syntax, address and
/// offset, with what the store's own find reads it as resolving to.
fn found_links(vault: &Vault, at: &str) -> BTreeMap<(String, String, Option<u64>), String> {
    let found = vault
        .snapshot()
        .find(
            &FindParams::new(VaultAddress::name(VaultName::new("notes").expect("a name")))
                .with_predicates([Predicate::path(at)])
                .with_columns([Column::links()]),
            &declared(),
        )
        .expect("a find");
    let [row] = &found.rows[..] else {
        panic!("`{at}` is not one row: {:?}", found.rows);
    };
    let links = row.links.clone().expect("the links column");
    assert_eq!(
        links.total,
        links.items.len() as u64,
        "every link of `{at}`"
    );
    links
        .items
        .iter()
        .map(|link| {
            let protocol = link
                .protocol
                .as_deref()
                .map(|protocol| format!("{protocol}://"))
                .unwrap_or_default();
            let resolves = match link.health() {
                LinkHealth::Healthy => {
                    format!("one:{}", link.targets.candidates()[0].path.as_str())
                }
                LinkHealth::Ambiguous => "several".to_string(),
                _ => "none".to_string(),
            };
            (
                (
                    format!("{:?}", link.family),
                    format!("{protocol}{}", link.target),
                    link.span.map(|span| span.byte_offset),
                ),
                resolves,
            )
        })
        .collect()
}

/// One drawn trial: a vault, a plan of deletes, moves and creates over it,
/// and every link the door says the plan moves compared with the store's
/// own reads of a store holding the vault before the plan and of one built
/// at the vault after it. Answers how many moved links were compared.
fn trial(order: StoredPathOrder, seed: u64) -> Result<usize, String> {
    let mut draw = Draw(seed);
    let identity = |at: &str| match order {
        Sensitive => at.to_string(),
        Folding => at.to_ascii_lowercase(),
    };
    let mut before: BTreeMap<String, String> = BTreeMap::new();
    for _ in 0..4 + draw.next() % 7 {
        let at = format!("{}{}.md", draw.pick(FOLDERS), draw.pick(STEMS));
        if before.keys().all(|held| identity(held) != identity(&at)) {
            let body: String = (0..draw.next() % 4)
                .map(|_| format!("{}\n\n", draw.pick(LINKS)))
                .collect();
            before.insert(at, format!("# h\n\n{body}"));
        }
    }
    let mut after = before.clone();
    let mut targets: BTreeMap<String, (bool, bool)> = BTreeMap::new();
    let mut sources: BTreeMap<String, String> = BTreeMap::new();
    let taken = |after: &BTreeMap<String, String>, targets: &BTreeMap<String, _>, at: &str| {
        after
            .keys()
            .chain(targets.keys())
            .any(|held| identity(held) == identity(at))
    };
    for at in before.keys() {
        match draw.next() % 5 {
            0 => {
                after.remove(at);
                targets.insert(at.clone(), (true, false));
            }
            1 => {
                let to = format!("{}{}.md", draw.pick(FOLDERS), draw.pick(STEMS));
                if !taken(&after, &targets, &to) {
                    let body = after.remove(at).expect("a document the vault holds");
                    after.insert(to.clone(), body);
                    targets.insert(at.clone(), (true, false));
                    targets.insert(to.clone(), (false, true));
                    sources.insert(to, at.clone());
                }
            }
            _ => {}
        }
    }
    let to = format!("{}{}.md", draw.pick(FOLDERS), draw.pick(STEMS));
    if !taken(&after, &targets, &to) {
        after.insert(to.clone(), "# h\n".to_string());
        targets.insert(to, (false, true));
    }

    let built = |side: &str, documents: &BTreeMap<String, String>| {
        let mut vault = Vault::new(&format!("resolution-oracle-{order:?}-{seed}-{side}"), order);
        let documents: Vec<(&str, &str)> = documents
            .iter()
            .map(|(at, body)| (at.as_str(), body.as_str()))
            .collect();
        vault.write(&documents);
        vault
    };
    let was = built("before", &before);
    let is = built("after", &after);

    let mut expected = BTreeSet::new();
    for holder in after.keys() {
        let source = sources.get(holder).unwrap_or(holder);
        if !before.contains_key(source) {
            continue;
        }
        let then = found_links(&was, source);
        for (link, now) in found_links(&is, holder) {
            let resolved = &then[&link];
            if *resolved != now {
                expected.insert((holder.clone(), link.1, link.2, resolved.clone(), now));
            }
        }
    }

    let overlay = targets
        .iter()
        .fold(PathOverlay::new(), |overlay, (at, (was, is))| {
            overlay.with(path(at), *was, *is)
        });
    let probed: Vec<ProbedLink> = sources
        .iter()
        .flat_map(|(to, from)| {
            derived(to, &after[to])
                .links
                .into_iter()
                .map(|link| ProbedLink {
                    before_holder: path(from),
                    after_holder: path(to),
                    link,
                    written: false,
                })
        })
        .collect();
    let mut moved = BTreeSet::new();
    was.snapshot()
        .resolution_changes(&overlay, &probed, &declared(), |change| {
            let (holder, address, before, after) = read(&change);
            if before != after {
                moved.insert((
                    holder,
                    address,
                    change.link.span.map(|span| span.byte_offset),
                    before,
                    after,
                ));
            }
        })
        .map_err(|refusal| format!("the door refused: {refusal}"))?;
    for address in TARGETS {
        let (naming, _) = was
            .snapshot()
            .target_naming(&overlay, address, PlanSide::After, &declared())
            .map_err(|refusal| format!("the door refused to name `{address}`: {refusal}"))?;
        let read = (read_naming(&was, address).0, read_naming(&is, address));
        if (&naming.before, (&naming.after, &naming.candidates))
            != (&read.0, (&read.1.0, &read.1.1))
        {
            return Err(format!(
                "{order:?} seed {seed}\nbefore {before:#?}\nplan {targets:?}\n`{address}` named \
                 {naming:?}\nthe store's reads name {read:?}"
            ));
        }
    }
    if moved != expected {
        return Err(format!(
            "{order:?} seed {seed}\nbefore {before:#?}\nplan {targets:?}\nmoved from {sources:?}\n\
             missing {:#?}\nspurious {:#?}",
            expected.difference(&moved).collect::<Vec<_>>(),
            moved.difference(&expected).collect::<Vec<_>>(),
        ));
    }
    Ok(expected.len())
}

/// **The door agrees with the store rebuilt at the plan's after-state.** Over
/// drawn vaults and plans of deletes, moves and creates, on both roots, every
/// link the door says a plan moves — and nothing else — is a link the store's
/// own find reads as resolving one way in a store holding the vault before
/// the plan and another in a store built at the vault after it, read from the
/// holder's source before the plan. Every target the door names over the
/// plan names, on each side, what the store's own reads name in the store
/// holding that side, the head of several included.
#[test]
fn the_door_agrees_with_the_store_rebuilt_at_the_after_state() {
    let mut compared = 0;
    for order in [Sensitive, Folding] {
        for seed in 0..16 {
            compared += trial(order, seed).unwrap_or_else(|failure| panic!("{failure}"));
        }
    }
    assert!(
        compared > 0,
        "the draws moved no link, so nothing was compared"
    );
}
