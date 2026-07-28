//! The invariant-to-mechanism mapping, and the checks that keep it honest.
//!
//! The thirteen boundary invariants in `docs/architecture.md` are not all
//! carried the same way: some by an edge the allowlist withholds, some by a
//! lint, some by a judgment made in review. **This table is the authoritative
//! mapping**, and it is code rather than prose so that each claim it makes is
//! checked against the thing that carries it — the allowlist in
//! [`crate::architecture`] and the workspace-root `clippy.toml`.
//!
//! Two directions bind, and both matter:
//!
//! - An invariant whose mechanism says "edge-held" names the edges the
//!   allowlist must withhold, and [`consistency_violations`] fails if the
//!   allowlist carries one of them.
//! - A live lint rule names entries the `clippy.toml` must configure, and
//!   every configured entry must belong to some live rule. A lint that
//!   arrives without an edit here is unclaimed, and a rule that goes live
//!   without its configuration is a claim nothing enforces.
//!
//! A **review-held** entry is the honest name for a gap: no rule expresses it
//! yet, so it is judged by a person or not at all.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::architecture::{ALLOWED_EDGES, WORKSPACE_CRATES};

/// Whether a lint rule is configured today, or named against the day its
/// subject exists.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LintState {
    /// Configured in the workspace-root `clippy.toml`, and failing builds.
    Live,
    /// Named, not configured: the crate or symbol it governs does not exist
    /// yet. Its arrival is an edit here and an edit to the configuration, in
    /// the same change.
    Pending,
}

/// One symbol-level rule the dependency graph cannot express.
pub struct LintRule {
    pub name: &'static str,
    pub state: LintState,
    /// The path prefixes this rule owns in the workspace-root `clippy.toml`.
    /// Every configured entry belongs to exactly one live rule through them.
    pub prefixes: &'static [&'static str],
    /// Entries the rule requires, as `(clippy.toml key, path)`. A live rule
    /// carries at least one; a pending rule carries none.
    pub required: &'static [(&'static str, &'static str)],
}

/// The symbol-level ruleset, live and pending alike.
///
/// Rules are adopted one at a time, as each invariant's subject becomes real.
/// A pending rule is here so that its arrival is a deliberate edit rather than
/// a discovery.
pub const LINT_RULES: &[LintRule] = &[
    LintRule {
        name: "std::fs disallowed workspace-wide",
        state: LintState::Live,
        prefixes: &["std::fs::", "std::path::Path::"],
        required: &[
            ("disallowed-methods", "std::fs::read"),
            ("disallowed-methods", "std::fs::write"),
            ("disallowed-methods", "std::fs::read_dir"),
            ("disallowed-methods", "std::fs::remove_file"),
            ("disallowed-methods", "std::fs::create_dir_all"),
            ("disallowed-methods", "std::fs::File::open"),
            ("disallowed-methods", "std::fs::File::create"),
            ("disallowed-methods", "std::fs::OpenOptions::new"),
            ("disallowed-types", "std::fs::File"),
            ("disallowed-types", "std::fs::OpenOptions"),
        ],
    },
    LintRule {
        name: "no direct stdout writes outside norn-console",
        state: LintState::Live,
        prefixes: &["std::print", "std::println", "std::io::stdout"],
        required: &[
            ("disallowed-macros", "std::print"),
            ("disallowed-macros", "std::println"),
            ("disallowed-methods", "std::io::stdout"),
        ],
    },
    LintRule {
        name: "no SQLite connection opened outside norn-store",
        state: LintState::Pending,
        prefixes: &[],
        required: &[],
    },
    LintRule {
        name: "no serde_json::Value crossing the wire seam",
        state: LintState::Pending,
        prefixes: &[],
        required: &[],
    },
    LintRule {
        name: "no norn-config registry-surface use outside norn-host",
        state: LintState::Pending,
        prefixes: &[],
        required: &[],
    },
];

/// What an edge-held invariant asks of the allowlist.
pub enum EdgeClaim {
    /// Named edges the allowlist withholds.
    Absent(&'static [(&'static str, &'static str)]),
    /// A crate with no workspace dependency at all.
    NoDependencies(&'static str),
    /// The complete set of crates permitted to depend on one crate.
    OnlyDependents {
        on: &'static str,
        from: &'static [&'static str],
    },
}

/// How one invariant is carried. An invariant may carry several.
pub enum Mechanism {
    /// Held by the shape of the dependency allowlist: the architecture gate
    /// fails when the edge appears.
    Edge(EdgeClaim),
    /// Held by a symbol-level rule, named from [`LINT_RULES`].
    Lint(&'static str),
    /// Held by a judgment made in review, because no rule expresses it. A
    /// review-held invariant rots quietly, which is why it is written down.
    Review(&'static str),
}

/// One boundary invariant and everything that carries it.
pub struct Invariant {
    /// Its number in `docs/architecture.md`.
    pub number: u8,
    pub claim: &'static str,
    pub mechanisms: &'static [Mechanism],
}

/// The thirteen boundary invariants, each with the mechanisms that carry it.
pub const INVARIANTS: &[Invariant] = &[
    Invariant {
        number: 1,
        claim: "norn-client never depends on norn-store, norn-host or norn-serve, so no \
                client-side path can open a database or serve in-process",
        mechanisms: &[Mechanism::Edge(EdgeClaim::Absent(&[
            ("norn-client", "norn-store"),
            ("norn-client", "norn-host"),
            ("norn-client", "norn-serve"),
        ]))],
    },
    Invariant {
        number: 2,
        claim: "norn-host drives the substrate and norn-store implements it: driver calls live \
                in norn-store, and among product crates norn-host alone links it",
        // Not edge-held. `norn-testkit -> norn-store` is a permitted edge, so
        // the absence of edges cannot say "no other crate opens a
        // connection"; the lint scoped to the crates that ship says it, and
        // the sole-applier half is a judgment.
        mechanisms: &[
            Mechanism::Lint("no SQLite connection opened outside norn-store"),
            Mechanism::Review("that one applier executes every plan, judged with invariant 4"),
        ],
    },
    Invariant {
        number: 3,
        claim: "norn-wire has zero workspace dependencies and zero effects, and nothing crosses \
                the client/host seam that is not a wire type",
        mechanisms: &[
            Mechanism::Edge(EdgeClaim::NoDependencies("norn-wire")),
            Mechanism::Lint("no serde_json::Value crossing the wire seam"),
        ],
    },
    Invariant {
        number: 4,
        claim: "one plan vocabulary and one applier; a second execution path is a defect",
        mechanisms: &[Mechanism::Review(
            "whether a change is a second spelling of an existing operation",
        )],
    },
    Invariant {
        number: 5,
        claim: "surface-specific parameters are defects; surfaces render the vocabulary and \
                never define it",
        mechanisms: &[Mechanism::Review(
            "whether a parameter belongs to the vocabulary or to one surface",
        )],
    },
    Invariant {
        number: 6,
        claim: "one render seam, owned by norn-console: a verb or client module that writes \
                stdout or resolves color and tty itself is a defect",
        mechanisms: &[
            Mechanism::Lint("no direct stdout writes outside norn-console"),
            Mechanism::Review("whether color and tty resolution happens outside the render seam"),
        ],
    },
    Invariant {
        number: 7,
        claim: "norn-console never depends on a workspace crate, so it stays extraction-ready",
        mechanisms: &[Mechanism::Edge(EdgeClaim::NoDependencies("norn-console"))],
    },
    Invariant {
        number: 8,
        claim: "two vault-effect seams and only two: in the shipped product norn-fs and \
                norn-store are the only crates that touch a vault",
        // The database half is the shape of the allowlist: only the host
        // links the store into a running process, and the testkit never
        // ships. The filesystem half has no edge to hold it — norn-client has
        // machine-local filesystem effects and no norn-fs edge — so the
        // filesystem rule and its use-site allows are what carry it.
        mechanisms: &[
            Mechanism::Edge(EdgeClaim::OnlyDependents {
                on: "norn-store",
                from: &["norn-host", "norn-testkit"],
            }),
            Mechanism::Lint("std::fs disallowed workspace-wide"),
        ],
    },
    Invariant {
        number: 9,
        claim: "the fs event stream carries filesystem facts only, from a single producer",
        mechanisms: &[Mechanism::Review(
            "whether an event on the fs bus is a filesystem fact or a domain event",
        )],
    },
    Invariant {
        number: 10,
        claim: "one parser: all document syntax is read and written through norn-text",
        mechanisms: &[Mechanism::Review(
            "whether code interprets document text outside norn-text",
        )],
    },
    Invariant {
        number: 11,
        claim: "machine-local state has one owner: norn-config owns config-directory bytes, and \
                its registry surface is host-only",
        mechanisms: &[
            Mechanism::Lint("std::fs disallowed workspace-wide"),
            Mechanism::Lint("no norn-config registry-surface use outside norn-host"),
        ],
    },
    Invariant {
        number: 12,
        claim: "norn-embed is blind: no embed-to-store and no embed-to-fs edge, so inference \
                cannot reach findings or plans",
        mechanisms: &[Mechanism::Edge(EdgeClaim::Absent(&[
            ("norn-embed", "norn-store"),
            ("norn-embed", "norn-fs"),
        ]))],
    },
    Invariant {
        number: 13,
        claim: "the orchestrator is protocol-blind: norn-host depends on no protocol or serving \
                crate, so a protocol type in orchestrator code is a compile error",
        mechanisms: &[Mechanism::Edge(EdgeClaim::Absent(&[
            ("norn-host", "norn-serve"),
            ("norn-host", "norn-mcp"),
            ("norn-host", "norn"),
        ]))],
    },
];

/// The rule of that name, if the ruleset carries it.
pub fn lint_rule(name: &str) -> Option<&'static LintRule> {
    LINT_RULES.iter().find(|rule| rule.name == name)
}

/// The entries configured in one `clippy.toml`, as `(key, path)`.
///
/// The reader is narrow on purpose: it accepts the shape this workspace's
/// configuration is written in — a key, a bracketed list, one `{ path = "..",
/// reason = ".." }` entry per line — and refuses anything else rather than
/// skipping past it. A parser that shrugs at a line it does not understand
/// would report a rule as configured when it is not.
#[derive(Clone, Debug, Default)]
pub struct ClippyConfig {
    pub path: PathBuf,
    pub entries: BTreeSet<(String, String)>,
}

impl ClippyConfig {
    /// Read the workspace-root configuration. One file holds the whole
    /// ruleset, because clippy's configuration does not merge per-crate.
    pub fn read(workspace_root: &Path) -> Result<Self, String> {
        let path = workspace_root.join("clippy.toml");
        #[allow(clippy::disallowed_methods)] // Reading the workspace's own configuration.
        let text = std::fs::read_to_string(&path)
            .map_err(|e| format!("could not read {}: {e}", path.display()))?;
        let mut config = Self::parse(&text)?;
        config.path = path;
        Ok(config)
    }

    pub fn parse(text: &str) -> Result<Self, String> {
        let mut config = ClippyConfig::default();
        let mut key: Option<&str> = None;
        for (number, line) in text.lines().enumerate() {
            let line = line.trim();
            let at = number + 1;
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            match key {
                None => {
                    let opened = line
                        .split_once(" = [")
                        .filter(|(_, rest)| rest.is_empty())
                        .map(|(name, _)| name);
                    key =
                        Some(opened.ok_or_else(|| {
                            format!("line {at} opens no configuration key: {line}")
                        })?);
                }
                Some(current) => {
                    if line == "]" {
                        key = None;
                        continue;
                    }
                    let path = entry_path(line)
                        .ok_or_else(|| format!("line {at} carries no `path`: {line}"))?;
                    config
                        .entries
                        .insert((current.to_string(), path.to_string()));
                }
            }
        }
        match key {
            None => Ok(config),
            Some(unclosed) => Err(format!("`{unclosed}` is never closed")),
        }
    }

    pub fn carries(&self, key: &str, path: &str) -> bool {
        self.entries.contains(&(key.to_string(), path.to_string()))
    }
}

fn entry_path(line: &str) -> Option<&str> {
    let (_, rest) = line.split_once("path = \"")?;
    let (path, _) = rest.split_once('"')?;
    Some(path)
}

/// Every way the mapping contradicts the allowlist or the configured lints,
/// one line each. A consistent mapping produces none.
pub fn consistency_violations(config: &ClippyConfig) -> Vec<String> {
    let mut problems = Vec::new();
    let known: BTreeSet<&str> = WORKSPACE_CRATES.iter().copied().collect();
    let allowed: BTreeSet<(&str, &str)> = ALLOWED_EDGES.iter().copied().collect();

    let mut numbers: BTreeSet<u8> = BTreeSet::new();
    for invariant in INVARIANTS {
        if !numbers.insert(invariant.number) {
            problems.push(format!("invariant {} is mapped twice", invariant.number));
        }
        if invariant.mechanisms.is_empty() {
            problems.push(format!(
                "invariant {} names no mechanism at all, not even review",
                invariant.number
            ));
        }
        for mechanism in invariant.mechanisms {
            match mechanism {
                Mechanism::Edge(claim) => {
                    edge_claim_violations(invariant.number, claim, &known, &allowed, &mut problems)
                }
                Mechanism::Lint(name) => match lint_rule(name) {
                    None => problems.push(format!(
                        "invariant {} names lint rule `{name}`, which the ruleset does not carry",
                        invariant.number
                    )),
                    Some(rule) => lint_rule_violations(rule, config, &mut problems),
                },
                Mechanism::Review(judgment) => {
                    if judgment.trim().is_empty() {
                        problems.push(format!(
                            "invariant {} is review-held with no judgment named",
                            invariant.number
                        ));
                    }
                }
            }
        }
    }
    for number in 1..=13u8 {
        if !numbers.contains(&number) {
            problems.push(format!("invariant {number} is not mapped to any mechanism"));
        }
    }

    for (key, path) in &config.entries {
        let claimed = LINT_RULES
            .iter()
            .filter(|rule| rule.state == LintState::Live)
            .any(|rule| rule.prefixes.iter().any(|prefix| path.starts_with(prefix)));
        if !claimed {
            problems.push(format!(
                "`{key}` configures `{path}`, which no live rule in the mapping claims"
            ));
        }
    }

    problems
}

fn lint_rule_violations(rule: &LintRule, config: &ClippyConfig, problems: &mut Vec<String>) {
    match rule.state {
        LintState::Live => {
            if rule.required.is_empty() {
                problems.push(format!(
                    "rule `{}` is live and requires no configuration, so nothing carries it",
                    rule.name
                ));
            }
            for (key, path) in rule.required {
                if !config.carries(key, path) {
                    problems.push(format!(
                        "rule `{}` is live, but {} does not configure `{path}` under `{key}`",
                        rule.name,
                        config.path.display()
                    ));
                }
            }
        }
        LintState::Pending => {
            if !rule.required.is_empty() {
                problems.push(format!(
                    "rule `{}` is pending and names required configuration; a rule with \
                     configuration is live",
                    rule.name
                ));
            }
        }
    }
}

fn edge_claim_violations(
    number: u8,
    claim: &EdgeClaim,
    known: &BTreeSet<&str>,
    allowed: &BTreeSet<(&str, &str)>,
    problems: &mut Vec<String>,
) {
    let unknown =
        |name: &str| format!("invariant {number} names `{name}`, which the crate map does not");
    match *claim {
        EdgeClaim::Absent(edges) => {
            for &(from, to) in edges {
                for name in [from, to] {
                    if !known.contains(name) {
                        problems.push(unknown(name));
                    }
                }
                if allowed.contains(&(from, to)) {
                    problems.push(format!(
                        "invariant {number} requires no `{from}` -> `{to}` edge, and the \
                         allowlist permits one"
                    ));
                }
            }
        }
        EdgeClaim::NoDependencies(crate_name) => {
            if !known.contains(crate_name) {
                problems.push(unknown(crate_name));
            }
            for &(from, to) in allowed {
                if from == crate_name {
                    problems.push(format!(
                        "invariant {number} requires `{crate_name}` to depend on nothing, and \
                         the allowlist permits `{from}` -> `{to}`"
                    ));
                }
            }
        }
        EdgeClaim::OnlyDependents { on, from } => {
            if !known.contains(on) {
                problems.push(unknown(on));
            }
            for &dependent in from {
                if !known.contains(dependent) {
                    problems.push(unknown(dependent));
                }
                if !allowed.contains(&(dependent, on)) {
                    problems.push(format!(
                        "invariant {number} names `{dependent}` as a permitted dependent of \
                         `{on}`, and the allowlist carries no such edge"
                    ));
                }
            }
            for &(permitted_from, permitted_to) in allowed {
                if permitted_to == on && !from.contains(&permitted_from) {
                    problems.push(format!(
                        "invariant {number} lists the crates permitted to depend on `{on}`, and \
                         the allowlist permits `{permitted_from}` -> `{on}` besides"
                    ));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> ClippyConfig {
        ClippyConfig {
            path: PathBuf::from("/workspace/clippy.toml"),
            entries: LINT_RULES
                .iter()
                .flat_map(|rule| rule.required)
                .map(|(key, path)| ((*key).to_string(), (*path).to_string()))
                .collect(),
        }
    }

    #[test]
    fn the_mapping_is_consistent_with_the_allowlist_and_the_ruleset() {
        assert_eq!(consistency_violations(&config()), Vec::<String>::new());
    }

    #[test]
    fn every_mechanism_names_a_rule_the_ruleset_carries() {
        for invariant in INVARIANTS {
            for mechanism in invariant.mechanisms {
                if let Mechanism::Lint(name) = mechanism {
                    assert!(
                        lint_rule(name).is_some(),
                        "invariant {} names unknown rule `{name}`",
                        invariant.number
                    );
                }
            }
        }
    }

    #[test]
    fn a_live_rule_with_no_configuration_is_caught() {
        let mut config = config();
        config.entries.remove(&(
            "disallowed-methods".to_string(),
            "std::fs::read".to_string(),
        ));
        let problems = consistency_violations(&config);
        assert!(
            problems.iter().any(|p| p.contains("does not configure")),
            "{problems:?}"
        );
    }

    #[test]
    fn a_configured_entry_no_rule_claims_is_caught() {
        let mut config = config();
        config.entries.insert((
            "disallowed-methods".to_string(),
            "rusqlite::Connection::open".to_string(),
        ));
        let problems = consistency_violations(&config);
        assert!(
            problems.iter().any(|p| p.contains("no live rule")),
            "{problems:?}"
        );
    }

    #[test]
    fn the_configuration_reader_takes_entries_and_refuses_anything_else() {
        let config = ClippyConfig::parse(
            "# a comment\n\
             disallowed-methods = [\n\
             \x20   # why the next one is here\n\
             \x20   { path = \"std::fs::read\", reason = \"the seam owns reads\" },\n\
             ]\n",
        )
        .expect("a configuration in the shape this workspace writes");
        assert!(config.carries("disallowed-methods", "std::fs::read"));
        assert_eq!(config.entries.len(), 1);

        let unclosed = ClippyConfig::parse("disallowed-types = [\n").expect_err("an unclosed key");
        assert!(unclosed.contains("never closed"), "{unclosed}");

        let stray = ClippyConfig::parse("avoid-breaking-exported-api = true\n")
            .expect_err("a key that is not a list");
        assert!(stray.contains("opens no configuration key"), "{stray}");

        let entryless = ClippyConfig::parse("disallowed-types = [\n    { }\n]\n")
            .expect_err("an entry naming no path");
        assert!(entryless.contains("carries no `path`"), "{entryless}");
    }
}
