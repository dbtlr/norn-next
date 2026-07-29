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
//! there is no attribute to remove, because binding a case means editing its
//! entry to name the tests that carry it, and [`Registry::audit`] holds those
//! names to real functions. A case whose venue is layer 0 is bindable in the
//! harness as it stands, so leaving one dormant costs a stated reason.
//!
//! This module is the loader and the gate. The suite it gates lives in the
//! `norn` bin package, beside the registry data.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::{Path, PathBuf};

use serde::Deserialize;

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
    /// Why a case that could bind today does not. Required of a dormant
    /// layer-0 case, because layer 0 is what exists.
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
}

impl fmt::Display for TestRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}::{}", self.file.display(), self.function)
    }
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
        let text = read(path)?;
        serde_json::from_str(&text).map_err(|source| RegistryError::Parse {
            path: path.to_path_buf(),
            source,
        })
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
    /// named once and states something falsifiable with a citation behind it,
    /// that the mandatory set is exactly the one the harness pins, that a
    /// bound case names tests that exist, and that a dormant case at the
    /// layer that exists says why it is still dormant.
    ///
    /// `workspace_root` is where a test reference's path resolves from.
    pub fn audit(&self, workspace_root: &Path) -> Vec<String> {
        let mut problems = Vec::new();
        self.audit_venues(&mut problems);
        self.audit_cases(&mut problems);
        self.audit_mandatory(&mut problems);
        self.audit_bindings(workspace_root, &mut problems);
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

    fn audit_bindings(&self, workspace_root: &Path, problems: &mut Vec<String>) {
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
                        match TestRef::parse(reference) {
                            Err(problem) => problems.push(format!("`{name}`: {problem}")),
                            Ok(test) => {
                                let path = workspace_root.join(&test.file);
                                let source = sources
                                    .entry(path)
                                    .or_insert_with_key(|path| read(path).ok());
                                match source {
                                    None => problems.push(format!(
                                        "`{name}` names `{test}`, whose file is not in the \
                                         workspace"
                                    )),
                                    Some(text) if !declares_test(text, &test.function) => {
                                        problems.push(format!(
                                            "`{name}` names `{test}`, and that file declares no \
                                             `#[test] fn {}`",
                                            test.function
                                        ));
                                    }
                                    Some(_) => {}
                                }
                            }
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
                        None if case.venue == 0 => problems.push(format!(
                            "`{name}` is dormant at layer 0, which is the layer that exists, and \
                             states no reason"
                        )),
                        _ => {}
                    }
                }
            }
        }
    }
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
    use super::{Registry, TestRef, declares_test, is_kebab_case, opens_fn};
    use std::path::Path;

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
