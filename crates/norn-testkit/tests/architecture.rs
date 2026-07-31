//! The architecture gate, run against this workspace.
//!
//! The comparison logic and the mapping's consistency rules are unit-tested
//! against synthetic graphs and configurations in the crate itself. This
//! suite is the gate proper: it reads the workspace as cargo sees it, under
//! every feature selection the matrix names, and holds it to the allowlist —
//! it reads the workspace-root `clippy.toml` and holds the
//! invariant-to-mechanism mapping to what that file configures — and it reads
//! `docs/architecture.md` and holds the transcribed tables to the document
//! they were transcribed from.

use std::collections::BTreeSet;
use std::sync::OnceLock;

use norn_testkit::architecture::{
    ALLOWED_EDGES, DEFAULT_SELECTION, DocumentedMap, FEATURE_MATRIX, MatrixReading,
    WORKSPACE_CRATES, crate_directories, documented_invariants, documented_map, exclusion_gaps,
    feature_coverage_violations, isolation_violations, read_architecture_document,
};
use norn_testkit::invariants::{
    ClippyConfig, INVARIANTS, LINT_RULES, LintState, consistency_violations,
};

/// One reading of the workspace per selection, shared by every test here:
/// `cargo metadata` runs once per selection rather than once per assertion.
fn reading() -> &'static MatrixReading {
    static READING: OnceLock<MatrixReading> = OnceLock::new();
    READING
        .get_or_init(|| MatrixReading::read(FEATURE_MATRIX).expect("reading the workspace graph"))
}

fn document() -> &'static DocumentedMap {
    static DOCUMENT: OnceLock<DocumentedMap> = OnceLock::new();
    DOCUMENT.get_or_init(|| {
        let markdown =
            read_architecture_document(reading().root()).expect("reading the architecture map");
        documented_map(&markdown).expect("parsing the architecture map")
    })
}

#[test]
fn the_workspace_dependency_graph_matches_the_allowlist() {
    assert_eq!(reading().violations(), Vec::<String>::new());
}

/// Heavy-dependency isolation, read off the resolve graph. A crate's own
/// manifest test can only say what that crate asks for; a feature is the
/// dependent's to turn on, and what the workspace actually links under a
/// plain `cargo build` is visible only here.
#[test]
fn an_isolated_crate_links_nothing_under_the_default_selection() {
    let graph = reading()
        .under(DEFAULT_SELECTION)
        .expect("the matrix carries the default selection");
    assert_eq!(isolation_violations(graph), Vec::<String>::new());
}

#[test]
fn the_matrix_sees_every_feature_a_member_declares() {
    assert_eq!(
        feature_coverage_violations(&reading().declared_features(), FEATURE_MATRIX),
        Vec::<String>::new()
    );
}

/// A crate directory the workspace excludes is invisible to every gate here:
/// no edge of its is read, no lint of ours binds it, no test of ours runs it.
#[test]
fn the_member_set_has_no_exclusion_gaps() {
    let root = reading().root();
    let present = crate_directories(root).expect("reading the crate directories");
    assert!(
        !present.is_empty(),
        "the scan found no crate directory under {}",
        root.display()
    );
    for (selection, graph) in reading().readings() {
        assert_eq!(
            exclusion_gaps(&present, &graph.member_directories),
            Vec::<String>::new(),
            "under the `{selection}` selection",
        );
    }
}

#[test]
fn the_gate_reads_every_member_of_the_workspace() {
    for (selection, graph) in reading().readings() {
        assert!(
            !graph.members.is_empty(),
            "the gate read no workspace members at all under the `{selection}` selection"
        );
    }
}

/// The allowlist and the crate list in [`norn_testkit::architecture`] are
/// transcriptions. This is the check that they are faithful ones.
#[test]
fn the_transcribed_tables_match_the_architecture_document() {
    let documented = document();
    let crates: BTreeSet<String> = WORKSPACE_CRATES.iter().map(|c| (*c).to_string()).collect();
    assert_eq!(
        documented.crates, crates,
        "the crate map in docs/architecture.md and WORKSPACE_CRATES disagree"
    );

    let edges: BTreeSet<(String, String)> = ALLOWED_EDGES
        .iter()
        .map(|(from, to)| ((*from).to_string(), (*to).to_string()))
        .collect();
    assert_eq!(
        documented.edges, edges,
        "the dependency allowlist in docs/architecture.md and ALLOWED_EDGES disagree"
    );
}

/// The mapping numbers its invariants after the document's. A claim added to
/// one and not the other is the drift this catches.
#[test]
fn the_mapping_carries_every_invariant_the_document_states() {
    let markdown =
        read_architecture_document(reading().root()).expect("reading the architecture map");
    let documented = documented_invariants(&markdown).expect("the document's invariant numbers");
    let mapped: BTreeSet<u8> = INVARIANTS
        .iter()
        .map(|invariant| invariant.number)
        .collect();
    assert_eq!(documented, mapped);
}

#[test]
fn the_invariant_mapping_agrees_with_the_configured_lints() {
    let config = ClippyConfig::read(reading().root()).expect("reading the boundary-lint ruleset");
    assert_eq!(consistency_violations(&config), Vec::<String>::new());
}

/// The ruleset is adopted one rule at a time, so a live rule and a pending
/// one are both expected — and the file has to carry the live ones.
#[test]
fn every_live_rule_is_configured_and_every_pending_rule_is_not() {
    let config = ClippyConfig::read(reading().root()).expect("reading the boundary-lint ruleset");
    for rule in LINT_RULES {
        match rule.state {
            LintState::Live => {
                for (key, path) in rule.required {
                    assert!(
                        config.carries(key, path),
                        "rule `{}` is live and `{path}` is not configured under `{key}`",
                        rule.name
                    );
                }
            }
            LintState::Pending => assert!(
                rule.required.is_empty(),
                "rule `{}` is pending and names configuration",
                rule.name
            ),
        }
    }
}
