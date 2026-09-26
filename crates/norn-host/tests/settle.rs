//! **How long rung 1 takes**, measured over every leg of every workload family
//! the churn driver's roll carries.
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
//! # The reading is one duration
//!
//! **From the instant a phase's final act landed to the instant the derived
//! store first holds what a build from zero over that tree holds.** That is the
//! whole clock, and it is the only duration this file records.
//!
//! The stopping condition is the comparator the equivalence bar is taken over,
//! not a cheaper stand-in for it. A census of paths and content hashes is not
//! that comparator: one flush is a changeset plus the findings recorded after
//! it, each in its own transaction, so at the instant every path and hash
//! agrees the flush's findings are provably not yet recorded and the store is
//! not yet equivalent. A clock stopped there would systematically under-report
//! by the findings-maintenance tail — which is the tail the validity family
//! exists to drive.
//!
//! **So the poll is two-stage, and the reading is taken at the second stage.**
//! Each poll first asks the cheap census whether the paths and hashes agree,
//! because reading the whole projection of a ≥5k vault at a 50 ms cadence would
//! measure this instrument rather than the subject. Once they do, the poll
//! reads the full [`StoreProjection`] — documents, frontmatter, bodies,
//! headings, blocks, tags, links, indexed terms, **findings** and the pinned
//! schema — and the confirming read is the first such read that the next poll
//! finds unchanged. What the clock stopped on is then held to the projection
//! the equivalence bar is taken over, so "the store had reached its final
//! state" is an assertion rather than an argument from quiescence.
//!
//! # Where the clock stops, and what bounds the error
//!
//! **The reading is taken at the instant the confirming read began, not the
//! instant it returned.** A read observes the store as of its start, so a read
//! that returns the settled state says the settle had already happened when
//! that read began. The time the read itself takes is the instrument
//! materialising a ≥5k-document projection — at the `soak` profile that is
//! several hundred milliseconds, enough to dominate the subject — and charging
//! it to the subject would make the ceiling a bar on
//! [`StoreProjection::read`].
//!
//! **What is left is a resolution, not a bias.** The settle lies somewhere
//! between the start of the last poll that found the store unsettled and the
//! instant the confirming read began; nothing here samples in between. The
//! reading is the top of that window, so it never under-reports — and how wide
//! the window was is **measured per leg and recorded beside the reading**,
//! rather than assumed from the cadence. It is a census read plus a sleep: the
//! sleep is what [`norn_testkit::wait::LONGEST_POLL_GAP`] tops out, and the
//! census read grows with the vault, so at the ≥5k profile the window is wider
//! than the cadence alone would say. A ceiling authored over these numbers is
//! authored against the resolutions recorded with them.
//!
//! **A leg whose resolution equals its reading was settled before the
//! instrument first looked.** That leg's first poll found the store already
//! agreeing, so no poll ever saw it unsettled and the window is the whole
//! elapsed time: the reading bounds the settle from above and does not measure
//! it. At the ≥5k profile every leg currently reads that way — the census of a
//! ≥5k tree takes longer than the settle it is looking for — which is why the
//! legs there read within a fifth of each other while the same legs at `small`
//! spread more than tenfold. The readings are sound as bounds, and a ceiling
//! authored over them bounds what they bound.
//!
//! # `Ready` is a property, not a second clock
//!
//! The entry is already publishing `Ready` when the final change lands, and
//! ordinary churn never takes that away: a poll therefore cannot time a
//! *transition* to `Ready`, and a duration to it would be the cost of one
//! `state()` call rather than a fact about the subject. So the second term is
//! recorded as what it is — a boolean per leg, sampled at the poll that
//! confirmed the reading: **the attachment is publishing `Ready` where the
//! store has reached equivalence**, which says the churn the family applied
//! withdrew no trust. It is not a claim that the entry held `Ready` at every
//! instant in between; nothing here samples between polls.
//!
//! # Every leg of every family
//!
//! The families and their seeds are `norn_testkit::churn`'s roll, which is the
//! same table the churn suite runs, so a reading here is a reading over the
//! workload that suite judges. Every leg the roll carries is timed, including
//! family 4's third phase: replacing the vault's declaration re-pins it,
//! discards every finding keyed by the old fingerprint and heals the whole
//! vault under the new one, which is the widest settle the roll can ask for. A
//! control file reaches a host only when a reload asks for it, so that leg's
//! clock starts when the reload returns — the act that delivers the change.
//!
//! # The scale
//!
//! The scheduled lane names the `soak` profile, which is the ≥5k-document scale
//! every other soak bar in the workspace is authored over. Every leg reads
//! higher there than at the 120-document `small` profile the families are
//! authored at, and by different multiples per leg — but the two bands are not
//! the same subject read at two scales, because at ≥5k every leg is
//! floor-bound in the sense above. So the comparison says only that a ceiling
//! calibrated at `small` would sit far below the scale it gates — and because
//! a bar admits a reading only at or under it, such a ceiling would fail every
//! scheduled run rather than pass it. That is the reason the lane names the
//! profile: the scale the number was taken at has to be the scale it judges. `NORN_SETTLE_PROFILE` names the profile, the lane
//! sets it explicitly so the scale a recorded reading was taken at is workflow
//! text, and a local run defaults to `small` because a developer is not
//! calibrating.
//!
//! Each generated tree sits in a testkit sandbox, which is a unix-only harness.
#![cfg(unix)]
#![allow(clippy::disallowed_methods)] // Harness scaffolding: this suite's own generated tree.

mod attach;
mod baselines;
mod tree;

use std::path::Path;
use std::time::{Duration, Instant};

use norn_host::ReloadRefusal;
use norn_store::Store;
use norn_testkit::certification::ledger::{ExitBar, NAMED_EXIT_BARS};
use norn_testkit::churn::{Applied, Family, Folding, Script};
use norn_testkit::equivalence::{StoreProjection, assert_operationally_valid};
use norn_testkit::process::Sandbox;
use norn_testkit::wait::{Convergence, Observed, wait_until};
use norn_wire::{TrustState, VaultName};

use tree::{Census, census};

/// The variable naming the generated profile the families are churned over.
const PROFILE_ENV: &str = "NORN_SETTLE_PROFILE";

/// The profile a run measures at where none is named, which is the scale the
/// churn families are authored over. The scheduled lane names `soak` instead.
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

/// **The clock on rung 1**, over every leg of every churn family.
///
/// One vault per family, so a family's reading is a settle over a tree nothing
/// else has been churning. **Each family's readings are recorded as they
/// land**, before the next family starts and before anything is judged: the
/// readings are the step's product whether or not a bar refuses one, and a run
/// whose last family times out must still leave the six behind it.
#[test]
#[ignore = "soak-lane case: runs in the nightly soak lane, not the workspace suite"]
fn every_churn_family_settles_inside_the_ceiling() {
    baselines::assert_the_profile_the_bars_were_authored_on();
    let profile = declared_profile();
    baselines::record(
        "churn settle",
        &[
            ("profile", profile.clone()),
            (
                "settle ceiling (ms)",
                match baselines::SOAK_SETTLE_CEILING {
                    Some(ceiling) => baselines::milliseconds(ceiling),
                    None => "unauthored".to_string(),
                },
            ),
            // The sleep half of the poll gap. The other half is the census
            // read, which grows with the vault — so each leg's measured
            // resolution is recorded beside its reading rather than derived
            // from this.
            (
                "poll sleep cap (ms)",
                baselines::milliseconds(norn_testkit::wait::LONGEST_POLL_GAP),
            ),
        ],
    );

    let mut readings: Vec<(String, Settle)> = Vec::new();
    for family in Family::ALL {
        let measured = measure(*family, &profile);
        let rows: Vec<(String, String)> = measured
            .iter()
            .flat_map(|(leg, settle)| {
                [
                    (
                        format!("{leg}: to equivalence (ms)"),
                        baselines::milliseconds(settle.equivalent),
                    ),
                    (
                        format!("{leg}: resolution (ms)"),
                        baselines::milliseconds(settle.resolution),
                    ),
                    (
                        format!("{leg}: Ready at equivalence"),
                        if settle.ready { "yes" } else { "no" }.to_string(),
                    ),
                ]
            })
            .collect();
        let rendered: Vec<(&str, String)> = rows
            .iter()
            .map(|(label, value)| (label.as_str(), value.clone()))
            .collect();
        baselines::record(&format!("churn settle: {}", family.name()), &rendered);
        readings.extend(measured);
    }

    // Judged after every reading is recorded, so the first leg that fails a bar
    // does not take the readings behind it with it.
    for (leg, settle) in &readings {
        assert!(
            settle.ready,
            "`{leg}` reached equivalence {} ms after its final change and the attachment was not \
             publishing `Ready` there, so the churn withdrew trust the settle never gave back",
            baselines::milliseconds(settle.equivalent)
        );
    }
    let Some(ceiling) = baselines::SOAK_SETTLE_CEILING else {
        return;
    };
    for (leg, settle) in &readings {
        assert!(
            baselines::fits(settle.equivalent, ceiling),
            "`{leg}` took {} ms to hold what a build from zero holds after its final change, past \
             the {} ms ceiling, over the `{profile}` profile",
            baselines::milliseconds(settle.equivalent),
            baselines::milliseconds(ceiling)
        );
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
/// pointing at another crate's baselines name constants with no unauthored
/// state, and the registry's own sweep holds every one of those to `armed`.
#[test]
fn the_ledgers_armed_claims_match_the_authored_baselines() {
    const HERE: &str = "crates/norn-host/tests/baselines/mod.rs::";

    /// A constant that is not an `Option` is authored in every build there is.
    /// Naming it here binds the arm to the constant, so moving or renaming one
    /// fails to compile rather than leaving the claim standing over nothing.
    fn always_authored<T>(_bar: T) -> bool {
        true
    }

    let mut held = 0;
    for bar in norn_testkit::certification::ledger::NAMED_EXIT_BARS {
        let Some(constant) = bar.authored_at.strip_prefix(HERE) else {
            continue;
        };
        let authored = match constant {
            "SOAK_PEAK_RSS_CEILING_BYTES" => baselines::SOAK_PEAK_RSS_CEILING_BYTES.is_some(),
            "SOAK_HIGH_WATER_RSS_CEILING_BYTES" => {
                baselines::SOAK_HIGH_WATER_RSS_CEILING_BYTES.is_some()
            }
            "SOAK_SETTLE_CEILING" => baselines::SOAK_SETTLE_CEILING.is_some(),
            "SOAK_QUIESCENT_FD_RETENTION" => baselines::SOAK_QUIESCENT_FD_RETENTION.is_some(),
            "ATTACH_PEAK_RSS_CEILING_BYTES" => {
                always_authored(baselines::ATTACH_PEAK_RSS_CEILING_BYTES)
            }
            "ATTACH_PAIR_PEAK_RSS_PER_MILLE" => {
                always_authored(baselines::ATTACH_PAIR_PEAK_RSS_PER_MILLE)
            }
            "SOAK_FD_GROWTH_ALLOWANCE" => always_authored(baselines::SOAK_FD_GROWTH_ALLOWANCE),
            "SOAK_RECOVERY_DOSE" => always_authored(baselines::SOAK_RECOVERY_DOSE),
            "SOAK_RSS_SLOPE_PER_MILLE" => always_authored(baselines::SOAK_RSS_SLOPE_PER_MILLE),
            "FD_BUDGET" => always_authored(baselines::FD_BUDGET),
            "READ_PEAK_RSS_CEILING_BYTES" => {
                always_authored(baselines::READ_PEAK_RSS_CEILING_BYTES)
            }
            "READ_OVER_ATTACH_PEAK_RSS_PER_MILLE" => {
                always_authored(baselines::READ_OVER_ATTACH_PEAK_RSS_PER_MILLE)
            }
            "READ_DEMAND_REREADINGS_PER_CONTENDED_ACQUISITION" => {
                always_authored(baselines::READ_DEMAND_REREADINGS_PER_CONTENDED_ACQUISITION)
            }
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

/// Why `bars` does not meet Layer 3's read exit, or `None` where it does.
///
/// **The exit asks for a read that is barred, and barred in earnest**: some
/// bar the ledger names bars a read through a live hold — a name opening with
/// `read-` — and every such bar is armed. A roll with no read bar has nothing
/// to hold a read to, and one whose read bar is unarmed records a reading and
/// holds nothing against it.
fn the_read_exit_is_unmet(bars: &[ExitBar]) -> Option<String> {
    let read: Vec<&ExitBar> = bars
        .iter()
        .filter(|bar| bar.name.starts_with("read-"))
        .collect();
    if read.is_empty() {
        return Some("no exit bar the ledger names bars a read".to_string());
    }
    let unarmed: Vec<&str> = read
        .iter()
        .filter(|bar| !bar.armed)
        .map(|bar| bar.name)
        .collect();
    if unarmed.is_empty() {
        None
    } else {
        Some(format!(
            "the read bars {unarmed:?} are unarmed, so a run records a read and holds nothing \
             against it"
        ))
    }
}

/// **Layer 3's read exit.** The ledger names a read bar, and every read bar it
/// names is armed.
///
/// Controls: the same roll with one read bar un-authored fails naming it, and
/// a roll with no read bar at all fails.
#[test]
fn every_read_bar_the_ledger_names_is_armed() {
    assert_eq!(the_read_exit_is_unmet(NAMED_EXIT_BARS), None);

    let mut unauthored = NAMED_EXIT_BARS.to_vec();
    let read = unauthored
        .iter_mut()
        .find(|bar| bar.name.starts_with("read-"))
        .expect("the ledger names a read bar");
    read.armed = false;
    let name = read.name;
    let problem = the_read_exit_is_unmet(&unauthored).expect("an unarmed read bar");
    assert!(problem.contains(name), "{problem}");

    let no_read: Vec<ExitBar> = NAMED_EXIT_BARS
        .iter()
        .copied()
        .filter(|bar| !bar.name.starts_with("read-"))
        .collect();
    assert!(
        the_read_exit_is_unmet(&no_read).is_some(),
        "a roll naming no read bar met the read exit"
    );
}

/// **The negative control.** A reading past a ceiling is refused by the
/// comparison every measurement bar in this crate makes.
///
/// The bars themselves cannot state this: a passing run says only that its
/// reading fit, and a comparison that had stopped refusing would report exactly
/// that. So the comparison is fed a reading past a ceiling of each shape it is
/// asked about — the bytes the peak-resident-set bars read, the duration the
/// settle bar reads, the count the descriptor budget reads and the per-mille
/// ratio the two slope bars read — and a reading at the ceiling is required to
/// pass beside each one, because a bar states the most a subject may cost and
/// costing exactly that is not costing more.
///
/// **The recovery dose is the one bar stated as a floor**, and it makes the
/// same comparison with the arguments the other way round: the dose is the
/// reading and the run's count is the ceiling. Its control is therefore a run
/// that recovered nothing — `fits(SOAK_RECOVERY_DOSE, 0)` must refuse — which
/// is what says the dose is a bound and not a number the summary prints.
#[test]
fn a_reading_past_a_ceiling_is_refused_by_the_comparison_every_bar_makes() {
    let bytes = baselines::ATTACH_PEAK_RSS_CEILING_BYTES;
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

    let ratio = baselines::SOAK_RSS_SLOPE_PER_MILLE;
    assert!(baselines::fits(ratio, ratio));
    assert!(!baselines::fits(ratio + 1, ratio));

    // The quiescent-retention bar is a descriptor count, the same shape the
    // budget above is stated in, so this control falls back to that shape
    // while the bar is unauthored; an authored value stands on its own under
    // this control with no edit here. The arming test beside this holds
    // `SOAK_QUIESCENT_FD_RETENTION`'s authored state to the registry's armed
    // claim.
    let retention = baselines::SOAK_QUIESCENT_FD_RETENTION.unwrap_or(baselines::FD_BUDGET);
    assert!(baselines::fits(retention, retention));
    assert!(!baselines::fits(retention + 1, retention));

    // The re-reading ceiling is a count per acquisition, and the counter lane
    // compares the widest any one acquisition took against it: a count
    // against a count.
    let rereadings = baselines::READ_DEMAND_REREADINGS_PER_CONTENDED_ACQUISITION;
    assert!(baselines::fits(rereadings, rereadings));
    assert!(!baselines::fits(rereadings + 1, rereadings));

    let dose = baselines::SOAK_RECOVERY_DOSE;
    assert!(baselines::fits(dose, dose));
    assert!(
        !baselines::fits(dose, 0),
        "a run that recovered nothing meets the dose, so the dose bounds no run"
    );
}

/// What one leg's settle cost, from its final change.
#[derive(Clone, Copy, Debug)]
struct Settle {
    /// To the derived store holding what a build from zero over the same tree
    /// holds.
    equivalent: Duration,
    /// Whether the attachment was publishing `Ready` at the poll that
    /// confirmed the reading.
    ready: bool,
    /// How wide the window the settle actually fell in was: from the start of
    /// the last poll that found the store unsettled to the reading itself.
    /// The reading is the top of that window, so this is how much earlier the
    /// settle may have happened.
    resolution: Duration,
}

/// A reading that has been taken but not yet confirmed by a second read.
#[derive(Clone, Copy, Debug)]
struct Candidate {
    /// The start of the full projection read this candidate was taken at.
    at: Duration,
    /// The start of the poll before it, which found the store unsettled.
    after: Duration,
}

/// A candidate taken at `began`, bounded below by the poll before it.
///
/// Where there was no poll before it this is the first one, so nothing has
/// observed the store unsettled and the lower bound is the final change
/// itself: the whole elapsed window is the resolution.
fn candidate_at(began: Duration, before_this_one: Option<Duration>) -> Candidate {
    Candidate {
        at: began,
        after: before_this_one.unwrap_or(Duration::ZERO),
    }
}

/// One leg's reading, and the projection the clock stopped on.
struct Reading {
    settle: Settle,
    /// What the store held when the clock stopped, which the equivalence bar
    /// below is required to be taken over.
    stopped_on: StoreProjection,
}

/// How a phase's acts reach the host.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Delivery {
    /// The watcher reports them, so the final act is the last write.
    Watched,
    /// They are a control file, inert until a reload reads it — so the act that
    /// delivers the phase is the reload, and the clock starts when it returns.
    Reloaded,
}

/// The label a family's sandbox is named for.
///
/// Matched exhaustively over the driver's roll, so a family added there is a
/// family this file fails to compile without.
fn label(family: Family) -> &'static str {
    match family {
        Family::OrdinaryEditing => "settle-ordinary",
        Family::AtomicReplacement => "settle-atomic",
        Family::CaseFlip => "settle-case-flip",
        Family::CaseRenamedParent => "settle-case-rename-parent",
        Family::Burst => "settle-burst",
        Family::ValidityTransitions => "settle-validity",
        Family::ExternalTools => "settle-tools",
    }
}

/// **One family's measurement.** Settle the opening phase, then time every leg
/// the roll carries from its own final act, and take the equivalence bar over
/// what the last clock stopped on.
///
/// **Every leg's clock is held to a state the store stayed in**, not just the
/// last one: a leg followed by another is checked before that one's acts land,
/// and the last leg is checked against the projection the equivalence bar is
/// taken over.
fn measure(family: Family, profile: &str) -> Vec<(String, Settle)> {
    let sandbox =
        Sandbox::new(Path::new(env!("CARGO_TARGET_TMPDIR")), label(family)).expect("a sandbox");
    // Asked before the tree is generated and outside the vault, so the probe's
    // own file is never a change a host is asked to reconcile.
    let ground = tree::ground(&sandbox.work_dir());
    let folding = ground.folding;
    let workload = family.workload(&ground);
    let third = family.third_phase(&ground);
    let vault = attach::Vault::generate(&sandbox.work_dir().join("attached"), profile);

    let (readings, stopped_on) = {
        let host = vault.host();
        let lease = attach::attach_and_wait(&host, vault.name());
        let mut store = vault.store();

        let opening = phase(
            &vault,
            &host,
            &mut store,
            workload.opening(),
            folding,
            Delivery::Watched,
        );
        let changing = phase(
            &vault,
            &host,
            &mut store,
            workload.changing(),
            folding,
            Delivery::Watched,
        );
        assert_moved(workload.changing(), &opening.1, &changing.1);

        let mut readings = vec![(family.name().to_string(), changing.0.settle)];
        let mut stopped_on = changing.0.stopped_on;
        if let Some(script) = &third {
            // Held here, before the leg that follows destroys the evidence.
            // The reload re-pins the vault's declaration and discards every
            // finding keyed by the old fingerprint, so after it nothing can
            // ask whether this leg's clock stopped on a state the store stayed
            // in — and a clock that stopped early biases the reading down.
            assert_the_store_has_not_moved(family.name(), &mut store, &stopped_on);
            let replaced = phase(
                &vault,
                &host,
                &mut store,
                script,
                folding,
                Delivery::Reloaded,
            );
            assert_moved(script, &changing.1, &replaced.1);
            readings.push((
                format!("{}, under a schema replacement", family.name()),
                replaced.0.settle,
            ));
            stopped_on = replaced.0.stopped_on;
        }
        drop(store);
        drop(lease);
        drop(host);
        (readings, stopped_on)
    };

    assert_it_converged_on_a_build_from_zero(family, &sandbox, &vault, &stopped_on);
    readings
}

/// **The clock stopped on a state the store then stayed in.**
///
/// A leg whose projection the store moved off is a clock that stopped early,
/// and an early stop biases the reading down — which is the direction a
/// calibration cannot afford. The family's last leg is held to this by
/// [`assert_it_converged_on_a_build_from_zero`], against the very projection
/// the equivalence bar reads; a leg with another behind it is held to it here,
/// while the state it stopped on still stands.
fn assert_the_store_has_not_moved(leg: &str, store: &mut Store, stopped_on: &StoreProjection) {
    let standing = StoreProjection::read(store).expect("projecting the settled store");
    stopped_on.compare(&standing).assert_equal(&format!(
        "{leg}: the store moved after the clock stopped, so the reading is not a reading of a \
         state the settle reached"
    ));
}

/// **The phase asked the host for something.** A census the phase's own opening
/// one equals is a settle over a host that was asked for nothing, and a reading
/// taken over it is a reading of an idle poll loop.
fn assert_moved(script: &Script, before: &Census, after: &Census) {
    assert_ne!(
        before,
        after,
        "`{}` applied its acts and the tree reads exactly as it did before them, so this reading \
         is a settle over a host that was asked for nothing",
        script.name()
    );
}

/// Apply one phase and settle over it, timing from its final act.
///
/// The clock starts the instant the phase is delivered — when `apply` returns
/// for a watched phase, and when the reload returns for a control-file one —
/// and stops at the instant the confirming projection read began, which is the
/// latest instant the store is known to have already settled by. The poll gap
/// before that instant is the reading's resolution.
fn phase(
    vault: &attach::Vault,
    host: &attach::ServingHost,
    store: &mut Store,
    script: &Script,
    folding: Folding,
    delivery: Delivery,
) -> (Reading, Census) {
    let mut applied = Applied::default();
    script
        .apply_range(vault.path(), 0..script.steps().len(), &mut applied)
        .unwrap_or_else(|problem| panic!("{problem}\n{script}"));
    if delivery == Delivery::Reloaded {
        reload(host, vault.name(), &applied);
    }
    let since = Instant::now();
    let tree = census(vault.path(), folding);
    assert!(
        !tree.rows.is_empty(),
        "`{}` left a tree holding no document at all, so a settle over it converges on \
         nothing\n{applied}",
        script.name()
    );
    tree.assert_the_script_read_the_tree_the_same_way(script);

    // The two stages of the poll. `confirmed` holds a candidate reading and the
    // projection that was read at it; the next poll's read has to agree with
    // that projection before the candidate becomes the reading.
    let mut confirmed: Option<(Candidate, StoreProjection)> = None;
    // The start of the poll before this one, which is the latest instant the
    // store is known to have still been unsettled at.
    let mut previous_poll: Option<Duration> = None;
    let mut ready = false;
    let mut resolution = Duration::ZERO;
    let equivalent = wait_until(
        &format!("the derived store to hold what `{}` implies", script.name()),
        CONVERGING.budget_for(applied.steps()),
        || {
            // The poll's own start, before the census it opens with. A poll
            // that found the store unsettled bounds the settle from below, and
            // it does so from here rather than from where its census finished.
            let entered = since.elapsed();
            let before_this_one = previous_poll.replace(entered);
            if let Some(why) = tree.disagreement(store) {
                confirmed = None;
                return Observed::pending(why);
            }
            // Sampled before the read, because the read observes the store as
            // of the instant it began: whatever it returns had already been
            // reached then, and the time it takes to materialise is the
            // instrument's cost rather than the subject's.
            let began = since.elapsed();
            let seen = StoreProjection::read(store).expect("projecting the settling store");
            match confirmed.take() {
                Some((candidate, held)) if held.compare(&seen).is_equal() => {
                    ready = host.state(vault.name()) == Ok(TrustState::Ready);
                    resolution = candidate.at.saturating_sub(candidate.after);
                    confirmed = Some((candidate, seen));
                    Observed::Met(candidate.at)
                }
                // A candidate that the read just disagreed with: the store
                // moved between the two reads, so this read becomes the new
                // candidate and the divergence is what the wait reports.
                Some((_, before)) => {
                    let divergence = before.compare(&seen).divergence;
                    confirmed = Some((candidate_at(began, before_this_one), seen));
                    Observed::pending(match divergence {
                        Some(divergence) => {
                            format!("the derived store is still moving: {divergence}")
                        }
                        None => "the derived store's whole projection to be read twice".to_string(),
                    })
                }
                // No candidate: either this is the first poll whose census
                // agreed, or the one before it saw the census disagree and
                // dropped what it held. Either way there is nothing to compare
                // this read against yet.
                None => {
                    confirmed = Some((candidate_at(began, before_this_one), seen));
                    Observed::pending(
                        "the derived store's whole projection to be read twice".to_string(),
                    )
                }
            }
        },
    )
    .unwrap_or_else(|failure| panic!("{failure}\n{applied}"));

    let (_, stopped_on) = confirmed.expect("the reading was taken over a projection");
    (
        Reading {
            settle: Settle {
                equivalent,
                ready,
                resolution,
            },
            stopped_on,
        },
        tree,
    )
}

/// Ask the host to read the vault's control files, which is what activates a
/// replaced declaration.
fn reload(host: &attach::ServingHost, name: &VaultName, applied: &Applied) {
    wait_until(
        "the explicit reload to activate the vault's control files",
        CONVERGING.budget_for(applied.steps()),
        || match host.reload(name) {
            Ok(_) => Observed::Met(()),
            Err(ReloadRefusal::Unavailable(standing)) => {
                Observed::pending(format!("the entry stands at {standing:?}"))
            }
            Err(refused) => panic!("the reload was refused: {refused:?}\n{applied}"),
        },
    )
    .unwrap_or_else(|failure| panic!("{failure}\n{applied}"));
}

/// **The bar the reading stands on.** What the clock stopped over is what a
/// derivation from zero over the same final tree holds.
///
/// Two comparisons, and the first is what makes the reading a reading of
/// equivalence: the projection the clock stopped on is held to the settled
/// store as it stands now, so a clock that stopped on a state the store then
/// moved off fails the case rather than reporting a number. The second is the
/// bar itself. An equivalence that fails says the clock stopped on something
/// other than convergence, which is a failed case rather than a slow one.
fn assert_it_converged_on_a_build_from_zero(
    family: Family,
    sandbox: &Sandbox,
    vault: &attach::Vault,
    stopped_on: &StoreProjection,
) {
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
        &format!("{}: the settled derivation", family.name()),
    );
    assert_operationally_valid(
        &mut right,
        &format!("{}: the derivation from zero", family.name()),
    );
    let left = StoreProjection::read(&mut left).expect("projecting the settled store");
    let right = StoreProjection::read(&mut right).expect("projecting the store built from zero");
    stopped_on.compare(&left).assert_equal(&format!(
        "{}: the store moved after the clock stopped, so the reading is not a reading of the \
         state this bar is taken over",
        family.name()
    ));
    left.compare(&right).assert_equal(&format!(
        "{}: the settle this case timed did not converge on a build from zero",
        family.name()
    ));
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
