//! The package-shard certification: each PR test shard builds every test unit
//! with the features a whole-workspace `cargo test` would.
//!
//! CI splits `cargo test` into two package shards (see `docs/ci.md`). Cargo
//! unifies features across the packages one invocation selects, so a
//! dev-dependency in a package one shard leaves out can switch a feature on in
//! another package, and the shard without it then builds that package's tests
//! without the feature. This module reads the claim from stable `cargo tree`
//! output, which honors package selection and resolver-2 unification;
//! `cargo metadata` does not, since it resolves the whole workspace whatever
//! is selected.
//!
//! The selections come from `.github/workflows/ci.yml` itself, so the guard
//! cannot drift from the jobs it certifies. Both reads include normal,
//! development and build edges for the host target, the one tests are built
//! for.

use std::collections::{BTreeMap, BTreeSet};
use std::process::Command;

use serde_yaml::Value;

/// The jobs of `ci.yml` that are the two package shards.
pub const SHARD_JOBS: [&str; 2] = ["build", "build-host"];

/// The flags of one `cargo tree` that select packages.
pub type Selection = Vec<String>;

/// What `cargo tree` resolved for one selection.
#[derive(Debug, Default, Eq, PartialEq)]
pub struct TreeReading {
    /// The workspace packages the selection names, the roots of the tree.
    pub selected: BTreeSet<String>,
    /// The features each package in the tree is built with, by package name.
    pub features: BTreeMap<String, BTreeSet<String>>,
}

impl TreeReading {
    /// Reads `cargo tree --prefix depth` output: a depth-0 package line is a
    /// selected root, and every `name feature "f"` line, at any depth, is a
    /// feature the resolve turned on for `name`.
    ///
    /// The `default` node is not read: cargo prints it for a package a
    /// dependency edge reaches with default features whether or not the
    /// package declares one, and omits it for a root that declares none. What
    /// a declared `default` turns on appears as feature nodes of its own.
    pub fn parse(tree: &str) -> Self {
        let mut reading = Self::default();
        for line in tree.lines() {
            let depth = line.chars().take_while(char::is_ascii_digit).count();
            let mut words = line[depth..].split_whitespace();
            let (Some(name), Some(second)) = (words.next(), words.next()) else {
                continue;
            };
            if second == "feature" {
                let feature = words.next().map(|f| f.trim_matches('"'));
                if let Some(feature) = feature.filter(|f| *f != "default") {
                    reading
                        .features
                        .entry(name.to_string())
                        .or_default()
                        .insert(feature.to_string());
                }
            } else if &line[..depth] == "0" && second.starts_with('v') {
                reading.selected.insert(name.to_string());
            }
        }
        reading
    }

    /// Runs `cargo tree` under `selection`.
    ///
    /// The cargo that built this test reads the tree, so the reading cannot
    /// come from a different toolchain than the one under test.
    pub fn read(selection: &[String]) -> Result<Self, String> {
        let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
        let output = Command::new(&cargo)
            .args(["tree", "--locked", "--prefix", "depth"])
            .args(["--edges", "normal,dev,build,features"])
            .args(selection)
            .output()
            .map_err(|e| format!("could not run `{cargo} tree`: {e}"))?;
        if !output.status.success() {
            return Err(format!(
                "`{cargo} tree {}` failed: {}",
                selection.join(" "),
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
        let tree = String::from_utf8(output.stdout)
            .map_err(|e| format!("cargo tree is not UTF-8: {e}"))?;
        Ok(Self::parse(&tree))
    }

    fn features_of(&self, package: &str) -> BTreeSet<&str> {
        self.features
            .get(package)
            .map(|set| set.iter().map(String::as_str).collect())
            .unwrap_or_default()
    }
}

/// One shard: the job that runs it and the package selection its `cargo test`
/// names.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Shard {
    pub job: String,
    pub selection: Selection,
}

/// The shard selections `workflow` (the text of `ci.yml`) runs.
///
/// Each shard job must hold exactly one `cargo test` step, and its command may
/// carry only `--locked` and the package selectors the guard understands; any
/// other flag could change what is built, so it is refused rather than
/// ignored.
pub fn shards_in(workflow: &str) -> Result<Vec<Shard>, String> {
    let document: Value =
        serde_yaml::from_str(workflow).map_err(|e| format!("ci.yml is not YAML: {e}"))?;
    SHARD_JOBS
        .iter()
        .map(|job| {
            let steps = document
                .get("jobs")
                .and_then(|jobs| jobs.get(*job))
                .and_then(|job| job.get("steps"))
                .and_then(Value::as_sequence)
                .ok_or_else(|| format!("ci.yml has no job `{job}` with steps"))?;
            let commands: Vec<&str> = steps
                .iter()
                .filter_map(|step| step.get("run").and_then(Value::as_str))
                .filter(|run| run.contains("cargo test"))
                .collect();
            let [command] = commands[..] else {
                return Err(format!(
                    "job `{job}` must run exactly one `cargo test` step, found {}",
                    commands.len()
                ));
            };
            Ok(Shard {
                job: (*job).to_string(),
                selection: selection_of(command)?,
            })
        })
        .collect()
}

/// The package-selecting flags of one `cargo test` command line.
fn selection_of(command: &str) -> Result<Selection, String> {
    let mut words = command.split_whitespace();
    let is_cargo_test = words.by_ref().any(|word| word == "cargo") && words.next() == Some("test");
    if !is_cargo_test {
        return Err(format!("`{command}` is not a `cargo test` command"));
    }
    let mut selection = Selection::new();
    while let Some(word) = words.next() {
        match word {
            "--locked" => {}
            "--workspace" => selection.push(word.to_string()),
            "-p" | "--package" | "--exclude" => {
                let name = words
                    .next()
                    .ok_or_else(|| format!("`{command}` ends after `{word}`"))?;
                selection.extend([word.to_string(), name.to_string()]);
            }
            other => {
                return Err(format!(
                    "`{command}` carries `{other}`, which the shard certification does not read"
                ));
            }
        }
    }
    Ok(selection)
}

/// Everything the shards fail to certify, as sentences naming the package and
/// the feature: a package in no shard or in both, and a package built under a
/// shard with other features than the whole-workspace run gives it.
pub fn violations(whole: &TreeReading, shards: &[(Shard, TreeReading)]) -> Vec<String> {
    let mut found = Vec::new();
    for package in &whole.selected {
        let holders: Vec<&str> = shards
            .iter()
            .filter(|(_, reading)| reading.selected.contains(package))
            .map(|(shard, _)| shard.job.as_str())
            .collect();
        if holders.len() != 1 {
            found.push(format!(
                "package `{package}` is tested by {} shards ({}); it must be in exactly one",
                holders.len(),
                holders.join(", ")
            ));
        }
    }
    for (shard, reading) in shards {
        for package in reading.selected.difference(&whole.selected) {
            found.push(format!(
                "shard `{}` selects `{package}`, which the whole workspace does not hold",
                shard.job
            ));
        }
        for package in reading.selected.intersection(&whole.selected) {
            let (whole_features, shard_features) =
                (whole.features_of(package), reading.features_of(package));
            for feature in whole_features.difference(&shard_features) {
                found.push(format!(
                    "shard `{}` builds `{package}` without feature `{feature}`, which a whole-workspace run enables",
                    shard.job
                ));
            }
            for feature in shard_features.difference(&whole_features) {
                found.push(format!(
                    "shard `{}` builds `{package}` with feature `{feature}`, which a whole-workspace run does not enable",
                    shard.job
                ));
            }
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reading(selected: &[&str], features: &[(&str, &str)]) -> TreeReading {
        let mut reading = TreeReading {
            selected: selected.iter().map(|s| (*s).to_string()).collect(),
            ..TreeReading::default()
        };
        for (package, feature) in features {
            reading
                .features
                .entry((*package).to_string())
                .or_default()
                .insert((*feature).to_string());
        }
        reading
    }

    fn shard(job: &str) -> Shard {
        Shard {
            job: job.to_string(),
            selection: Vec::new(),
        }
    }

    #[test]
    fn a_tree_reads_its_roots_and_every_feature_at_any_depth() {
        let tree = "0norn-fs v0.0.0 (/w/norn-fs)\n\
                    1norn-fs feature \"default\" (command-line)\n\
                    2norn-fs v0.0.0 (/w/norn-fs) (*)\n\
                    1norn-fs feature \"test-support\"\n\
                    2other v1.0.0\n\
                    3norn-fs feature \"induced-failure\" (*)\n";
        let read = TreeReading::parse(tree);
        assert_eq!(read.selected, BTreeSet::from(["norn-fs".to_string()]));
        assert_eq!(
            read.features_of("norn-fs"),
            BTreeSet::from(["test-support", "induced-failure"])
        );
    }

    #[test]
    fn shards_that_partition_the_workspace_with_equal_features_conform() {
        let whole = reading(&["a", "b"], &[("a", "x")]);
        let shards = [
            (shard("one"), reading(&["a"], &[("a", "x")])),
            (shard("two"), reading(&["b"], &[])),
        ];
        assert_eq!(violations(&whole, &shards), Vec::<String>::new());
    }

    #[test]
    fn a_feature_one_shard_loses_is_named_with_its_package() {
        let whole = reading(&["a", "b"], &[("a", "x")]);
        let shards = [
            (shard("one"), reading(&["a"], &[])),
            (shard("two"), reading(&["b"], &[])),
        ];
        let found = violations(&whole, &shards);
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].contains("`a`") && found[0].contains("feature `x`"));
    }

    #[test]
    fn a_feature_a_shard_gains_is_refused_too() {
        let whole = reading(&["a"], &[]);
        let shards = [(shard("one"), reading(&["a"], &[("a", "y")]))];
        assert_eq!(violations(&whole, &shards).len(), 1);
    }

    #[test]
    fn a_package_in_no_shard_or_in_two_fails() {
        let whole = reading(&["a", "b", "c"], &[]);
        let shards = [
            (shard("one"), reading(&["a", "b"], &[])),
            (shard("two"), reading(&["b"], &[])),
        ];
        let found = violations(&whole, &shards);
        assert_eq!(found.len(), 2, "{found:?}");
        assert!(found.iter().any(|f| f.contains("`b`")));
        assert!(found.iter().any(|f| f.contains("`c`")));
    }

    const WORKFLOW: &str = "
jobs:
  build:
    steps:
      - run: cargo build --locked --workspace
      - run: .github/scripts/flake-tripwire.sh cargo test --locked --workspace --exclude norn-host --exclude norn-testkit
  build-host:
    steps:
      - run: cargo test --locked -p norn-host -p norn-testkit
";

    #[test]
    fn the_shard_selections_are_read_from_the_workflow() {
        let shards = shards_in(WORKFLOW).expect("reading the shards");
        let selections: Vec<String> = shards.iter().map(|s| s.selection.join(" ")).collect();
        assert_eq!(
            selections,
            [
                "--workspace --exclude norn-host --exclude norn-testkit",
                "-p norn-host -p norn-testkit"
            ]
        );
    }

    #[test]
    fn a_flag_the_guard_does_not_read_is_refused() {
        let workflow = WORKFLOW.replace("--locked -p", "--locked --release -p");
        let error = shards_in(&workflow).expect_err("an unread flag");
        assert!(error.contains("--release"), "{error}");
    }

    #[test]
    fn a_shard_job_without_exactly_one_test_step_is_refused() {
        let workflow = WORKFLOW.replace("cargo test --locked -p", "cargo nextest run -p");
        assert!(shards_in(&workflow).is_err());
    }
}
