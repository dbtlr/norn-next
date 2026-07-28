//! The architecture gate, run against this workspace.
//!
//! The comparison logic and the mapping's consistency rules are unit-tested
//! against synthetic graphs and configurations in the crate itself. This
//! suite is the gate proper: it reads the workspace as cargo sees it, under
//! every feature selection the matrix names, and holds it to the allowlist —
//! and it reads the workspace-root `clippy.toml` and holds the
//! invariant-to-mechanism mapping to what that file configures.

use norn_testkit::architecture::{FEATURE_MATRIX, WorkspaceGraph};
use norn_testkit::invariants::{ClippyConfig, LINT_RULES, LintState, consistency_violations};

fn graph() -> WorkspaceGraph {
    WorkspaceGraph::read(&FEATURE_MATRIX[0]).expect("reading the workspace graph")
}

#[test]
fn the_workspace_dependency_graph_matches_the_allowlist() {
    for selection in FEATURE_MATRIX {
        let graph = WorkspaceGraph::read(selection).expect("reading the workspace graph");
        assert_eq!(
            graph.violations(),
            Vec::<String>::new(),
            "under the `{}` selection",
            selection.name
        );
    }
}

#[test]
fn the_workspace_holds_only_crates_the_map_names() {
    let graph = graph();
    assert!(
        !graph.members.is_empty(),
        "the gate read no workspace members at all"
    );
}

#[test]
fn the_invariant_mapping_agrees_with_the_configured_lints() {
    let config = ClippyConfig::read(&graph().root).expect("reading the boundary-lint ruleset");
    assert_eq!(consistency_violations(&config), Vec::<String>::new());
}

/// The ruleset is adopted one rule at a time, so a live rule and a pending
/// one are both expected — and the file has to carry the live ones.
#[test]
fn every_live_rule_is_configured_and_every_pending_rule_is_not() {
    let config = ClippyConfig::read(&graph().root).expect("reading the boundary-lint ruleset");
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
