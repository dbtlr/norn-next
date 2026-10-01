//! The resolution change set's store half: what each link a plan reaches
//! resolves to before the plan and after it, read on one snapshot with the
//! plan's targets overlaid on the documents the store holds.
//!
//! Every case runs over a root that tells spellings apart and over one that
//! folds ASCII case, and the documents are derived from Markdown through the
//! text layer, as the host derives them.

use norn_store::{
    ContentModel, LinkChange, LinkFact, PathOverlay, ProbedLink, Provenance, ResolutionStatement,
    ResolutionWork, Snapshot, Store, StoredPathOrder,
};
use norn_wire::{Pattern, Resolves};

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
