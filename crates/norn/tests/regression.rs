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
//! # A reason is held to the tree, not just to being present
//!
//! A reason is prose, and prose about a subject that has not been built reads
//! the same forever after the subject lands. So a reason at these layers states
//! the workspace paths it stands on, each claimed as one the tree holds or one
//! it does not, and the audit resolves every one of them:
//! [`a_dormancy_reason_whose_subject_landed_fails_the_audit`] is that gate seen
//! from the failing side. A dormancy claim standing on a path therefore expires
//! by itself, and re-deriving it — binding the case, or restating why a built
//! subject still carries nothing — is the only way back to green.
//!
//! A subject is not always a whole path. A reason often waits on one name
//! inside a file the tree already holds — a member absent from a counter
//! vocabulary, a guard function nobody wrote, a type a module will grow — and
//! states it as a `symbol-absent` ground: a `<file>::<Symbol>` pair the audit
//! reads out of that file's declaration lines. The file has to be there for
//! either symbol claim, because an absent name in an absent file is a path
//! absence, and the name is pre-committed rather than guessed at: a dormant case
//! is the contract the carrier is authored against.
//!
//! What is left waits on nothing any subject names — a shell step, a module of
//! an integration target, a reader behind a feature, a vocabulary nobody has
//! written, a guard with no home decided. Those reasons expire only when a
//! person re-derives them, so the class is pinned by name in
//! [`UNFALSIFIABLE_DORMANCY`]: what the registry guarantees mechanically is that
//! the class is a reviewed list rather than a silent majority.
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

use norn_testkit::regression::{
    Binding, BindingStatus, Ground, Kind, LAYER_LANDING, Registry, TestIndex,
};

/// Every case the registry carries. A silent drop fails here; a deliberate
/// removal moves this number in the same diff as the entry.
const CASE_TOTAL: usize = 106;

/// The whole registry's contract, as one value.
///
/// Read from a reviewed diff, not derived: every field of every case goes into
/// it, so an edit anywhere in the registry fails this until somebody updates
/// the constant, which is the moment the edit becomes a thing a reviewer
/// looked at. This is the fixture generator's contract digest applied to a
/// registry.
const CONTRACT_DIGEST: &str = "527f75ab703590fbbbf196573b7ec7af8f2a6310af9b91b5090e8556c9102fd6";

/// The cases carried by tests today, by name.
///
/// Pinned rather than counted, because which ones are bound is the whole
/// claim: these are the ones whose subject — the harness, and the substrate
/// under it — already exists and is asserted over. A case that stops being
/// carried has to leave this list to pass, which is a diff a reviewer reads.
/// Compared as a set, because the order cases sit in the file is the file's
/// business.
const BOUND_CASES: &[&str] = &[
    "a-measurement-lane-proves-it-measured",
    "a-mutation-confirms-the-file-it-holds-before-it-publishes",
    "cache-identity-is-total",
    "cost-is-independent-of-vault-size",
    "encoding-prefix-transparency",
    "fixtures-carry-real-content-volume",
    "frontmatter-roundtrip-or-refuse",
    "harness-condition-waits-have-deadlines",
    "harness-processes-are-bounded-and-exec-safe",
    "harness-runs-under-isolated-state-roots",
    "harness-waits-have-deadlines",
    "one-field-edit-is-a-one-field-diff",
];

/// The dormant cases at or below [`LAYER_LANDING`] whose reason states no
/// absence, by name — **the dormancy the falsifiability gate cannot refute.**
///
/// A reason waiting on a name inside a file the tree already holds states that
/// name as a `symbol-absent` ground and leaves this list. What is left is the
/// residue no subject reaches, and it is five classes rather than a bag:
///
/// - **A shell step.** The carrier is a line of `lane-suite.sh`, which no
///   `<file>::<fn>` reference and no Rust declaration names.
/// - **A module of an integration target.** The carrier compiles, and the
///   reference grammar names top-level `tests/` files only; NORN-159 owns
///   widening it. A `compile_fail` doctest sits in the same class.
/// - **A reader behind a feature.** The declaration is there; what is absent is
///   the `induced-failure` selection cargo would compile it under, which is a
///   fact about a build rather than about a file's text.
/// - **An absent vault-rule vocabulary.** No file is settled to declare a rule
///   set, so no name can be pre-committed as the carrier's.
/// - **A guard with no settled home.** The scan or lint the case waits on has
///   no file decided on to hold it, and a symbol ground names a file.
///
/// The grounds beside such a reason hold the subjects it cites as present, so
/// the audit still catches those moving; the claim that something is missing is
/// refuted by nobody but the next person to read the code.
///
/// Pinned for the reason [`BOUND_CASES`] is: the size of this class is the
/// honest scope of the gate. A case that gains an absence leaves the list, a
/// reason that newly waits on a subject nothing names joins it, and either way
/// the edit is in the diff.
const UNFALSIFIABLE_DORMANCY: &[&str] = &[
    "a-create-never-takes-a-name-somebody-else-holds",
    "a-measurement-step-asserts-a-nonzero-pass-count",
    "a-sidecar-is-keyed-by-its-own-model-and-scoped-by-the-store-epoch",
    "comment-claims-are-test-bound",
    "derived-findings-are-materialized-and-maintained",
    "each-file-is-read-once-per-build",
    "guard-binds-executed-sql",
    "harness-assertions-observe-stable-facts",
    "instrumentation-exists-and-is-consumed",
    "maintenance-touches-the-affected-set-only",
    "output-parity-cannot-certify-structure",
    "present-but-unusable-config-refuses-loudly",
    "steps-report-their-own-outcome",
    "substrate-capabilities-are-probed-before-they-are-relied-on",
    "unsatisfiable-config-is-rejected-at-load",
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
/// target, which is thirteen targets across five packages today.
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

/// **A dormancy reason goes stale loudly.** A reason standing on a subject's
/// absence fails this registry's own audit the moment the workspace holds that
/// subject.
///
/// The binding installed below is a reason of exactly the shape the gate exists
/// to catch: it says no database is opened, while `crates/norn-store/src/store.rs`
/// is in the tree opening one. Nothing about the prose says it is out of date —
/// prose never does — so the check is over the grounds beside it, and it is the
/// reason the registry's own layer-0 and layer-1 entries can be trusted to
/// describe the workspace as it is rather than as it was.
///
/// The audit runs against an empty test index here, because what a bound case's
/// carriers compiled into is a different question and asking cargo about it
/// costs a full listing pass. The problem asserted is the grounds' own.
#[test]
fn a_dormancy_reason_whose_subject_landed_fails_the_audit() {
    let mut registry = registry();
    let subject = "storage-configuration-is-explicit";
    let case = registry
        .cases
        .iter_mut()
        .find(|case| case.name == subject)
        .unwrap_or_else(|| panic!("no case named `{subject}`"));
    case.binding = Binding {
        status: BindingStatus::Dormant,
        tests: Vec::new(),
        reason: Some("no database is opened to configure".to_string()),
        grounds: vec![Ground::Absent("crates/norn-store/src/store.rs".to_string())],
    };

    let problems = registry.audit(&workspace_root(), &TestIndex::default());
    let stale: Vec<&String> = problems
        .iter()
        .filter(|problem| {
            problem.starts_with(&format!("`{subject}`"))
                && problem.contains("The subject landed, so the reason is stale")
        })
        .collect();
    assert_eq!(
        stale.len(),
        1,
        "`{subject}` stands on the absence of a file the workspace holds, and the audit said: \
         {problems:#?}"
    );
}

/// **The gate states its own reach.** The dormant cases the audit cannot
/// refute are exactly the ones pinned in [`UNFALSIFIABLE_DORMANCY`].
///
/// A ground is a claim about a path, so a reason whose missing subject is not a
/// path — a guard, a counter member, a vocabulary, a carrier this grammar
/// cannot name — carries no claim that can fail. Left unnamed, that class grows
/// by one every time a reason is written the easy way, and the registry reads
/// as if every reason were held to the tree. Named, it is a list a reviewer can
/// count, and a reason that gains an absence has to leave it in the same diff.
#[test]
fn the_dormancy_the_gate_cannot_refute_is_the_pinned_set() {
    let registry = registry();
    let unrefuted: BTreeSet<&str> = registry
        .dormant_without_an_absence()
        .map(|case| case.name.as_str())
        .collect();
    let pinned: BTreeSet<&str> = UNFALSIFIABLE_DORMANCY.iter().copied().collect();
    assert_eq!(
        unrefuted, pinned,
        "the set of dormancy reasons standing on no absence moved. A case that gained an `absent` \
         ground leaves this pin; a reason that now waits on a subject no path names joins it."
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
