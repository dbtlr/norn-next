//! The regression registry and its dormancy gate.
//!
//! The registry is the named-case record of every defect class this line
//! carries forward. A case states a **falsifiable present-tense property**
//! and is checked by counters and structural assertions over bytes — never by
//! comparing bytes to a recording, which is the coverage corpus's job and
//! carries no authority here.
//!
//! **A case is dormant until its venue lands.** `venue` names the first layer
//! at which a real test can bind the property, and dormancy is structural:
//! binding a case means editing its entry to name the tests that carry it, and
//! [`Registry::audit`] holds those names to functions that really compile into
//! the suite. A case at or below [`LAYER_LANDING`] is bindable against a
//! subject that exists, so leaving one dormant costs a stated reason.
//!
//! **The audit holds content, not only shape.** [`Registry::contract_digest`]
//! is one value over every case's name, kind, mandatory flag, venue, property,
//! sources and binding, pinned as a constant in the suite: an edit to any of
//! them moves it, so a gutted property, a swapped citation, a re-laned venue
//! and a shrunk binding each fail the gate the same way a deleted case does.
//! Test references are checked against cargo's own list of what compiled into
//! each target, so a reference naming a string literal, a function behind a
//! disabled `cfg`, or a file whose spelling differs only in case is refused.
//!
//! This module is the loader and the gate. The suite it gates lives in the
//! `norn` bin package, beside the registry data.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::fmt;
use std::path::{Component, Path, PathBuf};
use std::process::Command;

use norn_fixtures::digest::{Sha256, hex};
use serde::Deserialize;

use crate::json::{JsonError, read_json};

/// The cases that must be present, by name.
///
/// Pinned here rather than read from the registry, for the reason the corpus
/// pins its category lists: a case named in the data alone could be dropped
/// by editing the data. These five were ratified individually, and the audit
/// requires the registry to mark exactly these `mandatory` — so removing one
/// fails the gate whichever file the edit lands in.
pub const MANDATORY_CASES: &[&str] = &[
    "error-variant-matches-the-operation-reported",
    "forecast-and-apply-are-one-classifier",
    "mutation-honors-its-planned-flags",
    "one-display-source-per-semantic",
    "unknown-sort-or-projection-keys-never-silently-no-op",
];

/// The highest layer whose subject exists, and so the highest venue at which a
/// case may be bound.
///
/// A case at or below it could be carried by a real test today, which is what
/// makes leaving one dormant a statement rather than a wait: [`Registry::audit`]
/// requires every dormant case at or below this layer to say why. Raising it is
/// a reviewed edit made when the next layer starts landing, and it raises the
/// bar on every case that was already sitting there.
pub const LAYER_LANDING: u8 = 1;

/// The layers a case's venue names, indexed by layer number.
///
/// A venue is the first layer at which a real test can bind the property, not
/// the layer that will eventually own it: several cases bind at the substrate
/// and are re-asserted higher up. Layer 0 is the harness itself, so a layer-0
/// case is bindable against what exists today.
pub const VENUE_NAMES: &[&str] = &[
    "fixtures and testkit",
    "substrate",
    "lockdown",
    "queries",
    "mutations",
    "repair",
    "surfaces",
];

/// The prose records a case may cite by name rather than by identifier.
///
/// A citation is normally an identifier, and [`Registry::audit`] holds every
/// other source to that shape so a plausible-looking invention is refused.
/// Three records carry no identifiers at all — they are running logs and a
/// sweep of a board that no longer exists — so they are named here, and a
/// fourth spelling is a typo rather than a fourth record.
pub const NAMED_SOURCES: &[&str] = &[
    "frozen NRN board sweep",
    "norn-legacy annoyances log",
    "norn-legacy engineering learnings",
];

/// The `#[ignore]` reason prefixes that name a CI lane.
///
/// A bound case may be carried by an ignored test only when a lane adopts it,
/// because a lane runs a suite's ignored cases wholesale and is therefore the
/// thing that makes the carrier run at all. An `#[ignore]` for any other
/// reason means the binding names a test nothing executes.
///
/// Which file may use which prefix is decided by [`crate::lanes`], against the
/// table the file's own crate holds; this asks only that a bound carrier's
/// ignore reason names a lane.
pub const LANE_IGNORE_PREFIXES: &[&str] =
    &["counter-lane case:", "memory-lane case:", "soak-lane case:"];

/// What kind of obligation a case is.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub enum Kind {
    /// A misbehavior this line produced, stated as the property that forbids
    /// it.
    DefectClass,
    /// A shape that already satisfied the doctrine, kept so that a later
    /// change cannot quietly lose it.
    PositiveControl,
    /// A property about enforcement itself: how a guard, a budget or a
    /// comment failed to hold the thing it named.
    EnforcementClass,
}

impl fmt::Display for Kind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Kind::DefectClass => "defect-class",
            Kind::PositiveControl => "positive-control",
            Kind::EnforcementClass => "enforcement-class",
        };
        f.write_str(name)
    }
}

/// One layer of the venue scale, as the registry declares it.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Venue {
    pub layer: u8,
    pub name: String,
}

/// Whether a case is carried by tests today.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub enum BindingStatus {
    /// Named, and not yet carried by anything.
    Dormant,
    /// Carried by the tests the entry names.
    Bound,
}

/// A case's binding: the tests carrying it, or the reason it carries none.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Binding {
    pub status: BindingStatus,
    /// The tests that carry the property, as `<workspace-relative file>::<fn>`.
    #[serde(default)]
    pub tests: Vec<String>,
    /// Why a case that could bind today does not. Required of a dormant case
    /// at or below [`LAYER_LANDING`], because that is what exists.
    #[serde(default)]
    pub reason: Option<String>,
}

/// One named regression case.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Case {
    pub name: String,
    pub kind: Kind,
    /// Pinned in [`MANDATORY_CASES`] as well, so the data alone cannot drop
    /// it.
    #[serde(default)]
    pub mandatory: bool,
    /// The first layer at which a real test can bind the property.
    pub venue: u8,
    /// The falsifiable present-tense contract. Measured numbers appear where
    /// they sharpen it; provenance does not, because [`Case::sources`]
    /// carries that.
    pub property: String,
    /// The task, seed and artifact identifiers the class was mined from.
    pub sources: Vec<String>,
    pub binding: Binding,
}

/// The loaded registry.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Registry {
    pub note: String,
    pub venues: Vec<Venue>,
    pub cases: Vec<Case>,
}

/// A reference to a test that carries a case: a file and a function in it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TestRef {
    /// Workspace-relative, so one spelling resolves from anywhere.
    pub file: PathBuf,
    pub function: String,
}

impl TestRef {
    /// Read `<workspace-relative file>::<fn>`, or say what is wrong with it.
    ///
    /// The path is required to be relative and free of parent traversal: a
    /// reference that could point outside the workspace would resolve
    /// differently depending on where the suite ran from.
    pub fn parse(reference: &str) -> Result<TestRef, String> {
        let Some((file, function)) = reference.rsplit_once("::") else {
            return Err(format!(
                "`{reference}` is not a `<file>::<test fn>` reference"
            ));
        };
        if file.is_empty() || function.is_empty() {
            return Err(format!("`{reference}` names an empty file or function"));
        }
        let path = Path::new(file);
        if path.is_absolute() || path.components().any(|c| c.as_os_str() == "..") {
            return Err(format!(
                "`{file}` is not a workspace-relative path without parent traversal"
            ));
        }
        Ok(TestRef {
            file: path.to_path_buf(),
            function: function.to_string(),
        })
    }

    /// Which cargo target this reference's file compiles into, and the module
    /// path a test in it is listed under.
    ///
    /// Only the two shapes the registry can cite resolve: a file under a
    /// crate's `src/`, whose tests compile into that crate's library target,
    /// and a top-level file under a crate's `tests/`, which is an integration
    /// target of its own. A bench, an example, a binary and a nested file
    /// under `tests/` are all real cargo shapes and none of them is one this
    /// grammar names, so each is refused rather than guessed at.
    pub fn target(&self) -> Result<TargetRef, String> {
        let parts: Vec<&str> = self
            .file
            .components()
            .map(|c| c.as_os_str().to_str().unwrap_or(""))
            .collect();
        let [crates, package, kind, rest @ ..] = parts.as_slice() else {
            return Err(format!(
                "`{}` is not `crates/<package>/{{src,tests}}/…`",
                self.file.display()
            ));
        };
        if *crates != "crates" || rest.is_empty() {
            return Err(format!(
                "`{}` is not `crates/<package>/{{src,tests}}/…`",
                self.file.display()
            ));
        }
        let last = rest[rest.len() - 1];
        let Some(stem) = last.strip_suffix(".rs") else {
            return Err(format!(
                "`{}` is not a Rust source file",
                self.file.display()
            ));
        };
        match *kind {
            "src" => {
                // A test in `src/a/b.rs` is listed as `a::b::…`; one in
                // `src/a/mod.rs` as `a::…`; one in `src/lib.rs` at the root.
                let mut modules: Vec<&str> = rest[..rest.len() - 1].to_vec();
                if stem != "mod" && stem != "lib" {
                    modules.push(stem);
                }
                if stem == "main" {
                    return Err(format!(
                        "`{}` is a binary target, which this grammar does not name: cite a \
                         library or integration test",
                        self.file.display()
                    ));
                }
                let mut prefix = modules.join("::");
                if !prefix.is_empty() {
                    prefix.push_str("::");
                }
                Ok(TargetRef {
                    package: (*package).to_string(),
                    target: Target::Lib,
                    module_prefix: prefix,
                })
            }
            "tests" if rest.len() == 1 => Ok(TargetRef {
                package: (*package).to_string(),
                target: Target::Integration(stem.to_string()),
                module_prefix: String::new(),
            }),
            "tests" => Err(format!(
                "`{}` is a module of an integration target rather than a target: cite the \
                 `tests/<name>.rs` the test compiles into",
                self.file.display()
            )),
            other => Err(format!(
                "`{}` sits under `{other}/`, which is neither `src/` nor `tests/`",
                self.file.display()
            )),
        }
    }
}

impl fmt::Display for TestRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}::{}", self.file.display(), self.function)
    }
}

/// A cargo test target: the one place a listed test name is unambiguous.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum Target {
    /// The package's library unit tests.
    Lib,
    /// One integration target, by its file stem under `tests/`.
    Integration(String),
}

impl Target {
    /// The cargo arguments selecting exactly this target.
    fn selector(&self) -> Vec<String> {
        match self {
            Target::Lib => vec!["--lib".to_string()],
            Target::Integration(name) => vec!["--test".to_string(), name.clone()],
        }
    }
}

impl fmt::Display for Target {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Target::Lib => f.write_str("--lib"),
            Target::Integration(name) => write!(f, "--test {name}"),
        }
    }
}

/// A cargo target, and the module path a test in the cited file is listed
/// under within it.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct TargetRef {
    pub package: String,
    pub target: Target,
    /// Empty for an integration target and for `src/lib.rs`; `a::b::` for
    /// `src/a/b.rs`.
    pub module_prefix: String,
}

impl TargetRef {
    /// The pair a listing is keyed by.
    fn key(&self) -> (String, Target) {
        (self.package.clone(), self.target.clone())
    }
}

/// What one cargo target compiled: every test in it, and which of them are
/// ignored.
#[derive(Clone, Debug, Default)]
pub struct Listing {
    pub all: BTreeSet<String>,
    pub ignored: BTreeSet<String>,
}

/// The tests cargo says are compiled into each cited target.
///
/// This is what makes a binding a claim about the built suite rather than
/// about the text of a file. A function named only inside a string literal, or
/// declared behind a `cfg` nothing turns on, is in the source and not in the
/// list — and a binding that names one is a binding nothing runs.
#[derive(Clone, Debug, Default)]
pub struct TestIndex {
    /// A target cargo refused to list keeps the refusal, so it reaches the
    /// caller as one more thing wrong with the binding that named it rather
    /// than as a collection step that blew up before the audit ran.
    listings: BTreeMap<(String, Target), Result<Listing, String>>,
}

impl TestIndex {
    /// Build an index from listings supplied directly, for exercising the
    /// audit against registries that name no real test.
    pub fn from_listings(listings: impl IntoIterator<Item = ((String, Target), Listing)>) -> Self {
        TestIndex {
            listings: listings
                .into_iter()
                .map(|(key, listing)| (key, Ok(listing)))
                .collect(),
        }
    }

    /// Ask cargo what compiled into each target, from `workspace_root`.
    ///
    /// One pair of invocations per target: `--list` is everything the target
    /// compiled, and `--list --ignored` is the subset a plain run skips. The
    /// two are asked separately because `--list` alone does not say which is
    /// which, and the difference is what decides whether a bound carrier
    /// actually runs.
    ///
    /// Cargo's status lines go to standard error and the test binary's list to
    /// standard output, so the two cannot be read as one interleaved stream —
    /// which is why the target is selected on the command line rather than
    /// parsed out of the output.
    pub fn from_cargo(workspace_root: &Path, targets: impl IntoIterator<Item = TargetRef>) -> Self {
        let wanted: BTreeSet<(String, Target)> =
            targets.into_iter().map(|target| target.key()).collect();
        let mut listings = BTreeMap::new();
        for (package, target) in wanted {
            let listing = list(workspace_root, &package, &target, false).and_then(|all| {
                let ignored = list(workspace_root, &package, &target, true)?;
                Ok(Listing { all, ignored })
            });
            listings.insert((package, target), listing);
        }
        TestIndex { listings }
    }

    /// Whether `function` is a live test in `target`'s listing, and whether it
    /// is ignored — or what is wrong with the reference.
    fn resolve(&self, target: &TargetRef, function: &str) -> Result<bool, String> {
        let listing = match self.listings.get(&target.key()) {
            Some(Ok(listing)) => listing,
            Some(Err(problem)) => return Err(problem.clone()),
            None => {
                return Err(format!(
                    "no test list was collected for `{} {}`",
                    target.package, target.target
                ));
            }
        };
        let matched: Vec<&String> = listing
            .all
            .iter()
            .filter(|listed| matches_function(listed, &target.module_prefix, function))
            .collect();
        if matched.is_empty() {
            return Err(format!(
                "cargo compiled no test `{}{function}` into `{} {}`",
                target.module_prefix, target.package, target.target
            ));
        }
        // Ignored if any spelling of the name is: the question is whether the
        // binding names something a plain run executes.
        Ok(matched
            .iter()
            .any(|listed| listing.ignored.contains(*listed)))
    }
}

/// Whether a listed test name is `prefix` followed by a module path ending in
/// `function`.
fn matches_function(listed: &str, prefix: &str, function: &str) -> bool {
    let Some(rest) = listed.strip_prefix(prefix) else {
        return false;
    };
    rest.rsplit("::").next() == Some(function)
}

/// One `cargo test … -- --list` run, as the set of test names it printed.
fn list(
    workspace_root: &Path,
    package: &str,
    target: &Target,
    ignored_only: bool,
) -> Result<BTreeSet<String>, String> {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| OsStr::new("cargo").to_os_string());
    let mut command = Command::new(&cargo);
    command
        .current_dir(workspace_root)
        .arg("test")
        .arg("--locked")
        .arg("-p")
        .arg(package)
        .args(target.selector())
        .arg("--")
        .arg("--list");
    if ignored_only {
        command.arg("--ignored");
    }
    let output = command
        .output()
        .map_err(|e| format!("could not run cargo to list `{package} {target}`'s tests: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "listing `{package} {target}`'s tests failed ({}): {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| line.strip_suffix(": test"))
        .map(|name| name.trim().to_string())
        .collect())
}

#[derive(Debug)]
pub enum RegistryError {
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    Parse {
        path: PathBuf,
        source: serde_json::Error,
    },
}

impl fmt::Display for RegistryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RegistryError::Read { path, source } => {
                write!(f, "could not read {}: {source}", path.display())
            }
            RegistryError::Parse { path, source } => {
                write!(f, "could not parse {}: {source}", path.display())
            }
        }
    }
}

impl std::error::Error for RegistryError {}

impl Registry {
    /// Load the registry file at `path`.
    ///
    /// Parsing is strict in both directions: an unknown field is refused, so
    /// a field renamed in the data and not in the types fails here rather
    /// than being read as absent.
    pub fn load(path: &Path) -> Result<Self, RegistryError> {
        read_json(path).map_err(|error| match error {
            JsonError::Read(source) => RegistryError::Read {
                path: path.to_path_buf(),
                source,
            },
            JsonError::Parse(source) => RegistryError::Parse {
                path: path.to_path_buf(),
                source,
            },
        })
    }

    /// One value over the whole contract every case states.
    ///
    /// Every field a reviewer weighs goes in — name, kind, mandatory flag,
    /// venue, property, sources and binding — so any edit to any of them moves
    /// it. That is the point: the case total catches a deletion, and this
    /// catches everything a deletion-plus-replacement would hide. A property
    /// gutted to a word, a citation swapped for another, a venue quietly
    /// re-laned, a binding shrunk from four tests to one — each moves this
    /// value, and moving it means editing a constant a reviewer reads.
    ///
    /// Cases are digested in name order, so reordering the file alone does not
    /// move it, and every field is absorbed behind its own length, so no two
    /// different registries can be run together into the same bytes.
    pub fn contract_digest(&self) -> String {
        let mut cases: Vec<&Case> = self.cases.iter().collect();
        cases.sort_by(|left, right| left.name.cmp(&right.name));

        let mut hasher = Sha256::new();
        hasher.update_framed(&(cases.len() as u64).to_be_bytes());
        for case in cases {
            hasher.update_framed(case.name.as_bytes());
            hasher.update_framed(case.kind.to_string().as_bytes());
            hasher.update_framed(if case.mandatory { b"mandatory" } else { b"" });
            hasher.update_framed(&[case.venue]);
            hasher.update_framed(case.property.as_bytes());
            hasher.update_framed(&(case.sources.len() as u64).to_be_bytes());
            for source in &case.sources {
                hasher.update_framed(source.as_bytes());
            }
            hasher.update_framed(match case.binding.status {
                BindingStatus::Bound => b"bound",
                BindingStatus::Dormant => b"dormant",
            });
            hasher.update_framed(&(case.binding.tests.len() as u64).to_be_bytes());
            for test in &case.binding.tests {
                hasher.update_framed(test.as_bytes());
            }
            match &case.binding.reason {
                // Framed behind a presence marker, so no reason and an empty
                // reason are different registries.
                Some(reason) => {
                    hasher.update_framed(b"reason");
                    hasher.update_framed(reason.as_bytes());
                }
                None => hasher.update_framed(b"no-reason"),
            }
        }
        hex(&hasher.finish())
    }

    /// Every cargo target a bound case cites, for [`TestIndex::from_cargo`].
    ///
    /// A reference this cannot resolve is left out rather than reported: the
    /// audit reports it, with the rest of what is wrong with that case.
    pub fn cited_targets(&self) -> BTreeSet<TargetRef> {
        self.bound_cases()
            .flat_map(|case| case.binding.tests.iter())
            .filter_map(|reference| TestRef::parse(reference).ok())
            .filter_map(|test| test.target().ok())
            .collect()
    }

    /// The cases carried by tests today.
    pub fn bound_cases(&self) -> impl Iterator<Item = &Case> {
        self.cases
            .iter()
            .filter(|case| case.binding.status == BindingStatus::Bound)
    }

    /// The cases awaiting their venue.
    pub fn dormant_cases(&self) -> impl Iterator<Item = &Case> {
        self.cases
            .iter()
            .filter(|case| case.binding.status == BindingStatus::Dormant)
    }

    /// The cases at one venue.
    pub fn cases_at(&self, venue: u8) -> impl Iterator<Item = &Case> {
        self.cases.iter().filter(move |case| case.venue == venue)
    }

    /// Structural violations, one line each. A sound registry produces none.
    ///
    /// This judges no property — a property is judged by the test that binds
    /// it. It checks only that the record holds together: that every case is
    /// named once, states something falsifiable no other case already states,
    /// and cites sources of a shape a citation has; that the mandatory set is
    /// exactly the one the harness pins; that a bound case names tests cargo
    /// compiled into the suite; and that a dormant case at or below the layer
    /// that exists says why it is still dormant.
    ///
    /// `workspace_root` is where a test reference's path resolves from, and
    /// `tests` is what cargo says each cited target compiled.
    pub fn audit(&self, workspace_root: &Path, tests: &TestIndex) -> Vec<String> {
        let mut problems = Vec::new();
        self.audit_venues(&mut problems);
        self.audit_cases(&mut problems);
        self.audit_properties(&mut problems);
        self.audit_mandatory(&mut problems);
        self.audit_bindings(workspace_root, tests, &mut problems);
        problems
    }

    fn audit_venues(&self, problems: &mut Vec<String>) {
        let declared: Vec<(u8, &str)> = self
            .venues
            .iter()
            .map(|venue| (venue.layer, venue.name.as_str()))
            .collect();
        let pinned: Vec<(u8, &str)> = VENUE_NAMES
            .iter()
            .enumerate()
            .map(|(layer, name)| (layer as u8, *name))
            .collect();
        if declared != pinned {
            problems.push(format!(
                "the declared venue scale is not the one the harness pins: {declared:?} against \
                 {pinned:?}"
            ));
        }
    }

    fn audit_cases(&self, problems: &mut Vec<String>) {
        let mut seen: BTreeSet<&str> = BTreeSet::new();
        for case in &self.cases {
            let name = case.name.as_str();
            if !seen.insert(name) {
                problems.push(format!("`{name}` is named twice"));
            }
            if !is_kebab_case(name) {
                problems.push(format!(
                    "`{name}` is not kebab-case: a name is lowercase words joined by single \
                     hyphens"
                ));
            }
            if case.property.trim().is_empty() {
                problems.push(format!("`{name}` states no property"));
            }
            if case.sources.is_empty() {
                problems.push(format!("`{name}` cites no source"));
            }
            for source in &case.sources {
                if source.trim().is_empty() {
                    problems.push(format!("`{name}` cites an empty source"));
                } else if !is_identifier(source) && !NAMED_SOURCES.contains(&source.as_str()) {
                    problems.push(format!(
                        "`{name}` cites `{source}`, which is neither a `NRN-…`/`NORN-…` \
                         identifier nor one of the named records the harness allows"
                    ));
                }
            }
            if usize::from(case.venue) >= VENUE_NAMES.len() {
                problems.push(format!(
                    "`{name}` names venue {}, which is not a layer",
                    case.venue
                ));
            }
        }
    }

    /// No two cases state the same property.
    ///
    /// Two names over one property is a case counted twice: the total says the
    /// registry carries two obligations where it carries one, and binding
    /// either reads as covering both. Comparison ignores how the text is
    /// wrapped, because a re-wrap is not a second property.
    fn audit_properties(&self, problems: &mut Vec<String>) {
        let mut seen: BTreeMap<String, &str> = BTreeMap::new();
        for case in &self.cases {
            let normalized = case
                .property
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            if normalized.is_empty() {
                continue;
            }
            match seen.get(&normalized) {
                Some(first) => problems.push(format!(
                    "`{}` states the property `{first}` already states",
                    case.name
                )),
                None => {
                    seen.insert(normalized, case.name.as_str());
                }
            }
        }
    }

    fn audit_mandatory(&self, problems: &mut Vec<String>) {
        let declared: BTreeSet<&str> = self
            .cases
            .iter()
            .filter(|case| case.mandatory)
            .map(|case| case.name.as_str())
            .collect();
        let pinned: BTreeSet<&str> = MANDATORY_CASES.iter().copied().collect();
        for missing in pinned.difference(&declared) {
            problems.push(format!(
                "the registry does not carry `{missing}` as mandatory, which the harness pins as \
                 mandatory"
            ));
        }
        for extra in declared.difference(&pinned) {
            problems.push(format!(
                "the registry marks `{extra}` mandatory, which the harness does not pin as \
                 mandatory"
            ));
        }
    }

    fn audit_bindings(&self, workspace_root: &Path, tests: &TestIndex, problems: &mut Vec<String>) {
        // One read per file, however many cases name it.
        let mut sources: BTreeMap<PathBuf, Option<String>> = BTreeMap::new();
        for case in &self.cases {
            let name = case.name.as_str();
            match case.binding.status {
                BindingStatus::Bound => {
                    if case.binding.tests.is_empty() {
                        problems.push(format!("`{name}` is bound and names no test"));
                    }
                    if case.binding.reason.is_some() {
                        problems.push(format!(
                            "`{name}` is bound and states a reason for dormancy"
                        ));
                    }
                    for reference in &case.binding.tests {
                        for problem in self.audit_carrier(
                            workspace_root,
                            tests,
                            &mut sources,
                            reference.as_str(),
                        ) {
                            problems.push(format!("`{name}` {problem}"));
                        }
                    }
                }
                BindingStatus::Dormant => {
                    if !case.binding.tests.is_empty() {
                        problems.push(format!("`{name}` is dormant and names tests"));
                    }
                    match &case.binding.reason {
                        Some(reason) if reason.trim().is_empty() => {
                            problems.push(format!("`{name}` states an empty reason"));
                        }
                        None if case.venue <= LAYER_LANDING => problems.push(format!(
                            "`{name}` is dormant at layer {}, which is at or below the layer that \
                             exists, and states no reason",
                            case.venue
                        )),
                        _ => {}
                    }
                }
            }
        }
    }

    /// What is wrong with one carrier reference, or nothing.
    ///
    /// Four things have to hold for a binding to be a claim about the built
    /// suite. The reference has to resolve to a cargo target this grammar
    /// names. Its path has to exist spelled exactly as written, checked
    /// against the directory's own entries rather than by asking whether a
    /// file is there — a case-insensitive filesystem answers yes to a spelling
    /// that fails on a case-sensitive one. The file has to declare the
    /// function with `#[test]`, which is what pins the reference to the file
    /// it names rather than to the target as a whole. And cargo has to have
    /// compiled it: a name that appears only in a string literal, or behind a
    /// `cfg` nothing turns on, is in the source and not in the list.
    ///
    /// An ignored carrier passes only when a lane adopts it, because a lane
    /// running a suite's ignored cases wholesale is the only thing that makes
    /// an ignored test run.
    fn audit_carrier(
        &self,
        workspace_root: &Path,
        tests: &TestIndex,
        sources: &mut BTreeMap<PathBuf, Option<String>>,
        reference: &str,
    ) -> Vec<String> {
        let test = match TestRef::parse(reference) {
            Ok(test) => test,
            Err(problem) => return vec![problem],
        };
        let target = match test.target() {
            Ok(target) => target,
            Err(problem) => return vec![format!("names `{test}`, whose {problem}")],
        };

        let mut problems = Vec::new();
        if !resolves_case_exactly(workspace_root, &test.file) {
            return vec![format!(
                "names `{test}`, whose file is not in the workspace under that exact spelling"
            )];
        }
        let source = sources
            .entry(workspace_root.join(&test.file))
            .or_insert_with_key(|path| read(path).ok());
        let Some(text) = source else {
            return vec![format!("names `{test}`, whose file could not be read")];
        };
        if !declares_test(text, &test.function) {
            problems.push(format!(
                "names `{test}`, and that file declares no `#[test] fn {}`",
                test.function
            ));
        }

        match tests.resolve(&target, &test.function) {
            Err(problem) => problems.push(format!("names `{test}`, and {problem}")),
            Ok(false) => {}
            Ok(true) => {
                let reason = ignore_reason(text, &test.function);
                let adopted = reason.is_some_and(|reason| {
                    LANE_IGNORE_PREFIXES.iter().any(|p| reason.starts_with(p))
                });
                if !adopted {
                    problems.push(format!(
                        "names `{test}`, which cargo compiled as ignored with the reason {}. An \
                         ignored carrier runs only where a lane adopts it, so its reason opens \
                         with one of {LANE_IGNORE_PREFIXES:?}",
                        reason.map_or("none".to_string(), |reason| format!("{reason:?}"))
                    ));
                }
            }
        }
        problems
    }
}

/// Whether every component of `relative` is spelled the way the directory
/// holding it spells it, and the last one is a file.
///
/// `Path::is_file` is not this question: a case-insensitive filesystem answers
/// it for `Tests/Lanes.rs` and a case-sensitive one does not, so a reference
/// that passes here and fails in CI is exactly what asking it would allow.
#[allow(clippy::disallowed_methods)] // Resolves a reference against the workspace's own directory entries.
fn resolves_case_exactly(workspace_root: &Path, relative: &Path) -> bool {
    let mut current = workspace_root.to_path_buf();
    let components: Vec<&OsStr> = relative
        .components()
        .filter_map(|component| match component {
            Component::Normal(name) => Some(name),
            _ => None,
        })
        .collect();
    let Some((last, directories)) = components.split_last() else {
        return false;
    };
    for name in directories {
        if !holds(&current, name, true) {
            return false;
        }
        current.push(name);
    }
    holds(&current, last, false)
}

/// Whether `dir` holds an entry named exactly `name`, of the kind wanted.
#[allow(clippy::disallowed_methods)] // Resolves a reference against the workspace's own directory entries.
fn holds(dir: &Path, name: &OsStr, want_directory: bool) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
    entries.filter_map(Result::ok).any(|entry| {
        entry.file_name() == name
            && entry
                .file_type()
                .is_ok_and(|kind| kind.is_dir() == want_directory)
    })
}

/// Whether `source` is a citation identifier: `NRN-` or `NORN-`, an optional
/// seed or artifact marker, and a number.
///
/// This is a shape check and nothing more. Whether `NRN-4711` names a record
/// that ever existed is not something this workspace can answer — the board it
/// would ask is frozen and gone — so a citation's *existence* is held by the
/// review that adds it, and its *shape* is held here, which is what stops a
/// fabricated-looking identifier from passing as one.
fn is_identifier(source: &str) -> bool {
    let Some(rest) = source
        .strip_prefix("NORN-")
        .or_else(|| source.strip_prefix("NRN-"))
    else {
        return false;
    };
    let digits = rest
        .strip_prefix('s')
        .or_else(|| rest.strip_prefix('a'))
        .unwrap_or(rest);
    !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit())
}

/// The reason of the `#[ignore = "…"]` above `fn <name>`, if it carries one.
///
/// The attribute block is read the way [`declares_test`] reads it, so an
/// `#[ignore]` separated from its function by a doc comment or another
/// attribute is still that function's.
fn ignore_reason<'a>(source: &'a str, name: &str) -> Option<&'a str> {
    let lines: Vec<&str> = source.lines().collect();
    for (index, line) in lines.iter().enumerate() {
        if !opens_fn(line.trim(), name) {
            continue;
        }
        for above in lines[..index].iter().rev() {
            let above = above.trim();
            if let Some(rest) = above.strip_prefix("#[ignore") {
                let (_, quoted) = rest.split_once('"')?;
                let (reason, _) = quoted.split_once('"')?;
                return Some(reason);
            }
            if above.is_empty() || above.starts_with("#[") || above.starts_with("//") {
                continue;
            }
            break;
        }
    }
    None
}

/// Whether `name` is lowercase words joined by single hyphens.
fn is_kebab_case(name: &str) -> bool {
    !name.is_empty()
        && name.split('-').all(|word| {
            !word.is_empty()
                && word
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        })
}

/// Whether `source` declares `#[test] fn <name>`.
///
/// The attribute is read from the lines above the function rather than from
/// the line before it, because a test carries doc comments and further
/// attributes between the two — `#[ignore]` among them. Anything that is not
/// an attribute, a comment or a blank line ends the search, so a `#[test]`
/// belonging to the function above is never read as this one's.
fn declares_test(source: &str, name: &str) -> bool {
    let lines: Vec<&str> = source.lines().collect();
    for (index, line) in lines.iter().enumerate() {
        if !opens_fn(line.trim(), name) {
            continue;
        }
        for above in lines[..index].iter().rev() {
            let above = above.trim();
            if above == "#[test]" {
                return true;
            }
            if above.is_empty() || above.starts_with("#[") || above.starts_with("//") {
                continue;
            }
            break;
        }
    }
    false
}

/// Whether a trimmed line declares `fn <name>`, with or without a visibility
/// or `async` qualifier ahead of it.
fn opens_fn(line: &str, name: &str) -> bool {
    let mut rest = line;
    for qualifier in ["pub(crate) ", "pub ", "async ", "unsafe ", "const "] {
        if let Some(stripped) = rest.strip_prefix(qualifier) {
            rest = stripped;
        }
    }
    let Some(rest) = rest.strip_prefix("fn ") else {
        return false;
    };
    let Some(rest) = rest.strip_prefix(name) else {
        return false;
    };
    rest.starts_with('(') || rest.starts_with('<')
}

#[allow(clippy::disallowed_methods)] // Reads the registry and the test sources it names.
fn read(path: &Path) -> Result<String, RegistryError> {
    std::fs::read_to_string(path).map_err(|source| RegistryError::Read {
        path: path.to_path_buf(),
        source,
    })
}

#[cfg(test)]
mod tests {
    use super::{
        Binding, BindingStatus, Case, Kind, LANE_IGNORE_PREFIXES, LAYER_LANDING, Listing,
        MANDATORY_CASES, Registry, Target, TestIndex, TestRef, VENUE_NAMES, Venue, declares_test,
        ignore_reason, is_identifier, is_kebab_case, opens_fn,
    };
    use std::collections::BTreeSet;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// The one integration target the synthetic workspace holds, and the three
    /// carriers in it: a plain test, a test an ignore-lane adopts, and a test
    /// ignored for a reason no lane names.
    const CARRIER_SOURCE: &str = "\
#[test]
fn a_carrier() {}

#[test]
#[ignore = \"soak-lane case: nightly work\"]
fn an_adopted_carrier() {}

#[test]
#[ignore = \"flaky on this machine\"]
fn an_orphan_carrier() {}

/// A name that appears in the source and never in a test binary: this is the
/// spoof the cargo listing exists to catch.
#[test]
#[cfg(any())]
fn a_carrier_nothing_compiles() {}

fn a_name_in_prose() {
    let _ = \"fn a_carrier_in_a_string_literal() {}\";
}
";

    /// A scratch workspace holding `crates/demo/tests/suite.rs`, unique to the
    /// caller so tests running beside each other never share one.
    #[allow(clippy::disallowed_methods)] // Builds the tree the reference resolver is tested against.
    fn scratch() -> PathBuf {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let serial = NEXT.fetch_add(1, Ordering::Relaxed);
        let root =
            std::env::temp_dir().join(format!("norn-regression-{}-{serial}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let tests = root.join("crates/demo/tests");
        std::fs::create_dir_all(&tests).expect("a scratch workspace");
        std::fs::write(tests.join("suite.rs"), CARRIER_SOURCE).expect("a carrier source");
        // A second target, deliberately absent from `index`, so a binding
        // reaching a target nothing was listed for has somewhere to point.
        std::fs::write(tests.join("other.rs"), CARRIER_SOURCE).expect("a second carrier source");
        root
    }

    /// What cargo would say the scratch workspace's one target compiled: the
    /// `cfg(any())` function and the one named in a string literal are absent,
    /// because neither is a test.
    fn index() -> TestIndex {
        TestIndex::from_listings([(
            ("demo".to_string(), Target::Integration("suite".to_string())),
            Listing {
                all: ["a_carrier", "an_adopted_carrier", "an_orphan_carrier"]
                    .into_iter()
                    .map(String::from)
                    .collect(),
                ignored: ["an_adopted_carrier", "an_orphan_carrier"]
                    .into_iter()
                    .map(String::from)
                    .collect(),
            },
        )])
    }

    fn case(name: &str, venue: u8, binding: Binding) -> Case {
        Case {
            name: name.to_string(),
            kind: Kind::DefectClass,
            mandatory: false,
            venue,
            property: format!("the property {name} states"),
            sources: vec!["NRN-1".to_string()],
            binding,
        }
    }

    fn dormant(reason: Option<&str>) -> Binding {
        Binding {
            status: BindingStatus::Dormant,
            tests: Vec::new(),
            reason: reason.map(String::from),
        }
    }

    fn bound(tests: &[&str]) -> Binding {
        Binding {
            status: BindingStatus::Bound,
            tests: tests.iter().map(|test| (*test).to_string()).collect(),
            reason: None,
        }
    }

    /// A registry that audits clean against [`scratch`] and [`index`]: the
    /// pinned venue scale, the five mandatory cases, one bound case, and one
    /// dormant layer-0 case with its reason.
    fn sound() -> Registry {
        let mut cases: Vec<Case> = MANDATORY_CASES
            .iter()
            .map(|name| {
                let mut case = case(name, 4, dormant(None));
                case.mandatory = true;
                case
            })
            .collect();
        cases.push(case(
            "a-bound-case",
            0,
            bound(&["crates/demo/tests/suite.rs::a_carrier"]),
        ));
        cases.push(case(
            "a-dormant-layer-zero-case",
            0,
            dormant(Some("its subject has not landed")),
        ));
        Registry {
            note: "a scratch registry".to_string(),
            venues: VENUE_NAMES
                .iter()
                .enumerate()
                .map(|(layer, name)| Venue {
                    layer: layer as u8,
                    name: (*name).to_string(),
                })
                .collect(),
            cases,
        }
    }

    /// The problems a mutated registry produces.
    fn problems(mutate: impl FnOnce(&mut Registry)) -> Vec<String> {
        let root = scratch();
        let mut registry = sound();
        mutate(&mut registry);
        registry.audit(&root, &index())
    }

    /// A mutation the audit refuses, naming what it said.
    fn refused(mutate: impl FnOnce(&mut Registry), needle: &str) {
        let found = problems(mutate);
        assert!(
            found.iter().any(|problem| problem.contains(needle)),
            "expected a problem naming {needle:?}, got {found:#?}"
        );
    }

    /// The case named, mutably.
    fn find<'a>(registry: &'a mut Registry, name: &str) -> &'a mut Case {
        registry
            .cases
            .iter_mut()
            .find(|case| case.name == name)
            .unwrap_or_else(|| panic!("no case named `{name}`"))
    }

    #[test]
    fn a_sound_registry_produces_no_problem() {
        assert_eq!(problems(|_| {}), Vec::<String>::new());
    }

    #[test]
    fn a_name_used_twice_is_caught() {
        refused(
            |registry| {
                let duplicate = registry.cases[0].clone();
                registry.cases.push(duplicate);
            },
            "is named twice",
        );
    }

    #[test]
    fn a_name_that_is_not_kebab_case_is_caught() {
        refused(
            |registry| find(registry, "a-bound-case").name = "A_Bound_Case".to_string(),
            "is not kebab-case",
        );
    }

    #[test]
    fn a_property_gutted_to_nothing_is_caught() {
        refused(
            |registry| find(registry, "a-bound-case").property = "   ".to_string(),
            "states no property",
        );
    }

    #[test]
    fn two_cases_stating_one_property_are_caught() {
        refused(
            |registry| {
                let stolen = find(registry, "a-bound-case").property.clone();
                find(registry, "a-dormant-layer-zero-case").property = stolen;
            },
            "states the property",
        );
    }

    #[test]
    fn one_property_reflowed_is_still_that_property() {
        refused(
            |registry| {
                let stolen = find(registry, "a-bound-case").property.clone();
                find(registry, "a-dormant-layer-zero-case").property =
                    format!("  {}  ", stolen.replace(' ', "\n   "));
            },
            "states the property",
        );
    }

    #[test]
    fn a_case_citing_nothing_is_caught() {
        refused(
            |registry| find(registry, "a-bound-case").sources.clear(),
            "cites no source",
        );
    }

    #[test]
    fn an_empty_citation_is_caught() {
        refused(
            |registry| find(registry, "a-bound-case").sources = vec![" ".to_string()],
            "cites an empty source",
        );
    }

    #[test]
    fn a_citation_of_a_shape_no_identifier_has_is_caught() {
        for fabricated in [
            "JIRA-12",
            "NRN-",
            "NRN-12a",
            "nrn-12",
            "some notes",
            "NORN-x1",
        ] {
            refused(
                |registry| find(registry, "a-bound-case").sources = vec![fabricated.to_string()],
                "which is neither a",
            );
        }
    }

    #[test]
    fn a_venue_off_the_scale_is_caught() {
        refused(
            |registry| find(registry, "a-bound-case").venue = 7,
            "which is not a layer",
        );
    }

    #[test]
    fn a_renamed_venue_is_caught() {
        refused(
            |registry| registry.venues[2].name = "the lockdown".to_string(),
            "is not the one the harness pins",
        );
    }

    #[test]
    fn a_mandatory_flag_dropped_is_caught() {
        refused(
            |registry| find(registry, MANDATORY_CASES[0]).mandatory = false,
            "does not carry",
        );
    }

    #[test]
    fn a_mandatory_flag_the_harness_does_not_pin_is_caught() {
        refused(
            |registry| find(registry, "a-bound-case").mandatory = true,
            "which the harness does not pin as mandatory",
        );
    }

    #[test]
    fn a_bound_case_naming_no_test_is_caught() {
        refused(
            |registry| find(registry, "a-bound-case").binding.tests.clear(),
            "is bound and names no test",
        );
    }

    #[test]
    fn a_bound_case_stating_a_reason_is_caught() {
        refused(
            |registry| {
                find(registry, "a-bound-case").binding.reason = Some("waiting".to_string());
            },
            "is bound and states a reason",
        );
    }

    #[test]
    fn a_dormant_case_naming_tests_is_caught() {
        refused(
            |registry| {
                find(registry, "a-dormant-layer-zero-case").binding.tests =
                    vec!["crates/demo/tests/suite.rs::a_carrier".to_string()];
            },
            "is dormant and names tests",
        );
    }

    /// A dormant case at any layer whose subject exists has to say why. The
    /// requirement is not layer 0's alone: layer 1 has landed, so a case
    /// sitting there dormant and silent is an obligation nobody wrote down.
    #[test]
    fn a_dormant_case_at_a_landed_layer_with_no_reason_is_caught() {
        for venue in 0..=LAYER_LANDING {
            refused(
                |registry| {
                    let case = find(registry, "a-dormant-layer-zero-case");
                    case.venue = venue;
                    case.binding.reason = None;
                },
                "states no reason",
            );
        }
        // Above the landed layer there is nothing to explain: the subject has
        // not been built.
        assert_eq!(
            problems(|registry| {
                let case = find(registry, "a-dormant-layer-zero-case");
                case.venue = LAYER_LANDING + 1;
                case.binding.reason = None;
            }),
            Vec::<String>::new()
        );
    }

    #[test]
    fn an_empty_reason_is_caught() {
        refused(
            |registry| {
                find(registry, "a-dormant-layer-zero-case").binding.reason = Some("  ".to_string());
            },
            "states an empty reason",
        );
    }

    #[test]
    fn a_reference_that_is_not_a_file_and_a_function_is_caught() {
        refused(
            |registry| find(registry, "a-bound-case").binding.tests = vec!["a_carrier".to_string()],
            "is not a `<file>::<test fn>` reference",
        );
    }

    #[test]
    fn a_reference_reaching_outside_the_workspace_is_caught() {
        refused(
            |registry| {
                find(registry, "a-bound-case").binding.tests =
                    vec!["../elsewhere/tests/suite.rs::a_carrier".to_string()];
            },
            "workspace-relative path without parent traversal",
        );
    }

    #[test]
    fn a_reference_to_a_target_shape_the_grammar_does_not_name_is_caught() {
        for (reference, said) in [
            (
                "crates/demo/benches/suite.rs::a_carrier",
                "sits under `benches/`",
            ),
            ("crates/demo/src/main.rs::a_carrier", "is a binary target"),
            (
                "crates/demo/tests/suite/inner.rs::a_carrier",
                "is a module of an integration target",
            ),
            (
                "demo/tests/suite.rs::a_carrier",
                "is not `crates/<package>/{src,tests}/…`",
            ),
            (
                "crates/demo/tests/suite.txt::a_carrier",
                "is not a Rust source file",
            ),
        ] {
            refused(
                |registry| {
                    find(registry, "a-bound-case").binding.tests = vec![reference.to_string()];
                },
                said,
            );
        }
    }

    #[test]
    fn a_reference_whose_file_is_not_in_the_workspace_is_caught() {
        refused(
            |registry| {
                find(registry, "a-bound-case").binding.tests =
                    vec!["crates/demo/tests/absent.rs::a_carrier".to_string()];
            },
            "not in the workspace under that exact spelling",
        );
    }

    #[test]
    fn a_reference_whose_spelling_differs_only_in_case_is_caught() {
        for reference in [
            "crates/demo/tests/Suite.rs::a_carrier",
            "crates/Demo/tests/suite.rs::a_carrier",
        ] {
            refused(
                |registry| {
                    find(registry, "a-bound-case").binding.tests = vec![reference.to_string()];
                },
                "not in the workspace under that exact spelling",
            );
        }
    }

    #[test]
    fn a_reference_to_a_function_the_file_does_not_declare_is_caught() {
        refused(
            |registry| {
                find(registry, "a-bound-case").binding.tests =
                    vec!["crates/demo/tests/suite.rs::a_name_in_prose".to_string()];
            },
            "declares no `#[test] fn",
        );
    }

    #[test]
    fn a_reference_cargo_never_compiled_is_caught() {
        // The `cfg(any())` function and the one spelled inside a string
        // literal are both in the source and in neither test binary.
        for function in [
            "a_carrier_nothing_compiles",
            "a_carrier_in_a_string_literal",
        ] {
            refused(
                |registry| {
                    find(registry, "a-bound-case").binding.tests =
                        vec![format!("crates/demo/tests/suite.rs::{function}")];
                },
                "cargo compiled no test",
            );
        }
    }

    #[test]
    fn a_target_nothing_was_listed_for_is_caught() {
        // `other.rs` is a real file declaring a real test, and no listing was
        // collected for the target it compiles into. The audit fails closed
        // rather than reading a missing listing as nothing to check.
        refused(
            |registry| {
                find(registry, "a-bound-case").binding.tests =
                    vec!["crates/demo/tests/other.rs::a_carrier".to_string()];
            },
            "no test list was collected",
        );
    }

    #[test]
    fn a_target_cargo_refused_to_list_is_caught() {
        // Cargo declining to list a target is a thing the audit says, not a
        // collection step that fails before the audit runs: the reference that
        // asked for it is the thing that is wrong. The scratch tree is not a
        // cargo workspace, so listing anything in it fails — while the file the
        // reference names is really there and really declares the test, which
        // is what puts the refusal on the only remaining check.
        let root = scratch();
        let reference = TestRef::parse("crates/demo/tests/suite.rs::a_carrier").expect("a pair");
        let refused = TestIndex::from_cargo(&root, [reference.target().expect("a target")]);
        let registry = sound();
        let found = registry.audit(&root, &refused);
        assert!(
            found
                .iter()
                .any(|problem| problem.contains("listing `demo --test suite`'s tests failed")),
            "{found:#?}"
        );
        // And the same registry against a listing that was collected has
        // nothing wrong with it, so the refusal is what the audit reported.
        assert_eq!(registry.audit(&root, &index()), Vec::<String>::new());
    }

    #[test]
    fn an_ignored_carrier_no_lane_adopts_is_caught() {
        refused(
            |registry| {
                find(registry, "a-bound-case").binding.tests =
                    vec!["crates/demo/tests/suite.rs::an_orphan_carrier".to_string()];
            },
            "An ignored carrier runs only where a lane adopts it",
        );
    }

    #[test]
    fn an_ignored_carrier_a_lane_adopts_passes() {
        let found = problems(|registry| {
            find(registry, "a-bound-case").binding.tests =
                vec!["crates/demo/tests/suite.rs::an_adopted_carrier".to_string()];
        });
        assert_eq!(found, Vec::<String>::new());
    }

    #[test]
    fn a_lane_prefix_is_read_off_the_source() {
        assert_eq!(
            ignore_reason(CARRIER_SOURCE, "an_adopted_carrier"),
            Some("soak-lane case: nightly work")
        );
        assert_eq!(
            ignore_reason(CARRIER_SOURCE, "an_orphan_carrier"),
            Some("flaky on this machine")
        );
        assert_eq!(ignore_reason(CARRIER_SOURCE, "a_carrier"), None);
        assert!(
            LANE_IGNORE_PREFIXES
                .iter()
                .any(|prefix| "soak-lane case: nightly work".starts_with(prefix))
        );
    }

    #[test]
    fn an_identifier_is_a_prefix_a_marker_and_a_number() {
        for source in ["NRN-1", "NRN-s20", "NRN-a63", "NORN-13", "NORN-a1"] {
            assert!(is_identifier(source), "{source}");
        }
        for source in ["NRN", "NRN-", "NRN-x1", "NRN-12a", "nrn-1", "N-1", "12"] {
            assert!(!is_identifier(source), "{source}");
        }
    }

    #[test]
    fn a_reference_resolves_to_the_target_its_tests_compile_into() {
        let cases = [
            ("crates/a/src/lib.rs::f", Target::Lib, ""),
            ("crates/a/src/process.rs::f", Target::Lib, "process::"),
            ("crates/a/src/one/two.rs::f", Target::Lib, "one::two::"),
            ("crates/a/src/one/mod.rs::f", Target::Lib, "one::"),
            (
                "crates/a/tests/lanes.rs::f",
                Target::Integration("lanes".to_string()),
                "",
            ),
        ];
        for (reference, target, prefix) in cases {
            let found = TestRef::parse(reference)
                .expect(reference)
                .target()
                .expect(reference);
            assert_eq!(found.package, "a", "{reference}");
            assert_eq!(found.target, target, "{reference}");
            assert_eq!(found.module_prefix, prefix, "{reference}");
        }
    }

    #[test]
    fn every_cited_target_is_collected_once() {
        let registry = sound();
        let targets: BTreeSet<_> = registry.cited_targets();
        assert_eq!(targets.len(), 1);
        let only = targets.into_iter().next().expect("one target");
        assert_eq!(only.package, "demo");
        assert_eq!(only.target, Target::Integration("suite".to_string()));
    }

    /// One edit to a registry, named by what it changes.
    type Mutation = Box<dyn Fn(&mut Registry)>;

    #[test]
    fn the_digest_moves_for_every_field_a_case_states() {
        let base = sound().contract_digest();
        let mutations: Vec<(&str, Mutation)> = vec![
            (
                "a gutted property",
                Box::new(|registry: &mut Registry| {
                    find(registry, "a-bound-case").property = "x".to_string();
                }),
            ),
            (
                "a swapped citation",
                Box::new(|registry: &mut Registry| {
                    find(registry, "a-bound-case").sources = vec!["NRN-999".to_string()];
                }),
            ),
            (
                "a dropped citation",
                Box::new(|registry: &mut Registry| {
                    find(registry, "a-bound-case").sources.clear();
                }),
            ),
            (
                "a re-laned venue",
                Box::new(|registry: &mut Registry| {
                    find(registry, "a-dormant-layer-zero-case").venue = 3;
                }),
            ),
            (
                "a changed kind",
                Box::new(|registry: &mut Registry| {
                    find(registry, "a-bound-case").kind = Kind::PositiveControl;
                }),
            ),
            (
                "a dropped mandatory flag",
                Box::new(|registry: &mut Registry| {
                    find(registry, MANDATORY_CASES[0]).mandatory = false;
                }),
            ),
            (
                "a shrunk binding",
                Box::new(|registry: &mut Registry| {
                    find(registry, "a-bound-case").binding.tests.clear();
                }),
            ),
            (
                "a renamed case",
                Box::new(|registry: &mut Registry| {
                    find(registry, "a-bound-case").name = "a-renamed-case".to_string();
                }),
            ),
            (
                "a replaced case at the same count",
                Box::new(|registry: &mut Registry| {
                    let last = registry.cases.len() - 1;
                    registry.cases[last] =
                        case("a-different-case", 0, dormant(Some("a different reason")));
                }),
            ),
            (
                "a rewritten reason",
                Box::new(|registry: &mut Registry| {
                    find(registry, "a-dormant-layer-zero-case").binding.reason =
                        Some("another reason".to_string());
                }),
            ),
            (
                "an emptied reason",
                Box::new(|registry: &mut Registry| {
                    find(registry, "a-dormant-layer-zero-case").binding.reason =
                        Some(String::new());
                }),
            ),
            (
                "a dropped reason",
                Box::new(|registry: &mut Registry| {
                    find(registry, "a-dormant-layer-zero-case").binding.reason = None;
                }),
            ),
        ];
        for (what, mutate) in mutations {
            let mut registry = sound();
            mutate(&mut registry);
            assert_ne!(registry.contract_digest(), base, "{what} did not move it");
        }
    }

    #[test]
    fn the_digest_ignores_the_order_the_cases_sit_in() {
        let base = sound().contract_digest();
        let mut reordered = sound();
        reordered.cases.reverse();
        assert_eq!(reordered.contract_digest(), base);
    }

    fn registry(cases: &str) -> Result<Registry, serde_json::Error> {
        let venues = r#"[
            {"layer": 0, "name": "fixtures and testkit"},
            {"layer": 1, "name": "substrate"},
            {"layer": 2, "name": "lockdown"},
            {"layer": 3, "name": "queries"},
            {"layer": 4, "name": "mutations"},
            {"layer": 5, "name": "repair"},
            {"layer": 6, "name": "surfaces"}
        ]"#;
        serde_json::from_str(&format!(
            r#"{{"note": "n", "venues": {venues}, "cases": [{cases}]}}"#
        ))
    }

    const DORMANT: &str = r#"{
        "name": "a-case",
        "kind": "defect-class",
        "venue": 3,
        "property": "a property",
        "sources": ["NRN-1"],
        "binding": {"status": "dormant"}
    }"#;

    #[test]
    fn a_case_loads_with_the_fields_the_registry_carries() {
        let registry = registry(DORMANT).expect("a well-formed case");
        let case = &registry.cases[0];
        assert_eq!(case.name, "a-case");
        assert!(!case.mandatory);
        assert!(case.binding.tests.is_empty());
    }

    #[test]
    fn an_unknown_field_is_refused() {
        let case = DORMANT.replace(r#""venue": 3"#, r#""venue": 3, "layer": 3"#);
        let problem = registry(&case).expect_err("an unknown field is not a field");
        assert!(problem.to_string().contains("layer"), "{problem}");
    }

    #[test]
    fn an_unknown_kind_is_refused() {
        let case = DORMANT.replace("defect-class", "regression");
        registry(&case).expect_err("`regression` is not a kind");
    }

    #[test]
    fn a_reference_names_a_workspace_relative_file_and_a_function() {
        let test = TestRef::parse("crates/norn-testkit/src/process.rs::a_test").expect("a pair");
        assert_eq!(test.file, Path::new("crates/norn-testkit/src/process.rs"));
        assert_eq!(test.function, "a_test");
    }

    #[test]
    fn a_reference_that_could_leave_the_workspace_is_refused() {
        for reference in [
            "/etc/passwd::a_test",
            "../elsewhere/tests/a.rs::a_test",
            "crates/a.rs",
            "::a_test",
        ] {
            TestRef::parse(reference).expect_err(reference);
        }
    }

    #[test]
    fn a_test_is_found_past_the_attributes_and_comments_between_it_and_its_marker() {
        let source = "#[test]\n#[ignore = \"soak-lane case: x\"]\n/// doc\nfn wanted() {}\n";
        assert!(declares_test(source, "wanted"));
    }

    #[test]
    fn a_function_with_no_marker_of_its_own_does_not_borrow_the_one_above() {
        let source = "#[test]\nfn first() {\n    let x = 1;\n}\n\nfn wanted() {}\n";
        assert!(declares_test(source, "first"));
        assert!(!declares_test(source, "wanted"));
    }

    #[test]
    fn a_name_that_only_prefixes_a_test_is_not_that_test() {
        let source = "#[test]\nfn wanted_but_longer() {}\n";
        assert!(!declares_test(source, "wanted"));
    }

    #[test]
    fn a_declaration_is_read_through_its_qualifiers() {
        assert!(opens_fn("pub fn wanted()", "wanted"));
        assert!(opens_fn("fn wanted<T>()", "wanted"));
        assert!(!opens_fn("fn wanted_more()", "wanted"));
        assert!(!opens_fn("let wanted = 1;", "wanted"));
    }

    #[test]
    fn a_name_is_lowercase_words_joined_by_single_hyphens() {
        assert!(is_kebab_case("one-obvious-path"));
        assert!(is_kebab_case("nrn-42-and-friends"));
        for name in [
            "",
            "-leading",
            "trailing-",
            "two--hyphens",
            "Upper",
            "with_underscore",
        ] {
            assert!(!is_kebab_case(name), "{name} is not kebab-case");
        }
    }
}
