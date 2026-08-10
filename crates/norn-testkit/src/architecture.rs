//! The architecture gate: the dependency allowlist as data, and the
//! comparison that holds the workspace to it.
//!
//! The allowlist in `docs/architecture.md` is the complete set of permitted
//! workspace normal-dependency edges, and [`ALLOWED_EDGES`] is that table
//! transcribed. A new edge is a deliberate edit here, never a side effect of
//! adding an import: an edge the table does not carry fails the gate whether
//! or not it points somewhere sensible. The transcription is itself checked —
//! [`documented_map`] parses the tables out of the document and the gate
//! asserts set equality, so the two spellings cannot drift.
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
//!
//! # What the gate refuses to be blind to
//!
//! `cargo metadata` reports a *resolved* graph, and two things vanish from it
//! unless the gate goes looking:
//!
//! - **An edge behind an off-by-default feature.** An `optional = true`
//!   dependency is absent from the resolve graph under the default selection.
//!   The gate therefore reads the workspace once per entry in
//!   [`FEATURE_MATRIX`] and judges the readings together, and
//!   [`feature_coverage_violations`] fails when a member declares a feature no
//!   selection turns on.
//! - **A dependency a *consumer* switched on.** Features belong to the
//!   dependent, so a crate that links nothing on its own terms links whatever
//!   a dependent's `features = [..]` reaches. [`isolation_violations`] reads
//!   what [`ISOLATED_CRATES`] actually resolve under the default selection,
//!   which is where both halves of that show.
//! - **A local crate the workspace does not hold.** A package excluded from
//!   the workspace, or living outside the workspace root, is not a member, so
//!   neither its edges nor its lints nor its tests are anyone's subject. A
//!   member depending on such a crate fails the gate, and
//!   [`exclusion_gaps`] fails when a directory under `crates/` holds a
//!   manifest the workspace does not carry.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
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

/// The crates that link nothing at all in a build that is not a test run.
///
/// `norn-embed` is here for heavy-dependency isolation: a model runtime never
/// enters a development build. That crate's own manifest is held to the rule
/// in `crates/norn-embed/tests/isolation.rs`, and a manifest is only half of
/// it — **features belong to the dependent**, so a consumer writing
/// `norn-embed = { features = [..] }`, or forwarding one of its own defaults
/// into it, would compile the runtime into every development build with that
/// manifest test still green. The resolve graph is where both halves show,
/// which is why the second half is read here.
pub const ISOLATED_CRATES: &[&str] = &["norn-embed"];

/// The name of the reading a plain `cargo build` corresponds to.
pub const DEFAULT_SELECTION: &str = "default";

/// One reading of the workspace: the arguments that select which edges
/// `cargo metadata` reports.
pub struct FeatureSelection {
    pub name: &'static str,
    pub args: &'static [&'static str],
}

/// The selections the gate evaluates.
///
/// Two entries, and the second is the one that makes the first honest. The
/// default resolution is what a plain `cargo build` sees; `--all-features`
/// turns on every optional dependency, which is the only way an edge behind
/// an off-by-default feature reaches the resolve graph at all. A crate that
/// carries a target-specific dependency adds a `--filter-platform` entry, on
/// the same terms.
///
/// The matrix is judged as a whole, not selection by selection: see
/// [`MatrixReading::violations`].
pub const FEATURE_MATRIX: &[FeatureSelection] = &[
    FeatureSelection {
        name: DEFAULT_SELECTION,
        args: &[],
    },
    FeatureSelection {
        name: "all features",
        args: &["--all-features"],
    },
];

/// The workspace's own dependency edges, split by kind.
///
/// Only member-to-member edges are recorded as `normal` or `build`. An edge
/// to a registry crate is not the allowlist's subject; an edge to a *local*
/// crate that is not a member is [`WorkspaceGraph::unearned`], which is a
/// violation on its own.
#[derive(Clone, Debug, Default)]
pub struct WorkspaceGraph {
    pub root: PathBuf,
    pub members: BTreeSet<String>,
    /// The directory each member's manifest sits in. Compared against the
    /// directories under `crates/` to catch an excluded member.
    pub member_directories: BTreeSet<PathBuf>,
    pub normal: BTreeSet<(String, String)>,
    pub build: BTreeSet<(String, String)>,
    /// Normal or build edges from a member to a local package the workspace
    /// does not hold as a member, as `(from, name of the dependency)`.
    ///
    /// A package whose manifest is under `<root>/vendor/` *and* whose name the
    /// root manifest replaces through `[patch.crates-io]` is not one of these:
    /// it is a published release with a patch applied, and the edge to it is
    /// the third-party edge the registry release already carried.
    pub unearned: BTreeSet<(String, String)>,
    /// Every normal or build edge from a member to *any* package under this
    /// reading, registry crates included, as `(from, name of the
    /// dependency)`. The allowlist's subject is member-to-member edges; this
    /// is what a member actually links, which is [`isolation_violations`]'s
    /// subject. Development edges are absent: they are in no build that is
    /// not a test run.
    pub linked: BTreeSet<(String, String)>,
    /// Every feature a member declares, as `(crate, feature)`. Cargo lists an
    /// optional dependency's implicit feature here under the dependency's own
    /// name; a `dep:`-prefixed spelling is only ever a value, never a key.
    pub declared_features: BTreeSet<(String, String)>,
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
        let mut manifest_of: BTreeMap<&str, &str> = BTreeMap::new();
        for package in array(&root, "packages")? {
            let (Some(id), Some(name)) = (
                package.get("id").and_then(Value::as_str),
                package.get("name").and_then(Value::as_str),
            ) else {
                return Err("a package carries no `id` or no `name`".to_string());
            };
            name_of.insert(id, name);
            if let Some(manifest) = package.get("manifest_path").and_then(Value::as_str) {
                manifest_of.insert(id, manifest);
            }
        }
        let named = |id: &str| -> Result<String, String> {
            name_of
                .get(id)
                .map(|name| (*name).to_string())
                .ok_or_else(|| format!("package `{id}` has no entry in `packages`"))
        };

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
                .map(|id| named(id))
                .collect::<Result<_, _>>()?,
            ..WorkspaceGraph::default()
        };

        for package in array(&root, "packages")? {
            let id = package
                .get("id")
                .and_then(Value::as_str)
                .ok_or("a package carries no `id`")?;
            if !member_ids.contains(id) {
                continue;
            }
            if let Some(manifest) = package.get("manifest_path").and_then(Value::as_str) {
                let manifest = PathBuf::from(manifest);
                let directory = manifest
                    .parent()
                    .ok_or_else(|| format!("`{}` names no directory", manifest.display()))?;
                graph.member_directories.insert(directory.to_path_buf());
            }
            let name = named(id)?;
            if let Some(features) = package.get("features").and_then(Value::as_object) {
                for feature in features.keys() {
                    graph
                        .declared_features
                        .insert((name.clone(), feature.clone()));
                }
            }
        }

        let resolve = root
            .get("resolve")
            .ok_or("cargo metadata carries no resolve graph, so no edge can be read")?;
        let mut patched: Option<BTreeSet<String>> = None;
        for node in array(resolve, "nodes")? {
            let id = node
                .get("id")
                .and_then(Value::as_str)
                .ok_or("a resolve node carries no `id`")?;
            if !member_ids.contains(id) {
                continue;
            }
            let from = named(id)?;
            for dep in array(node, "deps")? {
                let Some(pkg) = dep.get("pkg").and_then(Value::as_str) else {
                    return Err("a resolve edge carries no `pkg`".to_string());
                };
                let member = member_ids.contains(pkg);
                let to = named(pkg)?;
                let vendored = is_vendored_package(
                    manifest_of.get(pkg).copied(),
                    &to,
                    &graph.root,
                    &mut patched,
                )?;
                let local = is_local_package(pkg) && !vendored;
                for dep_kind in array(dep, "dep_kinds")? {
                    let edge = (from.clone(), to.clone());
                    let kind = dep_kind.get("kind").and_then(Value::as_str);
                    // A normal dependency reports no kind at all.
                    if matches!(kind, None | Some("build")) {
                        graph.linked.insert(edge.clone());
                    }
                    match (member, kind) {
                        (true, None) => {
                            graph.normal.insert(edge);
                        }
                        (true, Some("build")) => {
                            graph.build.insert(edge);
                        }
                        (false, None | Some("build")) if local => {
                            graph.unearned.insert(edge);
                        }
                        _ => {}
                    }
                }
            }
        }
        Ok(graph)
    }

    /// Every way this one reading departs from the allowlist on its own
    /// terms, whatever any other reading shows. Each of these is a failure
    /// under any selection.
    pub fn local_violations(&self) -> Vec<String> {
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

        for (from, to) in &self.build {
            problems.push(format!(
                "`{from}` build-depends on `{to}`; the allowlist is a set of normal edges, and a \
                 build-dependency between members is not one of them"
            ));
        }

        for (from, to) in &self.unearned {
            problems.push(format!(
                "`{from}` depends on the local crate `{to}`, which the workspace does not hold as \
                 a member; a crate outside the member set is outside the allowlist, the lint \
                 ruleset and every test the workspace runs"
            ));
        }

        problems
    }

    /// Allowlist edges that both endpoints being present makes mandatory, and
    /// that this reading does not carry.
    pub fn missing_edges(&self) -> Vec<String> {
        missing_edges(
            &self.members.iter().map(String::as_str).collect(),
            &self
                .normal
                .iter()
                .map(|(from, to)| (from.as_str(), to.as_str()))
                .collect(),
            "the workspace does not carry the edge",
        )
    }

    /// Every way this graph departs from the allowlist, one line each. A
    /// conforming workspace produces none.
    pub fn violations(&self) -> Vec<String> {
        let mut problems = self.local_violations();
        problems.extend(self.missing_edges());
        problems
    }
}

/// Every selection's reading of the workspace, judged together.
///
/// The two directions are asymmetric on purpose. **An edge outside the
/// allowlist fails if any selection shows it** — a violation hidden behind a
/// feature flag is still a violation. **A required edge is satisfied if any
/// selection shows it** — an edge that only exists when a feature is on is
/// still an edge, and demanding it under the default resolution would demand
/// that no permitted edge is ever optional. The unknown-crate,
/// build-dependency and unearned-local-crate checks run under every
/// selection, because each is a failure on its own terms.
pub struct MatrixReading {
    readings: Vec<(&'static str, WorkspaceGraph)>,
}

impl MatrixReading {
    /// Read the workspace once per selection.
    pub fn read(matrix: &'static [FeatureSelection]) -> Result<Self, String> {
        if matrix.is_empty() {
            return Err("the feature matrix names no selection, so the gate reads nothing".into());
        }
        let mut readings = Vec::new();
        for selection in matrix {
            readings.push((selection.name, WorkspaceGraph::read(selection)?));
        }
        Ok(MatrixReading { readings })
    }

    /// A reading assembled from graphs already in hand, for exercising the
    /// across-selection judgment without running cargo. Not a constructor the
    /// gate offers: [`MatrixReading::read`] is the one that guarantees at
    /// least one reading, which every accessor here relies on.
    #[cfg(test)]
    fn from_readings(readings: Vec<(&'static str, WorkspaceGraph)>) -> Self {
        MatrixReading { readings }
    }

    pub fn readings(&self) -> &[(&'static str, WorkspaceGraph)] {
        &self.readings
    }

    /// The reading taken under the named selection, if the matrix names one.
    pub fn under(&self, selection: &str) -> Option<&WorkspaceGraph> {
        self.readings
            .iter()
            .find(|(name, _)| *name == selection)
            .map(|(_, graph)| graph)
    }

    /// The workspace root, as the first selection reported it.
    pub fn root(&self) -> &Path {
        &self.readings[0].1.root
    }

    /// Every feature any selection saw a member declare.
    pub fn declared_features(&self) -> BTreeSet<(String, String)> {
        self.readings
            .iter()
            .flat_map(|(_, graph)| graph.declared_features.iter().cloned())
            .collect()
    }

    /// Every way the workspace departs from the allowlist across the whole
    /// matrix, one line each.
    pub fn violations(&self) -> Vec<String> {
        let mut problems = Vec::new();
        for (selection, graph) in &self.readings {
            for problem in graph.local_violations() {
                problems.push(format!("under the `{selection}` selection: {problem}"));
            }
        }
        let members: BTreeSet<&str> = self
            .readings
            .iter()
            .flat_map(|(_, graph)| graph.members.iter().map(String::as_str))
            .collect();
        let normal: BTreeSet<(&str, &str)> = self
            .readings
            .iter()
            .flat_map(|(_, graph)| {
                graph
                    .normal
                    .iter()
                    .map(|(from, to)| (from.as_str(), to.as_str()))
            })
            .collect();
        problems.extend(missing_edges(
            &members,
            &normal,
            "no selection in the matrix carries the edge",
        ));
        problems
    }
}

fn missing_edges(
    members: &BTreeSet<&str>,
    normal: &BTreeSet<(&str, &str)>,
    complaint: &str,
) -> Vec<String> {
    let mut problems = Vec::new();
    for (from, to) in ALLOWED_EDGES {
        if !members.contains(from) || !members.contains(to) {
            continue;
        }
        if !normal.contains(&(*from, *to)) {
            problems.push(format!(
                "the allowlist permits `{from}` -> `{to}` and both crates are present, but \
                 {complaint}"
            ));
        }
    }
    problems
}

/// Every dependency an [isolated crate](ISOLATED_CRATES) links under this
/// reading, one line each. A conforming workspace produces none.
///
/// Read this against the [default](DEFAULT_SELECTION) reading and no other.
/// A selection passing `--all-features` turns on the optional dependency an
/// isolated crate is allowed to have behind a feature, which is the whole
/// arrangement working rather than a violation of it.
pub fn isolation_violations(graph: &WorkspaceGraph) -> Vec<String> {
    let isolated: BTreeSet<&str> = ISOLATED_CRATES.iter().copied().collect();
    graph
        .linked
        .iter()
        .filter(|(from, _)| isolated.contains(from.as_str()))
        .map(|(from, to)| {
            format!(
                "`{from}` links `{to}` in a default build. `{from}` links nothing outside a \
                 test run: a heavy dependency belongs behind a feature, and a feature a \
                 dependent turns on is a dependency of every build that reaches it"
            )
        })
        .collect()
}

/// Every declared feature the matrix cannot turn on, one line each.
///
/// A selection passing `--all-features` covers everything by construction, so
/// with that entry present this check is quiet. It is here for the day
/// somebody narrows the matrix: a feature nobody selects hides every edge
/// behind it, and the gate would go green without ever having looked.
pub fn feature_coverage_violations(
    declared: &BTreeSet<(String, String)>,
    matrix: &[FeatureSelection],
) -> Vec<String> {
    if matrix
        .iter()
        .any(|selection| selection.args.contains(&"--all-features"))
    {
        return Vec::new();
    }
    let mut selected: BTreeSet<String> = BTreeSet::new();
    selected.insert("default".to_string());
    for selection in matrix {
        let mut args = selection.args.iter().copied();
        while let Some(arg) = args.next() {
            let list = match arg {
                "--features" | "-F" => args.next(),
                _ => arg.strip_prefix("--features="),
            };
            for feature in list.unwrap_or("").split([',', ' ']) {
                if !feature.is_empty() {
                    selected.insert(feature.to_string());
                }
            }
        }
    }
    declared
        .iter()
        .filter(|(name, feature)| {
            !selected.contains(feature) && !selected.contains(&format!("{name}/{feature}"))
        })
        .map(|(name, feature)| {
            format!(
                "`{name}` declares the `{feature}` feature and no selection in the matrix turns \
                 it on, so every edge behind it is invisible to the gate"
            )
        })
        .collect()
}

/// Directories under `crates/` that hold a manifest the workspace does not
/// carry as a member, one line each.
pub fn exclusion_gaps(
    crate_directories: &BTreeSet<PathBuf>,
    member_directories: &BTreeSet<PathBuf>,
) -> Vec<String> {
    crate_directories
        .difference(member_directories)
        .map(|directory| {
            format!(
                "`{}` holds a manifest and is not a workspace member; an excluded crate takes \
                 its edges, its lints and its tests out of every gate",
                directory.display()
            )
        })
        .collect()
}

/// Every directory under `<root>/crates/`, at whatever depth, that holds a
/// `Cargo.toml`. A directory that holds one claims its whole subtree: the
/// scan does not descend past it, so a workspace nested inside a crate is
/// not reported as more crates.
#[allow(clippy::disallowed_methods)] // Reading the workspace's own layout, which is this gate's subject.
pub fn crate_directories(root: &Path) -> Result<BTreeSet<PathBuf>, String> {
    let mut directories = BTreeSet::new();
    scan_for_manifests(&root.join("crates"), &mut directories)?;
    Ok(directories)
}

#[allow(clippy::disallowed_methods)] // Reading the workspace's own layout, which is this gate's subject.
fn scan_for_manifests(dir: &Path, found: &mut BTreeSet<PathBuf>) -> Result<(), String> {
    let entries =
        std::fs::read_dir(dir).map_err(|e| format!("could not read {}: {e}", dir.display()))?;
    for entry in entries {
        let entry = entry.map_err(|e| format!("could not read {}: {e}", dir.display()))?;
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        if path.join("Cargo.toml").is_file() {
            found.insert(path);
        } else {
            scan_for_manifests(&path, found)?;
        }
    }
    Ok(())
}

/// The text of `<root>/docs/architecture.md`, which the tables here are
/// transcribed from.
#[allow(clippy::disallowed_methods)] // Reading the document this gate transcribes.
pub fn read_architecture_document(root: &Path) -> Result<String, String> {
    let path = root.join("docs").join("architecture.md");
    std::fs::read_to_string(&path).map_err(|e| format!("could not read {}: {e}", path.display()))
}

/// The crate map and the dependency allowlist, as `docs/architecture.md`
/// states them.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DocumentedMap {
    pub crates: BTreeSet<String>,
    pub edges: BTreeSet<(String, String)>,
}

/// Parse the crate table and the edge table out of the architecture
/// document.
///
/// The reader is narrow and loud: it takes the two tables by the sentence
/// that introduces each, requires the exact column count, requires at least
/// one data row, and reads crate names as the backtick-quoted spans of a
/// cell — so `` `norn` (bin) `` names `norn` and a cell reading *(none)*
/// names nothing. A change to the document's table shape fails this parse
/// rather than quietly producing an empty set.
pub fn documented_map(markdown: &str) -> Result<DocumentedMap, String> {
    let mut map = DocumentedMap::default();

    for row in table_after(markdown, "### The crates", &["Crate", "Owns", "Earned by"])? {
        let names = quoted(&row[0]);
        if names.len() != 1 {
            return Err(format!(
                "the crate table's row `{}` names {} crates, and a row names exactly one",
                row[0].trim(),
                names.len()
            ));
        }
        map.crates.insert(names[0].clone());
    }

    for row in table_after(
        markdown,
        "Written out, the permitted edges are exactly:",
        &["From", "To"],
    )? {
        let (from, to) = (quoted(&row[0]), quoted(&row[1]));
        if from.is_empty() {
            return Err(format!(
                "the edge table's row `{}` names no crate on the left",
                row[0].trim()
            ));
        }
        for source in &from {
            for target in &to {
                map.edges.insert((source.clone(), target.clone()));
            }
        }
    }

    Ok(map)
}

/// The numbers of the boundary invariants the document states.
pub fn documented_invariants(markdown: &str) -> Result<BTreeSet<u8>, String> {
    let mut numbers = BTreeSet::new();
    let mut inside = false;
    for line in markdown.lines() {
        if line.starts_with("### ") {
            inside = line.trim() == "### Boundary invariants";
            continue;
        }
        if !inside {
            continue;
        }
        let Some((head, rest)) = line.split_once(". ") else {
            continue;
        };
        if !rest.starts_with("**") {
            continue;
        }
        if let Ok(number) = head.parse::<u8>() {
            numbers.insert(number);
        }
    }
    if numbers.is_empty() {
        return Err("the document states no numbered boundary invariant".to_string());
    }
    Ok(numbers)
}

/// The data rows of the markdown table that follows `anchor`, whose header
/// must read exactly `header`.
fn table_after(markdown: &str, anchor: &str, header: &[&str]) -> Result<Vec<Vec<String>>, String> {
    let columns = header.len();
    let mut lines = markdown
        .lines()
        .skip_while(|line| !line.trim().starts_with(anchor));
    lines
        .next()
        .ok_or_else(|| format!("the document carries no line opening `{anchor}`"))?;

    let mut lines = lines.skip_while(|line| line.trim().is_empty());
    let found: Vec<String> = cells(lines.next().unwrap_or_default())
        .iter()
        .map(|cell| cell.trim().to_string())
        .collect();
    if found != header {
        return Err(format!(
            "the table under `{anchor}` has the columns {found:?} and the reader expects \
             {header:?}"
        ));
    }
    let delimiter = cells(lines.next().unwrap_or_default());
    if delimiter.len() != columns || !delimiter.iter().all(|cell| cell.trim().starts_with("---")) {
        return Err(format!(
            "the table under `{anchor}` carries no header delimiter"
        ));
    }

    let mut rows = Vec::new();
    for line in lines {
        if !line.trim_start().starts_with('|') {
            break;
        }
        let row = cells(line);
        if row.len() != columns {
            return Err(format!(
                "a row of the table under `{anchor}` has {} cells and the reader expects \
                 {columns}: {line}",
                row.len()
            ));
        }
        rows.push(row);
    }
    if rows.is_empty() {
        return Err(format!("the table under `{anchor}` has no rows"));
    }
    Ok(rows)
}

fn cells(line: &str) -> Vec<String> {
    let line = line.trim();
    if !line.starts_with('|') {
        return Vec::new();
    }
    line.trim_matches('|')
        .split('|')
        .map(str::to_string)
        .collect()
}

/// The backtick-quoted spans of a cell, in order.
fn quoted(cell: &str) -> Vec<String> {
    let mut spans = Vec::new();
    let mut rest = cell;
    while let Some((_, after)) = rest.split_once('`') {
        let Some((span, tail)) = after.split_once('`') else {
            break;
        };
        spans.push(span.to_string());
        rest = tail;
    }
    spans
}

/// Whether a package id names a crate on this filesystem rather than one from
/// a registry. Cargo writes the current id as `path+file://…` and the older
/// one as `name version (path+file://…)`.
fn is_local_package(id: &str) -> bool {
    id.starts_with("path+file://") || id.contains("(path+file://")
}

/// Whether a local package is a patched copy of a published release rather
/// than a crate of this workspace.
///
/// Two things have to hold, and neither alone is the exemption. The package's
/// manifest sits under `<root>/vendor/`, and the root manifest replaces a
/// crate of that name through `[patch.crates-io]`. The second is what makes
/// the first mean anything: the directory is only a patch surface because the
/// patch table points at it, so a local crate parked under `vendor/` and wired
/// in as an ordinary path dependency is a local crate the workspace excludes.
///
/// A package that satisfies both is the same third-party crate the registry
/// release was, so the edge to it is not the allowlist's subject and the
/// package is not a crate the crate map names.
///
/// The manifest path is read from the package's own `manifest_path`, not from
/// its id: cargo percent-encodes the path in an id, so a workspace under a
/// directory with a space in its name has an id that matches no real path.
///
/// The patch table is read at most once, and only when a candidate reaches
/// this far, so a reading over metadata whose workspace root is not on this
/// filesystem never touches the disk.
fn is_vendored_package(
    manifest_path: Option<&str>,
    name: &str,
    root: &Path,
    patched: &mut Option<BTreeSet<String>>,
) -> Result<bool, String> {
    let Some(manifest) = manifest_path else {
        return Ok(false);
    };
    if !Path::new(manifest).starts_with(root.join("vendor")) {
        return Ok(false);
    }
    let patched = match patched {
        Some(known) => known,
        empty => empty.insert(patched_crates(root)?),
    };
    Ok(patched.contains(name))
}

/// The crate names the workspace root manifest replaces through
/// `[patch.crates-io]`.
#[allow(clippy::disallowed_methods)] // Reading the workspace's own layout, which is this gate's subject.
fn patched_crates(root: &Path) -> Result<BTreeSet<String>, String> {
    let manifest = root.join("Cargo.toml");
    let text = std::fs::read_to_string(&manifest)
        .map_err(|e| format!("cannot read `{}`: {e}", manifest.display()))?;
    let document: toml::Table = text
        .parse()
        .map_err(|e| format!("`{}` is not readable as TOML: {e}", manifest.display()))?;
    Ok(document
        .get("patch")
        .and_then(|patch| patch.get("crates-io"))
        .and_then(toml::Value::as_table)
        .map(|table| table.keys().cloned().collect())
        .unwrap_or_default())
}

/// Run `cargo metadata` under `args` and hand back its JSON.
///
/// The cargo that built this test is the cargo that reads the workspace, so
/// the reading cannot come from a different toolchain than the one under
/// test.
pub fn cargo_metadata(args: &[&str]) -> Result<String, String> {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
    let output = Command::new(&cargo)
        .args(["metadata", "--format-version", "1", "--locked"])
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
            ..WorkspaceGraph::default()
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
        violation_mentions(&graph, "the workspace does not carry the edge");
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
          "workspace_members": ["path+file:///workspace/crates/norn#0.0.0",
                                "path+file:///workspace/crates/norn-testkit#0.0.0"],
          "packages": [
            {"id": "path+file:///workspace/crates/norn#0.0.0", "name": "norn",
             "manifest_path": "/workspace/crates/norn/Cargo.toml", "features": {}},
            {"id": "path+file:///workspace/crates/norn-testkit#0.0.0", "name": "norn-testkit",
             "manifest_path": "/workspace/crates/norn-testkit/Cargo.toml", "features": {}}
          ],
          "resolve": {"nodes": [
            {"id": "path+file:///workspace/crates/norn#0.0.0", "deps": [
              {"pkg": "path+file:///workspace/crates/norn-testkit#0.0.0",
               "dep_kinds": [{"kind": "dev"}]}
            ]},
            {"id": "path+file:///workspace/crates/norn-testkit#0.0.0", "deps": []}
          ]}
        }"#;
        let graph = WorkspaceGraph::parse(metadata).expect("parsing metadata");
        assert_eq!(graph.members.len(), 2);
        assert!(graph.normal.is_empty());
        assert!(graph.build.is_empty());
        assert!(graph.unearned.is_empty());
        assert_eq!(
            graph.member_directories,
            [
                PathBuf::from("/workspace/crates/norn"),
                PathBuf::from("/workspace/crates/norn-testkit"),
            ]
            .into_iter()
            .collect()
        );
    }

    #[test]
    fn an_edge_leaving_the_workspace_is_not_the_allowlists_subject() {
        let metadata = r#"{
          "workspace_root": "/workspace",
          "workspace_members": ["path+file:///workspace/crates/norn-wire#0.0.0"],
          "packages": [
            {"id": "path+file:///workspace/crates/norn-wire#0.0.0", "name": "norn-wire",
             "manifest_path": "/workspace/crates/norn-wire/Cargo.toml"},
            {"id": "registry+https://github.com/rust-lang/crates.io-index#serde@1.0.0",
             "name": "serde"}
          ],
          "resolve": {"nodes": [
            {"id": "path+file:///workspace/crates/norn-wire#0.0.0", "deps": [
              {"pkg": "registry+https://github.com/rust-lang/crates.io-index#serde@1.0.0",
               "dep_kinds": [{"kind": null}]}
            ]}
          ]}
        }"#;
        let graph = WorkspaceGraph::parse(metadata).expect("parsing metadata");
        assert!(graph.normal.is_empty());
        assert!(graph.unearned.is_empty());
        assert_eq!(graph.violations(), Vec::<String>::new());
    }

    /// A local crate the workspace excludes is not a member, so its edges,
    /// its lints and its tests are nobody's subject. A member reaching one is
    /// the gate's business even though neither endpoint pair is in the graph.
    #[test]
    fn a_member_depending_on_a_local_crate_the_workspace_excludes_fails() {
        let metadata = r#"{
          "workspace_root": "/workspace",
          "workspace_members": ["path+file:///workspace/crates/norn-host#0.0.0"],
          "packages": [
            {"id": "path+file:///workspace/crates/norn-host#0.0.0", "name": "norn-host",
             "manifest_path": "/workspace/crates/norn-host/Cargo.toml"},
            {"id": "path+file:///workspace/crates/norn-secret#0.0.0", "name": "norn-secret",
             "manifest_path": "/workspace/crates/norn-secret/Cargo.toml"}
          ],
          "resolve": {"nodes": [
            {"id": "path+file:///workspace/crates/norn-host#0.0.0", "deps": [
              {"pkg": "path+file:///workspace/crates/norn-secret#0.0.0",
               "dep_kinds": [{"kind": null}]}
            ]}
          ]}
        }"#;
        let graph = WorkspaceGraph::parse(metadata).expect("parsing metadata");
        assert_eq!(
            graph.unearned,
            [("norn-host".to_string(), "norn-secret".to_string())]
                .into_iter()
                .collect()
        );
        violation_mentions(&graph, "which the workspace does not hold as a member");
    }

    /// A workspace on disk whose root manifest is `patch`, for the vendor
    /// exemption's two halves.
    ///
    /// The directory name carries a space on purpose. Cargo percent-encodes
    /// the path inside a package id, so a reading that compared ids against
    /// the workspace root would find no vendored package here at all — and
    /// would report the exemption's absence as a workspace violation, on a
    /// machine whose only peculiarity is where it keeps its checkout.
    #[allow(clippy::disallowed_methods)] // Building the tree the reader is tested against.
    fn workspace_root_with_patch_table(label: &str, patch: &str) -> PathBuf {
        let root =
            std::env::temp_dir().join(format!("norn arch gate-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("a scratch workspace root");
        std::fs::write(root.join("Cargo.toml"), patch).expect("a root manifest");
        root
    }

    /// The metadata one member and one package under `vendor/` produce, with
    /// the id spelled the way cargo spells it: percent-encoded.
    fn vendored_metadata(root: &Path) -> String {
        let encoded = root.display().to_string().replace(' ', "%20");
        format!(
            r#"{{
          "workspace_root": "{root}",
          "workspace_members": ["path+file://{encoded}/crates/norn-fs#0.0.0"],
          "packages": [
            {{"id": "path+file://{encoded}/crates/norn-fs#0.0.0", "name": "norn-fs",
             "manifest_path": "{root}/crates/norn-fs/Cargo.toml"}},
            {{"id": "path+file://{encoded}/vendor/notify#8.2.0", "name": "notify",
             "manifest_path": "{root}/vendor/notify/Cargo.toml"}}
          ],
          "resolve": {{"nodes": [
            {{"id": "path+file://{encoded}/crates/norn-fs#0.0.0", "deps": [
              {{"pkg": "path+file://{encoded}/vendor/notify#8.2.0",
               "dep_kinds": [{{"kind": null}}]}}
            ]}}
          ]}}
        }}"#,
            root = root.display()
        )
    }

    /// A patched release under `vendor/` is the third-party crate it patches,
    /// so a member reaching it reaches no crate of this workspace.
    ///
    /// The two neighbouring cases are what keep this narrow: the exemption
    /// reads one directory *and* the patch table, and a local crate that
    /// misses either is still a violation.
    #[test]
    #[allow(clippy::disallowed_methods)] // Cleaning up the tree the reader is tested against.
    fn a_member_depending_on_a_vendored_release_is_not_reaching_a_workspace_crate() {
        let root = workspace_root_with_patch_table(
            "patched",
            "[patch.crates-io]\nnotify = { path = \"vendor/notify\" }\n",
        );
        let graph = WorkspaceGraph::parse(&vendored_metadata(&root)).expect("parsing metadata");
        let _ = std::fs::remove_dir_all(&root);

        assert!(graph.unearned.is_empty());
        assert!(graph.normal.is_empty());
        assert!(
            graph
                .linked
                .contains(&("norn-fs".to_string(), "notify".to_string())),
            "a vendored release a member links must still be visible to the isolation rule"
        );
        assert_eq!(graph.violations(), Vec::<String>::new());
    }

    /// **The bar under the vendor exemption.** A package under `vendor/` that
    /// the patch table does not name is a local crate the workspace excludes.
    ///
    /// The forbidden shape is a directory-shaped exemption. Under one, any
    /// path dependency moved beneath `vendor/` leaves the allowlist, the lint
    /// ruleset and every test the workspace runs, and the gate reports
    /// nothing. What earns the exemption is the patch table: it is what makes
    /// the directory a replacement for a published release.
    #[test]
    #[allow(clippy::disallowed_methods)] // Cleaning up the tree the reader is tested against.
    fn a_vendored_package_the_patch_table_does_not_name_still_fails() {
        let root = workspace_root_with_patch_table(
            "unpatched",
            "[patch.crates-io]\nserde = { path = \"vendor/serde\" }\n",
        );
        let graph = WorkspaceGraph::parse(&vendored_metadata(&root)).expect("parsing metadata");
        let _ = std::fs::remove_dir_all(&root);

        assert_eq!(
            graph.unearned,
            [("norn-fs".to_string(), "notify".to_string())]
                .into_iter()
                .collect()
        );
        violation_mentions(&graph, "which the workspace does not hold as a member");
    }

    /// The same, spelled as a build dependency: an excluded crate reached
    /// from a build script is reached all the same.
    #[test]
    fn a_build_edge_to_a_local_crate_the_workspace_excludes_fails() {
        let metadata = r#"{
          "workspace_root": "/workspace",
          "workspace_members": ["path+file:///workspace/crates/norn-host#0.0.0"],
          "packages": [
            {"id": "path+file:///workspace/crates/norn-host#0.0.0", "name": "norn-host"},
            {"id": "path+file:///elsewhere/norn-secret#0.0.0", "name": "norn-secret"}
          ],
          "resolve": {"nodes": [
            {"id": "path+file:///workspace/crates/norn-host#0.0.0", "deps": [
              {"pkg": "path+file:///elsewhere/norn-secret#0.0.0",
               "dep_kinds": [{"kind": "build"}]}
            ]}
          ]}
        }"#;
        let graph = WorkspaceGraph::parse(metadata).expect("parsing metadata");
        violation_mentions(&graph, "which the workspace does not hold as a member");
    }

    /// A dev-dependency on a local non-member is the fixture-generator shape
    /// and is not this check's subject.
    #[test]
    fn a_development_edge_to_a_local_non_member_is_not_a_violation() {
        let metadata = r#"{
          "workspace_root": "/workspace",
          "workspace_members": ["path+file:///workspace/crates/norn-host#0.0.0"],
          "packages": [
            {"id": "path+file:///workspace/crates/norn-host#0.0.0", "name": "norn-host"},
            {"id": "path+file:///elsewhere/helper#0.0.0", "name": "helper"}
          ],
          "resolve": {"nodes": [
            {"id": "path+file:///workspace/crates/norn-host#0.0.0", "deps": [
              {"pkg": "path+file:///elsewhere/helper#0.0.0", "dep_kinds": [{"kind": "dev"}]}
            ]}
          ]}
        }"#;
        let graph = WorkspaceGraph::parse(metadata).expect("parsing metadata");
        assert!(graph.unearned.is_empty());
    }

    #[test]
    fn a_members_declared_features_are_read() {
        let metadata = r#"{
          "workspace_root": "/workspace",
          "workspace_members": ["path+file:///workspace/crates/norn-embed#0.0.0"],
          "packages": [
            {"id": "path+file:///workspace/crates/norn-embed#0.0.0", "name": "norn-embed",
             "features": {"norn-fixtures": ["dep:norn-fixtures"], "soak": ["norn-fixtures"]}}
          ],
          "resolve": {"nodes": [
            {"id": "path+file:///workspace/crates/norn-embed#0.0.0", "deps": []}
          ]}
        }"#;
        let graph = WorkspaceGraph::parse(metadata).expect("parsing metadata");
        assert_eq!(
            graph.declared_features,
            [
                ("norn-embed".to_string(), "norn-fixtures".to_string()),
                ("norn-embed".to_string(), "soak".to_string()),
            ]
            .into_iter()
            .collect()
        );
    }

    /// The shape the manifest half of the rule cannot see: `norn-embed`
    /// declares its runtime `optional = true` behind a feature — which its
    /// own isolation test passes on — and a consumer turns that feature on.
    /// Cargo resolves the dependency into the default build, and the edge is
    /// there to be read.
    #[test]
    fn a_dependency_a_consumer_switched_on_fails_an_isolated_crate() {
        let metadata = r#"{
          "workspace_root": "/workspace",
          "workspace_members": ["path+file:///workspace/crates/norn-embed#0.0.0",
                                "path+file:///workspace/crates/norn-host#0.0.0"],
          "packages": [
            {"id": "path+file:///workspace/crates/norn-embed#0.0.0", "name": "norn-embed"},
            {"id": "path+file:///workspace/crates/norn-host#0.0.0", "name": "norn-host"},
            {"id": "registry+https://github.com/rust-lang/crates.io-index#a-runtime@1.0.0",
             "name": "a-runtime"},
            {"id": "registry+https://github.com/rust-lang/crates.io-index#toml@1.0.0",
             "name": "toml"}
          ],
          "resolve": {"nodes": [
            {"id": "path+file:///workspace/crates/norn-embed#0.0.0", "deps": [
              {"pkg": "registry+https://github.com/rust-lang/crates.io-index#a-runtime@1.0.0",
               "dep_kinds": [{"kind": null}]},
              {"pkg": "registry+https://github.com/rust-lang/crates.io-index#toml@1.0.0",
               "dep_kinds": [{"kind": "dev"}]}
            ]},
            {"id": "path+file:///workspace/crates/norn-host#0.0.0", "deps": [
              {"pkg": "path+file:///workspace/crates/norn-embed#0.0.0",
               "dep_kinds": [{"kind": null}]}
            ]}
          ]}
        }"#;
        let graph = WorkspaceGraph::parse(metadata).expect("parsing metadata");
        let problems = isolation_violations(&graph);
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(
            problems[0].contains("`norn-embed` links `a-runtime`"),
            "{problems:?}"
        );
        assert_eq!(
            graph.violations(),
            Vec::<String>::new(),
            "the allowlist's subject is member edges, and this one is permitted"
        );
    }

    /// A development dependency is in no build that is not a test run, and
    /// `norn-embed`'s own manifest reader is one.
    #[test]
    fn a_development_dependency_does_not_break_isolation() {
        let metadata = r#"{
          "workspace_root": "/workspace",
          "workspace_members": ["path+file:///workspace/crates/norn-embed#0.0.0"],
          "packages": [
            {"id": "path+file:///workspace/crates/norn-embed#0.0.0", "name": "norn-embed"},
            {"id": "registry+https://github.com/rust-lang/crates.io-index#toml@1.0.0",
             "name": "toml"}
          ],
          "resolve": {"nodes": [
            {"id": "path+file:///workspace/crates/norn-embed#0.0.0", "deps": [
              {"pkg": "registry+https://github.com/rust-lang/crates.io-index#toml@1.0.0",
               "dep_kinds": [{"kind": "dev"}]}
            ]}
          ]}
        }"#;
        let graph = WorkspaceGraph::parse(metadata).expect("parsing metadata");
        assert!(graph.linked.is_empty());
        assert_eq!(isolation_violations(&graph), Vec::<String>::new());
    }

    #[test]
    fn every_isolated_crate_is_a_crate_the_map_names() {
        let known: BTreeSet<&str> = WORKSPACE_CRATES.iter().copied().collect();
        for name in ISOLATED_CRATES {
            assert!(known.contains(name), "`{name}` is not a known crate");
        }
    }

    #[test]
    fn the_matrix_names_the_default_selection() {
        assert!(
            FEATURE_MATRIX
                .iter()
                .any(|selection| selection.name == DEFAULT_SELECTION),
            "the isolation reading is taken under the default selection, and \
             the matrix does not name one"
        );
    }

    #[test]
    fn a_reading_is_found_by_the_selection_it_was_taken_under() {
        let reading = matrix(&[("default", conforming())]);
        assert!(reading.under(DEFAULT_SELECTION).is_some());
        assert!(reading.under("all features").is_none());
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
    fn the_feature_matrix_names_an_all_features_selection() {
        for selection in FEATURE_MATRIX {
            assert!(!selection.name.is_empty());
        }
        assert!(
            FEATURE_MATRIX
                .iter()
                .any(|selection| selection.args.contains(&"--all-features")),
            "no selection turns every feature on, so a feature-gated edge would be invisible"
        );
    }

    fn matrix(readings: &[(&'static str, WorkspaceGraph)]) -> MatrixReading {
        MatrixReading::from_readings(readings.to_vec())
    }

    /// An edge that only one selection shows is still an edge, and still
    /// fails when the allowlist does not carry it.
    #[test]
    fn an_edge_visible_under_one_selection_only_fails_the_matrix() {
        let mut behind_a_feature = conforming();
        behind_a_feature
            .normal
            .insert(("norn-store".to_string(), "norn-host".to_string()));
        let reading = matrix(&[
            ("default", conforming()),
            ("all features", behind_a_feature),
        ]);
        let problems = reading.violations();
        assert!(
            problems.iter().any(|p| p.contains(
                "under the `all features` selection: `norn-store` depends on `norn-host`"
            )),
            "{problems:?}"
        );
    }

    /// The other direction: a permitted edge that exists only when a feature
    /// is on satisfies the required direction, because demanding it under
    /// every selection would forbid a permitted edge from being optional.
    #[test]
    fn a_required_edge_shown_by_one_selection_satisfies_the_matrix() {
        let mut without = conforming();
        without
            .normal
            .remove(&("norn-store".to_string(), "norn-wire".to_string()));
        let reading = matrix(&[("default", without), ("all features", conforming())]);
        assert_eq!(reading.violations(), Vec::<String>::new());
    }

    #[test]
    fn a_required_edge_no_selection_shows_fails_the_matrix() {
        let mut without = conforming();
        without
            .normal
            .remove(&("norn-store".to_string(), "norn-wire".to_string()));
        let reading = matrix(&[("default", without.clone()), ("all features", without)]);
        let problems = reading.violations();
        assert!(
            problems
                .iter()
                .any(|p| p.contains("no selection in the matrix carries the edge")),
            "{problems:?}"
        );
    }

    #[test]
    fn a_feature_no_selection_turns_on_is_a_coverage_gap() {
        const NARROW: &[FeatureSelection] = &[FeatureSelection {
            name: "default",
            args: &[],
        }];
        let declared: BTreeSet<(String, String)> = [
            ("norn".to_string(), "default".to_string()),
            ("norn".to_string(), "soak".to_string()),
        ]
        .into_iter()
        .collect();
        let problems = feature_coverage_violations(&declared, NARROW);
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(problems[0].contains("`soak`"), "{problems:?}");

        const NAMED: &[FeatureSelection] = &[FeatureSelection {
            name: "soak",
            args: &["--features", "soak"],
        }];
        assert_eq!(
            feature_coverage_violations(&declared, NAMED),
            Vec::<String>::new()
        );
        assert_eq!(
            feature_coverage_violations(&declared, FEATURE_MATRIX),
            Vec::<String>::new()
        );
    }

    #[test]
    fn a_crate_directory_the_workspace_does_not_hold_is_an_exclusion_gap() {
        let present: BTreeSet<PathBuf> = ["/w/crates/norn", "/w/crates/norn-secret"]
            .into_iter()
            .map(PathBuf::from)
            .collect();
        let members: BTreeSet<PathBuf> =
            ["/w/crates/norn"].into_iter().map(PathBuf::from).collect();
        let problems = exclusion_gaps(&present, &members);
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(problems[0].contains("norn-secret"), "{problems:?}");
        assert_eq!(exclusion_gaps(&members, &members), Vec::<String>::new());
    }

    /// The directory reader against a tree built for it, so the scan is
    /// tested without waiting for somebody to exclude a real crate. The
    /// manifest under `crates/tools/secret` sits two levels down, with no
    /// manifest at `crates/tools` itself, so it is only found by recursing.
    #[test]
    #[allow(clippy::disallowed_methods)] // Building the tree the reader is tested against.
    fn the_crate_directory_reader_takes_directories_holding_a_manifest() {
        let root = std::env::temp_dir().join(format!("norn-crates-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for directory in [
            "crates/one",
            "crates/one/nested",
            "crates/two",
            "crates/notacrate",
            "crates/tools/secret",
        ] {
            std::fs::create_dir_all(root.join(directory)).expect("a scratch tree");
        }
        for directory in [
            "crates/one",
            "crates/one/nested",
            "crates/two",
            "crates/tools/secret",
        ] {
            std::fs::write(root.join(directory).join("Cargo.toml"), b"[package]\n")
                .expect("a manifest");
        }
        let found = crate_directories(&root).expect("reading the scratch tree");
        assert_eq!(
            found,
            [
                root.join("crates/one"),
                root.join("crates/two"),
                root.join("crates/tools/secret"),
            ]
            .into_iter()
            .collect()
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_document_reader_takes_both_tables() {
        let markdown = "\
### The crates

| Crate | Owns | Earned by |
|---|---|---|
| `norn-wire` | the vocabulary | one |
| `norn` (bin) | composition | two |

Written out, the permitted edges are exactly:

| From | To |
|---|---|
| `norn` (bin) | `norn-wire` |
| `norn-wire` | *(none)* |
";
        let map = documented_map(markdown).expect("both tables");
        assert_eq!(
            map.crates,
            ["norn".to_string(), "norn-wire".to_string()]
                .into_iter()
                .collect()
        );
        assert_eq!(
            map.edges,
            [("norn".to_string(), "norn-wire".to_string())]
                .into_iter()
                .collect()
        );
    }

    #[test]
    fn a_document_whose_table_changed_shape_fails_the_reader() {
        let missing = documented_map("nothing here").expect_err("no tables at all");
        assert!(missing.contains("carries no line opening"), "{missing}");

        let widened = "\
### The crates

| Crate | Owns | Earned by | Why |
|---|---|---|---|
| `norn` | a | b | c |
";
        let error = documented_map(widened).expect_err("a fourth column");
        assert!(error.contains("columns"), "{error}");

        let empty = "\
### The crates

| Crate | Owns | Earned by |
|---|---|---|

";
        let error = documented_map(empty).expect_err("a table with no rows");
        assert!(error.contains("no rows"), "{error}");
    }

    #[test]
    fn the_invariant_reader_takes_the_numbered_claims() {
        let markdown = "\
### Something else

1. **Not an invariant.**

### Boundary invariants

Thirteen invariants.

1. **The first claim.**
2. **The second claim.**

### After

3. **Not an invariant either.**
";
        assert_eq!(
            documented_invariants(markdown).expect("the numbered claims"),
            [1, 2].into_iter().collect()
        );
        assert!(documented_invariants("nothing").is_err());
    }
}
