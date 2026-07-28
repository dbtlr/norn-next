//! The architecture gate: the dependency allowlist as data, and the
//! comparison that holds the workspace to it.
//!
//! The allowlist in `docs/architecture.md` is the complete set of permitted
//! workspace normal-dependency edges, and [`ALLOWED_EDGES`] is that table
//! transcribed. A new edge is a deliberate edit here, never a side effect of
//! adding an import: an edge the table does not carry fails the gate whether
//! or not it points somewhere sensible.
//!
//! # What the gate compares
//!
//! The crate map describes the whole target shape and the workspace holds
//! only the crates earned so far, so equality is asserted **restricted to the
//! crates present**: every observed edge must be permitted, and every
//! permitted edge whose two endpoints are both present must be observed. An
//! edge whose endpoint has not been earned yet is neither required nor a
//! violation, and a crate the map does not name is a violation whatever it
//! depends on.
//!
//! Development-dependency edges are ignored: every crate's tests reach the
//! testkit, so they inherently cycle. Build-dependency edges between members
//! are rejected — the allowlist is a set of normal edges, and a build script
//! reaching another member is outside it.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::process::Command;

use serde_json::Value;

/// Every crate the crate map names, earned or not.
///
/// A workspace member outside this list fails the gate, so a crate cannot
/// join the workspace without an edit to the map and an edit here.
pub const WORKSPACE_CRATES: &[&str] = &[
    "norn",
    "norn-client",
    "norn-config",
    "norn-console",
    "norn-embed",
    "norn-fixtures",
    "norn-fs",
    "norn-host",
    "norn-mcp",
    "norn-serve",
    "norn-store",
    "norn-testkit",
    "norn-text",
    "norn-wire",
];

/// The permitted workspace normal-dependency edges, as `(from, to)`.
///
/// Seven crates are absent from the left column entirely — `norn-wire`,
/// `norn-text`, `norn-fs`, `norn-embed`, `norn-console`, `norn-config` and
/// `norn-fixtures` are leaves with zero workspace dependencies, and their
/// absence is what carries the invariants that say so.
pub const ALLOWED_EDGES: &[(&str, &str)] = &[
    ("norn", "norn-serve"),
    ("norn", "norn-client"),
    ("norn-serve", "norn-mcp"),
    ("norn-serve", "norn-host"),
    ("norn-serve", "norn-wire"),
    ("norn-serve", "norn-config"),
    ("norn-mcp", "norn-wire"),
    ("norn-host", "norn-store"),
    ("norn-host", "norn-wire"),
    ("norn-host", "norn-text"),
    ("norn-host", "norn-fs"),
    ("norn-host", "norn-embed"),
    ("norn-host", "norn-config"),
    ("norn-client", "norn-wire"),
    ("norn-client", "norn-console"),
    ("norn-client", "norn-config"),
    ("norn-store", "norn-wire"),
    ("norn-testkit", "norn-fixtures"),
    ("norn-testkit", "norn-wire"),
    ("norn-testkit", "norn-store"),
];

/// One reading of the workspace: the arguments that select which edges
/// `cargo metadata` reports.
///
/// The gate runs once per selection and the allowlist holds under each of
/// them, so an edge that appears only under some cargo flag is still an edge.
pub struct FeatureSelection {
    pub name: &'static str,
    pub args: &'static [&'static str],
}

/// The selections the gate evaluates.
///
/// The workspace declares no cargo features, so the matrix is one entry: the
/// default resolution. A crate that carries a feature adds the selection that
/// turns it on, and a crate that carries a target-specific dependency adds a
/// `--filter-platform` entry per target — both are edits to this table, which
/// is what makes a feature-gated or platform-gated edge visible to the gate
/// rather than hidden behind a flag nobody passes.
pub const FEATURE_MATRIX: &[FeatureSelection] = &[FeatureSelection {
    name: "default",
    args: &[],
}];

/// The workspace's own dependency edges, split by kind.
///
/// Only member-to-member edges are recorded. An edge to a registry crate is
/// not the allowlist's subject.
#[derive(Clone, Debug, Default)]
pub struct WorkspaceGraph {
    pub root: PathBuf,
    pub members: BTreeSet<String>,
    pub normal: BTreeSet<(String, String)>,
    pub build: BTreeSet<(String, String)>,
}

impl WorkspaceGraph {
    /// Read the graph under one feature selection.
    pub fn read(selection: &FeatureSelection) -> Result<Self, String> {
        Self::parse(&cargo_metadata(selection.args)?)
    }

    /// The graph `cargo metadata --format-version 1` describes.
    pub fn parse(metadata: &str) -> Result<Self, String> {
        let root: Value = serde_json::from_str(metadata)
            .map_err(|e| format!("cargo metadata is not readable as JSON: {e}"))?;

        let workspace_root = root
            .get("workspace_root")
            .and_then(Value::as_str)
            .ok_or("cargo metadata carries no `workspace_root`")?;

        let mut name_of: BTreeMap<&str, &str> = BTreeMap::new();
        for package in array(&root, "packages")? {
            let (Some(id), Some(name)) = (
                package.get("id").and_then(Value::as_str),
                package.get("name").and_then(Value::as_str),
            ) else {
                return Err("a package carries no `id` or no `name`".to_string());
            };
            name_of.insert(id, name);
        }

        let mut member_ids: BTreeSet<&str> = BTreeSet::new();
        for member in array(&root, "workspace_members")? {
            let id = member
                .as_str()
                .ok_or("a workspace member id is not a string")?;
            member_ids.insert(id);
        }

        let mut graph = WorkspaceGraph {
            root: PathBuf::from(workspace_root),
            members: member_ids
                .iter()
                .map(|id| {
                    name_of
                        .get(id)
                        .map(|name| (*name).to_string())
                        .ok_or_else(|| format!("workspace member `{id}` has no package entry"))
                })
                .collect::<Result<_, _>>()?,
            ..WorkspaceGraph::default()
        };

        let resolve = root
            .get("resolve")
            .ok_or("cargo metadata carries no resolve graph, so no edge can be read")?;
        for node in array(resolve, "nodes")? {
            let id = node
                .get("id")
                .and_then(Value::as_str)
                .ok_or("a resolve node carries no `id`")?;
            if !member_ids.contains(id) {
                continue;
            }
            let from = name_of[id];
            for dep in array(node, "deps")? {
                let Some(pkg) = dep.get("pkg").and_then(Value::as_str) else {
                    return Err("a resolve edge carries no `pkg`".to_string());
                };
                if !member_ids.contains(pkg) {
                    continue;
                }
                let to = name_of[pkg];
                for dep_kind in array(dep, "dep_kinds")? {
                    let edge = (from.to_string(), to.to_string());
                    match dep_kind.get("kind").and_then(Value::as_str) {
                        // A normal dependency reports no kind at all.
                        None => {
                            graph.normal.insert(edge);
                        }
                        Some("build") => {
                            graph.build.insert(edge);
                        }
                        Some(_) => {}
                    }
                }
            }
        }
        Ok(graph)
    }

    /// Every way this graph departs from the allowlist, one line each. A
    /// conforming workspace produces none.
    pub fn violations(&self) -> Vec<String> {
        let mut problems = Vec::new();
        let known: BTreeSet<&str> = WORKSPACE_CRATES.iter().copied().collect();
        let allowed: BTreeSet<(&str, &str)> = ALLOWED_EDGES.iter().copied().collect();

        for member in &self.members {
            if !known.contains(member.as_str()) {
                problems.push(format!(
                    "the workspace holds `{member}`, which the crate map does not name"
                ));
            }
        }

        for (from, to) in &self.normal {
            if !allowed.contains(&(from.as_str(), to.as_str())) {
                problems.push(format!(
                    "`{from}` depends on `{to}`, which the allowlist does not permit"
                ));
            }
        }

        for (from, to) in &allowed {
            if !self.members.contains(*from) || !self.members.contains(*to) {
                continue;
            }
            if !self
                .normal
                .contains(&((*from).to_string(), (*to).to_string()))
            {
                problems.push(format!(
                    "the allowlist permits `{from}` -> `{to}` and both crates are present, but \
                     the workspace does not carry the edge"
                ));
            }
        }

        for (from, to) in &self.build {
            problems.push(format!(
                "`{from}` build-depends on `{to}`; the allowlist is a set of normal edges, and a \
                 build-dependency between members is not one of them"
            ));
        }

        problems
    }
}

/// Run `cargo metadata` under `args` and hand back its JSON.
///
/// The cargo that built this test is the cargo that reads the workspace, so
/// the reading cannot come from a different toolchain than the one under
/// test.
pub fn cargo_metadata(args: &[&str]) -> Result<String, String> {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
    let output = Command::new(&cargo)
        .args(["metadata", "--format-version", "1"])
        .args(args)
        .output()
        .map_err(|e| format!("could not run `{cargo} metadata`: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "`{cargo} metadata` failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    String::from_utf8(output.stdout).map_err(|e| format!("cargo metadata is not UTF-8: {e}"))
}

fn array<'a>(value: &'a Value, key: &str) -> Result<&'a Vec<Value>, String> {
    value
        .get(key)
        .and_then(Value::as_array)
        .ok_or_else(|| format!("cargo metadata carries no `{key}` array"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn graph(members: &[&str], normal: &[(&str, &str)]) -> WorkspaceGraph {
        WorkspaceGraph {
            root: PathBuf::from("/workspace"),
            members: members.iter().map(|m| (*m).to_string()).collect(),
            normal: normal
                .iter()
                .map(|(f, t)| ((*f).to_string(), (*t).to_string()))
                .collect(),
            build: BTreeSet::new(),
        }
    }

    /// A workspace holding two crates and the one allowlist edge between
    /// them. Every crate the map names but the workspace has not earned is
    /// simply absent.
    fn conforming() -> WorkspaceGraph {
        graph(
            &["norn-host", "norn-store", "norn-wire"],
            &[
                ("norn-host", "norn-store"),
                ("norn-host", "norn-wire"),
                ("norn-store", "norn-wire"),
            ],
        )
    }

    fn violation_mentions(graph: &WorkspaceGraph, needle: &str) {
        let problems = graph.violations();
        assert!(
            problems.iter().any(|p| p.contains(needle)),
            "expected a violation containing {needle:?}, got: {problems:?}"
        );
    }

    #[test]
    fn a_workspace_holding_part_of_the_map_conforms() {
        assert_eq!(conforming().violations(), Vec::<String>::new());
    }

    #[test]
    fn every_allowlist_endpoint_is_a_crate_the_map_names() {
        let known: BTreeSet<&str> = WORKSPACE_CRATES.iter().copied().collect();
        for (from, to) in ALLOWED_EDGES {
            assert!(known.contains(from), "`{from}` is not a known crate");
            assert!(known.contains(to), "`{to}` is not a known crate");
        }
    }

    #[test]
    fn an_edge_outside_the_allowlist_fails() {
        let mut graph = conforming();
        graph
            .normal
            .insert(("norn-store".to_string(), "norn-host".to_string()));
        violation_mentions(&graph, "which the allowlist does not permit");
    }

    #[test]
    fn a_missing_edge_between_two_present_crates_fails() {
        let mut graph = conforming();
        graph
            .normal
            .remove(&("norn-store".to_string(), "norn-wire".to_string()));
        violation_mentions(&graph, "but the workspace does not carry the edge");
    }

    /// The restriction that makes the gate binding today: an allowlist edge
    /// whose endpoint the workspace has not earned is neither required nor a
    /// violation.
    #[test]
    fn an_allowlist_edge_to_an_absent_crate_is_not_required() {
        let graph = graph(&["norn-wire"], &[]);
        assert_eq!(graph.violations(), Vec::<String>::new());
    }

    #[test]
    fn a_crate_the_map_does_not_name_fails() {
        let mut graph = conforming();
        graph.members.insert("norn-cache".to_string());
        violation_mentions(&graph, "which the crate map does not name");
    }

    #[test]
    fn a_build_dependency_between_members_fails() {
        let mut graph = conforming();
        graph
            .build
            .insert(("norn-host".to_string(), "norn-wire".to_string()));
        violation_mentions(&graph, "build-depends on");
    }

    /// A dev-dependency edge cycles by construction — every crate's tests
    /// reach the testkit — so it is read and discarded rather than judged.
    #[test]
    fn a_development_edge_is_not_read_as_a_dependency() {
        let metadata = r#"{
          "workspace_root": "/workspace",
          "workspace_members": ["a-id", "b-id"],
          "packages": [
            {"id": "a-id", "name": "norn"},
            {"id": "b-id", "name": "norn-testkit"}
          ],
          "resolve": {"nodes": [
            {"id": "a-id", "deps": [
              {"pkg": "b-id", "dep_kinds": [{"kind": "dev"}]}
            ]},
            {"id": "b-id", "deps": []}
          ]}
        }"#;
        let graph = WorkspaceGraph::parse(metadata).expect("parsing metadata");
        assert_eq!(graph.members.len(), 2);
        assert!(graph.normal.is_empty());
        assert!(graph.build.is_empty());
    }

    #[test]
    fn an_edge_leaving_the_workspace_is_not_the_allowlists_subject() {
        let metadata = r#"{
          "workspace_root": "/workspace",
          "workspace_members": ["a-id"],
          "packages": [
            {"id": "a-id", "name": "norn-wire"},
            {"id": "serde-id", "name": "serde"}
          ],
          "resolve": {"nodes": [
            {"id": "a-id", "deps": [
              {"pkg": "serde-id", "dep_kinds": [{"kind": null}]}
            ]}
          ]}
        }"#;
        let graph = WorkspaceGraph::parse(metadata).expect("parsing metadata");
        assert!(graph.normal.is_empty());
        assert_eq!(graph.violations(), Vec::<String>::new());
    }

    #[test]
    fn metadata_without_a_resolve_graph_is_refused() {
        let metadata = r#"{
          "workspace_root": "/workspace",
          "workspace_members": [],
          "packages": []
        }"#;
        let error = WorkspaceGraph::parse(metadata).expect_err("a graph with no edges to read");
        assert!(error.contains("resolve"), "{error}");
    }

    #[test]
    fn the_feature_matrix_names_every_selection_it_runs() {
        for selection in FEATURE_MATRIX {
            assert!(!selection.name.is_empty());
        }
    }
}
