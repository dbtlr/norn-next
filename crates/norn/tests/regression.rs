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
//! a deliberate edit naming the tests that carry it — there is no attribute
//! to remove and no environment variable to set.
//!
//! Layer 0 is the layer that exists, so a case whose venue is 0 is bindable
//! against the harness as it stands. Leaving one dormant is therefore a
//! statement rather than a wait, and [`the_registry_is_structurally_sound`]
//! requires it to be written down: a dormant layer-0 case with no reason
//! fails.
//!
//! # What this suite is not
//!
//! It judges no property. A property is judged by the test that binds it, and
//! until then the registry says only that the obligation is named, cited, and
//! assigned to a layer. What the audit does hold is the record: names unique,
//! properties and citations present, the mandatory set exactly the one the
//! harness pins, and every bound case naming a test that really exists.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use norn_testkit::regression::{BindingStatus, Kind, MANDATORY_CASES, Registry};

/// Every case the registry carries. A silent drop fails here; a deliberate
/// removal moves this number in the same diff as the entry.
const CASE_TOTAL: usize = 96;

/// The cases carried by tests today, by name.
///
/// Pinned rather than counted, because which four are bound is the whole
/// claim: these are the ones whose subject — the harness itself — already
/// exists. A case that stops being carried has to leave this list to pass,
/// which is a diff a reviewer reads.
const BOUND_CASES: &[&str] = &[
    "a-measurement-lane-proves-it-measured",
    "fixtures-carry-real-content-volume",
    "harness-processes-are-bounded-and-exec-safe",
    "harness-runs-under-isolated-state-roots",
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

/// The record holds together: every case is named once in kebab-case, states
/// a property and cites at least one source, sits at a venue the scale names,
/// and carries a binding that says something true — a bound case names tests
/// that exist in the workspace and declare themselves with `#[test]`, and a
/// dormant case names none and explains itself where its venue already
/// exists. The mandatory set is exactly the one the harness pins.
#[test]
fn the_registry_is_structurally_sound() {
    let problems = registry().audit(&workspace_root());
    assert!(
        problems.is_empty(),
        "the registry is not structurally sound:\n  {}",
        problems.join("\n  ")
    );
}

/// The five ratified classes are present by name.
///
/// The audit reconciles the registry's `mandatory` flags against the harness
/// list; this states the list itself, so that dropping a ratified class takes
/// an edit in two files and reads as what it is in both.
#[test]
fn the_mandatory_cases_are_carried_by_name() {
    let registry = registry();
    let carried: BTreeSet<&str> = registry
        .cases
        .iter()
        .filter(|case| case.mandatory)
        .map(|case| case.name.as_str())
        .collect();
    for name in MANDATORY_CASES {
        assert!(
            carried.contains(name),
            "`{name}` is ratified and the registry does not carry it"
        );
    }
    assert_eq!(
        carried.len(),
        MANDATORY_CASES.len(),
        "the registry carries a mandatory case the harness does not pin"
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

/// The cases carried by tests today are exactly the four whose subject
/// already exists. Everything else is dormant with its venue, so nothing is
/// bound to a test that does not carry it.
#[test]
fn the_bound_cases_are_the_ones_the_harness_already_carries() {
    let registry = registry();
    let bound: Vec<&str> = registry
        .bound_cases()
        .map(|case| case.name.as_str())
        .collect();
    assert_eq!(bound, BOUND_CASES, "the set of bound cases moved");
}

/// Every venue on the scale is accounted for, and layer 0 is the only one
/// with anything bound.
///
/// The second half is the honest statement of where this workspace is: the
/// harness exists, and nothing above it does. A case bound at a venue above 0
/// would be asserting against a subject that has not landed.
#[test]
fn only_the_layer_that_exists_carries_bound_cases() {
    let registry = registry();
    for case in registry.bound_cases() {
        assert_eq!(
            case.venue, 0,
            "`{}` is bound at layer {}, above the layer that exists",
            case.name, case.venue
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
