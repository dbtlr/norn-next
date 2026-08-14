//! **Rung 1 of the heal ladder, reached by ordinary operation.** A real host
//! maintains a vault while somebody else edits it, and what it converges on is
//! compared with a derivation that started from zero over the same final tree.
//!
//! Every case here has the same three-part shape.
//!
//! - **A workload runs against a live tree.** The scripts are
//!   `norn_testkit::churn`'s: seeded, ordered, and each step saying in words what
//!   it does, so a failure prints the workload rather than a stack of writes. The
//!   host is a production attachment with a real platform watcher, so what
//!   reaches it is what reaches a host on somebody's machine — no injected
//!   failure, no seam a test reached through.
//! - **The tree is settled, deterministically.** The wait is on a census: every
//!   markdown place in the tree either has the document row its bytes imply, or
//!   is one of the places the workload declared derives none. Nothing here sleeps
//!   for a fixed time and calls that convergence, and the budget is a runaway
//!   bound rather than a bar — how *long* a settle takes is a clock, and clocks
//!   belong to the scheduled lane.
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
//! The real-watcher lease is machine-wide and not reentrant, so the churn host
//! is dropped before the comparison derivation attaches. Every case here is
//! therefore two attachments in sequence over one tree.
//!
//! Each generated tree sits in a testkit sandbox, which is a unix-only harness.
#![cfg(unix)]
#![allow(clippy::disallowed_methods)] // Harness scaffolding: this suite's own generated tree.

mod attach;

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::time::Duration;

use norn_fs::{CaseSensitivity, ContentHash, PathNormalizer};
use norn_host::AttachMode;
#[cfg(feature = "induced-failure")]
use norn_host::EvidenceReading;
use norn_store::{DocumentPath, Store, StoredPathOrder, class_probe};
use norn_testkit::churn::{self, Act, Applied, Folding, Script, Step};
use norn_testkit::equivalence::{Population, StoreProjection, assert_operationally_valid};
use norn_testkit::process::Sandbox;
use norn_testkit::wait::{Convergence, Observed, wait_until};
use norn_wire::{FindingKind, TrustState};

/// The profile every case churns over.
///
/// 120 documents, shaped like a real vault — ambiguity classes, dangling links,
/// unicode and spaced stems, clutter that is not a document, and symbolic links
/// a walk refuses to follow. What each case adds on top is its own workload, so
/// the vault around the churn is the same in every one of them.
///
/// **The scale is chosen so that a changed-set bound can mean something.** Every
/// workload here names well under ten places, and each cost bar asserts that its
/// own ceiling is below the document count of this profile — a claim that
/// maintenance costs the changed set rather than the vault is not a claim at all
/// over a vault a bound could re-read and still fit. It is also small enough
/// that the two attachments each case makes cost a fraction of a second, which
/// is what keeps this suite in the per-PR lane.
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
#[test]
fn ordinary_editing_converges_on_a_build_from_zero() {
    let script = churn::ordinary_editing(41);
    let mut churned = churn_the_vault("churn-ordinary", &script, When::Settled);

    churned.judge("ordinary editing across nested directories", 0);
    churned.assert_maintenance_is_bracketed(&OPENS);
    churned.assert_maintenance_is_bracketed(&UPSERTS);
}

/// **Family 2.** Content landing whole at a name, and names moving between
/// directories.
///
/// A rename is two changed places — the name that emptied and the name that
/// filled — and a document that moves twice ends up somewhere no walk has ever
/// enumerated it before. The case flip is the case below, because what a flip
/// means is the volume's own answer.
#[test]
fn atomic_replacement_and_movement_converge_on_a_build_from_zero() {
    let sandbox = sandbox("churn-atomic");
    let script = churn::atomic_replacement(43);
    let mut churned = churn_the_vault_in(sandbox, &script, When::Settled);

    churned.judge("atomic replacement and movement", 0);
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
#[test]
fn a_case_flip_converges_on_a_build_from_zero() {
    let sandbox = sandbox("churn-case-flip");
    let folding = churn::folding(&sandbox.work_dir()).expect("a case probe over the sandbox");
    let script = churn::case_flip(73, folding);
    let mut churned = churn_the_vault_in(sandbox, &script, When::Settled);
    assert_eq!(
        folding, churned.folding,
        "two probes of one volume disagree"
    );
    churned.assert_the_flip_landed();

    if folding == Folding::Folded {
        churned = churned.then(&Script::new("nothing at all", Vec::new()), When::Settled);
    }
    churned.judge(&format!("a case flip on {folding}"), 0);
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
#[test]
fn a_burst_converges_on_the_last_bytes_written() {
    let script = churn::burst(47);
    let mut churned = churn_the_vault("churn-burst", &script, When::Settled);

    // The absolute claim beside the relative one: the hammered path holds the
    // last edit's bytes and none of the eleven writes before them.
    let last = churned
        .census
        .rows
        .get("churn/burst/hammered.md")
        .expect("the hammered path stands in the tree")
        .clone();
    churned.judge("a burst against one path and a burst across many", 0);
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
/// **No work bound.** A schema re-pin is a vault-scope act by contract: the
/// findings it discards stand at places no row does, and only a walk of the
/// whole vault re-files them. A changed-set bound over this workload would be a
/// bound against that contract.
#[test]
fn documents_crossing_validity_boundaries_converge_on_a_build_from_zero() {
    let oversized = oversized_frontmatter();
    let script = churn::validity_transitions(
        53,
        churn::SchemaGround {
            at: ".norn/schema.yaml",
            replacement: REPLACEMENT_SCHEMA,
        },
        &oversized,
    );
    let mut churned = churn_the_vault("churn-validity", &script, When::Settled);

    // Two documents stand past the frontmatter read bound at the end — the one
    // the workload pushed past it — and each carries a finding beside the row
    // it kept.
    churned.judge("transitions between readable, quarantined and degraded", 1);

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
/// by a tool renaming the directory they all sit in.
#[test]
fn an_external_tools_catch_up_converges_on_a_build_from_zero() {
    let script = churn::external_tools(59);
    let mut churned = churn_the_vault("churn-tools", &script, When::Settled);

    churned.judge("what external tools do to a vault", 0);
    churned.assert_maintenance_is_bracketed(&OPENS);
}

/// **Family 5, second shape.** The same edits, landing while the attach heal is
/// still running.
///
/// The demand is placed and the workload runs against the tree at once, so the
/// walk that establishes the attachment is reading a vault somebody else is
/// still writing to. Whether any one edit really lands inside the window between
/// that walk's enumeration and its opens is not something a black-box case can
/// force — the case that stages that window exactly is in `norn-host`'s own unit
/// suite, where the walk and the merge are two calls. What this one says is the
/// convergent claim: however the edits interleaved with the heal, the store ends
/// up holding what a build from zero over the final tree holds.
#[test]
fn edits_during_an_active_heal_converge_on_a_build_from_zero() {
    let script = churn::external_tools(61);
    let mut churned = churn_the_vault("churn-during-heal", &script, When::DuringTheHeal);

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
#[test]
fn an_ambiguity_classs_membership_change_converges_on_a_build_from_zero() {
    let mut ink = churn::Ink::new(67);
    let script = Script::new(
        "an ambiguity class gaining and losing members",
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
            Step::new(
                "write a third document into the same class",
                Act::Write {
                    at: "churn/class/three/shared.md".to_string(),
                    bytes: ink.document("shared, three"),
                },
            ),
            Step::new(
                "take the second one out of the class again",
                Act::Remove {
                    at: "churn/class/two/shared.md".to_string(),
                },
            ),
        ],
    );
    let mut churned = churn_the_vault("churn-class", &script, When::Settled);
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
    // The name is asked for outside the vault first, because a filesystem that
    // will not hold it makes this case unrunnable rather than failing: the name
    // is the whole subject, and a workload that could not write it would judge
    // a tree with no collision in it.
    let probe = sandbox.work_dir().join("bad\\name.probe");
    if let Err(problem) = std::fs::write(&probe, b"a name") {
        eprintln!(
            "skipped: this filesystem does not create `{}`: {problem}",
            probe.display()
        );
        return;
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
        churn_the_vault_in(sandbox, &script, When::Before).then(&clearing, When::Settled);
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
/// The negative control the bounds above stand on. One document is edited after
/// the vault has settled, and the account a host's jobs write has to show it:
/// a counter that read the same before and after would make every bracket above
/// a statement about a number nothing drives, and the upper half of each bracket
/// would pass a host that did no maintenance at all.
#[test]
fn one_seeded_edit_moves_the_maintenance_account() {
    let mut ink = churn::Ink::new(71);
    let script = Script::new(
        "one document, edited once",
        vec![Step::new(
            "write one document",
            Act::Write {
                at: "churn/seeded/one.md".to_string(),
                bytes: ink.document("one"),
            },
        )],
    );
    let mut churned = churn_the_vault("churn-seeded", &script, When::Settled);
    churned.judge("one document, edited once", 0);
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
    /// Which named input the changed set is counted in.
    named_input: &'static str,
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

/// **The bound on documents a maintenance pass opens**, over a workload whose
/// changed set is counted in places.
///
/// Profile: `small`, 120 generated documents, churned by workloads naming five
/// to nine places. The measured readings are 6 opens over 5 places and 18 over
/// 9, so the coefficient sits at roughly twice what a converged host spends:
/// it admits a place being opened more than once, which is what a host that
/// split one workload across several increments does.
const OPENS: WorkBound = WorkBound {
    counted: Counted::DocumentOpens,
    what: "documents opened",
    floor: 0,
    per_change: 4,
    named_input: "places the workload named",
};

/// **The bound on document rows a maintenance pass writes.**
///
/// Same profile and same named input. The coefficient is smaller than the one
/// above because a place is opened before it is judged and written only if it
/// derives — a removal upserts nothing — and because a host that coalesces a
/// workload writes each surviving document once however many times it was
/// edited. The measured reading is 2 upserts over 5 places.
const UPSERTS: WorkBound = WorkBound {
    counted: Counted::DocumentsUpserted,
    what: "document rows upserted",
    floor: 0,
    per_change: 3,
    named_input: "places the workload named",
};

/// **The bound on documents a burst opens**, whose changed set is counted in
/// steps rather than places.
///
/// Profile: `small`. Twelve edits to one path and six documents landing at once
/// are eighteen changes over seven places, and the bound is stated over the
/// eighteen: a host that coalesced them into one increment reads far under this,
/// and one that ran an increment per edit reads near it. Coalescing is an
/// optimization, so the bound admits both. The measured reading is 27 opens over
/// 18 steps.
const BURST_OPENS: WorkBound = WorkBound {
    counted: Counted::DocumentOpens,
    what: "documents opened",
    floor: 0,
    per_change: 4,
    named_input: "steps the workload applied",
};

/// **The bound's own negative control**, over the arithmetic rather than a host.
///
/// A bound is only worth stating if a reading can fail it, and the two ways one
/// can are stated here: a counter one over the ceiling for its changed set, and
/// the same counter against a bound tightened to admit nothing per change.
#[test]
fn a_work_bound_refuses_work_that_outgrew_its_changed_set() {
    let changes = 8;
    for bound in [&OPENS, &UPSERTS, &BURST_OPENS] {
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
    /// After the host has reached `Ready` over the tree as generated.
    Settled,
    /// While the attach heal is still running.
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
    /// What the host's jobs spent maintaining the churn, and nothing for the
    /// attach heal that came before it.
    #[cfg(feature = "induced-failure")]
    maintenance: EvidenceReading,
}

fn sandbox(label: &str) -> Sandbox {
    Sandbox::new(Path::new(env!("CARGO_TARGET_TMPDIR")), label).expect("a sandbox")
}

/// Generate a tree, attach a host, run `script` against it, and settle.
fn churn_the_vault(label: &str, script: &Script, when: When) -> Churned {
    churn_the_vault_in(sandbox(label), script, when)
}

/// The same, over a sandbox the caller already made — which is what a case that
/// had to ask the volume something before generating a tree needs.
fn churn_the_vault_in(sandbox: Sandbox, script: &Script, when: When) -> Churned {
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
        if when == When::Before {
            apply(script, self.vault.path(), &mut applied);
            let host = self.vault.host();
            let lease = attach::attach_and_wait(&host, self.vault.name());
            self.census = census(self.vault.path(), self.folding);
            self.census
                .assert_the_script_read_the_tree_the_same_way(script);
            settle(&self.vault, &host, &self.census, applied.steps());
            drop(lease);
            self.applied = applied;
            return self;
        }

        let host = self.vault.host();
        let held = match when {
            When::Settled => Some(attach::attach_and_wait(&host, self.vault.name())),
            When::DuringTheHeal => None,
            When::Before => unreachable!("the pre-attach arm returned above"),
        };
        #[cfg(feature = "induced-failure")]
        let before = host.evidence();
        let demanded = match when {
            // The demand is placed and the workload runs at once, so the walk
            // that establishes the attachment reads a tree somebody else is
            // still writing to.
            When::DuringTheHeal => Some(
                host.demand(self.vault.name(), AttachMode::Durable)
                    .expect("request attachment"),
            ),
            _ => None,
        };
        apply(script, self.vault.path(), &mut applied);
        let waited = match when {
            When::DuringTheHeal => Some(attach::attach_and_wait(&host, self.vault.name())),
            _ => None,
        };
        self.census = census(self.vault.path(), self.folding);
        self.census
            .assert_the_script_read_the_tree_the_same_way(script);
        settle(&self.vault, &host, &self.census, applied.steps());
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

    /// **The bar.** The churned store and a store built from zero over the same
    /// final tree hold the same derived facts, each is internally sound, and
    /// both stand on what the workload really left in the tree.
    ///
    /// `degraded` is how many documents kept a row and carry a finding beside
    /// it, which the census cannot count: it reads bytes and paths, and whether
    /// a frontmatter block was read is the text layer's answer.
    fn judge(&mut self, subject: &str, degraded: usize) {
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

    /// **The flip landed at the identity it was made at.** One row stands for
    /// the flipped place, and it holds the bytes that were last written there.
    ///
    /// The row is looked up by identity rather than by spelling, because the
    /// spelling is what differs between the two volumes and the claim does not:
    /// on either of them the vault holds one document there, and it holds the
    /// last thing written to it.
    fn assert_the_flip_landed(&self) {
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
        // bound however well the reading fits under it.
        let vault = norn_fixtures::Profile::by_name(PROFILE)
            .expect("the profile this suite churns")
            .docs as u64;
        assert!(
            bound.ceiling(changes) < vault,
            "the ceiling on {} for {changes} {} is {}, which admits re-reading all {vault} \
             documents of `{PROFILE}`",
            bound.what,
            bound.named_input,
            bound.ceiling(changes)
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
        if bound.named_input.starts_with("steps") {
            self.applied.steps()
        } else {
            self.applied.places().len()
        }
    }
}

/// The same claims, in a build whose readers are not compiled.
#[cfg(not(feature = "induced-failure"))]
impl Churned {
    fn assert_maintenance_is_bracketed(&self, _: &WorkBound) {}

    fn assert_the_account_moved(&self) {}
}

fn apply(script: &Script, root: &Path, applied: &mut Applied) {
    script
        .apply_range(root, 0..script.steps().len(), applied)
        .unwrap_or_else(|problem| panic!("{problem}\n{script}"));
}

/// **The settle.** Wait until the derived store agrees with the tree about
/// which places hold documents and what bytes they hold.
///
/// The condition is coarse on purpose — paths and content hashes, which is the
/// cheapest thing that says the host caught up — and what is judged afterwards
/// is everything a store holds. A wait on the whole judgment would be the bar
/// waiting for itself.
fn settle(vault: &attach::Vault, host: &attach::ServingHost, census: &Census, changes: usize) {
    let mut store = vault.store();
    let budget = SETTLING.budget_for(changes);
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
    .unwrap_or_else(|failure| panic!("{failure}"));
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
#[derive(Clone, Debug)]
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

/// Every derived path and the hash the row holds, read a bounded page at a
/// time.
fn derived_hashes(store: &mut Store) -> BTreeMap<String, String> {
    /// How many rows one page asks for: well under the page bound the store
    /// accepts, so reading a vault is many bounded pages and never one wide one.
    const PAGE: usize = 64;

    let request = store.begin_request();
    let mut held = BTreeMap::new();
    let mut after: Option<DocumentPath> = None;
    loop {
        let page = request
            .stored_documents_after_ordered(after.as_ref(), PAGE, StoredPathOrder::Sensitive)
            .expect("reading a page of derived documents");
        let Some(last) = page.last() else {
            return held;
        };
        after = Some(last.path.clone());
        for document in page {
            held.insert(
                document.path.as_str().to_string(),
                document.content_hash.clone(),
            );
        }
    }
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
