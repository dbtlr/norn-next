//! The regression stratum's structural audit.
//!
//! `tests/regression/registry.json` names every defect class this line
//! carries forward. Each entry states a **falsifiable present-tense
//! property** and cites the records it was mined from. A case is checked by
//! counters and structural assertions over bytes; it is never a comparison
//! against recorded bytes, which is the coverage corpus's job and carries no
//! authority here.
//!
//! **Most subjects do not exist yet, and that is what the registry is for.**
//! A class nobody can test today is otherwise a class nobody remembers, so
//! each one enters as a named, auditable obligation with the layer that will
//! make it testable written next to it.
//!
//! # Dormancy is structural, and venue is what ends it
//!
//! `venue` is the first layer at which a real test can bind the property:
//! 0 the harness, 1 the substrate, 2 the lockdown, 3 queries, 4 mutations,
//! 5 repair, 6 surfaces. A case binds when its venue lands, and binding it is
//! a deliberate edit naming the tests that carry it. Flipping the status alone
//! fails: a bound case with no tests is a problem, and every test it does name
//! is held to a function cargo compiled into the target the reference gives.
//!
//! A case at or below [`LAYER_LANDING`] is bindable against a subject that
//! exists. Leaving one dormant is therefore a statement rather than a wait,
//! and [`the_registry_is_structurally_sound`] requires it to be written down:
//! a dormant case at or below that layer with no reason fails.
//!
//! # What this suite is not
//!
//! It judges no property. A property is judged by the test that binds it, and
//! until then the registry says only that the obligation is named, cited, and
//! assigned to a layer. What the audit does hold is the record: names unique,
//! properties present and distinct, citations shaped like citations, the
//! mandatory set exactly the one the harness pins, and every bound case naming
//! a test cargo really compiled into the suite.
//!
//! # The digest is the ratchet
//!
//! [`CONTRACT_DIGEST`] is one value over every case's whole contract. The
//! total catches a deletion; the digest catches everything a
//! deletion-and-replacement would hide — a property gutted to a word, a
//! citation swapped, a venue re-laned, a binding shrunk. Any registry edit
//! moves it, and moving it means editing a constant here, in the same diff,
//! where a reviewer reads it.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use norn_testkit::regression::{BindingStatus, Kind, LAYER_LANDING, Registry, TestIndex};

/// Every case the registry carries. A silent drop fails here; a deliberate
/// removal moves this number in the same diff as the entry.
const CASE_TOTAL: usize = 104;

/// The whole registry's contract, as one value.
///
/// Read from a reviewed diff, not derived: every field of every case goes into
/// it, so an edit anywhere in the registry fails this until somebody updates
/// the constant, which is the moment the edit becomes a thing a reviewer
/// looked at. This is the fixture generator's contract digest applied to a
/// registry.
const CONTRACT_DIGEST: &str = "c2be3c32a81efdb0e4c6e3a20c27311ee5df5daf967dd5af0381c877d404ed02";

/// The cases carried by tests today, by name.
///
/// Pinned rather than counted, because which ones are bound is the whole
/// claim: these are the ones whose subject — the harness itself — already
/// exists. A case that stops being carried has to leave this list to pass,
/// which is a diff a reviewer reads. Compared as a set, because the order
/// cases sit in the file is the file's business.
const BOUND_CASES: &[&str] = &[
    "a-measurement-lane-proves-it-measured",
    "encoding-prefix-transparency",
    "fixtures-carry-real-content-volume",
    "frontmatter-roundtrip-or-refuse",
    "harness-condition-waits-have-deadlines",
    "harness-processes-are-bounded-and-exec-safe",
    "harness-runs-under-isolated-state-roots",
    "harness-waits-have-deadlines",
    "one-field-edit-is-a-one-field-diff",
];

fn workspace_root() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest
        .ancestors()
        .nth(2)
        .unwrap_or_else(|| panic!("no workspace root above {}", manifest.display()))
        .to_path_buf()
}

fn registry() -> Registry {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/regression/registry.json");
    Registry::load(&path).unwrap_or_else(|e| panic!("the registry did not load: {e}"))
}

/// The record holds together: every case is named once in kebab-case, states a
/// property no other case states and cites sources shaped like citations, sits
/// at a venue the scale names, and carries a binding that says something true
/// — a bound case names tests whose files resolve exactly as spelled, which
/// declare themselves with `#[test]`, and which cargo compiled into the target
/// the reference names — and a dormant case names none and explains itself
/// where its venue already exists. The mandatory set is exactly the one the
/// harness pins.
///
/// Asking cargo what compiled is what makes this a claim about the suite that
/// runs rather than about the text of a file: one `--list` pair per cited
/// target, which is ten targets across five packages today.
#[test]
fn the_registry_is_structurally_sound() {
    let registry = registry();
    let root = workspace_root();
    let tests = TestIndex::from_cargo(&root, registry.cited_targets());
    let problems = registry.audit(&root, &tests);
    assert!(
        problems.is_empty(),
        "the registry is not structurally sound:\n  {}",
        problems.join("\n  ")
    );
}

/// The contract every case states is the one that was reviewed.
///
/// The other pins in this file each hold one field: the total holds how many
/// cases there are, the bound list holds which are carried, the harness holds
/// which are mandatory. This holds all of them at once, and everything else a
/// case says — so an edit that keeps the count, the bound set and the
/// mandatory flags while rewriting what a case actually promises fails here.
#[test]
fn the_contract_digest_is_the_reviewed_one() {
    assert_eq!(
        registry().contract_digest(),
        CONTRACT_DIGEST,
        "the registry's contract moved. Every field of every case feeds this digest, so read the \
         diff before taking the new value: a deliberate edit updates CONTRACT_DIGEST beside it."
    );
}

/// The registry's size is pinned, and every case is either bound or dormant.
///
/// The total is what makes a quiet deletion loud: a class removed without
/// moving this number fails, and moving it is the deliberate act. The
/// bound-and-dormant sum is the same rule applied to the gate — a case cannot
/// fall out of both and disappear.
#[test]
fn the_case_total_is_pinned() {
    let registry = registry();
    assert_eq!(
        registry.cases.len(),
        CASE_TOTAL,
        "the registry's case total moved"
    );

    let bound = registry.bound_cases().count();
    let dormant = registry.dormant_cases().count();
    assert_eq!(
        bound + dormant,
        CASE_TOTAL,
        "the bound and dormant sets do not account for every case"
    );
}

/// The cases carried by tests today are exactly the ones whose subject
/// already exists. Everything else is dormant with its venue, so nothing is
/// bound to a test that does not carry it.
#[test]
fn the_bound_cases_are_the_ones_the_harness_already_carries() {
    let registry = registry();
    let bound: BTreeSet<&str> = registry
        .bound_cases()
        .map(|case| case.name.as_str())
        .collect();
    let pinned: BTreeSet<&str> = BOUND_CASES.iter().copied().collect();
    assert_eq!(bound, pinned, "the set of bound cases moved");
}

/// Every venue on the scale is accounted for, and nothing is bound above the
/// layer currently landing.
///
/// The second half is the honest statement of where this workspace is: a case
/// bound at a venue above [`LAYER_LANDING`] would be asserting against a
/// subject that cannot have landed yet.
#[test]
fn only_the_layer_that_exists_carries_bound_cases() {
    let registry = registry();
    for case in registry.bound_cases() {
        assert!(
            case.venue <= LAYER_LANDING,
            "`{}` is bound at layer {}, above the layer that is landing",
            case.name,
            case.venue
        );
    }
    for layer in 0..registry.venues.len() as u8 {
        assert!(
            registry.cases_at(layer).next().is_some(),
            "layer {layer} holds no case, so the scale names a venue nothing waits on"
        );
    }
}

/// Each kind is represented, and the positive controls are kept apart from
/// the defects.
///
/// A positive control states a shape that already held, and an enforcement
/// class states how a guard failed to hold what it named. Both are easy to
/// lose into the defect pile, where they read as things to fix rather than
/// things to keep and things to check with.
#[test]
fn every_kind_is_represented() {
    let registry = registry();
    for kind in [
        Kind::DefectClass,
        Kind::PositiveControl,
        Kind::EnforcementClass,
    ] {
        assert!(
            registry.cases.iter().any(|case| case.kind == kind),
            "no case is a {kind}"
        );
    }
}

/// A dormant case names its venue and nothing else; a bound case names its
/// tests. Neither ever does both.
///
/// The audit says this too. It is restated here because it is the gate: a
/// case that named tests while claiming dormancy would be carried by them
/// without anybody having decided it was.
#[test]
fn a_binding_names_tests_or_a_venue_never_both() {
    for case in &registry().cases {
        match case.binding.status {
            BindingStatus::Bound => assert!(
                !case.binding.tests.is_empty(),
                "`{}` is bound and names no test",
                case.name
            ),
            BindingStatus::Dormant => assert!(
                case.binding.tests.is_empty(),
                "`{}` is dormant and names tests",
                case.name
            ),
        }
    }
}
