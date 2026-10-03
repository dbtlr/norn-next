//! What the workflows run, read once for every guard that asks.
//!
//! Two guards ask the workflows the same kind of question. The lane guards in
//! [`crate::lanes`] ask which targets a step adopts the ignored cases of
//! wholesale, and with which features; the regression audit asks that too, and
//! also which packages a step tests with the carrier feature on. Both answers
//! are claims about what CI runs, so both come from this one reader.
//!
//! **A workflow is read as the YAML it is.** A step is the mapping under a
//! job's `steps`, its command is the parsed value of its `run` key, and its
//! environment is the parsed `env` maps of the workflow, the job and the step,
//! the nearer one winning. So a comment is never part of a value, a scalar
//! continued or folded onto the next line is read as the one command it is, and
//! a command spelled in a step's `name` or in another key is no command at all.
//!
//! **A step vouches for what it runs only when nothing decides it may not
//! run.** A step or job carrying `if:` or `continue-on-error:`, whatever the
//! value, may be skipped or may fail without failing the run, so it vouches for
//! nothing. That is the strict side: an `if: ${{ !cancelled() }}` that would
//! have run is refused with an `if: false` that would not.
//!
//! **A command is read whole or not at all.** It is one line, holding no shell
//! metacharacter anywhere — no pipe, list operator, redirection, comment,
//! expansion, escape, quote or subshell — so it is one process with the
//! arguments it spells. The arguments are then held to a grammar: optionally
//! the flake tripwire in front, then either the lane script with a package, a
//! target and harness arguments that select nothing, or `cargo test` with the
//! flags that name a package, a feature and one target and the few that change
//! nothing about which tests run. A command outside that grammar runs nothing,
//! so a step this cannot read fails whatever needed it rather than vouching for
//! a test it may not run.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde_yaml::Value;

use crate::regression::Target;

/// The script a CI step adopts a whole target's ignored cases through, as a
/// step spells it.
pub(crate) const LANE_SCRIPT: &str = ".github/scripts/lane-suite.sh";

/// The wrapper a step may run its command through, which runs the command it
/// is given and changes nothing about it.
const FLAKE_TRIPWIRE: &str = ".github/scripts/flake-tripwire.sh";

/// The environment key a lane step names its cargo features through.
pub(crate) const LANE_FEATURES: &str = "LANE_FEATURES";

/// The characters that make a command more, or other, than one process with
/// the arguments it spells.
const SHELL_METACHARACTERS: &[char] = &[
    '|', '&', ';', '<', '>', '#', '$', '\\', '\'', '"', '(', ')', '`', '\n',
];

/// One step of one job, as the workflow declares it.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct Step {
    /// The parsed value of the step's `run`, or `None` where it runs no
    /// command of its own.
    pub(crate) run: Option<String>,
    /// The workflow's, the job's and the step's `env`, the nearer winning.
    pub(crate) env: BTreeMap<String, String>,
    /// Whether the step or its job carries `if:` or `continue-on-error:`.
    pub(crate) conditional: bool,
}

/// One step that runs a target's ignored cases wholesale through
/// [`LANE_SCRIPT`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct LaneStep {
    pub(crate) package: String,
    pub(crate) target: String,
    /// The features the step names through [`LANE_FEATURES`].
    pub(crate) features: BTreeSet<String>,
    /// Whether the step runs whenever its workflow does: no `if:` and no
    /// `continue-on-error:` on it or its job.
    pub(crate) vouches: bool,
}

/// One `cargo test` invocation's reach over one package with one feature on.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) struct FeaturedRun {
    pub(crate) package: String,
    /// The one target the invocation selects, or `None` where it selects none
    /// and so runs every test target the package has.
    pub(crate) target: Option<Target>,
    pub(crate) feature: String,
}

/// What the workflows run.
#[derive(Debug, Default)]
pub(crate) struct CiSteps {
    /// Every lane step, whether or not it vouches.
    pub(crate) lanes: Vec<LaneStep>,
    /// What the vouching `cargo test` steps run with a feature on.
    pub(crate) featured: BTreeSet<FeaturedRun>,
}

impl CiSteps {
    /// Whether a vouching step runs `package`'s `target` with `feature` on.
    pub(crate) fn runs_with_feature(&self, package: &str, target: &Target, feature: &str) -> bool {
        self.featured.iter().any(|run| {
            run.package == package
                && run.feature == feature
                && run
                    .target
                    .as_ref()
                    .is_none_or(|selected| selected == target)
        })
    }

    /// The features the vouching lane steps adopting `package`'s `target`
    /// name between them, or `None` where no vouching lane step adopts it.
    pub(crate) fn adopted(&self, package: &str, target: &str) -> Option<BTreeSet<String>> {
        let mut adopting = self
            .lanes
            .iter()
            .filter(|lane| lane.vouches && lane.package == package && lane.target == target)
            .peekable();
        adopting.peek()?;
        Some(
            adopting
                .flat_map(|lane| lane.features.iter().cloned())
                .collect(),
        )
    }
}

/// What a command runs, where [`invocation`] can read it.
#[derive(Debug, Eq, PartialEq)]
enum Invocation {
    /// The lane script over one package's target.
    Lane { package: String, target: String },
    /// `cargo test`, with what it runs under a feature.
    Test(Vec<FeaturedRun>),
}

/// What the workflows in `directory` run.
///
/// A directory that cannot be read, a workflow that cannot be read or parsed,
/// and a directory holding no workflow are each an error rather than an empty
/// answer, so a caller asking whether a step runs something never reads
/// "nothing was read" as "nothing runs".
#[allow(clippy::disallowed_methods)] // Harness scaffolding: reads this repository's own workflow files.
pub(crate) fn ci_steps_in(directory: &Path) -> Result<CiSteps, String> {
    let entries = std::fs::read_dir(directory)
        .map_err(|e| format!("reading {} for workflows: {e}", directory.display()))?;
    let mut workflows = 0usize;
    let mut read = CiSteps::default();
    for entry in entries {
        let path = entry
            .map_err(|e| format!("reading {} for workflows: {e}", directory.display()))?
            .path();
        if !path.extension().is_some_and(|e| e == "yml" || e == "yaml") {
            continue;
        }
        workflows += 1;
        let text = std::fs::read_to_string(&path)
            .map_err(|e| format!("reading {}: {e}", path.display()))?;
        let steps = steps_in(&text).map_err(|e| format!("reading {}: {e}", path.display()))?;
        for step in steps {
            let Some(run) = step.run.as_deref() else {
                continue;
            };
            match invocation(run) {
                Some(Invocation::Lane { package, target }) => read.lanes.push(LaneStep {
                    package,
                    target,
                    features: features_named(step.env.get(LANE_FEATURES)),
                    vouches: !step.conditional,
                }),
                Some(Invocation::Test(runs)) if !step.conditional => read.featured.extend(runs),
                Some(Invocation::Test(_)) | None => {}
            }
        }
    }
    if workflows == 0 {
        return Err(format!(
            "{} holds no workflow, so nothing was read",
            directory.display()
        ));
    }
    Ok(read)
}

/// Every step of every job in `workflow`, or why the workflow is not one.
///
/// A job with no `steps` — one that calls a reusable workflow — runs no step
/// here. A `jobs`, job or step that is not the mapping or sequence a workflow
/// holds there is an error, because reading it as empty would read a workflow
/// this does not understand as one that runs nothing on purpose.
fn steps_in(workflow: &str) -> Result<Vec<Step>, String> {
    let document: Value =
        serde_yaml::from_str(workflow).map_err(|e| format!("not a YAML document: {e}"))?;
    let workflow_env = env_of(document.get("env"));
    let Some(jobs) = document.get("jobs") else {
        return Ok(Vec::new());
    };
    let jobs = jobs
        .as_mapping()
        .ok_or_else(|| "`jobs` is not a mapping".to_string())?;
    let mut steps = Vec::new();
    for (name, job) in jobs {
        let name = name.as_str().unwrap_or("?");
        if !job.is_mapping() {
            return Err(format!("job `{name}` is not a mapping"));
        }
        let job_conditional = is_conditional(job);
        let mut job_env = workflow_env.clone();
        job_env.extend(env_of(job.get("env")));
        let Some(declared) = job.get("steps") else {
            continue;
        };
        let declared = declared
            .as_sequence()
            .ok_or_else(|| format!("job `{name}`'s `steps` is not a sequence"))?;
        for step in declared {
            if !step.is_mapping() {
                return Err(format!("a step of job `{name}` is not a mapping"));
            }
            let mut env = job_env.clone();
            env.extend(env_of(step.get("env")));
            steps.push(Step {
                run: step.get("run").and_then(Value::as_str).map(str::to_string),
                env,
                conditional: job_conditional || is_conditional(step),
            });
        }
    }
    Ok(steps)
}

/// Whether a job or step carries a key that lets it be skipped or fail
/// without failing the run.
fn is_conditional(node: &Value) -> bool {
    node.get("if").is_some() || node.get("continue-on-error").is_some()
}

/// The scalar entries of an `env` mapping, each as the string the runner
/// exports. Anything else — an expression standing for the whole map, a
/// nested value — names nothing this can read.
fn env_of(env: Option<&Value>) -> BTreeMap<String, String> {
    let Some(map) = env.and_then(Value::as_mapping) else {
        return BTreeMap::new();
    };
    map.iter()
        .filter_map(|(key, value)| {
            let value = match value {
                Value::String(text) => text.clone(),
                Value::Number(number) => number.to_string(),
                Value::Bool(flag) => flag.to_string(),
                _ => return None,
            };
            Some((key.as_str()?.to_string(), value))
        })
        .collect()
}

/// The features an environment value names, split the way the lane script
/// hands them to cargo.
fn features_named(value: Option<&String>) -> BTreeSet<String> {
    value
        .map(|value| {
            value
                .split([',', ' '])
                .filter(|named| !named.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// What the command `run` runs, or `None` where it is not one this reads
/// whole.
fn invocation(run: &str) -> Option<Invocation> {
    // A block scalar's one line ends in a newline, which is not a second line.
    let line = run.trim_end_matches('\n');
    if line.contains(SHELL_METACHARACTERS) {
        return None;
    }
    let tokens: Vec<&str> = line.split_whitespace().collect();
    let command = match tokens.as_slice() {
        [FLAKE_TRIPWIRE, command @ ..] => command,
        command => command,
    };
    match command {
        [LANE_SCRIPT, package, target, harness @ ..]
            if !package.starts_with('-') && !target.starts_with('-') =>
        {
            harness_selects_nothing(harness)?;
            Some(Invocation::Lane {
                package: (*package).to_string(),
                target: (*target).to_string(),
            })
        }
        ["cargo", "test", arguments @ ..] => featured_runs_of(arguments).map(Invocation::Test),
        _ => None,
    }
}

/// The runs the arguments of one `cargo test` invocation make under a
/// feature, or `None` where an argument is one [`invocation`] does not read.
fn featured_runs_of(arguments: &[&str]) -> Option<Vec<FeaturedRun>> {
    let mut packages: Vec<&str> = Vec::new();
    let mut features: Vec<&str> = Vec::new();
    let mut targets: Vec<Target> = Vec::new();
    let mut tokens = arguments.iter().copied();
    while let Some(token) = tokens.next() {
        match token {
            "--locked" | "--frozen" | "--offline" | "--release" => {}
            "-p" | "--package" => packages.push(tokens.next()?),
            "-F" | "--features" => features.extend(tokens.next()?.split(',')),
            "--lib" => targets.push(Target::Lib),
            "--test" => targets.push(Target::Integration(tokens.next()?.to_string())),
            _ if token.starts_with("--package=") => packages.push(&token["--package=".len()..]),
            _ if token.starts_with("--features=") => {
                features.extend(token["--features=".len()..].split(','));
            }
            _ if token.starts_with("--test=") => {
                targets.push(Target::Integration(token["--test=".len()..].to_string()));
            }
            "--" => {
                let harness: Vec<&str> = tokens.collect();
                harness_selects_nothing(&harness)?;
                break;
            }
            // Any other token is not read, so the invocation runs nothing.
            _ => return None,
        }
    }
    let features: BTreeSet<&str> = features.into_iter().filter(|f| !f.is_empty()).collect();
    // Several selectors run several targets, and each is read as its own.
    let reach: Vec<Option<Target>> = if targets.is_empty() {
        vec![None]
    } else {
        targets.into_iter().map(Some).collect()
    };
    let mut runs = Vec::new();
    for package in &packages {
        for feature in &features {
            for target in &reach {
                runs.push(FeaturedRun {
                    package: (*package).to_string(),
                    target: target.clone(),
                    feature: (*feature).to_string(),
                });
            }
        }
    }
    Some(runs)
}

/// Whether `harness` arguments leave every test the invocation selected
/// running: only output and thread-count settings, with nothing that filters,
/// skips, or turns to the ignored cases.
fn harness_selects_nothing(harness: &[&str]) -> Option<()> {
    let mut tokens = harness.iter().copied();
    while let Some(token) = tokens.next() {
        match token {
            "--nocapture" | "--show-output" | "--quiet" | "-q" => {}
            "--test-threads" => {
                is_count(tokens.next()?).then_some(())?;
            }
            _ => is_count(token.strip_prefix("--test-threads=")?).then_some(())?,
        }
    }
    Some(())
}

/// Whether `text` is a positive decimal count.
fn is_count(text: &str) -> bool {
    !text.is_empty() && text.chars().all(|c| c.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::{FeaturedRun, Invocation, LANE_FEATURES, Step, invocation, steps_in};
    use crate::regression::Target;
    use std::collections::BTreeMap;

    /// One featured run, as the reader reports it.
    fn run(package: &str, target: Option<Target>, feature: &str) -> FeaturedRun {
        FeaturedRun {
            package: package.to_string(),
            target,
            feature: feature.to_string(),
        }
    }

    fn lane(package: &str, target: &str) -> Option<Invocation> {
        Some(Invocation::Lane {
            package: package.to_string(),
            target: target.to_string(),
        })
    }

    fn tested(runs: Vec<FeaturedRun>) -> Option<Invocation> {
        Some(Invocation::Test(runs))
    }

    /// **A lane step reads as its package and target**, bare or behind the
    /// flake tripwire, with harness arguments that select nothing behind it.
    #[test]
    fn a_lane_invocation_reads_as_its_package_and_target() {
        assert_eq!(
            invocation(
                ".github/scripts/flake-tripwire.sh .github/scripts/lane-suite.sh norn-host \
                 memory --nocapture --test-threads=1"
            ),
            lane("norn-host", "memory")
        );
        assert_eq!(
            invocation(".github/scripts/lane-suite.sh norn-text frontmatter_cost\n"),
            lane("norn-text", "frontmatter_cost")
        );
    }

    /// **A lane step passing its harness anything that selects cases runs
    /// something other than the target's ignored cases**, and so reads as
    /// running nothing.
    #[test]
    fn a_lane_invocation_with_a_selecting_harness_argument_runs_nothing() {
        for command in [
            ".github/scripts/lane-suite.sh norn-host memory a_filter",
            ".github/scripts/lane-suite.sh norn-host memory --exact a_case",
            ".github/scripts/lane-suite.sh norn-host memory --skip a_case",
            ".github/scripts/lane-suite.sh norn-host memory --test-threads=many",
            ".github/scripts/lane-suite.sh norn-host",
            ".github/scripts/lane-suite.sh --package norn-host memory",
            "scripts/lane-suite.sh norn-host memory",
            "echo .github/scripts/lane-suite.sh norn-host memory",
        ] {
            assert_eq!(invocation(command), None, "`{command}` was read");
        }
    }

    /// **Every spelling cargo takes for a package, a feature and a target reads
    /// alike**, behind the tripwire or not, with harness arguments that select
    /// nothing behind it.
    #[test]
    fn the_spellings_cargo_takes_for_a_package_feature_and_target_read_alike() {
        for (command, expected) in [
            (
                ".github/scripts/flake-tripwire.sh cargo test --locked -p norn-host --features \
                 induced-failure",
                vec![run("norn-host", None, "induced-failure")],
            ),
            (
                "cargo test --package=norn-host --features=induced-failure",
                vec![run("norn-host", None, "induced-failure")],
            ),
            (
                "cargo test --package norn-host -F induced-failure --release",
                vec![run("norn-host", None, "induced-failure")],
            ),
            (
                "cargo test --locked -p norn-host --features other,induced-failure",
                vec![
                    run("norn-host", None, "induced-failure"),
                    run("norn-host", None, "other"),
                ],
            ),
            (
                "cargo test -p norn-host --features induced-failure --test kill_recovery -- \
                 --nocapture --test-threads=1",
                vec![run(
                    "norn-host",
                    Some(Target::Integration("kill_recovery".to_string())),
                    "induced-failure",
                )],
            ),
            (
                "cargo test -p norn-host --features induced-failure --lib -- --test-threads 1 \
                 --show-output",
                vec![run("norn-host", Some(Target::Lib), "induced-failure")],
            ),
            (
                "cargo test -p norn-host --features induced-failure --test=lockdown\n",
                vec![run(
                    "norn-host",
                    Some(Target::Integration("lockdown".to_string())),
                    "induced-failure",
                )],
            ),
            ("cargo test --locked -p norn-host", vec![]),
        ] {
            let read = invocation(command).map(|read| match read {
                Invocation::Test(mut runs) => {
                    runs.sort();
                    Invocation::Test(runs)
                }
                other => other,
            });
            assert_eq!(read, tested(expected), "`{command}` read otherwise");
        }
    }

    /// **A command that runs less than, or other than, the selection it names
    /// runs nothing.** A name filter, an ignored-only run, an exact match, a
    /// selector or flag this does not read, any shell metacharacter — standing
    /// alone or inside a feature list — a second line, and a command that is
    /// not `cargo test` itself.
    #[test]
    fn a_command_this_cannot_read_whole_runs_nothing() {
        let build = "cargo test --locked -p norn-host --features induced-failure";
        let mut commands: Vec<String> = [
            "some_other_case",
            "-- --ignored",
            "-- --include-ignored",
            "-- --exact a_case",
            "-- some_other_case",
            "--tests",
            "--no-run",
            "--all-features",
            "--workspace --exclude norn-fs",
        ]
        .iter()
        .map(|tail| format!("{build} {tail}"))
        .collect();
        for metacharacter in [
            "|", "&", ";", "<", ">", "#", "$", "\\", "'", "\"", "(", ")", "`",
        ] {
            commands.push(format!("{build} {metacharacter}"));
        }
        commands.extend([
            format!("{build}\nexit 1"),
            // A metacharacter inside a token the grammar would otherwise take:
            // the shell runs a cargo that fails on feature `x` and then
            // `true`, and backgrounds the second.
            format!("{build},x||true"),
            format!("{build},&"),
            format!("echo {build}"),
            format!("true {build}"),
            format!("+nightly {build}"),
            "cargo clippy --locked -p norn-host --all-targets --features induced-failure"
                .to_string(),
        ]);
        for command in commands {
            assert_eq!(invocation(&command), None, "`{command}` was read");
        }
    }

    /// The steps of a workflow as the reader reports them.
    fn steps(workflow: &str) -> Vec<Step> {
        steps_in(workflow).unwrap_or_else(|e| panic!("{e}\n{workflow}"))
    }

    /// **A step's environment is the workflow's, the job's and its own, the
    /// nearer winning**, whichever side of `run` it is written on, and another
    /// job's environment is not part of it.
    #[test]
    fn a_steps_environment_is_layered_from_the_workflow_down() {
        let workflow = [
            "env:",
            "  LANE_FEATURES: from-the-workflow",
            "  WORKFLOW_ONLY: kept",
            "jobs:",
            "  measure:",
            "    env:",
            "      LANE_FEATURES: from-the-job",
            "    steps:",
            "      - name: Host mixed load",
            "        env:",
            "          LANE_FEATURES: induced-failure # a comment, not a feature",
            "        run: .github/scripts/lane-suite.sh norn-host host_soak",
            "      - run: echo inherits",
            "  publish:",
            "    env:",
            "      LANE_FEATURES: not-this-step",
            "    steps:",
            "      - run: echo published",
        ]
        .join("\n");
        let read = steps(&workflow);
        let features: Vec<Option<&str>> = read
            .iter()
            .map(|step| step.env.get(LANE_FEATURES).map(String::as_str))
            .collect();
        assert_eq!(
            features,
            vec![
                Some("induced-failure"),
                Some("from-the-job"),
                Some("not-this-step"),
            ]
        );
        assert_eq!(
            read[1].env.get("WORKFLOW_ONLY").map(String::as_str),
            Some("kept")
        );
    }

    /// **A command is the parsed scalar.** A plain scalar continued onto the
    /// next line and a folded block are one line with the continuation on it;
    /// a literal block keeps its lines; a key other than `run` is no command.
    #[test]
    fn a_command_is_the_parsed_value_of_run() {
        let workflow = [
            "jobs:",
            "  tests:",
            "    steps:",
            "      - name: cargo test -p norn-host --features induced-failure",
            "        run: cargo test -p norn-host",
            "          a_filter",
            "      - run: >",
            "          cargo test -p norn-host",
            "          a_filter",
            "      - run: |",
            "          exit 0",
            "          cargo test -p norn-host",
            "      - uses: actions/checkout@v7",
        ]
        .join("\n");
        let runs: Vec<Option<String>> = steps(&workflow).into_iter().map(|s| s.run).collect();
        assert_eq!(
            runs,
            vec![
                Some("cargo test -p norn-host a_filter".to_string()),
                Some("cargo test -p norn-host a_filter\n".to_string()),
                Some("exit 0\ncargo test -p norn-host\n".to_string()),
                None,
            ]
        );
    }

    /// **`if:` and `continue-on-error:` make a step conditional whatever their
    /// value, on the step or on its job.**
    #[test]
    fn a_condition_on_a_step_or_its_job_makes_it_conditional() {
        let workflow = [
            "jobs:",
            "  plain:",
            "    steps:",
            "      - run: echo runs",
            "      - if: ${{ !cancelled() }}",
            "        run: echo maybe",
            "      - continue-on-error: false",
            "        run: echo tolerated",
            "  guarded:",
            "    if: github.ref == 'refs/heads/main'",
            "    steps:",
            "      - run: echo maybe",
            "  tolerant:",
            "    continue-on-error: true",
            "    steps:",
            "      - run: echo tolerated",
        ]
        .join("\n");
        let conditional: Vec<bool> = steps(&workflow).iter().map(|s| s.conditional).collect();
        assert_eq!(conditional, vec![false, true, true, true, true]);
    }

    /// **A workflow that is not one is an error**, never an empty answer.
    #[test]
    fn a_workflow_this_cannot_read_is_an_error() {
        for workflow in [
            "jobs: [unclosed",
            "jobs: a list\n",
            "jobs:\n  build: just a string\n",
            "jobs:\n  build:\n    steps: a string\n",
            "jobs:\n  build:\n    steps:\n      - just a string\n",
        ] {
            assert!(
                steps_in(workflow).is_err(),
                "`{workflow}` was read as a workflow"
            );
        }
        assert_eq!(
            steps("jobs:\n  call:\n    uses: ./.github/workflows/other.yml\n"),
            Vec::<Step>::new()
        );
        assert_eq!(
            steps("env:\n  A: 1\n  B: true\njobs:\n  j:\n    steps:\n      - run: x\n")[0].env,
            BTreeMap::from([
                ("A".to_string(), "1".to_string()),
                ("B".to_string(), "true".to_string()),
            ])
        );
    }
}
