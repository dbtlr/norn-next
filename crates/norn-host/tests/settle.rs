//! **How long rung 1 takes**, measured for every churn family the
//! certification inventory carries.
//!
//! The churn suite beside this one states that a live attachment converges on
//! what a build from zero holds. It states nothing about *when*: its settle
//! budgets are runaway bounds and say so in their own words, and a runaway
//! bound is a bound on being stuck rather than a bound on being slow. This file
//! is the clock, and it is the scheduled lane's because a clock is — under
//! [ADR 0004](../../../docs/decisions/0004-ci-lanes-divide-by-evidence-kind.md),
//! counts and structural assertions gate a pull request and clocks and trends
//! belong to soak.
//!
//! # The two readings
//!
//! Both run from one origin: **the instant the workload's changing phase
//! applied its final act**. From there the case reads
//!
//! - **to equivalence** — when the derived store first holds exactly what the
//!   final tree implies, which is the state the equivalence bar is then taken
//!   over; and
//! - **to `Ready`** — when the attachment first publishes `Ready` at or after
//!   that.
//!
//! **A `Ready` before the store has caught up is not a reading.** The entry was
//! already publishing `Ready` when the change landed, and a poll cannot tell
//! that label apart from one a host republished after reconciling — so the
//! earliest `Ready` that can mean "the host is caught up" is one taken at or
//! after the store is. That ordering is by construction here, which is why the
//! second reading is never below the first.
//!
//! **Both readings are conservative by construction.** Each is sampled after
//! the observation that decided it rather than before, so the cost of the look
//! is inside the reading; the tree is read once after the final act and inside
//! the clock as well. And each is late by at most one poll gap, which the
//! shared cadence tops out at 50 ms. What that buys is a reading that never
//! flatters the subject: an authored ceiling over these numbers is a ceiling
//! over readings that include their own instrument.
//!
//! # What makes the second reading a reading of *equivalence*
//!
//! Nothing writes to the tree after the final act, so the store is quiescent
//! once it has converged — the instant it first agreed with the tree is the
//! state it is still in when the bar is taken. So the case converges, stops the
//! clock, lets the host go, derives the same tree a second time from zero
//! through machine-local directories that hold no row of the first, and
//! compares the two projections field by field. An equivalence that fails is a
//! failed case, not a slow one: the reading above it was a reading of something
//! other than convergence.
//!
//! # The scale
//!
//! The families are authored over the `small` profile in `churn.rs`, and that
//! is the scale this measures at by default. Whether maintenance costs the
//! changed set rather than the vault is the work bars' claim over there, not
//! this one's. `NORN_SETTLE_PROFILE` is what a calibration dispatch names
//! another profile through, and the lane names the profile explicitly so the
//! scale a recorded reading was taken at is workflow text rather than a
//! default.
//!
//! Each generated tree sits in a testkit sandbox, which is a unix-only harness.
#![cfg(unix)]
#![allow(clippy::disallowed_methods)] // Harness scaffolding: this suite's own generated tree.

mod attach;
mod baselines;

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::time::{Duration, Instant};

use norn_fs::ContentHash;
use norn_store::{DocumentPath, Store};
use norn_testkit::churn::{self, Applied, Folding, Phased, Script};
use norn_testkit::equivalence::{StoreProjection, assert_operationally_valid};
use norn_testkit::process::Sandbox;
use norn_testkit::wait::{Convergence, Observed, wait_until};
use norn_wire::TrustState;

/// The variable naming the generated profile the families are churned over.
const PROFILE_ENV: &str = "NORN_SETTLE_PROFILE";

/// The profile the churn families are authored over, which is what this
/// measures unless a run names another.
const DEFAULT_PROFILE: &str = "small";

/// The runaway bound on one settle.
///
/// **Not a bar.** The bar is [`baselines::SOAK_SETTLE_CEILING`], and it is the
/// only thing here that judges a duration. Reaching this bound is a host that
/// stopped converging rather than one that took too long, so it is wide enough
/// that no reading a ceiling would ever be authored over comes near it: the
/// floor covers an attach heal over the widest profile a run may name, and the
/// per-change allowance is what a wide workload adds to it.
const CONVERGING: Convergence = Convergence::new(
    Duration::from_secs(300),
    Duration::from_secs(10),
    Duration::from_secs(30),
);

/// **The clock on rung 1**, over every churn family.
///
/// One vault per family, so a family's reading is a settle over a tree nothing
/// else has been churning. Every reading is recorded whatever the ceiling is,
/// and the comparison happens only where one is authored — the calibration
/// state the ledger types every such run non-qualifying under.
#[test]
#[ignore = "soak-lane case: runs in the nightly soak lane, not the workspace suite"]
fn every_churn_family_settles_inside_the_ceiling() {
    baselines::assert_the_profile_the_bars_were_authored_on();
    let profile = declared_profile();

    let readings: Vec<(Family, Settle)> = Family::ALL
        .iter()
        .map(|family| (*family, family.settle(&profile)))
        .collect();

    let mut rows: Vec<(String, String)> = vec![
        ("profile".to_string(), profile.clone()),
        (
            "settle ceiling (ms)".to_string(),
            match baselines::SOAK_SETTLE_CEILING {
                Some(ceiling) => baselines::milliseconds(ceiling),
                None => "unauthored".to_string(),
            },
        ),
    ];
    for (family, settle) in &readings {
        rows.push((
            format!("{}: to equivalence (ms)", family.name()),
            baselines::milliseconds(settle.equivalent),
        ));
        rows.push((
            format!("{}: to Ready (ms)", family.name()),
            baselines::milliseconds(settle.ready),
        ));
    }
    let rendered: Vec<(&str, String)> = rows
        .iter()
        .map(|(label, value)| (label.as_str(), value.clone()))
        .collect();
    baselines::record("churn settle", &rendered);

    let Some(ceiling) = baselines::SOAK_SETTLE_CEILING else {
        return;
    };
    for (family, settle) in &readings {
        // Both readings are held against the one ceiling. `ready` dominates
        // `equivalent` by construction, so the second comparison is what really
        // decides the run — the first is what keeps a future edit that stopped
        // taking the `Ready` reading from silently leaving the bar unevaluated.
        for (what, reading) in [
            ("hold what the tree implies", settle.equivalent),
            ("publish `Ready` again", settle.ready),
        ] {
            assert!(
                baselines::fits(reading, ceiling),
                "`{}` took {} ms to {what} after its final change, past the {} ms ceiling, over \
                 the `{profile}` profile",
                family.name(),
                baselines::milliseconds(reading),
                baselines::milliseconds(ceiling)
            );
        }
    }
}

/// **Every bar the ledger names is held to the baseline it names.**
///
/// [`norn_testkit::certification::ledger::NAMED_EXIT_BARS`] is what stamps a
/// run's record non-qualifying while a named exit bar is unauthored, and the
/// values it makes those claims about live in this crate's baselines file. Two
/// files, one fact — so this holds them together: un-authoring a ceiling
/// without disarming the registry, or the reverse, fails a pull request rather
/// than letting a calibration run stamp itself qualifying.
///
/// **It is exhaustive over this crate's bars.** A registry entry pointing into
/// this crate's baselines that nothing here names fails, because a bar the
/// registry claims and nobody checks is a claim that drifts quietly. Entries
/// pointing at another crate's baselines are that crate's to hold.
#[test]
fn the_ledgers_armed_claims_match_the_authored_baselines() {
    const HERE: &str = "crates/norn-host/tests/baselines/mod.rs::";

    let mut held = 0;
    for bar in norn_testkit::certification::ledger::NAMED_EXIT_BARS {
        let Some(constant) = bar.authored_at.strip_prefix(HERE) else {
            continue;
        };
        let authored = match constant {
            "SOAK_PEAK_RSS_CEILING_BYTES" => baselines::SOAK_PEAK_RSS_CEILING_BYTES.is_some(),
            "SOAK_SETTLE_CEILING" => baselines::SOAK_SETTLE_CEILING.is_some(),
            // Not an `Option`: there is no state in which it is unauthored, and
            // the registry's own sweep holds it to `armed`.
            "FD_BUDGET" => true,
            other => panic!(
                "the ledger names `{other}` in this crate's baselines and nothing here holds its \
                 armed claim to the constant, so the two may drift apart quietly"
            ),
        };
        assert_eq!(
            bar.armed,
            authored,
            "the ledger claims `{}` is {} and `{constant}` reads otherwise",
            bar.name,
            if bar.armed { "armed" } else { "unarmed" }
        );
        held += 1;
    }
    assert!(
        held > 0,
        "no exit bar names this crate's baselines file, so this test holds nothing"
    );
}

/// **The negative control.** A reading past a ceiling is refused by the
/// comparison every measurement bar in this crate makes.
///
/// The bars themselves cannot state this: a passing run says only that its
/// reading fit, and a comparison that had stopped refusing would report exactly
/// that. So the comparison is fed a reading past a ceiling of each shape it is
/// asked about — the bytes the peak-resident-set bars read, the duration the
/// settle bar reads, and the count the descriptor budget reads — and a reading
/// at the ceiling is required to pass beside each one, because a bar states the
/// most a subject may cost and costing exactly that is not costing more.
#[test]
fn a_reading_past_a_ceiling_is_refused_by_the_comparison_every_bar_makes() {
    let bytes = 40 * 1024 * 1024u64;
    assert!(baselines::fits(bytes, bytes));
    assert!(!baselines::fits(bytes + 1, bytes));

    let duration = Duration::from_secs(30);
    assert!(baselines::fits(duration, duration));
    assert!(!baselines::fits(
        duration + Duration::from_nanos(1),
        duration
    ));

    let count = baselines::FD_BUDGET;
    assert!(baselines::fits(count, count));
    assert!(!baselines::fits(count + 1, count));
}

/// What one family's settle cost, from its final change.
#[derive(Clone, Copy, Debug)]
struct Settle {
    /// To the derived store holding what the final tree implies.
    equivalent: Duration,
    /// To the attachment publishing `Ready` at or after that.
    ready: Duration,
}

/// One churn family, named as the inventory and the architecture name it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Family {
    OrdinaryEditing,
    AtomicReplacement,
    CaseFlip,
    CaseRenamedParent,
    Burst,
    ValidityTransitions,
    ExternalTools,
}

impl Family {
    /// Every family the inventory's churn cases are workloads of.
    ///
    /// The inventory's remaining churn entries are the same workloads delivered
    /// differently — before anything attaches, against an active heal — or
    /// claims about the account rather than about convergence, and a settle
    /// measured over them would be a second reading of the workload above it.
    const ALL: &'static [Family] = &[
        Family::OrdinaryEditing,
        Family::AtomicReplacement,
        Family::CaseFlip,
        Family::CaseRenamedParent,
        Family::Burst,
        Family::ValidityTransitions,
        Family::ExternalTools,
    ];

    /// The name a reading and a failure carry, which is the family's own.
    fn name(&self) -> &'static str {
        match self {
            Family::OrdinaryEditing => "ordinary editing",
            Family::AtomicReplacement => "atomic replacement and movement",
            Family::CaseFlip => "a case flip",
            Family::CaseRenamedParent => "a case-renamed parent over a save",
            Family::Burst => "a burst",
            Family::ValidityTransitions => "validity transitions",
            Family::ExternalTools => "an external tool's catch-up",
        }
    }

    /// The label the family's sandbox is named for.
    fn label(&self) -> &'static str {
        match self {
            Family::OrdinaryEditing => "settle-ordinary",
            Family::AtomicReplacement => "settle-atomic",
            Family::CaseFlip => "settle-case-flip",
            Family::CaseRenamedParent => "settle-case-rename-parent",
            Family::Burst => "settle-burst",
            Family::ValidityTransitions => "settle-validity",
            Family::ExternalTools => "settle-tools",
        }
    }

    /// The workload, seeded the way `churn.rs` seeds it so that the tree this
    /// times a settle over is the tree that suite judges convergence over.
    fn workload(&self, folding: Folding, oversized: &[u8]) -> Phased {
        match self {
            Family::OrdinaryEditing => churn::ordinary_editing(41),
            Family::AtomicReplacement => churn::atomic_replacement(43),
            Family::CaseFlip => churn::case_flip(73, folding),
            Family::CaseRenamedParent => churn::case_rename_parent(79, folding),
            Family::Burst => churn::burst(47),
            Family::ValidityTransitions => churn::validity_transitions(53, oversized),
            Family::ExternalTools => churn::external_tools(59),
        }
    }

    /// **One family's measurement.** Settle the opening phase, apply the
    /// changing phase, time the two readings from its final act, and then take
    /// the equivalence bar over what the clock stopped on.
    fn settle(&self, profile: &str) -> Settle {
        let sandbox =
            Sandbox::new(Path::new(env!("CARGO_TARGET_TMPDIR")), self.label()).expect("a sandbox");
        // Asked before the tree is generated and outside the vault, so the
        // probe's own file is never a change a host is asked to reconcile.
        let folding = churn::folding(&sandbox.work_dir()).expect("a case probe over the sandbox");
        let workload = self.workload(folding, &oversized_frontmatter());
        let vault = attach::Vault::generate(&sandbox.work_dir().join("attached"), profile);

        let reading = {
            let host = vault.host();
            let lease = attach::attach_and_wait(&host, vault.name());
            let mut store = vault.store();

            let opened = self.phase(&vault, &host, &mut store, workload.opening()).1;
            let (reading, changed) = self.phase(&vault, &host, &mut store, workload.changing());
            assert_ne!(
                opened,
                changed,
                "`{}` applied its changing phase and the tree reads exactly as it did before it, \
                 so this reading is a settle over a host that was asked for nothing",
                workload.changing().name()
            );
            drop(store);
            drop(lease);
            drop(host);
            reading
        };

        self.assert_it_converged_on_a_build_from_zero(&sandbox, &vault);
        reading
    }

    /// Apply one phase and settle over it, timing from its final act.
    ///
    /// The clock starts the instant `apply` returns, which is the instant the
    /// last act landed. Everything after it — the walk that reads the tree the
    /// phase left, and every poll — is inside the reading.
    fn phase(
        &self,
        vault: &attach::Vault,
        host: &attach::ServingHost,
        store: &mut Store,
        script: &Script,
    ) -> (Settle, Tree) {
        let mut applied = Applied::default();
        script
            .apply_range(vault.path(), 0..script.steps().len(), &mut applied)
            .unwrap_or_else(|problem| panic!("{problem}\n{script}"));
        let since = Instant::now();
        let tree = read_the_tree(vault.path());
        assert!(
            !tree.rows.is_empty(),
            "`{}` left a tree holding no document at all, so a settle over it converges on \
             nothing\n{applied}",
            script.name()
        );

        let mut equivalent = None;
        let ready = wait_until(
            &format!(
                "the derived store and the entry to catch up with `{}`",
                script.name()
            ),
            CONVERGING.budget_for(applied.steps()),
            || {
                if equivalent.is_none() {
                    // Read first, sampled after: the cost of the look is inside
                    // the reading rather than shaved off it.
                    if let Some(why) = tree.disagreement(store) {
                        return Observed::pending(why);
                    }
                    equivalent = Some(since.elapsed());
                }
                let state = host.state(vault.name());
                let elapsed = since.elapsed();
                if state == Ok(TrustState::Ready) {
                    Observed::Met(elapsed)
                } else {
                    Observed::pending(format!("the entry is {state:?}"))
                }
            },
        )
        .unwrap_or_else(|failure| panic!("{failure}\n{applied}"));

        (
            Settle {
                equivalent: equivalent.expect("the store agreed before the entry was read ready"),
                ready,
            },
            tree,
        )
    }

    /// **The bar the reading stands on.** What the clock stopped over is what a
    /// derivation from zero over the same final tree holds.
    ///
    /// Nothing writes to the tree after the final act, so the store the second
    /// reading was taken at is the store compared here. An equivalence that
    /// fails says the clock stopped on something other than convergence, which
    /// is a failed case rather than a slow one.
    fn assert_it_converged_on_a_build_from_zero(&self, sandbox: &Sandbox, vault: &attach::Vault) {
        let second = vault.beside(&sandbox.work_dir().join("second-machine"));
        {
            let host = second.host();
            let lease = attach::attach_and_wait(&host, second.name());
            drop(lease);
        }

        let mut left = vault.store();
        let mut right = second.store();
        assert_operationally_valid(
            &mut left,
            &format!("{}: the settled derivation", self.name()),
        );
        assert_operationally_valid(
            &mut right,
            &format!("{}: the derivation from zero", self.name()),
        );
        let left = StoreProjection::read(&mut left).expect("projecting the settled store");
        let right =
            StoreProjection::read(&mut right).expect("projecting the store built from zero");
        left.compare(&right).assert_equal(&format!(
            "{}: the settle this case timed did not converge on a build from zero",
            self.name()
        ));
    }
}

/// What the tree holds, as the places a walk reads and the rows their bytes
/// imply.
///
/// The rule is a heal's: a name the document-path grammar refuses derives no
/// row, bytes no decoder accepts derive none, and everything else derives one
/// holding the hash of the bytes on disk.
///
/// **It is keyed by the spelling the tree renders**, where `churn.rs`'s census
/// keys by identity. The two answer different questions: that one waits for a
/// coarse agreement it then judges finely, and this one stops a clock. A row
/// left standing at the spelling a case flip moved away from is a store that
/// has not finished converging, and a comparison that folded the two spellings
/// together would stop the clock while it still stood.
#[derive(Clone, Debug, Eq, PartialEq)]
struct Tree {
    /// Each place that derives a row, to the hash its bytes imply.
    rows: BTreeMap<String, String>,
    /// Each place that derives no row.
    without_rows: BTreeSet<String>,
}

impl Tree {
    /// How the store disagrees with the tree, and nothing where they agree.
    fn disagreement(&self, store: &mut Store) -> Option<String> {
        /// How many disagreements one message carries.
        const REPORTED: usize = 8;

        let mut derived = BTreeMap::new();
        attach::for_each_derived_document(store, |document| {
            derived.insert(
                document.path.as_str().to_string(),
                document.content_hash.clone(),
            );
        });

        let mut apart = Vec::new();
        for (at, hash) in &self.rows {
            match derived.get(at) {
                Some(held) if held == hash => {}
                Some(held) => apart.push(format!("`{at}` holds {held} and the tree holds {hash}")),
                None => apart.push(format!("`{at}` stands in the tree and holds no row")),
            }
        }
        for at in &self.without_rows {
            if derived.contains_key(at) {
                apart.push(format!("`{at}` derives no document and holds a row"));
            }
        }
        for at in derived.keys() {
            if !self.rows.contains_key(at) {
                apart.push(format!("`{at}` holds a row and stands nowhere in the tree"));
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

/// Read the tree at `root` as the places a walk reads.
fn read_the_tree(root: &Path) -> Tree {
    let mut tree = Tree {
        rows: BTreeMap::new(),
        without_rows: BTreeSet::new(),
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
            if path.extension() != Some(std::ffi::OsStr::new("md")) {
                continue;
            }
            let Ok(relative) = path.strip_prefix(root) else {
                continue;
            };
            let at = relative.to_string_lossy().into_owned();
            let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("reading {at}: {e}"));
            if DocumentPath::new(&at).is_ok() && std::str::from_utf8(&bytes).is_ok() {
                tree.rows.insert(at, ContentHash::of(&bytes).to_string());
            } else {
                tree.without_rows.insert(at);
            }
        }
    }
    tree
}

/// The profile the families are churned over, as the environment declares it.
fn declared_profile() -> String {
    let Some(declared) = std::env::var_os(PROFILE_ENV) else {
        return DEFAULT_PROFILE.to_string();
    };
    let declared = declared.to_string_lossy().trim().to_string();
    assert!(
        norn_fixtures::Profile::by_name(&declared).is_some(),
        "{PROFILE_ENV} names `{declared}`, which is not a generated profile"
    );
    declared
}

/// A document whose frontmatter block is past the bound the text layer reads,
/// which is the state the validity family crosses out of and back into.
fn oversized_frontmatter() -> Vec<u8> {
    let mut block = String::from("a: ");
    while block.len() + 1 < norn_text::FRONTMATTER_MAX_BYTES * 2 {
        block.push('[');
    }
    block.push('\n');
    format!("---\n{block}---\n# body\n").into_bytes()
}
