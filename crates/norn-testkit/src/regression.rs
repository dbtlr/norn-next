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
//! **A dormancy reason is falsifiable too.** A reason is prose, and prose does
//! not notice its subject landing. So a dormant case at or below
//! [`LAYER_LANDING`] states [`Ground`]s beside the reason: the workspace paths
//! the reason stands on, each marked as one the workspace holds or one it does
//! not. The audit resolves every one of them, so **a reason standing on a path
//! the workspace does not hold fails the moment that path is in the tree** —
//! which is what makes that reason's staleness loud rather than a thing a review
//! has to notice. Each ground's path is also required to appear in the reason
//! text, so the prose and the mechanical claim cannot drift apart.
//!
//! **A subject is not always a whole path.** A reason often waits on one name
//! inside a file the tree already holds: a member absent from a vocabulary, a
//! guard function nobody wrote, a type a module will grow. Such a reason grounds
//! itself on a symbol — `symbol-present` and `symbol-absent` name a
//! `<file>::<Symbol>` pair, and the audit reads the declaration lines of that
//! file. Two rules keep the symbol arms off the shape a path absence is held
//! off: the file has to be a Rust source file that resolves, for **both**
//! claims, because an absent symbol in an absent file is a path absence and is
//! claimed as one; and the `Symbol` has to be one Rust identifier that is not a
//! keyword, spelled in the reason as well. A `symbol-absent`
//! ground therefore pre-commits the name the carrier takes when it lands, which
//! is what a dormant case is — an authoring contract, not a guess about what
//! somebody will call it.
//!
//! **A ground is a claim about a subject, and not every missing subject is
//! one.** A reason may wait on a step outside the Rust sources, on a home no
//! decision has picked yet, or on a carrier this file's reference grammar cannot
//! name. Its grounds then hold the subjects it does cite and nothing refutes the
//! rest, so the gate covers the absences that are path- or symbol-shaped and no
//! others. That class is
//! finite and named rather than left to be discovered:
//! [`Registry::dormant_without_an_absence`] enumerates the dormant cases
//! stating no `absent` ground, and the suite pins the set, so a reason joins the
//! class in a diff instead of drifting into it.
//!
//! **The audit holds content, not only shape.** [`Registry::contract_digest`]
//! is one value over every case's name, kind, mandatory flag, venue, property,
//! sources and binding — grounds included — pinned as a constant in the suite:
//! an edit to any of them moves it, so a gutted property, a swapped citation, a
//! re-laned venue and a shrunk binding each fail the gate the same way a
//! deleted case does. Test references are checked against cargo's own list of
//! what compiled into each target, so a reference naming a string literal, a
//! function behind a disabled `cfg`, or a file whose spelling differs only in
//! case is refused.
//!
//! This module is the loader and the gate. The suite it gates lives in the
//! `norn` bin package, beside the registry data.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{OsStr, OsString};
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

/// One machine-checkable claim a dormancy reason stands on.
///
/// A reason is prose about the workspace, and the workspace moves. A ground is
/// the part of that prose the audit can check: a path, and which way the
/// workspace is required to answer for it. [`Registry::audit`] resolves every
/// one against the tree, so a reason whose grounds stopped holding fails.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub enum Ground {
    /// A path the reason says the workspace does not hold — the subject that
    /// has not been built. **This is the falsifiability claim**: the audit
    /// fails when the path is there, because the reason has outlived its
    /// subject's absence.
    ///
    /// The path is the shallowest one the absence turns on: the crate directory
    /// while the crate is unbuilt, a module inside it once the crate is there.
    /// [`Registry::audit`] holds it to that by requiring the parent to resolve,
    /// because a deeper path guessed ahead of the tree is a spelling the tree
    /// may never use, and an absence nothing can satisfy is an absence that
    /// never ends. The cost of the shallow claim is that a case flips when the
    /// directory lands rather than when its own subject does, which is the
    /// moment the reason has to be re-derived against real code anyway.
    Absent(String),
    /// A path the reason cites as a subject that did land, which is what a
    /// reason for a case still dormant beside a built subject has to name. The
    /// audit fails when the path is not there, because the grounds moved.
    Present(String),
    /// A `<file>::<Symbol>` pair the reason says the tree does not declare —
    /// **the falsifiability claim at symbol grain**, for a subject that is one
    /// name inside a file rather than a file of its own.
    ///
    /// The file has to be there. A name is claimed absent only where the tree
    /// can answer for it, so a subject whose home is not built yet is a path
    /// absence and is claimed as one, and a subject whose home nobody has picked
    /// yet has no ground at all. What the claim costs is a decision. The
    /// registry pre-commits the name the carrier will take, and the audit fails
    /// the day that name is declared — which is the whole point. A carrier that
    /// lands under some other name leaves the claim standing, and that is why
    /// the name is written down in advance: a dormant case is the contract the
    /// carrier is authored against, not a guess about what somebody will call
    /// it.
    SymbolAbsent(String),
    /// A `<file>::<Symbol>` pair the reason cites as a declaration the tree
    /// carries. The audit fails when the file declares no such name, which is a
    /// rename walking out from under the prose.
    SymbolPresent(String),
}

impl Ground {
    /// The subject the claim is about: a path, or a whole `<file>::<Symbol>`
    /// pair.
    pub fn subject(&self) -> &str {
        match self {
            Ground::Absent(subject)
            | Ground::Present(subject)
            | Ground::SymbolAbsent(subject)
            | Ground::SymbolPresent(subject) => subject,
        }
    }

    /// The claim's name, as the data spells it.
    fn claim(&self) -> &'static str {
        match self {
            Ground::Absent(_) => "absent",
            Ground::Present(_) => "present",
            Ground::SymbolAbsent(_) => "symbol-absent",
            Ground::SymbolPresent(_) => "symbol-present",
        }
    }
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
    /// The paths the reason stands on. Required alongside the reason, and held
    /// to the tree by [`Registry::audit`]: this is what makes a reason go stale
    /// loudly instead of quietly.
    #[serde(default)]
    pub grounds: Vec<Ground>,
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
        Self::from_cargo_with_features(workspace_root, targets, &[])
    }

    /// The same, with `features` turned on for the whole invocation.
    ///
    /// **What compiled is a question about a feature set**, not about a target
    /// alone: a suite whose cases sit behind an off-by-default feature compiles
    /// to zero tests without it, and an index built without the feature would
    /// report every one of them missing. So a caller whose cases declare a
    /// feature asks under that feature, and a caller whose cases declare none
    /// asks under none. Each index is one feature set's answer; a caller
    /// spanning two builds two.
    pub fn from_cargo_with_features(
        workspace_root: &Path,
        targets: impl IntoIterator<Item = TargetRef>,
        features: &[&str],
    ) -> Self {
        let wanted: BTreeSet<(String, Target)> =
            targets.into_iter().map(|target| target.key()).collect();
        let mut listings = BTreeMap::new();
        for (package, target) in wanted {
            let listing =
                list(workspace_root, &package, &target, features, false).and_then(|all| {
                    let ignored = list(workspace_root, &package, &target, features, true)?;
                    Ok(Listing { all, ignored })
                });
            listings.insert((package, target), listing);
        }
        TestIndex { listings }
    }

    /// Everything cargo compiled into `target`, or what went wrong asking.
    ///
    /// Public because the reverse direction of a reconciliation is a claim
    /// about the whole target: every test a certification suite compiled is
    /// named by the inventory, which is a question about the listing rather
    /// than about one reference in it.
    pub fn compiled(&self, target: &TargetRef) -> Result<&Listing, String> {
        match self.listings.get(&target.key()) {
            Some(Ok(listing)) => Ok(listing),
            Some(Err(problem)) => Err(problem.clone()),
            None => Err(format!(
                "no test list was collected for `{} {}`",
                target.package, target.target
            )),
        }
    }

    /// Whether `function` is a live test in `target`'s listing, and whether it
    /// is ignored — or what is wrong with the reference.
    pub fn resolve(&self, target: &TargetRef, function: &str) -> Result<bool, String> {
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
///
/// Public because two readers resolve the same names: the reconciliation above,
/// against what cargo listed, and the certification lane's log reader, against
/// what the harness reported. One rule, so a name that resolves for one of them
/// resolves for the other.
pub fn matches_function(listed: &str, prefix: &str, function: &str) -> bool {
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
    features: &[&str],
    ignored_only: bool,
) -> Result<BTreeSet<String>, String> {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| OsStr::new("cargo").to_os_string());
    let mut command = Command::new(&cargo);
    command
        .current_dir(workspace_root)
        .arg("test")
        .arg("--locked")
        .arg("-p")
        .arg(package);
    if !features.is_empty() {
        command.arg("--features").arg(features.join(","));
    }
    command.args(target.selector()).arg("--").arg("--list");
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
    /// venue, property, sources and binding, down to the grounds a dormancy
    /// reason stands on — so any edit to any of them moves it. That is the
    /// point: the case total catches a deletion, and this catches everything a
    /// deletion-plus-replacement would hide. A property gutted to a word, a
    /// citation swapped for another, a venue quietly re-laned, a binding shrunk
    /// from four tests to one — each moves this value, and moving it means
    /// editing a constant a reviewer reads.
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
            hasher.update_framed(&(case.binding.grounds.len() as u64).to_be_bytes());
            for ground in &case.binding.grounds {
                hasher.update_framed(ground.claim().as_bytes());
                hasher.update_framed(ground.subject().as_bytes());
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

    /// The dormant cases at or below [`LAYER_LANDING`] stating no [`Ground`] of
    /// absence — **the dormancy the falsifiability gate cannot refute.**
    ///
    /// A reason in this class waits on something no subject names: a step
    /// outside the Rust sources, a home no decision has picked, a carrier the
    /// reference grammar cannot name. Its grounds hold the subjects it cites as
    /// present, so the audit still catches those moving, but nothing mechanical
    /// contradicts the claim that something is missing — that part expires only
    /// when a person re-derives it.
    ///
    /// A [`Ground::SymbolAbsent`] is an absence like [`Ground::Absent`] is: it
    /// fails the day its subject lands, so a case standing on one is out of this
    /// class.
    ///
    /// The set is what the suite pins, which is what keeps it a reviewed list:
    /// a case that gains an absence leaves it, and a reason that newly waits on
    /// a non-path subject fails the suite until it is written down.
    pub fn dormant_without_an_absence(&self) -> impl Iterator<Item = &Case> {
        self.dormant_cases().filter(|case| {
            case.venue <= LAYER_LANDING
                && !case
                    .binding
                    .grounds
                    .iter()
                    .any(|ground| matches!(ground, Ground::Absent(_) | Ground::SymbolAbsent(_)))
        })
    }

    /// Structural violations, one line each. A sound registry produces none.
    ///
    /// This judges no property — a property is judged by the test that binds
    /// it. It checks only that the record holds together: that every case is
    /// named once, states something falsifiable no other case already states,
    /// and cites sources of a shape a citation has; that the mandatory set is
    /// exactly the one the harness pins; that a bound case names tests cargo
    /// compiled into the suite; and that a dormant case at or below the layer
    /// that exists says why it is still dormant, on grounds that still hold
    /// against the tree.
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
                    if !case.binding.grounds.is_empty() {
                        problems.push(format!("`{name}` is bound and states grounds for dormancy"));
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
                    self.audit_grounds(workspace_root, case, problems);
                }
            }
        }
    }

    /// **The falsifiability gate.** What is wrong with one dormant case's
    /// grounds, held against the tree as it is now.
    ///
    /// A reason is prose, and prose about an absent subject reads the same
    /// forever. The grounds are the part of it a machine can check, and this is
    /// where the check happens: an `absent` path the workspace now holds means
    /// the subject landed while the reason went on saying it had not, so the
    /// case fails until somebody re-derives it — binds it, or restates why a
    /// built subject still carries nothing. A `present` path that is gone is the
    /// same failure from the other side: the reason is standing on a file the
    /// workspace no longer has.
    ///
    /// **The two arms fail in opposite directions, so the `absent` arm is held
    /// to its spelling.** A `present` path resolves or the audit says so; an
    /// `absent` path that fails to resolve *for any reason* reads as satisfied,
    /// so a padded component, a case-folded one, or a subject under a parent
    /// that is not there would pass today and go on passing after the subject
    /// landed. Each is refused: a ground's components are held to their own
    /// trimmed spelling, an `absent` subject the tree holds under another case
    /// is a landed subject rather than an absence, and an `absent` subject's
    /// parent has to resolve, which keeps the claim at the boundary the tree can
    /// answer.
    ///
    /// **A symbol ground is the same gate over one name inside a file.** Both
    /// arms require the file to resolve, because a name is claimed absent only
    /// where the tree can answer for it: an absent symbol in an absent file is
    /// a path absence, and reading it as a symbol absence would pass on the
    /// file's absence and go on passing after the file landed carrying the name.
    /// The file is held to being a Rust source file and the `Symbol` to being
    /// one non-keyword Rust identifier, for the same reason a path is held to
    /// its spelling — a name no file can declare, or a file whose lines this
    /// grammar does not describe, is an absence nothing can end.
    ///
    /// Every ground's subject is also required to appear in the reason itself,
    /// the whole `<file>::<Symbol>` pair for a symbol ground. The grounds are
    /// what the audit reads and the reason is what a person reads, and a reason
    /// that never names its own subject leaves the two free to disagree.
    ///
    /// Grounds are required exactly where a reason is — at or below
    /// [`LAYER_LANDING`], the layers whose subjects exist. Above it a case is
    /// waiting on a layer nobody has built, and there is no path to point at.
    fn audit_grounds(&self, workspace_root: &Path, case: &Case, problems: &mut Vec<String>) {
        let name = case.name.as_str();
        if case.binding.grounds.is_empty() {
            if case.venue <= LAYER_LANDING {
                problems.push(format!(
                    "`{name}` is dormant at layer {}, which is at or below the layer that exists, \
                     and states no grounds: a reason at these layers names the paths it stands on, \
                     so the audit fails when its subject lands",
                    case.venue
                ));
            }
            return;
        }
        let reason = case.binding.reason.as_deref().unwrap_or_default();
        let mut seen: BTreeSet<&str> = BTreeSet::new();
        for ground in &case.binding.grounds {
            let subject = ground.subject();
            if subject.trim().is_empty() {
                problems.push(format!("`{name}` states a ground naming no path"));
                continue;
            }
            if !seen.insert(subject) {
                problems.push(format!("`{name}` stands on `{subject}` twice"));
            }
            // The claim each arm makes is read here and passed on as one bit,
            // so a claim added to the grammar has to be routed here rather than
            // falling through a helper's wildcard.
            match ground {
                Ground::Absent(_) => {
                    audit_path_ground(workspace_root, name, subject, true, reason, problems);
                }
                Ground::Present(_) => {
                    audit_path_ground(workspace_root, name, subject, false, reason, problems);
                }
                Ground::SymbolAbsent(_) => {
                    audit_symbol_ground(workspace_root, name, subject, true, reason, problems);
                }
                Ground::SymbolPresent(_) => {
                    audit_symbol_ground(workspace_root, name, subject, false, reason, problems);
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
        if !resolves_case_exactly(workspace_root, &test.file, LastComponent::File) {
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

/// What is wrong with one ground claiming a whole path, or nothing. `absent` is
/// the claim: the path is one the tree is required not to hold, or one it is
/// required to hold.
fn audit_path_ground(
    workspace_root: &Path,
    name: &str,
    subject: &str,
    absent: bool,
    reason: &str,
    problems: &mut Vec<String>,
) {
    if let Some(fault) = ill_spelled_ground(subject) {
        problems.push(format!("`{name}` stands on `{subject}`, which is {fault}"));
        return;
    }
    if !reason.contains(subject) {
        problems.push(format!(
            "`{name}` stands on `{subject}` and its reason does not name it: a ground is the \
             reason's own claim, checked"
        ));
    }
    let resolution = resolve(
        workspace_root,
        Path::new(subject),
        LastComponent::FileOrDirectory,
    );
    match (absent, resolution) {
        (true, Resolution::Held) => problems.push(format!(
            "`{name}` stands on `{subject}` being absent, and the workspace holds it. The \
                     subject landed, so the reason is stale: bind the case, or restate why a built \
                     subject still carries nothing"
        )),
        (true, Resolution::Confusable { asked, found }) => {
            problems.push(format!(
                "`{name}` stands on `{subject}` being absent, and the workspace holds `{found}` \
                 where that path asks for `{asked}`. An absence claimed under a spelling the tree \
                 does not use is an absence nothing can end, so it never goes stale: spell the \
                 subject the way the tree spells it"
            ));
        }
        (true, Resolution::MissingAncestor { missing }) => {
            problems.push(format!(
                "`{name}` stands on `{subject}` being absent, and `{missing}` above it is not in \
                 the workspace either. An absence is claimed at the boundary the tree can answer \
                 — the shallowest path that is not there — because a deeper spelling the tree may \
                 never use is a claim its subject's landing does not touch"
            ));
        }
        (true, Resolution::MissingLeaf) => {}
        (false, Resolution::Held) => {}
        (false, _) => problems.push(format!(
            "`{name}` cites `{subject}` as a subject that landed, and the workspace holds no such \
             path under that exact spelling"
        )),
    }
}

/// What is wrong with one ground claiming one declaration inside a file, or
/// nothing. `absent` is the claim: the file is required to declare the name, or
/// required not to.
///
/// The file resolving is a precondition of **both** claims. An absent name in an
/// absent file is a path absence, and reading it here would pass on the file's
/// absence and go on passing after the file landed carrying the name.
fn audit_symbol_ground(
    workspace_root: &Path,
    name: &str,
    subject: &str,
    absent: bool,
    reason: &str,
    problems: &mut Vec<String>,
) {
    let (file, symbol) = subject.rsplit_once("::").unwrap_or((subject, ""));
    if file.contains("::") || !is_rust_identifier(symbol) {
        problems.push(format!(
            "`{name}` stands on `{subject}`, which is not a `<file>::<Symbol>` reference to one \
             declaration. A symbol ground names a single Rust identifier a file declares, because \
             a name no file can declare is a claim the scan can never refute"
        ));
        return;
    }
    if RUST_KEYWORDS.contains(&symbol) {
        problems.push(format!(
            "`{name}` stands on `{subject}`, whose `{symbol}` is a keyword: a Rust keyword names \
             nothing a file can declare, so no landing ends an absence claimed under one"
        ));
        return;
    }
    if !file.ends_with(".rs") {
        problems.push(format!(
            "`{name}` stands on `{subject}`, whose file is not a Rust source file. A symbol claim \
             is answered by a scan of Rust declaration lines, and running that scan over anything \
             else reads a grammar the file does not use"
        ));
        return;
    }
    if let Some(fault) = ill_spelled_ground(file) {
        problems.push(format!(
            "`{name}` stands on `{subject}`, whose file `{file}` is {fault}"
        ));
        return;
    }
    if !reason.contains(subject) {
        problems.push(format!(
            "`{name}` stands on `{subject}` and its reason does not name it: a ground is the \
             reason's own claim, checked"
        ));
    }
    if resolve(workspace_root, Path::new(file), LastComponent::File) != Resolution::Held {
        problems.push(format!(
            "`{name}` stands on `{subject}`, and `{file}` is not in the workspace under that \
             exact spelling. A symbol claim is read out of the file that would declare it, so the \
             file has to be there for either claim: an absent symbol in an absent file is a path \
             absence and is claimed as one"
        ));
        return;
    }
    let Ok(source) = read(&workspace_root.join(file)) else {
        problems.push(format!(
            "`{name}` stands on `{subject}`, whose file could not be read"
        ));
        return;
    };
    match (absent, declares_symbol(&source, symbol)) {
        (true, true) => problems.push(format!(
            "`{name}` stands on `{file}` declaring no `{symbol}`, and it declares one. The symbol \
             landed, so the reason is stale: bind the case, or restate why a built subject still \
             carries nothing"
        )),
        (false, false) => problems.push(format!(
            "`{name}` cites `{symbol}` as a name `{file}` declares, and that file declares no \
             such name"
        )),
        _ => {}
    }
}

/// What the last component of a resolved path is allowed to be.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LastComponent {
    /// A carrier reference names a file, because it names a function in one.
    File,
    /// A dormancy ground names a subject, which is a crate directory as often
    /// as it is a module file.
    FileOrDirectory,
}

/// How a path answers against the workspace's own directory entries.
///
/// Whether a path is there is one bit, and one bit is not enough for a claim of
/// absence: an absence has to be told apart from a spelling the tree cannot
/// resolve under any name, which reads the same to a bit and means the opposite.
#[derive(Clone, Debug, Eq, PartialEq)]
enum Resolution {
    /// Every component is spelled the way the directory holding it spells it,
    /// and the last is of the kind asked for.
    Held,
    /// A component resolves only under another spelling: the tree holds `found`
    /// where the path asks for `asked`, and the two differ in case alone.
    Confusable { asked: String, found: String },
    /// Every directory above the last component is there and the last is not.
    MissingLeaf,
    /// A directory above the last component is not there. `missing` is the
    /// first one, so what the path says about its own leaf is unanswerable.
    MissingAncestor { missing: String },
}

/// Whether every component of `relative` is spelled the way the directory
/// holding it spells it, and the last one is of the kind `last` allows.
///
/// `Path::is_file` is not this question: a case-insensitive filesystem answers
/// it for `Tests/Lanes.rs` and a case-sensitive one does not, so a reference
/// that passes here and fails in CI is exactly what asking it would allow.
fn resolves_case_exactly(workspace_root: &Path, relative: &Path, last: LastComponent) -> bool {
    resolve(workspace_root, relative, last) == Resolution::Held
}

/// How `relative` resolves under `workspace_root`, component by component.
///
/// The walk stops at the first component the holding directory does not spell
/// that way, and reports whether the directory spells it some other way — which
/// is the difference between a subject that is not there and a subject named
/// wrong.
fn resolve(workspace_root: &Path, relative: &Path, last: LastComponent) -> Resolution {
    let mut current = workspace_root.to_path_buf();
    let components: Vec<&OsStr> = relative
        .components()
        .filter_map(|component| match component {
            Component::Normal(name) => Some(name),
            _ => None,
        })
        .collect();
    let Some((leaf, directories)) = components.split_last() else {
        return Resolution::MissingAncestor {
            missing: relative.display().to_string(),
        };
    };
    for name in directories {
        if holds(&current, name, true) {
            current.push(name);
            continue;
        }
        return match spelled_otherwise(&current, name, Some(true)) {
            Some(found) => Resolution::Confusable {
                asked: shown(name),
                found: shown(&found),
            },
            None => Resolution::MissingAncestor {
                missing: shown(name),
            },
        };
    }
    let kind = match last {
        LastComponent::File => Some(false),
        LastComponent::FileOrDirectory => None,
    };
    if kind.map_or_else(
        || holds(&current, leaf, false) || holds(&current, leaf, true),
        |want_directory| holds(&current, leaf, want_directory),
    ) {
        return Resolution::Held;
    }
    match spelled_otherwise(&current, leaf, kind) {
        Some(found) => Resolution::Confusable {
            asked: shown(leaf),
            found: shown(&found),
        },
        None => Resolution::MissingLeaf,
    }
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

/// The entry `dir` holds for `name` under a different case, if it holds one.
///
/// `want_directory` restricts the kind; `None` takes either. Case is the only
/// difference this recognizes, because it is the one a case-insensitive
/// filesystem and a hurried edit both produce.
#[allow(clippy::disallowed_methods)] // Resolves a reference against the workspace's own directory entries.
fn spelled_otherwise(dir: &Path, name: &OsStr, want_directory: Option<bool>) -> Option<OsString> {
    let folded = name.to_string_lossy().to_lowercase();
    std::fs::read_dir(dir)
        .ok()?
        .filter_map(Result::ok)
        .find(|entry| {
            entry.file_name().to_string_lossy().to_lowercase() == folded
                && want_directory
                    .is_none_or(|want| entry.file_type().is_ok_and(|kind| kind.is_dir() == want))
        })
        .map(|entry| entry.file_name())
}

/// One path component as a problem message spells it.
fn shown(name: &OsStr) -> String {
    name.to_string_lossy().into_owned()
}

/// Why `subject` is not a spelling a [`Ground`] may stand on, or nothing.
///
/// A ground is resolved against directory entries, and for an `absent` ground
/// resolving to nothing is the answer that passes. A spelling no tree can ever
/// hold therefore passes forever, so the shape is held before the tree is asked:
/// a component padded with whitespace, an empty one, a bare dot and a parent
/// traversal are all refused whichever arm states them.
fn ill_spelled_ground(subject: &str) -> Option<&'static str> {
    if Path::new(subject).is_absolute() {
        return Some("not a workspace-relative path without parent traversal");
    }
    for component in subject.split('/') {
        if component == ".." {
            return Some("not a workspace-relative path without parent traversal");
        }
        if component.is_empty() || component == "." {
            return Some(
                "not a workspace-relative path whose every component names something: an empty \
                 component and a bare dot resolve to nothing, so an absence claimed under one \
                 never ends",
            );
        }
        if component != component.trim() {
            return Some(
                "a path with a component padded by whitespace, which no directory entry is \
                 spelled with: the padded spelling resolves to nothing, so an absence claimed \
                 under it never ends",
            );
        }
    }
    None
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

/// Whether `name` is spelled like one Rust identifier: a letter or underscore,
/// then letters, digits and underscores.
///
/// A symbol ground is held to this before the tree is read, for the reason a
/// path ground is held to its spelling. A name no Rust file could declare —
/// `foo-bar`, `Foo::bar`, nothing at all — is a name the declaration scan never
/// finds, so an absence claimed under one would pass forever.
fn is_rust_identifier(name: &str) -> bool {
    // A lone underscore is a reserved token, not a name: nothing declares it.
    if name == "_" {
        return false;
    }
    let mut characters = name.chars();
    characters
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && characters.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// The words spelled like an identifier that a declaration cannot be named.
///
/// Strict keywords and reserved ones together. A keyword passes
/// [`is_rust_identifier`] and names nothing, so `counters.rs::fn` would be an
/// absence no landing could ever end — and the raw spelling `r#fn` is a
/// different string, which this grammar does not read either.
const RUST_KEYWORDS: &[&str] = &[
    "Self", "abstract", "as", "async", "await", "become", "box", "break", "const", "continue",
    "crate", "do", "dyn", "else", "enum", "extern", "false", "final", "fn", "for", "if", "impl",
    "in", "let", "loop", "macro", "match", "mod", "move", "mut", "override", "priv", "pub", "ref",
    "return", "self", "static", "struct", "super", "trait", "true", "try", "type", "typeof",
    "unsafe", "unsized", "use", "virtual", "where", "while", "yield",
];

/// The bare words a declaration may carry ahead of its keyword, in any order
/// and any number. `pub` and `extern` are read by [`past_qualifiers`] instead,
/// because each may carry a group after it.
const BARE_QUALIFIERS: &[&str] = &["async", "unsafe", "const", "static", "default"];

/// The keywords that name a declaration whose name follows them.
///
/// `fn` is absent because [`opens_fn`] reads it, and `const` and `static` are
/// absent because they are stripped as qualifiers — `const LIMIT: u64 = 1;`
/// reaches the line-opening arm as `LIMIT: u64 = 1;`, which is the shape a
/// struct field has too.
const DECLARATION_KEYWORDS: &[&str] = &["struct ", "enum ", "union ", "trait ", "type ", "mod "];

/// Whether `source` declares `name` on a line of its own.
///
/// This is a scan of declaration lines, not a parse: the grammar reads the
/// shapes a pre-committed name lands in — a function, a struct, an enum, a
/// union, a trait, a type alias, a module, a constant, a static — and reads **a
/// line segment that opens with the name itself** as a declaration too, which is
/// how an enum member and a struct field are named. A line is cut at its commas
/// and opening braces first, so members sharing one line are each read.
///
/// **Under-reading is the failure this grammar must not have.** A declaration
/// the scan misses leaves a `symbol-absent` claim standing after its subject
/// landed, which is the exact staleness the grounds exist to catch, so every
/// qualifier a declaration can carry is stripped ahead of the name
/// ([`past_qualifiers`]) rather than matched in one fixed order.
///
/// **Over-reading is allowed, because it fails loud.** A match arm, a
/// struct-literal field and a bare tail expression all open with an identifier,
/// so a name used that way reads as declared; the case then fails the audit and
/// somebody re-derives the reason, which is the direction a falsifiability gate
/// may err in.
///
/// **The accepted bound is the macro.** A name produced by a macro invocation —
/// `counter! { StatementsPrepared }` — is not read, because reading it means
/// expanding macros rather than scanning lines. One `macro_rules!` exists in
/// this workspace, so the bound costs nothing today; a vocabulary that moved
/// behind a macro would need a ground of another kind.
fn declares_symbol(source: &str, name: &str) -> bool {
    source
        .lines()
        .any(|line| declares_symbol_here(line.trim(), name))
}

/// Whether one trimmed line declares `name`.
fn declares_symbol_here(line: &str, name: &str) -> bool {
    if opens_fn(line, name) {
        return true;
    }
    let rest = past_qualifiers(line);
    for keyword in DECLARATION_KEYWORDS {
        if let Some(after) = rest.strip_prefix(keyword)
            && named_before(after, name, " <({;:=")
        {
            return true;
        }
    }
    // The line-opening arm: an enum member, a field, a constant's name once its
    // `const` was stripped. The line is cut at its commas and opening braces so
    // that members sharing one line are each read, and whitespace ahead of the
    // follower is allowed, so `Foo ,` and `Foo = 1,` read the same as `Foo,`.
    rest.split([',', '{'])
        .any(|segment| named_before(segment.trim_start(), name, ",({:=}"))
}

/// `line` with every leading declaration qualifier removed, in whatever order
/// and however many times they appear.
///
/// One reader, two callers: the carrier scan and the symbol scan see the same
/// shapes, so a form one of them learns the other cannot miss. What is stripped
/// is a same-line `#[…]` attribute group, `pub` with an optional restriction
/// (`pub(crate)`, `pub(super)`, `pub(in path)`), `extern` with an optional
/// string ABI, and the bare words in [`BARE_QUALIFIERS`]. An attribute whose
/// brackets do not close on this line is left alone: it opens something the next
/// lines finish, and this reader is a reader of one line.
fn past_qualifiers(line: &str) -> &str {
    let mut rest = line.trim_start();
    loop {
        let stripped = past_attribute(rest)
            .or_else(|| past_visibility(rest))
            .or_else(|| past_extern(rest))
            .or_else(|| {
                BARE_QUALIFIERS
                    .iter()
                    .find_map(|word| past_word(rest, word))
            });
        match stripped {
            Some(shorter) => rest = shorter.trim_start(),
            None => return rest,
        }
    }
}

/// What follows a leading `#[…]` group, when one opens and closes on this line.
fn past_attribute(line: &str) -> Option<&str> {
    let opened = line
        .strip_prefix("#[")
        .or_else(|| line.strip_prefix("#!["))?;
    let mut depth = 1usize;
    for (index, character) in opened.char_indices() {
        match character {
            '[' => depth += 1,
            ']' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&opened[index + character.len_utf8()..]);
                }
            }
            _ => {}
        }
    }
    None
}

/// What follows a leading `pub`, with its parenthesised restriction if it has
/// one.
fn past_visibility(line: &str) -> Option<&str> {
    let rest = past_word(line, "pub")?;
    let Some(opened) = rest.strip_prefix('(') else {
        return Some(rest);
    };
    let mut depth = 1usize;
    for (index, character) in opened.char_indices() {
        match character {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&opened[index + character.len_utf8()..]);
                }
            }
            _ => {}
        }
    }
    None
}

/// What follows a leading `extern`, with its string ABI if it has one.
fn past_extern(line: &str) -> Option<&str> {
    let rest = past_word(line, "extern")?.trim_start();
    let Some(opened) = rest.strip_prefix('"') else {
        return Some(rest);
    };
    opened.split_once('"').map(|(_, after)| after)
}

/// What follows a leading `word`, when the line really opens with that whole
/// word rather than with a longer name starting the same way.
///
/// `pub` is stripped from `pub(crate) fn`, where nothing separates the word
/// from what follows it, and never from `public_holiday`.
fn past_word<'a>(line: &'a str, word: &str) -> Option<&'a str> {
    let rest = line.strip_prefix(word)?;
    match rest.chars().next() {
        None => Some(rest),
        Some(character) if character.is_whitespace() || character == '(' => Some(rest),
        Some(_) => None,
    }
}

/// Whether `rest` opens with `name` and then ends, or reaches one of
/// `followers` with only whitespace in between.
fn named_before(rest: &str, name: &str, followers: &str) -> bool {
    let Some(tail) = rest.strip_prefix(name) else {
        return false;
    };
    let tail = tail.trim_start();
    match tail.chars().next() {
        None => true,
        Some(character) => followers.contains(character),
    }
}

/// Whether a trimmed line declares `fn <name>`, whatever qualifiers it carries
/// ahead of the keyword.
fn opens_fn(line: &str, name: &str) -> bool {
    let Some(rest) = past_word(past_qualifiers(line), "fn") else {
        return false;
    };
    let Some(rest) = rest.trim_start().strip_prefix(name) else {
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
        Binding, BindingStatus, Case, Ground, Kind, LANE_IGNORE_PREFIXES, LAYER_LANDING, Listing,
        MANDATORY_CASES, Registry, Target, TestIndex, TestRef, VENUE_NAMES, Venue, declares_symbol,
        declares_test, ignore_reason, is_identifier, is_kebab_case, opens_fn,
    };
    use crate::scratch::Scratch;
    use std::collections::BTreeSet;
    use std::path::Path;

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

    /// The file a symbol ground is read out of: an enum whose members sit one
    /// per line, and a function behind a visibility qualifier.
    const VOCABULARY_SOURCE: &str = "\
pub(crate) enum Counter {
    Foo,
}

pub(crate) fn foo(count: u64) -> u64 {
    count
}
";

    /// A scratch workspace holding `crates/demo/tests/suite.rs`, unique to the
    /// caller so tests running beside each other never share one, and gone
    /// when the caller drops it.
    #[allow(clippy::disallowed_methods)] // Builds the tree the reference resolver is tested against.
    fn scratch() -> Scratch {
        let root = Scratch::new("norn-regression");
        let tests = root.join("crates/demo/tests");
        std::fs::create_dir_all(&tests).expect("a scratch workspace");
        std::fs::write(tests.join("suite.rs"), CARRIER_SOURCE).expect("a carrier source");
        std::fs::write(tests.join("vocabulary.rs"), VOCABULARY_SOURCE)
            .expect("a vocabulary source");
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
            grounds: Vec::new(),
        }
    }

    /// A dormant binding whose reason names the paths it stands on, which is
    /// what a case at or below the landed layer is held to.
    fn grounded(reason: &str, grounds: Vec<Ground>) -> Binding {
        Binding {
            status: BindingStatus::Dormant,
            tests: Vec::new(),
            reason: Some(reason.to_string()),
            grounds,
        }
    }

    fn bound(tests: &[&str]) -> Binding {
        Binding {
            status: BindingStatus::Bound,
            tests: tests.iter().map(|test| (*test).to_string()).collect(),
            reason: None,
            grounds: Vec::new(),
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
            grounded(
                "crates/demo/tests/suite.rs holds the recognition half, and the half this case \
                 waits on has no subject: crates/demo/src is not in the workspace",
                vec![
                    Ground::Present("crates/demo/tests/suite.rs".to_string()),
                    Ground::Absent("crates/demo/src".to_string()),
                ],
            ),
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
        registry.audit(root.root(), &index())
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
        // not been built, so neither a reason nor the grounds one stands on is
        // asked for.
        assert_eq!(
            problems(|registry| {
                let case = find(registry, "a-dormant-layer-zero-case");
                case.venue = LAYER_LANDING + 1;
                case.binding.reason = None;
                case.binding.grounds.clear();
            }),
            Vec::<String>::new()
        );
    }

    /// **The falsifiability gate.** A reason standing on a subject's absence
    /// fails the moment the workspace holds that subject.
    ///
    /// This is the whole point of a ground. The reason text still reads exactly
    /// as it did — prose does not notice a crate landing — and the audit does.
    #[test]
    fn a_dormancy_reason_whose_absent_subject_landed_is_caught() {
        refused(
            |registry| {
                let case = find(registry, "a-dormant-layer-zero-case");
                case.binding.reason = Some(
                    "no carrier exists: crates/demo/tests/suite.rs is not in the workspace"
                        .to_string(),
                );
                case.binding.grounds =
                    vec![Ground::Absent("crates/demo/tests/suite.rs".to_string())];
            },
            "and the workspace holds it. The subject landed, so the reason is stale",
        );
    }

    /// A directory is a subject too: a case waiting on a whole crate names the
    /// crate, and lands loudly when the crate does.
    #[test]
    fn a_dormancy_reason_whose_absent_subject_directory_landed_is_caught() {
        refused(
            |registry| {
                let case = find(registry, "a-dormant-layer-zero-case");
                case.binding.reason = Some("crates/demo/tests is not in the workspace".to_string());
                case.binding.grounds = vec![Ground::Absent("crates/demo/tests".to_string())];
            },
            "and the workspace holds it",
        );
    }

    /// The other direction: a reason citing a landed subject that is not there.
    #[test]
    fn a_dormancy_reason_citing_a_subject_the_workspace_does_not_hold_is_caught() {
        refused(
            |registry| {
                let case = find(registry, "a-dormant-layer-zero-case");
                case.binding.reason = Some("crates/demo/tests/gone.rs holds half".to_string());
                case.binding.grounds =
                    vec![Ground::Present("crates/demo/tests/gone.rs".to_string())];
            },
            "and the workspace holds no such path under that exact spelling",
        );
    }

    /// A subject whose spelling differs only in case is a different subject, for
    /// the reason a carrier reference is held to the same rule.
    #[test]
    fn a_ground_whose_spelling_differs_only_in_case_is_caught() {
        refused(
            |registry| {
                let case = find(registry, "a-dormant-layer-zero-case");
                case.binding.reason = Some("crates/demo/tests/Suite.rs holds half".to_string());
                case.binding.grounds =
                    vec![Ground::Present("crates/demo/tests/Suite.rs".to_string())];
            },
            "holds no such path under that exact spelling",
        );
    }

    /// **An absence is held to its spelling.** A padded component resolves to
    /// nothing, and for an `absent` ground resolving to nothing is the answer
    /// that passes — so the padding would buy a claim no landing can ever
    /// falsify. One space is the whole edit, and the reason reads unchanged
    /// because the prose has a space in the same place.
    #[test]
    fn an_absent_ground_padded_with_whitespace_is_caught() {
        refused(
            |registry| {
                let case = find(registry, "a-dormant-layer-zero-case");
                case.binding.reason = Some("crates/demo/src is not in the workspace".to_string());
                case.binding.grounds = vec![Ground::Absent("crates/demo/src ".to_string())];
            },
            "padded by whitespace",
        );
    }

    /// An `absent` subject the tree holds under another case is the landed
    /// subject, not an absence: the spelling is what failed to resolve.
    #[test]
    fn an_absent_ground_whose_spelling_differs_only_in_case_is_caught() {
        refused(
            |registry| {
                let case = find(registry, "a-dormant-layer-zero-case");
                case.binding.reason = Some("crates/demo/Tests is not in the workspace".to_string());
                case.binding.grounds = vec![Ground::Absent("crates/demo/Tests".to_string())];
            },
            "where that path asks for",
        );
    }

    /// An absence is claimed at the boundary the tree can answer. A path under a
    /// directory that is not there says nothing about its own leaf, and the leaf
    /// landing under a spelling nobody guessed leaves the claim standing.
    #[test]
    fn an_absent_ground_under_an_absent_parent_is_caught() {
        refused(
            |registry| {
                let case = find(registry, "a-dormant-layer-zero-case");
                case.binding.reason =
                    Some("crates/demo/src/verb.rs is not in the workspace".to_string());
                case.binding.grounds = vec![Ground::Absent("crates/demo/src/verb.rs".to_string())];
            },
            "above it is not in the workspace either",
        );
    }

    /// **The dormancy the gate cannot refute is enumerable.** A reason waiting
    /// on something that is not a path states no absence, so nothing mechanical
    /// contradicts it; what the registry can do is name that class, and this is
    /// the reading the suite pins.
    #[test]
    fn dormancy_stating_no_absence_is_enumerated() {
        let registry = sound();
        assert_eq!(
            registry.dormant_without_an_absence().count(),
            0,
            "the scratch registry's one landed-layer dormancy stands on an absence"
        );

        let mut prose_only = sound();
        let case = find(&mut prose_only, "a-dormant-layer-zero-case");
        case.binding.reason = Some("crates/demo/tests/suite.rs holds half".to_string());
        case.binding.grounds = vec![Ground::Present("crates/demo/tests/suite.rs".to_string())];
        let named: Vec<&str> = prose_only
            .dormant_without_an_absence()
            .map(|case| case.name.as_str())
            .collect();
        assert_eq!(named, vec!["a-dormant-layer-zero-case"]);
    }

    /// A dormant case at a landed layer states grounds, not prose alone.
    #[test]
    fn a_dormant_case_at_a_landed_layer_with_no_grounds_is_caught() {
        for venue in 0..=LAYER_LANDING {
            refused(
                |registry| {
                    let case = find(registry, "a-dormant-layer-zero-case");
                    case.venue = venue;
                    case.binding.grounds.clear();
                },
                "and states no grounds",
            );
        }
    }

    /// The reason and the grounds are one claim, so the reason names every path
    /// the audit checks for it.
    #[test]
    fn a_ground_the_reason_does_not_name_is_caught() {
        refused(
            |registry| {
                let case = find(registry, "a-dormant-layer-zero-case");
                case.binding.reason = Some("its subject has not landed".to_string());
                case.binding.grounds = vec![Ground::Absent("crates/demo/src".to_string())];
            },
            "and its reason does not name it",
        );
    }

    #[test]
    fn a_ground_naming_no_path_is_caught() {
        refused(
            |registry| {
                find(registry, "a-dormant-layer-zero-case").binding.grounds =
                    vec![Ground::Present("  ".to_string())];
            },
            "states a ground naming no path",
        );
    }

    #[test]
    fn one_path_stood_on_twice_is_caught() {
        refused(
            |registry| {
                let case = find(registry, "a-dormant-layer-zero-case");
                case.binding.grounds = vec![
                    Ground::Present("crates/demo/tests/suite.rs".to_string()),
                    Ground::Present("crates/demo/tests/suite.rs".to_string()),
                ];
            },
            "twice",
        );
    }

    #[test]
    fn a_ground_reaching_outside_the_workspace_is_caught() {
        for subject in ["/etc/passwd", "../elsewhere/crates/demo"] {
            refused(
                |registry| {
                    let case = find(registry, "a-dormant-layer-zero-case");
                    case.binding.reason = Some(format!("{subject} is not in the workspace"));
                    case.binding.grounds = vec![Ground::Absent(subject.to_string())];
                },
                "workspace-relative path without parent traversal",
            );
        }
    }

    /// **A symbol is a subject too.** A reason waiting on a name that is not in
    /// the tree yet grounds itself on the file that will declare it, and passes
    /// while that file declares no such name.
    #[test]
    fn a_symbol_ground_the_file_does_not_declare_is_an_absence() {
        let found = problems(|registry| {
            let case = find(registry, "a-dormant-layer-zero-case");
            case.binding.reason = Some(
                "the vocabulary carries no such member: this waits on a \
                 crates/demo/tests/vocabulary.rs::StatementsPrepared member of `Counter`, which is \
                 the name the carrier takes when it lands"
                    .to_string(),
            );
            case.binding.grounds = vec![Ground::SymbolAbsent(
                "crates/demo/tests/vocabulary.rs::StatementsPrepared".to_string(),
            )];
        });
        assert_eq!(found, Vec::<String>::new());
    }

    /// **The falsifiability gate, at symbol grain.** The member landed on its
    /// own line inside an enum, the prose still says it has not, and the audit
    /// is what notices.
    #[test]
    fn a_symbol_ground_whose_symbol_landed_is_caught() {
        refused(
            |registry| {
                let case = find(registry, "a-dormant-layer-zero-case");
                case.binding.reason = Some(
                    "waits on a crates/demo/tests/vocabulary.rs::Foo member of `Counter`"
                        .to_string(),
                );
                case.binding.grounds = vec![Ground::SymbolAbsent(
                    "crates/demo/tests/vocabulary.rs::Foo".to_string(),
                )];
            },
            "The symbol landed, so the reason is stale",
        );
    }

    /// **A symbol claim needs its file, both ways round.** An absent name in an
    /// absent file is a path absence: read as a symbol claim it would pass on
    /// the file's absence and go on passing after the file landed carrying the
    /// name.
    #[test]
    fn a_symbol_ground_whose_file_is_not_in_the_workspace_is_caught() {
        for ground in [
            Ground::SymbolAbsent("crates/demo/tests/gone.rs::Foo".to_string()),
            Ground::SymbolPresent("crates/demo/tests/gone.rs::Foo".to_string()),
        ] {
            refused(
                |registry| {
                    let case = find(registry, "a-dormant-layer-zero-case");
                    case.binding.reason =
                        Some("waits on crates/demo/tests/gone.rs::Foo".to_string());
                    case.binding.grounds = vec![ground];
                },
                "an absent symbol in an absent file is a path absence and is claimed as one",
            );
        }
    }

    /// The reason and the grounds are one claim at symbol grain too, and the
    /// whole `file::Symbol` is what the prose has to name.
    #[test]
    fn a_symbol_ground_the_reason_does_not_name_is_caught() {
        refused(
            |registry| {
                let case = find(registry, "a-dormant-layer-zero-case");
                case.binding.reason =
                    Some("crates/demo/tests/vocabulary.rs carries no such member".to_string());
                case.binding.grounds = vec![Ground::SymbolAbsent(
                    "crates/demo/tests/vocabulary.rs::StatementsPrepared".to_string(),
                )];
            },
            "and its reason does not name it",
        );
    }

    /// **A qualifier never hides a declaration.** Visibility restrictions, an
    /// ABI, a same-line attribute and any order of the rest are all stripped, so
    /// a `symbol-absent` claim cannot stay green because the carrier landed
    /// behind `pub(super)`.
    #[test]
    fn a_declaration_is_read_through_qualifiers_in_any_order() {
        for source in [
            "pub(super) fn collect_statistics(connection: &Connection) {",
            "pub(self) fn collect_statistics() {}",
            "pub(in crate::db) fn collect_statistics() {",
            "pub extern \"C\" fn collect_statistics() {",
            "extern \"C\" fn collect_statistics() {",
            "#[rustfmt::skip] pub fn collect_statistics() {",
            "#[inline] #[cold] pub(crate) unsafe fn collect_statistics() {",
            "default fn collect_statistics() {",
            "const unsafe fn collect_statistics() {",
            "unsafe const fn collect_statistics() {",
        ] {
            assert!(
                declares_symbol(source, "collect_statistics"),
                "`{source}` declares `collect_statistics`"
            );
        }
        assert!(declares_symbol(
            "#[derive(Clone)] pub struct Profile {",
            "Profile"
        ));
        // One qualifier reader, two callers: the carrier scan sees the same
        // shapes the symbol scan does.
        assert!(opens_fn(
            "#[rustfmt::skip] pub(super) fn wanted()",
            "wanted"
        ));
        assert!(!opens_fn("pub(super) fn wanted_more()", "wanted"));
    }

    /// A Rust keyword is not a name a file can declare, so an absence claimed
    /// under one could never be refuted — `r#fn` is a different spelling and
    /// this grammar reads neither.
    #[test]
    fn a_symbol_ground_naming_a_keyword_is_caught() {
        for keyword in ["fn", "crate", "self", "Self", "struct", "mod", "yield"] {
            let subject = format!("crates/demo/tests/vocabulary.rs::{keyword}");
            refused(
                |registry| {
                    let case = find(registry, "a-dormant-layer-zero-case");
                    case.binding.reason = Some(format!("waits on {subject}"));
                    case.binding.grounds = vec![Ground::SymbolAbsent(subject)];
                },
                "a Rust keyword names nothing a file can declare",
            );
        }
    }

    /// The scan is a Rust declaration scan, so its subject is a Rust file. A
    /// shell script would pass every other check and then be read with a
    /// grammar it has nothing to do with.
    #[test]
    fn a_symbol_ground_on_a_file_that_is_not_rust_is_caught() {
        refused(
            |registry| {
                let case = find(registry, "a-dormant-layer-zero-case");
                case.binding.reason =
                    Some("waits on .github/scripts/lane-suite.sh::run_the_lane".to_string());
                case.binding.grounds = vec![Ground::SymbolAbsent(
                    ".github/scripts/lane-suite.sh::run_the_lane".to_string(),
                )];
            },
            "whose file is not a Rust source file",
        );
    }

    /// A symbol ground names one declaration. A path with no symbol, a path and
    /// two, an empty name and a name no Rust file can declare are each a claim
    /// the scan could never refute, so each is refused before the tree is asked.
    #[test]
    fn a_symbol_ground_naming_no_single_declaration_is_caught() {
        for subject in [
            "crates/demo/tests/vocabulary.rs",
            "crates/demo/tests/vocabulary.rs::Foo::bar",
            "crates/demo/tests/vocabulary.rs::",
            "crates/demo/tests/vocabulary.rs::foo-bar",
            "crates/demo/tests/vocabulary.rs::9lives",
            "crates/demo/tests/vocabulary.rs::_",
        ] {
            refused(
                |registry| {
                    let case = find(registry, "a-dormant-layer-zero-case");
                    case.binding.reason = Some(format!("waits on {subject}"));
                    case.binding.grounds = vec![Ground::SymbolAbsent(subject.to_string())];
                },
                "is not a `<file>::<Symbol>` reference to one declaration",
            );
        }
    }

    /// The other direction: a symbol cited as one the tree declares has to be
    /// declared, so a rename under the prose fails.
    #[test]
    fn a_symbol_cited_as_declared_is_held_to_the_file() {
        let found = problems(|registry| {
            let case = find(registry, "a-dormant-layer-zero-case");
            case.binding.reason =
                Some("crates/demo/tests/vocabulary.rs::foo holds the recognition half".to_string());
            case.binding.grounds = vec![Ground::SymbolPresent(
                "crates/demo/tests/vocabulary.rs::foo".to_string(),
            )];
        });
        assert_eq!(found, Vec::<String>::new());

        refused(
            |registry| {
                let case = find(registry, "a-dormant-layer-zero-case");
                case.binding.reason = Some(
                    "crates/demo/tests/vocabulary.rs::renamed holds the recognition half"
                        .to_string(),
                );
                case.binding.grounds = vec![Ground::SymbolPresent(
                    "crates/demo/tests/vocabulary.rs::renamed".to_string(),
                )];
            },
            "and that file declares no such name",
        );
    }

    /// **A symbol absence is an absence.** It fails when its subject lands, so
    /// a case standing on one is not in the class the gate cannot refute.
    #[test]
    fn dormancy_standing_on_a_symbol_absence_is_not_unrefutable() {
        let mut registry = sound();
        let case = find(&mut registry, "a-dormant-layer-zero-case");
        case.binding.reason = Some(
            "waits on a crates/demo/tests/vocabulary.rs::StatementsPrepared member".to_string(),
        );
        case.binding.grounds = vec![Ground::SymbolAbsent(
            "crates/demo/tests/vocabulary.rs::StatementsPrepared".to_string(),
        )];
        assert_eq!(registry.dormant_without_an_absence().count(), 0);
    }

    /// A symbol ground's claim is part of the contract, so flipping it moves
    /// the digest the suite pins.
    #[test]
    fn the_digest_moves_when_a_symbol_grounds_claim_flips() {
        let subject = "crates/demo/tests/vocabulary.rs::Foo";
        let mut absent = sound();
        find(&mut absent, "a-dormant-layer-zero-case")
            .binding
            .grounds = vec![Ground::SymbolAbsent(subject.to_string())];
        let mut present = sound();
        find(&mut present, "a-dormant-layer-zero-case")
            .binding
            .grounds = vec![Ground::SymbolPresent(subject.to_string())];
        assert_ne!(absent.contract_digest(), present.contract_digest());
    }

    /// The declaration grammar reads the shapes a pre-committed name lands in,
    /// and reads a line-opening identifier as a declaration — which over-reads
    /// in the loud direction.
    #[test]
    fn a_declaration_is_read_off_its_own_line() {
        for (source, name) in [
            ("pub(crate) fn foo(count: u64) {}", "foo"),
            ("fn foo<T>(count: T) {}", "foo"),
            ("pub struct Profile {", "Profile"),
            ("pub enum Counter {", "Counter"),
            ("union Overlap {", "Overlap"),
            ("pub trait Probe: Send {", "Probe"),
            ("pub type InvalidationKey = u64;", "InvalidationKey"),
            ("mod counters;", "counters"),
            ("pub const LIMIT: u64 = 1;", "LIMIT"),
            ("static REGISTRY: u64 = 1;", "REGISTRY"),
            ("    StatementsPrepared,", "StatementsPrepared"),
            ("    documents: u64,", "documents"),
            ("    Held", "Held"),
            // Members sharing one line are read: the line-opening arm reads
            // every segment, not only the first.
            ("    Foo, StatementsPrepared,", "StatementsPrepared"),
            (
                "pub enum Counter { Foo, StatementsPrepared }",
                "StatementsPrepared",
            ),
        ] {
            assert!(
                declares_symbol(source, name),
                "`{source}` declares `{name}`"
            );
        }

        for (source, name) in [
            ("pub(crate) fn foo(count: u64) {}", "fo"),
            ("pub struct Profiles {", "Profile"),
            ("let _ = Counter::Foo;", "Foo"),
            ("//   Foo,", "Foo"),
            ("    StatementsPrepared plus one", "StatementsPrepared"),
        ] {
            assert!(
                !declares_symbol(source, name),
                "`{source}` does not declare `{name}`"
            );
        }
    }

    #[test]
    fn a_bound_case_stating_grounds_is_caught() {
        refused(
            |registry| {
                find(registry, "a-bound-case").binding.grounds =
                    vec![Ground::Absent("crates/demo/src".to_string())];
            },
            "is bound and states grounds",
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
        let refused = TestIndex::from_cargo(root.root(), [reference.target().expect("a target")]);
        let registry = sound();
        let found = registry.audit(root.root(), &refused);
        assert!(
            found
                .iter()
                .any(|problem| problem.contains("listing `demo --test suite`'s tests failed")),
            "{found:#?}"
        );
        // And the same registry against a listing that was collected has
        // nothing wrong with it, so the refusal is what the audit reported.
        assert_eq!(registry.audit(root.root(), &index()), Vec::<String>::new());
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
            (
                "a reground reason",
                Box::new(|registry: &mut Registry| {
                    find(registry, "a-dormant-layer-zero-case").binding.grounds =
                        vec![Ground::Absent("crates/demo/src".to_string())];
                }),
            ),
            (
                "a ground whose claim flipped",
                Box::new(|registry: &mut Registry| {
                    // The same two paths, one of them claimed the other way.
                    find(registry, "a-dormant-layer-zero-case").binding.grounds = vec![
                        Ground::Absent("crates/demo/tests/suite.rs".to_string()),
                        Ground::Absent("crates/demo/src".to_string()),
                    ];
                }),
            ),
            (
                "dropped grounds",
                Box::new(|registry: &mut Registry| {
                    find(registry, "a-dormant-layer-zero-case")
                        .binding
                        .grounds
                        .clear();
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
        assert!(case.binding.grounds.is_empty());
    }

    /// A ground is one claim over one path, and the claim is the key it is
    /// written under.
    #[test]
    fn grounds_load_as_the_claims_the_data_spells() {
        let case = DORMANT.replace(
            r#""status": "dormant""#,
            r#""status": "dormant", "grounds": [{"absent": "crates/absent"}, {"present": "crates/present"}]"#,
        );
        let registry = registry(&case).expect("a well-formed case");
        assert_eq!(
            registry.cases[0].binding.grounds,
            vec![
                Ground::Absent("crates/absent".to_string()),
                Ground::Present("crates/present".to_string()),
            ]
        );
    }

    /// A symbol claim is one more key over one more subject, and the subject is
    /// the whole `file::Symbol` pair.
    #[test]
    fn symbol_grounds_load_as_the_claims_the_data_spells() {
        let case = DORMANT.replace(
            r#""status": "dormant""#,
            r#""status": "dormant", "grounds": [{"symbol-absent": "crates/a.rs::Absent"}, {"symbol-present": "crates/a.rs::Present"}]"#,
        );
        let registry = registry(&case).expect("a well-formed case");
        assert_eq!(
            registry.cases[0].binding.grounds,
            vec![
                Ground::SymbolAbsent("crates/a.rs::Absent".to_string()),
                Ground::SymbolPresent("crates/a.rs::Present".to_string()),
            ]
        );
    }

    #[test]
    fn a_claim_the_grammar_does_not_name_is_refused() {
        let case = DORMANT.replace(
            r#""status": "dormant""#,
            r#""status": "dormant", "grounds": [{"missing": "crates/absent"}]"#,
        );
        registry(&case).expect_err("`missing` is not a claim");
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
