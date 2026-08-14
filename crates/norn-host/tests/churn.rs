//! **Rung 1 of the heal ladder, reached by ordinary operation.** A real host
//! maintains a vault while somebody else edits it, and what it converges on is
//! compared with a derivation that started from zero over the same final tree.
//!
//! Every case here has the same three-part shape.
//!
//! - **A workload runs against a live tree, in phases.** The scripts are
//!   `norn_testkit::churn`'s: seeded, ordered, and each step saying in words what
//!   it does, so a failure prints the workload rather than a stack of writes. The
//!   host is a production attachment with a real platform watcher, so what
//!   reaches it is what reaches a host on somebody's machine — no injected
//!   failure, no seam a test reached through. **At least one settle stands
//!   between the phase that puts content in the tree and the phase that changes
//!   it**, because a modification is only a modification against a row the host
//!   already holds: a workload landing whole between two polls asks a host for
//!   nothing but "these files appeared", and no prune, tombstone or
//!   re-derivation is on the path it takes.
//! - **The tree is settled, deterministically.** The wait is on a census: every
//!   markdown place in the tree either has the document row its bytes imply, or
//!   is one of the places the workload declared derives none. Nothing here sleeps
//!   for a fixed time and calls that convergence, and the budget is a runaway
//!   bound rather than a bar — how *long* a settle takes is a clock, and clocks
//!   belong to the scheduled lane. **Two claims stand between the acts and the
//!   wait**, because a phase that changed nothing satisfies every bar after it:
//!   a non-empty script has to leave a census the phase's opening one does not
//!   equal, and every place that stopped deriving is a death the store is then
//!   required to hold a tombstone for.
//! - **The bar is equivalence with a build from zero.** The vault is derived a
//!   second time through machine-local directories that hold no row, no lock and
//!   no shadow of the first, and the two derived stores are compared field by
//!   field by `norn_testkit::equivalence`. Both are asked to be internally sound
//!   on their own, and the comparison is floored by what the workload's own final
//!   tree holds, so two stores that derived nothing cannot agree their way to a
//!   pass.
//!
//! # The census is coarse and the bar is not
//!
//! The wait asks about paths and content hashes, which is the cheapest signal
//! that says the host has caught up. What is then judged is everything a store
//! holds — bodies, links, headings, blocks, tags, indexed terms, findings, the
//! pinned schema — so a host that landed the right hashes and derived the wrong
//! facts from them fails the bar it passed the wait for.
//!
//! # What the work bounds say
//!
//! Maintenance after an edit costs the changed set, not the vault. The account
//! a host's own jobs write is what says so, and it is read behind
//! `induced-failure` with the rest of the harness-reachable surface — so the
//! convergence bars above run in every lane and the cost bars run in that one.
//! Each bound is a bracket rather than a ceiling: a reading under the ceiling
//! stated for the changed set, and over what the same bound would admit for a
//! changed set of nothing. The upper half is the claim; the lower half is what
//! says the instrument moved.
//!
//! # Two hosts never serve at once
//!
//! The real-watcher lease is machine-wide and not reentrant, so each host here
//! is dropped before the next one attaches. Every case is therefore a sequence
//! of attachments over one tree — one for each phase it settles, a last one for
//! the derivation from zero it is judged against, and, for the case that lands
//! a phase inside a heal, one for each attempt that failed to catch a walk in
//! time.
//!
//! Each generated tree sits in a testkit sandbox, which is a unix-only harness.
#![cfg(unix)]
#![allow(clippy::disallowed_methods)] // Harness scaffolding: this suite's own generated tree.

mod attach;

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::time::{Duration, Instant};

use norn_fs::{CaseSensitivity, ContentHash, PathNormalizer};
#[cfg(feature = "induced-failure")]
use norn_host::EvidenceReading;
use norn_host::{AttachMode, DemandLease, ProductionEntryOps};
use norn_store::{DocumentPath, Provenance, Store, class_probe};
use norn_testkit::churn::{self, Act, Applied, Folding, Script, Step};
use norn_testkit::equivalence::{
    Population, StoreProjection, assert_operationally_valid, tombstones,
};
use norn_testkit::process::Sandbox;
use norn_testkit::wait::{Convergence, Observed, wait_until};
use norn_wire::{FindingKind, TrustState, WarmingPhase};

/// The profile every case churns over.
///
/// 120 documents, shaped like a real vault — ambiguity classes, dangling links,
/// unicode and spaced stems, clutter that is not a document, and symbolic links
/// a walk refuses to follow. What each case adds on top is its own workload, so
/// the vault around the churn is the same in every one of them.
///
/// **The scale is chosen so that a changed-set bound can mean something.** A
/// claim that maintenance costs the changed set rather than the vault is not a
/// claim at all over a vault a bound could mostly re-read and still fit, so
/// every cost bar asserts beside its own reading that its ceiling is under half
/// of these 120 documents. Against that guard of 60, the ceilings the bars here
/// really stand on are 8 opens over ordinary editing's and atomic replacement's
/// 4 places, 4 over the ambiguity class's 2, 4 upserts over the same 4 places,
/// 24 opens over the external tools' 12, and 36 over the burst's 18 steps — the
/// widest of them well under it.
///
/// The profile is also small enough that the two attachments each case makes
/// cost a fraction of a second, which is what keeps this suite in the per-PR
/// lane.
const PROFILE: &str = "small";

/// The runaway bound on settling, derived from the changed set.
///
/// **Not a bar.** A settle that reaches this is stuck rather than slow, and how
/// long one really takes is a clock this lane does not read — a wall-clock
/// ceiling tight enough to fail a slow convergence is the scheduled lane's to
/// state. The floor covers a watcher's own coalescing window and one heal over
/// the profile; the per-change allowance is what a wide workload adds to it,
/// which is why it is derived from the changed set rather than written flat.
const SETTLING: Convergence = Convergence::new(
    Duration::from_secs(30),
    Duration::from_secs(2),
    Duration::from_secs(10),
);

/// The runaway bound on catching an attach heal while it is still walking.
///
/// **Not a bar.** A walk over this profile is milliseconds long, so the one
/// case that lands a phase inside one races for it and re-attaches when it
/// loses — see [`caught_mid_walk`]. This bounds the whole race: reaching it
/// means an entry that never publishes a walk at all, rather than a machine
/// that lost a few races.
const CATCHING_A_HEAL: Duration = Duration::from_secs(60);

/// The bytes the validity workload replaces the vault's schema declaration
/// with.
///
/// A schema is opaque to derivation — it is read, hashed and pinned, and no
/// document is judged against it — so what makes this a schema *change* is that
/// the bytes differ from the ones the attachment pinned. The pin is what
/// discards every finding derived under the old fingerprint, and the heal that
/// follows is what derives them again.
const REPLACEMENT_SCHEMA: &[u8] = b"version: 1\n# a second declaration\n";

/// A document whose frontmatter block is past the bound the text layer reads.
///
/// The block never closes inside the bound, so the read refuses it by size: the
/// document keeps the row the act could derive and a document-scoped finding
/// stands beside it. Editing this document down to an ordinary one is the row
/// flip the content size drives, and editing an ordinary one up to this is the
/// same flip the other way.
fn oversized_frontmatter() -> Vec<u8> {
    let mut block = String::from("a: ");
    while block.len() + 1 < norn_text::FRONTMATTER_MAX_BYTES * 2 {
        block.push('[');
    }
    block.push('\n');
    format!("---\n{block}---\n# body\n").into_bytes()
}

// ---------------------------------------------------------------------------
// The workloads
// ---------------------------------------------------------------------------

/// **Family 1.** Ordinary creation, modification and deletion across nested
/// directories, with the cost of it bracketed.
///
/// The bracketed phase rewrites two rows the host holds and takes two away, so
/// the account it moves is a re-derivation, a prune and the tombstones that go
/// with them rather than four arrivals.
#[test]
fn ordinary_editing_converges_on_a_build_from_zero() {
    let workload = churn::ordinary_editing(41);
    let mut churned = churn_the_vault("churn-ordinary", &workload);

    churned.judge(workload.changing().name(), 0);
    churned.assert_rows_were_taken_away();
    churned.assert_maintenance_is_bracketed(&OPENS);
    churned.assert_maintenance_is_bracketed(&UPSERTS);
}

/// **The same edits, made while nothing was attached.**
///
/// A watcher reports what happens while it is installed, and the phase here
/// happens while it is not: the opening phase is settled and its host let go,
/// the edits and removals land against a tree nobody is watching, and a host
/// attaches afterwards. **The attach heal is then the only thing that can
/// converge them**, so this is the case that reaches the merge's own answer for
/// a row whose file the walk no longer yields — the arm every other case here
/// gets for free from a watcher report.
///
/// No cost bar. The account this phase moves is a heal's, which walks the vault
/// by contract, and a changed-set bound over a whole-vault act would be a bound
/// against that contract.
#[test]
fn edits_made_while_nothing_was_attached_converge_on_a_build_from_zero() {
    let workload = churn::ordinary_editing(79);
    let opened = attach_and_churn(sandbox("churn-detached"), workload.opening(), When::Settled);
    let mut churned = opened.then(workload.changing(), When::Before);

    churned.judge(workload.changing().name(), 0);
}

/// **Family 2.** Content landing whole at a name, and names moving between
/// directories.
///
/// A rename is two changed places — the name that emptied and the name that
/// filled — and a document that moves twice ends up somewhere no walk has ever
/// enumerated it before. The case flip is the case below, because what a flip
/// means is the volume's own answer.
///
/// Both acts land on rows the host holds: content arrives whole over a document
/// that already derived, and the name that moves is a name a row stands at.
#[test]
fn atomic_replacement_and_movement_converge_on_a_build_from_zero() {
    let workload = churn::atomic_replacement(43);
    let mut churned = churn_the_vault_in(sandbox("churn-atomic"), &workload);

    churned.judge(workload.changing().name(), 0);
    churned.assert_rows_were_taken_away();
    churned.assert_maintenance_is_bracketed(&OPENS);
}

/// **A name whose case flips**, which is a different act on each kind of volume.
///
/// **The platform lane is declared, not skipped.** The volume is asked twice —
/// once by a probe that writes a file and looks for it under another spelling,
/// and once through the normalizer a host itself resolves paths with — and the
/// two are required to agree, because a suite churning against one answer while
/// the host served the other would be judging a tree neither of them describes.
/// Both answers run the same three acts; what differs is what those acts mean.
///
/// Where two spellings are two places, the flip is a move and everything below
/// holds straight through.
///
/// Where they are one place, one thing does not. The increment a watcher report
/// drives **converges the document and not its spelling**: the row keeps the
/// spelling the vault was first walked at while holding the bytes that landed at
/// the flipped one, so a keyed read at the spelling on disk finds nothing until
/// the next attach heal re-walks the vault and moves the row. This case waits for
/// the identity to hold the last bytes, states that it does, and takes the
/// equivalence bar after that heal — and the difference between the two lanes is
/// a gap this suite found rather than a contract it is transcribing.
///
/// **The flip is held to three things a phase that did nothing would fail.**
/// The directory itself is asked what it renders the name as, which is the only
/// answer a volume that folds case cannot give twice; the row at the identity
/// holds bytes the opening phase's row did not; and one row stands there rather
/// than two. The re-attach below applies no acts at all, which is why this case
/// states them itself rather than leaning on the phase driver's own claim that a
/// phase moved the tree.
#[test]
fn a_case_flip_converges_on_a_build_from_zero() {
    let sandbox = sandbox("churn-case-flip");
    let folding = churn::folding(&sandbox.work_dir()).expect("a case probe over the sandbox");
    let workload = churn::case_flip(73, folding);
    let opened = attach_and_churn(sandbox, workload.opening(), When::Settled);
    // Read before the flip: what says the changing phase reached the row is the
    // identity holding bytes the opening phase did not put there.
    let before_the_flip = opened
        .census
        .rows
        .get(&identity(churn::FLIPPED_FROM, folding))
        .expect("the name that is about to flip stands in the tree")
        .clone();
    let mut churned = opened.then(workload.changing(), When::Settled);
    assert_eq!(
        folding, churned.folding,
        "two probes of one volume disagree"
    );
    churned.assert_the_flip_landed(&before_the_flip);

    if folding == Folding::Folded {
        churned = churned.then(&Script::new("nothing at all", Vec::new()), When::Settled);
    }
    churned.judge(workload.changing().name(), 0);
}

/// **Family 3.** A burst against one path and a burst across many.
///
/// The bar is what the store ends up holding rather than how many times it was
/// written to: whether a host coalesced twelve edits into one increment or ran
/// twelve is its own business, and the last bytes written to the hammered path
/// are what a build from zero finds there.
///
/// The bound here is stated over **steps** rather than places, because twelve
/// edits to one path are twelve changes a host may be asked to reconcile — a
/// bound stated over the one place they name would be a bound on coalescing,
/// which is an optimization and not a contract.
///
/// The hammered path is written once and settled over before the burst starts,
/// so every one of the twelve edits rewrites a row the host holds.
#[test]
fn a_burst_converges_on_the_last_bytes_written() {
    let workload = churn::burst(47);
    let mut churned = churn_the_vault("churn-burst", &workload);

    // The absolute claim beside the relative one: the hammered path holds the
    // last edit's bytes and none of the eleven writes before them.
    let last = churned
        .census
        .rows
        .get("churn/burst/hammered.md")
        .expect("the hammered path stands in the tree")
        .clone();
    churned.judge(workload.changing().name(), 0);
    churned.assert_derived_hash("churn/burst/hammered.md", &last);
    churned.assert_maintenance_is_bracketed(&BURST_OPENS);
}

/// **Family 4.** Documents crossing between readable, quarantined and degraded,
/// and a schema replacement under them.
///
/// Four boundaries are crossed, each in the direction the others are not: a
/// readable document becomes bytes no decoder accepts, an undecodable one
/// becomes text, a document past the frontmatter read bound comes back inside
/// it, and one inside it goes past. **The row flip is the point**: a quarantined
/// place holds no row and a finding stands at it, while a document whose block
/// went unread keeps its row and carries a finding beside it — so the two
/// transitions move different pillars and a build from zero has to reach the
/// same answer about both.
///
/// The schema replacement lands last. A re-pin discards every finding derived
/// under the fingerprint it replaced and heals the vault again, so what this
/// case says about the findings above is that they came back.
///
/// **Every crossing is made against what the host already holds.** The four
/// states are established and settled over first, so the quarantine takes a row
/// away, the recovery gives one back, and the two read-bound crossings move the
/// frontmatter projection of rows that stand throughout.
///
/// **No work bound.** A schema re-pin is a vault-scope act by contract: the
/// findings it discards stand at places no row does, and only a walk of the
/// whole vault re-files them. A changed-set bound over this workload would be a
/// bound against that contract.
#[test]
fn documents_crossing_validity_boundaries_converge_on_a_build_from_zero() {
    let oversized = oversized_frontmatter();
    let workload = churn::validity_transitions(
        53,
        churn::SchemaGround {
            at: ".norn/schema.yaml",
            replacement: REPLACEMENT_SCHEMA,
        },
        &oversized,
    );
    let mut churned = churn_the_vault("churn-validity", &workload);

    // One document stands past the frontmatter read bound at the end — the one
    // the workload pushed past it — and it carries a finding beside the row it
    // kept. The document that started past the bound was brought back inside
    // it.
    churned.judge(workload.changing().name(), 1);
    churned.assert_rows_were_taken_away();

    let mut store = churned.vault.store();
    let projection = StoreProjection::read(&mut store).expect("projecting the churned store");

    // The quarantined place: no row, and the cause named at the path.
    assert!(
        projection.document("churn/states/souring.md").is_none(),
        "a document row stands where the workload left bytes no decoder accepts"
    );
    assert_eq!(
        kinds_at(&projection, "churn/states/souring.md"),
        vec![FindingKind::BodyBytesNotUtf8.as_str().to_string()],
        "the quarantined place carries another cause"
    );

    // The place that came back: a row, and nothing standing at it.
    let recovered = projection
        .document("churn/states/recovering.md")
        .expect("the document that stopped being undecodable derives a row");
    assert!(
        !recovered.body.is_empty(),
        "the recovered document derived an empty body"
    );
    assert!(
        kinds_at(&projection, "churn/states/recovering.md").is_empty(),
        "a finding stands at a place that reads"
    );

    // The row flip the content size drives, in both directions. A block past
    // the bound leaves the row standing with no frontmatter projection; a block
    // inside it leaves a projection and no finding.
    let degraded = projection
        .document("churn/states/steady.md")
        .expect("a document whose block went unread keeps its row");
    assert!(
        degraded.frontmatter.is_none(),
        "the document past the read bound carries a frontmatter projection"
    );
    assert_eq!(
        kinds_at(&projection, "churn/states/steady.md"),
        vec![FindingKind::FrontmatterTooLarge.as_str().to_string()],
        "the document past the read bound carries another cause"
    );
    let shortened = projection
        .document("churn/states/overlong.md")
        .expect("the document brought back inside the bound keeps its row");
    assert!(
        shortened.frontmatter.is_some(),
        "the document inside the read bound carries no frontmatter projection"
    );
    assert!(
        kinds_at(&projection, "churn/states/overlong.md").is_empty(),
        "a finding stands beside a block that reads"
    );

    // The schema the workload replaced is the one the store pinned, and the
    // findings above were derived under it: a re-pin that discarded them and
    // healed nothing would have left the pillar empty.
    assert_eq!(
        projection
            .vault_schema()
            .expect("an attachment pins the vault schema")
            .bytes,
        REPLACEMENT_SCHEMA,
        "the store pinned bytes the vault no longer holds"
    );
}

/// **Family 5, first shape.** A synchronization client catching up after a
/// sleep, and an editor saving by writing a temporary file and renaming it over
/// the document.
///
/// The catch-up is what lands between a host's polls: creations, an edit and a
/// deletion arriving with nothing between them for a host to react to, followed
/// by a tool renaming the directory they all sit in. The tree those tools find
/// is one the host has already settled over, so the edit re-derives a standing
/// row, the deletion prunes one, and the rename moves five at once.
#[test]
fn an_external_tools_catch_up_converges_on_a_build_from_zero() {
    let workload = churn::external_tools(59);
    let mut churned = churn_the_vault("churn-tools", &workload);

    churned.judge(workload.changing().name(), 0);
    churned.assert_rows_were_taken_away();
    churned.assert_maintenance_is_bracketed(&TOOL_OPENS);
}

/// **Family 5, second shape.** The same edits, landing while an attach heal is
/// still running.
///
/// The opening phase is settled under one attachment which is then let go, so
/// the heal this case's edits land during is a **re-attach heal over a store
/// that already holds rows** — the walk enumerates documents the store stands
/// for, and the workload changes them underneath it.
///
/// **The overlap is forced and then witnessed.** The attachment is made over
/// and over until one of them is read part-way through its walk, and the first
/// act is held until that reading. Holding for `Warming` alone would not do it:
/// an entry `InstallingCoverage` is still acquiring the store, the lock and the
/// watcher, and a phase applied there lands wholly before the first document is
/// read, which is the catch-up case above wearing this one's name.
///
/// **The claim is about the first act.** The walk may finish while the rest of
/// the phase is still landing, and that is fine — what this case states is that
/// the phase began behind the walk's own cursor, which is the thing the catch-up
/// case cannot be. What is *not* witnessable from outside is finer still:
/// whether any one edit landed inside the window between the walk's enumeration
/// of a path and its open of that path is not something a black-box case can
/// force, and the case that stages that window exactly is in `norn-host`'s own
/// unit suite, where the walk and the merge are two calls.
///
/// So what this case proves that the catch-up case does not is that the edits
/// were concurrent with a heal in flight rather than delivered to a settled
/// host; the convergent claim is the same either way — however the edits
/// interleaved with the heal, the store ends up holding what a build from zero
/// over the final tree holds.
#[test]
fn edits_during_an_active_heal_converge_on_a_build_from_zero() {
    let workload = churn::external_tools(61);
    let opened = attach_and_churn(
        sandbox("churn-during-heal"),
        workload.opening(),
        When::Settled,
    );
    let mut churned = opened.then(workload.changing(), When::DuringTheHeal);

    churned.assert_the_workload_overlapped_a_heal();
    churned.judge("external tool edits landing during an active heal", 0);
}

/// **An ambiguity class whose membership changes.**
///
/// Three documents share one stem in three directories, and one of them leaves.
/// A class is what a stem's candidates are read out of, so its membership is
/// derived state like any other: a store that kept the departed document in the
/// class answers a resolution with a path that is not there.
///
/// The class is asked about directly on both sides, because membership is
/// derived from the rows rather than stored beside them — two stores holding the
/// same rows hold the same class by construction, and what could still be wrong
/// is the rows.
///
/// **The departure acts on a held row.** The class is established and settled
/// over before it changes, so the member that leaves is one the store has a row
/// for and the member that joins is added to a class that already resolves —
/// which is the only ordering in which membership is derived state being
/// *changed* rather than derived once.
///
/// The changing phase is one write and one removal over two places, which is
/// the ordinary-editing shape, so it takes [`OPENS`] rather than declaring an
/// exemption the way the validity and during-heal cases do.
#[test]
fn an_ambiguity_classs_membership_change_converges_on_a_build_from_zero() {
    let mut ink = churn::Ink::new(67);
    let opening = Script::new(
        "an ambiguity class: the members that will change",
        vec![
            Step::new(
                "write the first document of a shared stem",
                Act::Write {
                    at: "churn/class/one/shared.md".to_string(),
                    bytes: ink.document("shared, one"),
                },
            ),
            Step::new(
                "write the second, in another directory",
                Act::Write {
                    at: "churn/class/two/shared.md".to_string(),
                    bytes: ink.document("shared, two"),
                },
            ),
            Step::new(
                "write a document that links the shared stem without qualifying it",
                Act::Write {
                    at: "churn/class/citing.md".to_string(),
                    bytes: b"# citing\n\nSee [[shared]].\n".to_vec(),
                },
            ),
        ],
    );
    let changing = Script::new(
        "an ambiguity class gaining and losing members",
        vec![
            Step::new(
                "write a third document into the class the store already resolves",
                Act::Write {
                    at: "churn/class/three/shared.md".to_string(),
                    bytes: ink.document("shared, three"),
                },
            ),
            Step::new(
                "take the second one, which the host holds a row for, out of the class again",
                Act::Remove {
                    at: "churn/class/two/shared.md".to_string(),
                },
            ),
        ],
    );
    let mut churned = churn_the_vault("churn-class", &churn::Phased::new(opening, changing));
    churned.assert_rows_were_taken_away();
    churned.assert_maintenance_is_bracketed(&OPENS);
    churned.judge("an ambiguity class gaining and losing members", 0);

    let expected = vec![
        "churn/class/one/shared.md".to_string(),
        "churn/class/three/shared.md".to_string(),
    ];
    for (label, vault) in churned.both_derivations() {
        let mut store = vault.store();
        let probe = class_probe("shared").expect("a class probe over a stem");
        let members: Vec<String> = store
            .begin_request()
            .suffix_candidates(&probe)
            .expect("reading the class")
            .into_iter()
            .map(|path| path.as_str().to_string())
            .collect();
        assert_eq!(
            members, expected,
            "{label} holds another membership for the shared stem"
        );
    }
}

/// **A rendering collision that clears.**
///
/// A name the document-path grammar refuses is reported at the spelling norn
/// renders it as, and a finding there is withheld for as long as a readable
/// document stands at that spelling — calling a document that just derived
/// unreadable would be false. Removing the document vacates the place, and the
/// statement that was being withheld is due.
///
/// The forbidden shape is a finding that waits: the vault holds a document norn
/// cannot name, nothing says so, and the statement arrives only when some later
/// demand happens to reach that path. A build from zero over the final tree
/// files it, so an equivalence that holds is the withheld finding having
/// arrived.
#[test]
fn a_rendering_collision_that_clears_converges_on_a_build_from_zero() {
    let sandbox = sandbox("churn-collision");
    // The name is asked for outside the vault first, because the name is this
    // case's whole subject: a workload that could not write it would judge a
    // tree with no collision in it. **The platform lane is declared, not
    // skipped** — the same rule the case flip runs under. A filesystem that
    // refuses this name fails here, saying so, rather than logging a line and
    // reporting a pass over nothing.
    let probe = sandbox.work_dir().join("bad\\name.probe");
    if let Err(problem) = std::fs::write(&probe, b"a name") {
        panic!(
            "this filesystem will not create `{}`: {problem}. A backslash in a name is what this \
             case is about, so there is no tree to judge here and no lane declared for a volume \
             that refuses one — declare it, the way the case-flip case declares what its volume \
             does with case.",
            probe.display()
        );
    }
    std::fs::remove_file(&probe).expect("removing the probe");

    let script = Script::new(
        "a rendering collision that clears",
        vec![
            Step::new(
                "write the document whose spelling the rendering collides with",
                Act::Write {
                    at: "bad\u{fffd}name.md".to_string(),
                    bytes: b"---\ntitle: real\n---\n\n# a real document\n".to_vec(),
                },
            ),
            Step::new(
                "write the name the document-path grammar refuses",
                Act::Write {
                    at: "bad\\name.md".to_string(),
                    bytes: b"# a body\n".to_vec(),
                },
            ),
        ],
    )
    .without_rows_at("bad\\name.md");
    let clearing = Script::new(
        "the collision clears",
        vec![Step::new(
            "remove the document the rendering collided with",
            Act::Remove {
                at: "bad\u{fffd}name.md".to_string(),
            },
        )],
    )
    .without_rows_at("bad\\name.md");

    let mut churned =
        attach_and_churn(sandbox, &script, When::Before).then(&clearing, When::Settled);
    churned.judge("a rendering collision that clears", 0);

    let mut store = churned.vault.store();
    let projection = StoreProjection::read(&mut store).expect("projecting the churned store");
    assert_eq!(
        kinds_at(&projection, "bad\u{fffd}name.md"),
        vec![FindingKind::PathNamesNoDocument.as_str().to_string()],
        "the vacated place carries no statement about the name norn cannot spell"
    );
}

/// **The instrument moves with one seeded unit of work.**
///
/// **This is the control the brackets above stand on**, and it is a control
/// over a host rather than over arithmetic: one document is written, the vault
/// is settled, and then that one document — a row the store now holds — is
/// edited once. The account a host's jobs write has to show it. A counter that
/// read the same before and after would make every bracket above a statement
/// about a number nothing drives, and the upper half of each bracket would pass
/// a host that did no maintenance at all.
#[test]
fn one_seeded_edit_moves_the_maintenance_account() {
    let mut ink = churn::Ink::new(71);
    let workload = churn::Phased::new(
        Script::new(
            "one document, written",
            vec![Step::new(
                "write one document",
                Act::Write {
                    at: "churn/seeded/one.md".to_string(),
                    bytes: ink.document("one"),
                },
            )],
        ),
        Script::new(
            "one document, edited once",
            vec![Step::new(
                "edit the one document the host holds a row for",
                Act::Write {
                    at: "churn/seeded/one.md".to_string(),
                    bytes: ink.document("one, edited"),
                },
            )],
        ),
    );
    let mut churned = churn_the_vault("churn-seeded", &workload);
    churned.judge(workload.changing().name(), 0);
    churned.assert_the_account_moved();
}

// ---------------------------------------------------------------------------
// The work bounds
// ---------------------------------------------------------------------------

/// A bound on one counted kind of maintenance, stated over the changed set.
///
/// **Both halves are read.** The ceiling is the claim — maintenance after an
/// edit costs the changed set and not the vault. The floor is what a pass costs
/// before any change is accounted for, and it is read the other way: a reading
/// the bound would admit for a changed set of *nothing* is a reading the churn
/// did not move, and no ceiling over it means anything.
///
/// **Every floor here is zero**, which is not an oversight but the measurement:
/// a host asked to maintain nothing opens nothing and writes nothing, so a pass
/// that reached a document did so because a change named it. A nonzero floor
/// would be fixed overhead admitted in advance, and the lower half of the
/// bracket would stop saying anything for a workload smaller than it.
#[derive(Clone, Copy)]
struct WorkBound {
    /// Which counter of the host's own account this is a bound on.
    ///
    /// Read where the account is readable, which is the lane the cost bars run
    /// in; the constants themselves are authored in every lane so that a bound
    /// is one edit rather than two.
    #[cfg_attr(not(feature = "induced-failure"), allow(dead_code))]
    counted: Counted,
    /// What is counted, named as it reads in a failure.
    what: &'static str,
    /// The work a maintenance pass costs before any change is accounted for.
    floor: u64,
    /// What each unit of the changed set adds to the ceiling.
    per_change: u64,
    /// Which of the workload's own accounts the changed set is read out of.
    #[cfg_attr(not(feature = "induced-failure"), allow(dead_code))]
    counted_in: ChangedSetIn,
    /// The same, as it reads in a failure. Prose, so the sentence a bound fails
    /// with names the input the way the case above it does.
    named_input: &'static str,
}

/// Which of a workload's own accounts a bound's changed set is read out of.
///
/// A field rather than a reading of [`WorkBound::named_input`], because the
/// prose is there to be read by a person: a bound that dispatched off the
/// wording of its own failure message would change what it counted when
/// somebody rephrased it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ChangedSetIn {
    /// Places where document content changed.
    Places,
    /// Steps the workload applied.
    Steps,
}

/// Which counter of a host's account a bound is stated over.
///
/// A name rather than a reader, because the reader is compiled only where the
/// account is readable and the bounds are authored in every lane: a bound
/// carrying a closure over a type that is not there would put every constant
/// behind the feature that reads it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Counted {
    /// Files opened for their content.
    DocumentOpens,
    /// Document rows written.
    DocumentsUpserted,
}

impl WorkBound {
    fn ceiling(&self, changes: u64) -> u64 {
        self.floor
            .saturating_add(self.per_change.saturating_mul(changes))
    }

    fn holds(&self, counted: u64, changes: u64) -> bool {
        counted <= self.ceiling(changes)
    }
}

/// **The bound on documents a maintenance pass opens**, over the workloads
/// whose changing phase names a handful of places.
///
/// Profile: `small`, 120 generated documents. Ordinary editing and atomic
/// replacement each name **4 places**; the ambiguity class's change names **2**.
/// The measured readings, taken with the phase's acts landing together and again
/// with 150ms between them so that each act is delivered on its own:
///
/// | workload | changed set | acts together | acts spaced |
/// |---|---|---|---|
/// | ordinary editing | 4 places | 2 opens | 2 opens |
/// | atomic replacement | 4 places | 3 opens | **4 opens** |
/// | ambiguity class | 2 places | 2 opens | — |
///
/// **The coefficient is two because the second open is a mechanism, not
/// headroom.** A host reads and hashes a dirty path *before* comparing the hash
/// against the row it holds, so a path delivered again once it is already
/// current costs one open and writes nothing — which is what the spaced atomic
/// reading is: a rename delivered as the name that emptied and the name that
/// filled, and a replacement delivered beside its own staging write. A
/// coefficient of one leaves that reading exactly at the ceiling with nothing
/// between the bound and the mechanism.
///
/// **What this refuses** is the changed set read a third time, and — through the
/// vault-fraction guard asserted beside every reading — any ceiling wide enough
/// to hide a re-read of the vault. Departures cost no open at all, so a workload
/// of nothing but prunes reads far under this however it is delivered.
const OPENS: WorkBound = WorkBound {
    counted: Counted::DocumentOpens,
    what: "documents opened",
    floor: 0,
    per_change: 2,
    counted_in: ChangedSetIn::Places,
    named_input: "places where document content changed",
};

/// **The bound on document rows a maintenance pass writes.**
///
/// Same profile and same named input, over ordinary editing's changing phase: 4
/// places, measured **2 upserts** with the acts landing together and **2** again
/// with 150ms between them.
///
/// **The coefficient stays at one where [`OPENS`]'s is two**, and the asymmetry
/// is the read-before-compare mechanism seen from the other side: a re-delivered
/// path is read and hashed again, and the hash it comes back with is the one the
/// row already holds, so the second delivery costs the open and writes nothing.
/// A row is upserted once per state its bytes reach, and a phase leaves each
/// place in one state.
///
/// **What the coefficient admits** is every changed place deriving a row, which
/// is the workload of pure edits; **what the slack absorbs** is the prunes this
/// particular workload spends half its changed set on, since a place that
/// emptied is in the changed set and upserts nothing.
const UPSERTS: WorkBound = WorkBound {
    counted: Counted::DocumentsUpserted,
    what: "document rows upserted",
    floor: 0,
    per_change: 1,
    counted_in: ChangedSetIn::Places,
    named_input: "places where document content changed",
};

/// **The bound on documents an external tool's batch opens**, over a workload
/// whose changing phase moves a whole directory.
///
/// Profile: `small`. The changing phase names **12 places** — the editor's
/// second save, the four documents the catch-up lands, the one it edits, the one
/// it removes, and both ends of every document the directory rename moves. The
/// measured readings are **6 opens** with the acts landing together and **11**
/// with 150ms between them.
///
/// It is stated apart from [`OPENS`] because the shape of its changed set is
/// different: half of these places are the two ends of one move, and a move is
/// read at the arriving end only. So a workload that renames a directory sits
/// under a ceiling stated per place, and stating it separately is what keeps
/// that room from being read as headroom the editing workloads also have.
///
/// **The coefficient is two for the same reason [`OPENS`]'s is**: a host reads
/// and hashes before comparing, so a place delivered again once it is current
/// costs an open and writes nothing. The spaced reading of 11 over 12 places
/// leaves one open of room under a coefficient of one, which is a bound sitting
/// on the mechanism rather than over it. What this refuses is the changed set
/// read a third time.
const TOOL_OPENS: WorkBound = WorkBound {
    counted: Counted::DocumentOpens,
    what: "documents opened",
    floor: 0,
    per_change: 2,
    counted_in: ChangedSetIn::Places,
    named_input: "places where document content changed",
};

/// **The bound on documents a burst opens**, whose changed set is counted in
/// steps rather than places.
///
/// Profile: `small`. Twelve edits to one standing row and six documents landing
/// at once are eighteen changes over seven places, and the bound is stated over
/// the eighteen: a host that coalesced them into one increment reads far under
/// this, and one that reacted to every edit reads over it. Coalescing is an
/// optimization, so the bound admits both. The measured readings are **13
/// opens** with the acts landing together and **19** with 150ms between them.
///
/// **The coefficient is two, and the spread between those two deliveries is
/// why.** Landing together, the twelve edits coalesce into a handful of reads.
/// Landing spaced apart, each edit is delivered on its own and the hammered path
/// is read once per delivery — plus one read that finds the row already current,
/// because a host reads and hashes before it compares. Nineteen opens over
/// eighteen steps is a reading *over* the changed set and not under it, which a
/// coefficient of one refuses: one re-read of a changed place is the mechanism
/// rather than a regression. **What this refuses** is the same set read a third
/// time.
const BURST_OPENS: WorkBound = WorkBound {
    counted: Counted::DocumentOpens,
    what: "documents opened",
    floor: 0,
    per_change: 2,
    counted_in: ChangedSetIn::Steps,
    named_input: "steps the workload applied",
};

/// **The arithmetic these bounds are stated in, checked against itself.**
///
/// Not a control over a host: nothing here attaches, and every number in it is
/// one this test chose. What it says is that [`WorkBound::holds`] means what the
/// bounds above read as — a reading at the ceiling passes, one past it fails,
/// and a bound tightened to admit nothing per change refuses a reading its
/// untightened self admitted.
///
/// **The controls that stand over a host are elsewhere.** That these counters
/// really move with the churn is `one_seeded_edit_moves_the_maintenance_account`'s
/// claim; that a ceiling never grows wide enough to admit re-reading the vault is
/// asserted in `assert_maintenance_is_bracketed`, beside every reading it takes.
#[test]
fn the_work_bound_arithmetic_admits_its_ceiling_and_refuses_one_past_it() {
    let changes = 8;
    for bound in [&OPENS, &UPSERTS, &TOOL_OPENS, &BURST_OPENS] {
        let ceiling = bound.ceiling(changes);
        assert!(
            bound.holds(ceiling, changes),
            "`{}` refuses a reading at its own ceiling",
            bound.what
        );
        assert!(
            !bound.holds(ceiling + 1, changes),
            "`{}` admits a reading past its ceiling for {changes} {}",
            bound.what,
            bound.named_input
        );
        let tightened = WorkBound {
            per_change: 0,
            ..*bound
        };
        assert!(
            !tightened.holds(ceiling, changes),
            "`{}` tightened to admit nothing per change still admits {ceiling}",
            bound.what
        );
    }
}

// ---------------------------------------------------------------------------
// Running a workload
// ---------------------------------------------------------------------------

/// When a workload's edits land, relative to the attachment that maintains
/// them.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum When {
    /// Before anything attaches, so the attach heal is what derives them.
    Before,
    /// After a host has reached `Ready` over the tree as it stands.
    Settled,
    /// While an attach heal is still running.
    DuringTheHeal,
}

/// One vault, churned by a workload and settled.
struct Churned {
    sandbox: Sandbox,
    vault: attach::Vault,
    second: Option<attach::Vault>,
    /// What the volume the tree sits on does with case, which is what makes a
    /// place a place.
    folding: Folding,
    census: Census,
    applied: Applied,
    /// Every place that held a document row before a phase and holds none
    /// after it, as its identity and the spelling the tree held it at.
    ///
    /// This is what the store owes a death for, read off the tree rather than
    /// off the store: a place that derived and stopped is a row that went,
    /// however it went — removed, renamed away, or left holding bytes no
    /// decoder accepts. A place a later phase puts a document back at is
    /// dropped again, because a resurrection owes nothing.
    owed_deaths: BTreeMap<String, String>,
    /// What the entry's trust state was when the last phase's first act landed,
    /// where that phase ran against a heal rather than after one.
    overlapped: Option<TrustState>,
    /// What the host's jobs spent maintaining the churn, and nothing for the
    /// attach heal that came before it.
    #[cfg(feature = "induced-failure")]
    maintenance: EvidenceReading,
}

fn sandbox(label: &str) -> Sandbox {
    Sandbox::new(Path::new(env!("CARGO_TARGET_TMPDIR")), label).expect("a sandbox")
}

/// Generate a tree, attach a host, and run `workload`'s two phases with a
/// settle between them.
///
/// The settle between the phases is what makes the second one a workload
/// against rows rather than against an empty store, and it is why a bracket
/// taken afterwards is a bracket on maintenance: the account and the census
/// this hands back are the changing phase's alone.
fn churn_the_vault(label: &str, workload: &churn::Phased) -> Churned {
    churn_the_vault_in(sandbox(label), workload)
}

/// The same, over a sandbox the caller already made — which is what a case that
/// had to ask the volume something before generating a tree needs.
fn churn_the_vault_in(sandbox: Sandbox, workload: &churn::Phased) -> Churned {
    attach_and_churn(sandbox, workload.opening(), When::Settled)
        .then(workload.changing(), When::Settled)
}

/// Generate a tree, attach a host, run one script against it, and settle.
///
/// The phase-by-phase way in, for the cases that state their own phases: a
/// workload landing before anything attaches, and one landing while a heal
/// runs.
fn attach_and_churn(sandbox: Sandbox, script: &Script, when: When) -> Churned {
    // Asked before the tree is generated and outside the vault, so the probe's
    // own file is never a change a host is asked to reconcile.
    let folding = declared_folding(&sandbox.work_dir());
    let vault = attach::Vault::generate(&sandbox.work_dir().join("attached"), PROFILE);
    let census = census(vault.path(), folding);
    let churned = Churned {
        sandbox,
        vault,
        second: None,
        folding,
        census,
        applied: Applied::default(),
        owed_deaths: BTreeMap::new(),
        overlapped: None,
        #[cfg(feature = "induced-failure")]
        maintenance: EvidenceReading::default(),
    };
    churned.then(script, when)
}

impl Churned {
    /// Run another workload against the same tree, and settle again.
    ///
    /// The account and the census come from the last run, so a case that opens
    /// with a state and then changes it bounds the change rather than the
    /// opening.
    fn then(mut self, script: &Script, when: When) -> Churned {
        let mut applied = Applied::default();
        let opened = self.census.clone();
        self.overlapped = None;
        if when == When::Before {
            apply(script, self.vault.path(), &mut applied);
            let host = self.vault.host();
            let lease = attach::attach_and_wait(&host, self.vault.name());
            self.retake_the_census(script, &opened, &applied);
            settle(&self.vault, &host, &self.census, &applied);
            drop(lease);
            self.applied = applied;
            // The account of the phase before this one is not this phase's, and
            // an attach heal's is not a changed set's, so nothing is carried
            // forward for a bracket to read.
            #[cfg(feature = "induced-failure")]
            {
                self.maintenance = EvidenceReading::default();
            }
            return self;
        }

        // A phase that means to land inside a heal takes the attachment that
        // caught one part-way through its walk, so the first act lands against
        // a tree the walk has already read some of. Every other phase attaches
        // and settles before anything is applied.
        let (host, demanded) = match when {
            When::DuringTheHeal => {
                let (host, demanded, caught) = caught_mid_walk(&self.vault);
                self.overlapped = Some(caught);
                (host, Some(demanded))
            }
            _ => (self.vault.host(), None),
        };
        let held = match when {
            When::Settled => Some(attach::attach_and_wait(&host, self.vault.name())),
            When::DuringTheHeal => None,
            When::Before => unreachable!("the pre-attach arm returned above"),
        };
        // The baseline for this phase's account. A phase landing inside a heal
        // reads it with that heal already part-way through, so what it hands
        // back is a floor on the work rather than a changed set's whole cost —
        // which is why the case that runs that way states no cost bar.
        #[cfg(feature = "induced-failure")]
        let before = host.evidence();
        apply(script, self.vault.path(), &mut applied);
        let waited = match when {
            When::DuringTheHeal => Some(attach::attach_and_wait(&host, self.vault.name())),
            _ => None,
        };
        self.retake_the_census(script, &opened, &applied);
        settle(&self.vault, &host, &self.census, &applied);
        #[cfg(feature = "induced-failure")]
        {
            self.maintenance = host.evidence().since(before);
        }
        drop(waited);
        drop(demanded);
        drop(held);
        self.applied = applied;
        self
    }

    /// **Read the tree again, and hold the phase to what it says.**
    ///
    /// Three claims stand between a phase's acts and the settle that follows
    /// them.
    ///
    /// The workload's own declaration of the places that derive no row is
    /// checked against the bytes on disk, which is [`Census`]'s claim.
    ///
    /// **A phase that spelled acts and left the tree reading exactly as it did
    /// before them asks a host to converge on nothing**, and every bar past
    /// this point would pass a host that did nothing at all. So a non-empty
    /// script whose census matches the one the phase opened against fails here,
    /// where the workload is still on hand to print, rather than being absorbed
    /// by an equivalence that holds because neither derivation moved. An empty
    /// script is exempt: a phase applying no acts is a re-attach over a settled
    /// tree, which is a thing one case does on purpose.
    ///
    /// And every place that held a row before the phase and holds none after it
    /// is a death the store owes, accumulated here for the claims below to
    /// read.
    fn retake_the_census(&mut self, script: &Script, opened: &Census, applied: &Applied) {
        self.census = census(self.vault.path(), self.folding);
        self.census
            .assert_the_script_read_the_tree_the_same_way(script);
        assert!(
            script.steps().is_empty() || self.census != *opened,
            "`{}` applied {} steps and the tree reads exactly as it did before them — same \
             places, same hashes, same schema — so this phase asks the host for nothing and \
             every claim after it holds over a vault nothing changed\n{applied}",
            script.name(),
            applied.steps()
        );
        self.owed_deaths
            .retain(|place, _| !self.census.rows.contains_key(place));
        for place in opened.rows.keys() {
            if !self.census.rows.contains_key(place) {
                self.owed_deaths
                    .insert(place.clone(), opened.spellings[place].clone());
            }
        }
    }

    /// **Every place that stopped deriving is recorded as a death.**
    ///
    /// The account a host's jobs write is a count, and a count still moves for
    /// a store that dropped the row and wrote no tombstone. This is the state
    /// beside it: the tombstone pillar is drained and each owed place is looked
    /// for in it by the spelling the tree held it at.
    ///
    /// **It is asked of the churned store alone**, and through the per-store
    /// enumerator rather than through the comparator. A store built from zero
    /// over the same final tree never saw these documents and records no death
    /// for them, which is exactly why deaths are outside the cross-store
    /// projection — see `norn_testkit::equivalence` for that ruling.
    fn assert_the_deaths_were_recorded(&self) {
        if self.owed_deaths.is_empty() {
            return;
        }
        let mut store = self.vault.store();
        let recorded: BTreeSet<String> = tombstones(&mut store)
            .expect("draining the tombstone pillar")
            .into_iter()
            .map(|death| death.path.as_str().to_string())
            .collect();
        let missing: Vec<&String> = self
            .owed_deaths
            .values()
            .filter(|at| !recorded.contains(*at))
            .collect();
        assert!(
            missing.is_empty(),
            "these places held a document row before the churn and hold none after it, and the \
             store records no death at them: {missing:?}\nthe pillar holds {recorded:?}\n{}",
            self.applied
        );
    }

    /// **The bar.** The churned store and a store built from zero over the same
    /// final tree hold the same derived facts, each is internally sound, and
    /// both stand on what the workload really left in the tree.
    ///
    /// `degraded` is how many documents kept a row and carry a finding beside
    /// it, which the census cannot count: it reads bytes and paths, and whether
    /// a frontmatter block was read is the text layer's answer.
    ///
    /// The deaths the churn owes are asked for first, because that claim is
    /// about the churned store alone and the comparison below is about the pair.
    fn judge(&mut self, subject: &str, degraded: usize) {
        self.assert_the_deaths_were_recorded();
        let second = self
            .vault
            .beside(&self.sandbox.work_dir().join("second-machine"));
        {
            let host = second.host();
            let lease = attach::attach_and_wait(&host, second.name());
            drop(lease);
        }

        let mut left = self.vault.store();
        let mut right = second.store();
        assert_operationally_valid(&mut left, &format!("{subject}: the churned derivation"));
        assert_operationally_valid(&mut right, &format!("{subject}: the derivation from zero"));

        let left = StoreProjection::read(&mut left).expect("projecting the churned store");
        let right =
            StoreProjection::read(&mut right).expect("projecting the store built from zero");

        let floor = self.floor(degraded);
        for (label, projection) in [
            ("the churned derivation", &left),
            ("the derivation from zero", &right),
        ] {
            projection.assert_population_at_least(&format!("{subject}: {label}"), floor);
        }
        left.assert_equivalent(&right, &format!("{subject}\n{}", self.applied));
        // Kept, because the class and finding claims a case makes after this
        // one are asked of both derivations.
        self.second = Some(second);
    }

    /// **The non-vacuity floor, stated from the workload's own final tree.**
    ///
    /// Every number comes off the census rather than out of a constant: the
    /// documents are the markdown places whose bytes derive, the findings are
    /// the places that derive none plus the documents that kept a row and lost
    /// their frontmatter, and the fact rows and indexed terms are one apiece —
    /// a tree of documents holds facts and its words are indexed. A workload
    /// whose whole point were emptiness would state zero here, and none of them
    /// is: every one of these churns a generated vault that keeps standing.
    fn floor(&self, degraded: usize) -> Population {
        let documents = self.census.rows.len();
        assert!(
            documents > 0,
            "the census found no markdown place holding a document, so every claim below is \
             vacuous:\n{}",
            self.applied
        );
        Population {
            documents,
            facts: documents,
            findings: self.census.without_rows.len() + degraded,
            indexed_terms: documents,
            vault_schema_pinned: true,
        }
    }

    /// **The flip landed at the identity it was made at**, and the tree and the
    /// store both say so.
    ///
    /// Three claims, because a volume that folds case answers a keyed read at
    /// either spelling and a store that did nothing would satisfy any one of
    /// them on its own.
    ///
    /// - **The directory renders the flipped spelling.** The name is read out
    ///   of the directory entry rather than probed for, because `exists` at
    ///   either spelling is what a folding volume answers whatever the rename
    ///   did.
    /// - **One row stands at the identity**, looked up by identity rather than
    ///   by spelling: the spelling is what differs between the two volumes and
    ///   this claim does not.
    /// - **That row holds bytes the opening phase did not put there**, which is
    ///   what says the changing phase reached the store at all.
    fn assert_the_flip_landed(&self, before_the_flip: &str) {
        let flipped = Path::new(churn::FLIPPED_TO)
            .file_name()
            .expect("the flipped name")
            .to_string_lossy()
            .into_owned();
        assert_eq!(
            dirent_spelling(self.vault.path(), churn::FLIPPED_TO),
            flipped,
            "the directory renders the flipped place at another spelling, so the rename this case \
             is about did not reach the tree"
        );

        let place = identity(churn::FLIPPED_TO, self.folding);
        let expected = self
            .census
            .rows
            .get(&place)
            .expect("the flipped name stands in the tree");
        let mut store = self.vault.store();
        let held: Vec<(String, String)> = derived_hashes(&mut store)
            .into_iter()
            .filter(|(path, _)| identity(path, self.folding) == place)
            .collect();
        assert_eq!(
            held.len(),
            1,
            "the flipped place holds {} rows: {held:?}",
            held.len()
        );
        assert_eq!(
            &held[0].1, expected,
            "the row at `{}` does not hold the bytes last written to the flipped name",
            held[0].0
        );
        assert_ne!(
            held[0].1, before_the_flip,
            "the row at `{}` holds the hash the opening phase settled on, so the changing phase's \
             acts reached neither the tree nor the store",
            held[0].0
        );
    }

    /// **The workload really landed inside the walk**, witnessed twice.
    ///
    /// The first reading is what the race aimed at: the entry's own published
    /// progress, read between the demand and the phase's first act, showing a
    /// heal already half way through the vault. `Ready` there would mean the
    /// heal finished before the churn began and `InstallingCoverage` would mean
    /// it had not started — either makes this the catch-up case wearing another
    /// name, with every claim it makes about concurrency vacuous.
    ///
    /// **The second reading is what the host did**, and it is the one that
    /// cannot be arranged by a lucky sample. A walk passes a place once, so a
    /// row the walk enumerated and found standing is a row only a watcher
    /// report can take away afterwards — and the store records which of the two
    /// took each row. Deaths carrying nothing but `heal-prune` are a phase that
    /// landed wholly ahead of the walk, whatever the entry was publishing at
    /// the time; a `watcher-removal` among them is the phase having landed
    /// behind the walk's own cursor, which is as close to "inside the walk" as
    /// anything outside a host can witness.
    fn assert_the_workload_overlapped_a_heal(&self) {
        let observed = self
            .overlapped
            .as_ref()
            .expect("this case ran a phase against an active heal");
        assert!(
            past_the_workloads_places(observed),
            "the entry was {observed:?} when the workload's first act landed, so the heal this \
             case means to churn against had not walked past the places the workload names\n{}",
            self.applied
        );

        let mut store = self.vault.store();
        let deaths = tombstones(&mut store).expect("draining the tombstone pillar");
        assert!(
            deaths
                .iter()
                .any(|death| death.provenance == Provenance::WatcherRemoval),
            "every death this churn produced was the heal's own prune — {:?} — so the walk had \
             not yet reached the places the workload changed and this phase landed ahead of it \
             rather than inside it\n{}",
            deaths
                .iter()
                .map(|death| (death.path.as_str(), death.provenance))
                .collect::<Vec<(&str, Provenance)>>(),
            self.applied
        );
    }

    /// The churned store holds `hash` at `path`.
    fn assert_derived_hash(&self, path: &str, hash: &str) {
        let mut store = self.vault.store();
        let held = store
            .begin_request()
            .stored_document(&DocumentPath::new(path).expect("a document path"))
            .expect("reading a document")
            .map(|document| document.content_hash);
        assert_eq!(
            held.as_deref(),
            Some(hash),
            "`{path}` does not hold the bytes the workload last wrote there"
        );
    }

    /// Both derivations, so a case can ask each of them the same question.
    fn both_derivations(&self) -> Vec<(&'static str, &attach::Vault)> {
        let mut pair = vec![("the churned derivation", &self.vault)];
        if let Some(second) = self.second.as_ref() {
            pair.push(("the derivation from zero", second));
        }
        pair
    }
}

/// The evidence-reading half, which is compiled where its readers are.
#[cfg(feature = "induced-failure")]
impl Churned {
    /// **The bracket.** The counter `read` names is under the bound's ceiling
    /// for this workload's changed set, and over the floor it would sit at had
    /// nothing changed.
    fn assert_maintenance_is_bracketed(&self, bound: &WorkBound) {
        let changes = self.changed_set(bound) as u64;
        eprintln!(
            "MEASURE {} over {changes}: {:?}",
            bound.what, self.maintenance
        );
        let counted = match bound.counted {
            Counted::DocumentOpens => self.maintenance.document_opens,
            Counted::DocumentsUpserted => self.maintenance.documents_upserted,
        };
        assert!(
            bound.holds(counted, changes),
            "{} came to {counted} over {changes} {}, and the bound is {} + {} × {changes} = \
             {}\n{}\n{:?}",
            bound.what,
            bound.named_input,
            bound.floor,
            bound.per_change,
            bound.ceiling(changes),
            self.applied,
            self.maintenance
        );
        assert!(
            !bound.holds(counted, 0),
            "{} came to {counted}, which the bound admits for a changed set of nothing — so this \
             counter is not what the churn moved\n{}\n{:?}",
            bound.what,
            self.applied,
            self.maintenance
        );
        // A bound that admitted re-reading the vault would not be a changed-set
        // bound however well the reading fits under it — and a ceiling at most
        // of the vault is barely narrower than one at all of it. The guard is
        // therefore stated as a fraction: a ceiling past half the documents is
        // a bound a whole-vault pass could hide inside twice over.
        let vault = norn_fixtures::Profile::by_name(PROFILE)
            .expect("the profile this suite churns")
            .docs as u64;
        assert!(
            bound.ceiling(changes) < vault / 2,
            "the ceiling on {} for {changes} {} is {}, which is over half the {vault} documents \
             of `{PROFILE}` and so admits most of a whole-vault re-read",
            bound.what,
            bound.named_input,
            bound.ceiling(changes)
        );
    }

    /// **The phase acted on rows the host was holding**, as many of them as the
    /// tree says it did.
    ///
    /// A prune is the one thing a workload cannot get by appearing: a place
    /// nothing stood at derives a row and writes no death, so a reading of no
    /// deletions and no tombstones says the whole workload reached the host as
    /// "these files are new" however many removals and renames its script
    /// spells. Every case that states this runs a phase whose script takes a
    /// document away, moves one, or leaves one holding bytes no decoder
    /// accepts, and each of those is a row the phase before it settled over.
    ///
    /// **The count is the tree's**, read off the two censuses as the places
    /// that derived before the phase and derive nothing after it. A bare
    /// "something died" passes a host that recorded one death and dropped the
    /// other three, which is the same defect at a smaller scale.
    ///
    /// The comparison is `>=` rather than `==` in both counters. A directory
    /// removal reaches a host as the documents beneath it, a rename reaches it
    /// as two places, and a host is free to spend more deaths reconciling one
    /// place than the tree spells — what this refuses is spending fewer.
    fn assert_rows_were_taken_away(&self) {
        let owed = self.owed_deaths.len() as u64;
        assert!(
            owed > 0,
            "this case states that rows were taken away, and every place holding a document row \
             before the churn holds one after it\n{}",
            self.applied
        );
        assert!(
            self.maintenance.documents_deleted >= owed
                && self.maintenance.tombstones_recorded >= owed,
            "{owed} places stopped deriving — {:?} — and the account records {} deletions and {} \
             tombstones: {:?}\n{}",
            self.owed_deaths.values().collect::<Vec<&String>>(),
            self.maintenance.documents_deleted,
            self.maintenance.tombstones_recorded,
            self.maintenance,
            self.applied
        );
    }

    /// **The seeded-work control.** Something the churn did is in the account.
    fn assert_the_account_moved(&self) {
        assert!(
            self.maintenance.document_opens > 0
                && self.maintenance.documents_upserted > 0
                && self.maintenance.changesets_applied > 0,
            "the churn landed and the host's account records nothing: {:?}\n{}",
            self.maintenance,
            self.applied
        );
        assert_eq!(
            (
                self.maintenance.recoveries_run,
                self.maintenance.rebuilds_run
            ),
            (0, 0),
            "ordinary churn climbed a rung of the heal ladder: {:?}",
            self.maintenance
        );
    }

    /// How many units of the changed set this bound is stated over.
    fn changed_set(&self, bound: &WorkBound) -> usize {
        match bound.counted_in {
            ChangedSetIn::Places => self.applied.places().len(),
            ChangedSetIn::Steps => self.applied.steps(),
        }
    }
}

/// The same claims, in a build whose readers are not compiled.
#[cfg(not(feature = "induced-failure"))]
impl Churned {
    fn assert_maintenance_is_bracketed(&self, _: &WorkBound) {}

    fn assert_the_account_moved(&self) {}

    fn assert_rows_were_taken_away(&self) {}
}

/// **Attach, and catch the heal part-way through its walk.**
///
/// A walk over this profile takes milliseconds, so catching one is a race
/// rather than a wait — and this runs the race until it wins. An attempt whose
/// entry reaches `Ready` before it was ever read far enough into the walk is
/// let go and made again over the same tree, which costs an attachment and
/// changes nothing: the workload has not been applied yet, so a lost race is a
/// re-attach rather than a churn run twice.
///
/// **The look is a tight one.** Consecutive published readings are tens of
/// microseconds apart, and a poll that backed off between them would sample
/// past the whole walk — which is why this is not one of the suite's budgeted
/// waits.
fn caught_mid_walk(
    vault: &attach::Vault,
) -> (
    attach::ServingHost,
    DemandLease<ProductionEntryOps>,
    TrustState,
) {
    let deadline = Instant::now() + CATCHING_A_HEAL;
    let mut attempts = 0;
    loop {
        attempts += 1;
        let host = vault.host();
        let demanded = host
            .demand(vault.name(), AttachMode::Durable)
            .expect("request attachment");
        loop {
            let observed = host.state(vault.name());
            match &observed {
                Ok(state) if past_the_workloads_places(state) => {
                    return (host, demanded, state.clone());
                }
                Ok(TrustState::Ready) => break,
                _ => {}
            }
            assert!(
                Instant::now() < deadline,
                "no attach heal walked far enough to churn against inside {CATCHING_A_HEAL:?} \
                 over {attempts} attachments; the entry is {observed:?}"
            );
            std::hint::spin_loop();
        }
        drop(demanded);
        drop(host);
        assert!(
            Instant::now() < deadline,
            "{attempts} attachments reached `Ready` without ever being read part-way through \
             their walk, inside {CATCHING_A_HEAL:?}"
        );
    }
}

/// Whether this reading is a heal that has already walked past the places the
/// churn workloads write to.
///
/// **A walk passes a place once.** An act landing before the walk reaches its
/// path is an act the walk itself absorbs — which is the catch-up shape, not
/// the concurrent one — and an act landing after it is one only a watcher
/// report can reconcile. The workloads here write under a single directory the
/// walk reaches inside its first dozen documents, so a heal that has counted
/// half the vault is past every place they name.
///
/// This is what the race aims at. What says it hit is the provenance the store
/// recorded its deaths under, which
/// [`Churned::assert_the_workload_overlapped_a_heal`] reads.
fn past_the_workloads_places(state: &TrustState) -> bool {
    let vault = norn_fixtures::Profile::by_name(PROFILE)
        .expect("the profile this suite churns")
        .docs as u64;
    matches!(
        state,
        TrustState::Warming {
            phase: WarmingPhase::Healing,
            healed,
            ..
        } if healed * 2 >= vault
    )
}

/// The spelling the directory holding `at` renders it at.
///
/// Read out of the directory entry rather than probed for, because a volume
/// that folds case answers a keyed read at either spelling: what says a rename
/// re-spelled a place is the name the directory hands back, and nothing else
/// on such a volume can.
fn dirent_spelling(root: &Path, at: &str) -> String {
    let full = root.join(at);
    let directory = full.parent().expect("a document sits in a directory");
    let folded = full
        .file_name()
        .expect("a document has a name")
        .to_string_lossy()
        .to_ascii_lowercase();
    let mut rendered: Vec<String> = std::fs::read_dir(directory)
        .unwrap_or_else(|e| panic!("reading {}: {e}", directory.display()))
        .map(|entry| {
            entry
                .expect("a directory entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .filter(|held| held.to_ascii_lowercase() == folded)
        .collect();
    assert_eq!(
        rendered.len(),
        1,
        "the directory holding `{at}` renders {rendered:?} for that name"
    );
    rendered.pop().expect("the one spelling")
}

fn apply(script: &Script, root: &Path, applied: &mut Applied) {
    script
        .apply_range(root, 0..script.steps().len(), applied)
        .unwrap_or_else(|problem| panic!("{problem}\n{script}"));
}

/// **The settle.** Wait until the derived store agrees with the tree about
/// which places hold documents and what bytes they hold.
///
/// The condition is coarse on purpose — paths, content hashes and the pinned
/// schema, which is the cheapest thing that says the host caught up — and what
/// is judged afterwards is everything a store holds. A wait on the whole
/// judgment would be the bar waiting for itself.
///
/// **A settle that runs out says what the workload did.** The steps that really
/// ran are printed beside the disagreement, because a wait failing on a hash at
/// a path names one place and the script names the acts that put every place in
/// the state the host is behind on.
fn settle(vault: &attach::Vault, host: &attach::ServingHost, census: &Census, applied: &Applied) {
    let mut store = vault.store();
    let budget = SETTLING.budget_for(applied.steps());
    wait_until(
        "the derived store to agree with the tree the workload left",
        budget,
        || {
            let state = host.state(vault.name());
            if state != Ok(TrustState::Ready) {
                return Observed::pending(format!("the entry is {state:?}"));
            }
            match census.disagreement(&mut store) {
                None => Observed::Met(()),
                Some(why) => Observed::pending(why),
            }
        },
    )
    .unwrap_or_else(|failure| panic!("{failure}\n{applied}"));
}

// ---------------------------------------------------------------------------
// The census
// ---------------------------------------------------------------------------

/// What the tree holds, read as places rather than as documents.
///
/// A place is a markdown file a walk reads. Whether it derives a row is decided
/// here the same way a heal decides it: a name the document-path grammar
/// refuses derives none, bytes no decoder accepts derive none, and everything
/// else derives one holding the hash of the bytes on disk.
///
/// **A place is keyed by its identity, not by its spelling.** On a volume that
/// folds case, `Note.md` and `note.md` are one place, and a census comparing the
/// two renderings byte for byte would call a document that never moved a place
/// with no row standing beside a row standing nowhere. Whether two derivations
/// agree about the *spelling* a document is rendered at is the equivalence
/// comparator's question, asked of the whole projection rather than of this
/// coarse signal.
///
/// **What keying by identity costs is duplicates.** Two derived rows whose
/// spellings fold together collapse into one entry here, so a store holding
/// both would look to this wait exactly like a store holding the right one. That
/// is a limit of the signal and not a gap in the suite: the equivalence
/// comparator reads every row of both projections, and the case that flips a
/// name's case asks directly how many rows stand at the flipped identity — and
/// asks the directory itself what spelling it renders there.
///
/// **Two readings are compared for equality**, which is how a phase that
/// applied acts and moved nothing is caught: the places, the hashes, the places
/// that derive none and the vault's schema declaration all take part, because
/// each of them is something a changing phase may be the only mover of.
#[derive(Clone, Debug, Eq, PartialEq)]
struct Census {
    /// What the volume does with case, which is what makes two spellings one
    /// place or two.
    folding: Folding,
    /// The identity of each place, to the hash the bytes there imply.
    rows: BTreeMap<String, String>,
    /// The identity of each place that derives no row.
    without_rows: BTreeSet<String>,
    /// Each identity's spelling on disk, which is what a failure names.
    spellings: BTreeMap<String, String>,
    /// The vault's own schema declaration, as the tree holds it.
    ///
    /// **A schema replacement changes no path and no hash**, so a store that
    /// has not yet taken it agrees with every other part of this reading. A
    /// settle that stopped there would let a case go on to drop the host with
    /// the re-pin still pending — which discards it — and fail downstream as a
    /// flake about findings nothing re-derived.
    schema: Option<Vec<u8>>,
}

/// A path as its identity on a volume with this case behavior.
///
/// The fold is ASCII, which is the fold the derived store's own path ordering
/// uses: a suite folding more than the store does would call two places one that
/// the store keeps apart.
fn identity(path: &str, folding: Folding) -> String {
    match folding {
        Folding::Folded => path.to_ascii_lowercase(),
        Folding::Distinct => path.to_string(),
    }
}

impl Census {
    /// **The workload's declaration and the tree agree.** Every place the script
    /// said derives no row is a place this reading of the tree also finds
    /// derives none.
    ///
    /// The declaration is the driver's, made where the workload is written, and
    /// this reading is made from the bytes on disk afterwards. They are two
    /// answers to one question, so a workload that meant to leave a quarantined
    /// place and left a readable one is caught here rather than passing a bar
    /// about a state it never reached.
    fn assert_the_script_read_the_tree_the_same_way(&self, script: &Script) {
        for declared in script.places_without_rows() {
            let place = identity(declared, self.folding);
            assert!(
                self.without_rows.contains(&place),
                "`{}` says `{declared}` derives no row, and the tree there does derive one",
                script.name()
            );
        }
    }

    /// How the store disagrees with the tree, and nothing where they agree.
    ///
    /// Every disagreement is reported rather than the first, because the two
    /// halves of one defect read as two lines: a row at a spelling the tree no
    /// longer holds and a place with no row are the same rename seen from each
    /// end, and a message naming only one of them sends a reader looking for a
    /// document that moved rather than for the move.
    fn disagreement(&self, store: &mut Store) -> Option<String> {
        /// How many disagreements one message carries. A workload that
        /// diverged everywhere says so in the count.
        const REPORTED: usize = 8;

        let derived: BTreeMap<String, (String, String)> = derived_hashes(store)
            .into_iter()
            .map(|(path, hash)| (identity(&path, self.folding), (path, hash)))
            .collect();
        let mut apart = Vec::new();
        for (place, hash) in &self.rows {
            let at = &self.spellings[place];
            match derived.get(place) {
                Some((_, held)) if held == hash => {}
                Some((_, held)) => {
                    apart.push(format!("`{at}` holds {held} and the tree holds {hash}"));
                }
                None => apart.push(format!("`{at}` stands in the tree and holds no row")),
            }
        }
        for place in &self.without_rows {
            if derived.contains_key(place) {
                apart.push(format!(
                    "`{}` derives no document and holds a row",
                    self.spellings[place]
                ));
            }
        }
        for (place, (spelling, _)) in &derived {
            if !self.rows.contains_key(place) {
                apart.push(format!(
                    "`{spelling}` holds a row and stands nowhere in the tree"
                ));
            }
        }
        if let Some(declared) = self.schema.as_ref() {
            let pinned = store
                .begin_request()
                .vault_schema_pin()
                .expect("reading the pinned vault schema");
            match pinned {
                Some(pin) if &pin.bytes == declared => {}
                Some(_) => apart.push(
                    "the store pins bytes the vault's schema declaration no longer holds"
                        .to_string(),
                ),
                None => {
                    apart.push("the vault declares a schema and the store pins none".to_string())
                }
            }
        }
        if apart.is_empty() {
            return None;
        }
        let total = apart.len();
        apart.truncate(REPORTED);
        Some(format!("{total} disagreements: {}", apart.join("; ")))
    }
}

/// Read the tree at `root` as places, keyed by identity on a volume with this
/// case behavior.
fn census(root: &Path, folding: Folding) -> Census {
    let mut census = Census {
        folding,
        rows: BTreeMap::new(),
        without_rows: BTreeSet::new(),
        spellings: BTreeMap::new(),
        schema: std::fs::read(root.join(".norn/schema.yaml")).ok(),
    };
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        let entries = std::fs::read_dir(&directory)
            .unwrap_or_else(|e| panic!("reading {}: {e}", directory.display()));
        for entry in entries {
            let entry = entry.expect("a directory entry");
            let path = entry.path();
            let kind = entry.file_type().expect("an entry's type");
            // A symbolic link is not a place: a walk refuses to follow one, so
            // a `.md` link derives nothing however it resolves.
            if kind.is_symlink() {
                continue;
            }
            if kind.is_dir() {
                // Norn's own subtree carries the schema declaration and the
                // mechanism scratch root, and no document.
                if path.file_name() != Some(std::ffi::OsStr::new(".norn")) {
                    pending.push(path);
                }
                continue;
            }
            let Some(relative) = markdown_place(root, &path) else {
                continue;
            };
            let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("reading {relative}: {e}"));
            let place = identity(&relative, folding);
            let derives =
                DocumentPath::new(&relative).is_ok() && std::str::from_utf8(&bytes).is_ok();
            if derives {
                census
                    .rows
                    .insert(place.clone(), ContentHash::of(&bytes).to_string());
            } else {
                census.without_rows.insert(place.clone());
            }
            census.spellings.insert(place, relative);
        }
    }
    census
}

/// The vault-relative spelling of `path`, where it is a markdown file.
fn markdown_place(root: &Path, path: &Path) -> Option<String> {
    if path.extension() != Some(std::ffi::OsStr::new("md")) {
        return None;
    }
    let relative = path.strip_prefix(root).ok()?;
    Some(relative.to_string_lossy().into_owned())
}

/// Every derived path and the hash the row holds.
///
/// The page loop is the attach fixture's, which every suite in this crate reads
/// a whole vault through: the bound on one page is the point of it, and one
/// place to state it is one place to keep it bounded.
fn derived_hashes(store: &mut Store) -> BTreeMap<String, String> {
    let mut held = BTreeMap::new();
    attach::for_each_derived_document(store, |document| {
        held.insert(
            document.path.as_str().to_string(),
            document.content_hash.clone(),
        );
    });
    held
}

/// The kinds of every finding standing at `path`, sorted.
fn kinds_at(projection: &StoreProjection, path: &str) -> Vec<String> {
    let mut kinds: Vec<String> = projection
        .findings()
        .iter()
        .filter(|finding| finding.path == path)
        .map(|finding| finding.kind.clone())
        .collect();
    kinds.sort();
    kinds
}

/// **The platform lane, declared.** What the volume under `at` does with case,
/// asked two ways and required to agree.
fn declared_folding(at: &Path) -> Folding {
    let probed = churn::folding(at).expect("a case probe over the sandbox");
    let normalizer = PathNormalizer::detect(at).expect("a normalizer over the sandbox");
    let resolved = match normalizer.case_sensitivity() {
        CaseSensitivity::Insensitive => Folding::Folded,
        CaseSensitivity::Sensitive => Folding::Distinct,
    };
    assert_eq!(
        probed, resolved,
        "a probe that wrote a file and looked for it under another spelling says this is {probed}, \
         and the normalizer a host resolves paths with says it is {resolved}"
    );
    eprintln!("the churn suite is running against {probed}");
    probed
}
