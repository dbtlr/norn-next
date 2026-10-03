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
//! run.** A step or job carrying `continue-on-error:`, whatever the value, may
//! fail without failing the run, and one carrying an `if:` may be skipped —
//! unless the condition is `always()` or `!cancelled()`, which only widen when
//! a step runs. Any other condition vouches for nothing, which is the strict
//! side: a condition that would have held is refused with one that would not.
//! Nor does any step of a workflow holding a YAML merge key, `<<`, anywhere:
//! the parser does not apply it, so what it merges in would go unseen. Nor
//! does a step whose environment — its own, its job's and its workflow's —
//! sets any key but the project's own, `LANE_FEATURES` and those beginning
//! `NORN_`: a runner, wrapper, compiler, search path or preloaded library
//! named by any other may decide what the command builds and runs. Nor does a
//! step whose `env`, or its job's or workflow's, cannot be read whole.
//!
//! **A command is read whole or not at all.** It is one line, holding no shell
//! metacharacter anywhere — no pipe, list operator, redirection, comment,
//! expansion, brace, wildcard, tilde, escape, quote or subshell — and it runs
//! under `bash` or `sh`, named or left as the runner's default, so it is one
//! process with the arguments it spells: each of those shells splits such a
//! line on whitespace and does nothing else to it. Any other `shell:`, on the
//! step or as a job's or the workflow's `defaults.run.shell`, is a template
//! the runner hands the command to, and the template decides what runs —
//! `true {0}` runs nothing. The arguments are then held to a grammar:
//! optionally the flake tripwire in front, then either the lane script with a
//! package, a target and harness arguments that select nothing, or
//! `cargo test` with the flags that name a package, a feature and one target
//! and the few that change nothing about which tests run. A command outside that
//! grammar runs nothing, so a step this cannot read fails whatever needed it
//! rather than vouching for a test it may not run.
//!
//! **When a workflow runs is not judged here.** Its triggers and their path
//! filters, and a step's `working-directory`, are read by no rule above, on
//! purpose: they decide whether and where CI runs a workflow, the same for a
//! lane step as for a `cargo test` step, not what a step that runs vouches
//! for.
//!
//! **Nor is what earlier steps leave behind.** A step is judged by its own
//! text and the mappings above it, not by the state earlier steps in its job
//! leave for it — an entry written to `$GITHUB_ENV` or `$GITHUB_PATH`, a file
//! such as `.cargo/config.toml` or `rust-toolchain.toml` — which a static
//! reading cannot see; review of the workflow's diff is the backstop there.

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
/// the arguments it spells: besides the operators, quotes and substitutions,
/// the braces, wildcards and tilde `bash` and `sh` expand a word holding them
/// into other words.
const SHELL_METACHARACTERS: &[char] = &[
    '|', '&', ';', '<', '>', '#', '$', '\\', '\'', '"', '(', ')', '`', '\n', '{', '}', '*', '?',
    '[', ']', '~',
];

/// One step of one job, as the workflow declares it.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct Step {
    /// The parsed value of the step's `run`, or `None` where it runs no
    /// command of its own.
    pub(crate) run: Option<String>,
    /// The workflow's, the job's and the step's `env`, the nearer winning, or
    /// empty where one of them cannot be read — and then the step does not
    /// vouch.
    pub(crate) env: BTreeMap<String, String>,
    /// Whether the step vouches for what its command runs: nothing around the
    /// command may skip it, tolerate its failure, or change what it runs.
    pub(crate) vouches: bool,
}

/// One step that runs a target's ignored cases wholesale through
/// [`LANE_SCRIPT`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct LaneStep {
    pub(crate) package: String,
    pub(crate) target: String,
    /// The features the step names through [`LANE_FEATURES`].
    pub(crate) features: BTreeSet<String>,
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
    /// The vouching lane steps: a lane step that may be skipped, or whose
    /// failure is tolerated, adopts nothing.
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

    /// The features the lane steps adopting `package`'s `target` name between
    /// them, or `None` where no lane step adopts it.
    pub(crate) fn adopted(&self, package: &str, target: &str) -> Option<BTreeSet<String>> {
        let mut adopting = self
            .lanes
            .iter()
            .filter(|lane| lane.package == package && lane.target == target)
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
                // A step that may be skipped, may fail unnoticed, or may run
                // other than its command spells vouches for nothing it runs.
                _ if !step.vouches => {}
                Some(Invocation::Lane { package, target }) => read.lanes.push(LaneStep {
                    package,
                    target,
                    features: features_named(step.env.get(LANE_FEATURES)),
                }),
                Some(Invocation::Test(runs)) => read.featured.extend(runs),
                None => {}
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
    let merged = carries_a_merge_key(&document);
    let workflow_env = layered(Some(&BTreeMap::new()), document.get("env"));
    let workflow_shell = Shell::defaulted_by(&document);
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
        let job_shell = workflow_shell.under(Shell::defaulted_by(job));
        let job_env = layered(workflow_env.as_ref(), job.get("env"));
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
            let env = layered(job_env.as_ref(), step.get("env"));
            let env_reads = env
                .as_ref()
                .is_some_and(|env| env.keys().all(|key| is_the_projects_own(key)));
            steps.push(Step {
                run: step.get("run").and_then(Value::as_str).map(str::to_string),
                env: env.unwrap_or_default(),
                vouches: !(job_conditional || is_conditional(step))
                    && job_shell.under(Shell::named_by(step)).runs_one_process()
                    && env_reads
                    && !merged,
            });
        }
    }
    Ok(steps)
}

/// Whether a YAML merge key, `<<`, sits in any mapping under `value`.
///
/// The parser does not apply a merge: it leaves `<<` a key like any other, so
/// a mapping holding one is not the mapping the runner reads, and what it
/// merges in goes unseen. Where it sits does not matter to the answer — a
/// workflow holding one anywhere vouches for nothing — because the strict side
/// costs nothing while no workflow here merges.
fn carries_a_merge_key(value: &Value) -> bool {
    match value {
        Value::Mapping(mapping) => mapping
            .iter()
            .any(|(key, value)| key.as_str() == Some("<<") || carries_a_merge_key(value)),
        Value::Sequence(sequence) => sequence.iter().any(carries_a_merge_key),
        Value::Tagged(tagged) => carries_a_merge_key(&tagged.value),
        _ => false,
    }
}

/// The shell a step's command runs under, as one level of the workflow names
/// it.
#[derive(Clone, Copy, Debug)]
enum Shell<'a> {
    /// No shell is named here, so an outer level's stands, or else the
    /// runner's default — `bash`, or `sh` where there is no `bash`, on the
    /// Linux and macOS runners these workflows run on.
    Unnamed,
    /// The value of a `shell:` key, read as it is written.
    Named(&'a Value),
    /// A `defaults`, or its `run`, that is not the mapping a workflow holds
    /// there, so whatever shell it names cannot be read.
    Unread,
}

impl<'a> Shell<'a> {
    /// The shell a step's own `shell:` names.
    fn named_by(step: &'a Value) -> Self {
        step.get("shell").map_or(Shell::Unnamed, Shell::Named)
    }

    /// The shell a job's or workflow's `defaults.run.shell` names.
    fn defaulted_by(node: &'a Value) -> Self {
        let Some(defaults) = node.get("defaults") else {
            return Shell::Unnamed;
        };
        if !defaults.is_mapping() {
            return Shell::Unread;
        }
        match defaults.get("run") {
            None => Shell::Unnamed,
            Some(run) if run.is_mapping() => Shell::named_by(run),
            Some(_) => Shell::Unread,
        }
    }

    /// The shell in force where `nearer` is the next level in: the nearer
    /// name wins, and a level that cannot be read leaves nothing read.
    fn under(self, nearer: Self) -> Self {
        match (self, nearer) {
            (Shell::Unread, _) | (_, Shell::Unread) => Shell::Unread,
            (outer, Shell::Unnamed) => outer,
            (_, named) => named,
        }
    }

    /// Whether this shell runs a command holding no shell metacharacter as
    /// one process with the arguments it spells. `bash` and `sh` do — the
    /// runner runs the script file with them, and each splits such a line on
    /// whitespace alone. Any other `shell:` is a command template the runner
    /// hands the script file to, so it decides what runs: `true {0}` runs
    /// nothing, and `pwsh` or `python` read the line by other rules.
    fn runs_one_process(self) -> bool {
        match self {
            Shell::Unnamed => true,
            Shell::Named(Value::String(shell)) => shell == "bash" || shell == "sh",
            Shell::Named(_) | Shell::Unread => false,
        }
    }
}

/// Whether a job or step carries a key that lets it be skipped or fail
/// without failing the run: any `continue-on-error:`, and any `if:` other
/// than one of [`WIDENING_CONDITIONS`].
fn is_conditional(node: &Value) -> bool {
    let narrows = node.get("if").is_some_and(|condition| {
        !condition
            .as_str()
            .is_some_and(|condition| WIDENING_CONDITIONS.contains(&unwrapped(condition)))
    });
    narrows || node.get("continue-on-error").is_some()
}

/// The `if:` conditions that only widen when a step runs: each runs it in
/// every case the default `success()` does, and in more. Any other condition
/// may skip a step the run needed, so it fails closed.
const WIDENING_CONDITIONS: &[&str] = &["always()", "!cancelled()"];

/// A condition with its optional `${{ }}` wrapper and surrounding whitespace
/// taken off.
fn unwrapped(condition: &str) -> &str {
    let condition = condition.trim();
    condition
        .strip_prefix("${{")
        .and_then(|inner| inner.strip_suffix("}}"))
        .map_or(condition, str::trim)
}

/// `outer` with the entries of the `env` mapping `inner` over it, each as the
/// string the runner exports, or `None` where either cannot be read whole.
///
/// An `env` that is anything but a mapping of names to scalars — an
/// expression standing for the whole map, a nested value, a key that is not a
/// name — may export anything, so it is read as nothing rather than as
/// exporting nothing.
fn layered(
    outer: Option<&BTreeMap<String, String>>,
    inner: Option<&Value>,
) -> Option<BTreeMap<String, String>> {
    let mut env = outer?.clone();
    let Some(inner) = inner else {
        return Some(env);
    };
    for (key, value) in inner.as_mapping()? {
        let value = match value {
            Value::String(text) => text.clone(),
            Value::Number(number) => number.to_string(),
            Value::Bool(flag) => flag.to_string(),
            _ => return None,
        };
        env.insert(key.as_str()?.to_string(), value);
    }
    Some(env)
}

/// Whether an environment key is one of the project's own: [`LANE_FEATURES`],
/// or one beginning `NORN_`.
///
/// Only norn's harness, scripts and tests read these, so none can replace
/// cargo, rustc or the binaries they build. Every other key may: cargo and
/// rustc take a runner, a wrapper, a compiler and flags from their own keys,
/// the shell finds `cargo` through `PATH`, and the loader preloads whatever
/// `LD_PRELOAD` names — so the rule is the short list that cannot, not the
/// open one that can.
fn is_the_projects_own(key: &str) -> bool {
    key == LANE_FEATURES || key.starts_with("NORN_")
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

/// Whether `text` is a positive decimal count: digits alone, not all of them
/// zero.
fn is_count(text: &str) -> bool {
    text.chars().all(|c| c.is_ascii_digit()) && text.chars().any(|c| c != '0')
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
            ".github/scripts/lane-suite.sh norn-host memory --test-threads=0",
            ".github/scripts/lane-suite.sh norn-host memory --test-threads 00",
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
            "|", "&", ";", "<", ">", "#", "$", "\\", "'", "\"", "(", ")", "`", "{", "}", "*", "?",
            "[", "]", "~",
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
            // Brace expansion: the shell runs `--features
            // induced-failure,induced-failure` with `induced-failure,zzz` as a
            // name filter that matches no test.
            format!("{build},{{induced-failure,zzz}}"),
            // Pathname expansion: `-p *` is every file in the working
            // directory, and all but the first are name filters.
            "cargo test --locked -p * --features induced-failure".to_string(),
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

    /// Whether a step guarded by `if: <condition>` reads as conditional, with
    /// the condition on the step and then on its job.
    fn conditional_under(condition: &str) -> (bool, bool) {
        let on_step = format!("jobs:\n  j:\n    steps:\n      - if: {condition}\n        run: x\n");
        let on_job = format!("jobs:\n  j:\n    if: {condition}\n    steps:\n      - run: x\n");
        (!steps(&on_step)[0].vouches, !steps(&on_job)[0].vouches)
    }

    /// **A condition that only widens when a step runs leaves it vouching**,
    /// on the step or its job, bare or inside `${{ }}`.
    #[test]
    fn a_condition_that_only_widens_when_a_step_runs_leaves_it_unconditional() {
        for condition in [
            "always()",
            "${{ always() }}",
            // YAML reads a bare leading `!` as a tag, so the bare spelling is
            // quoted, as a workflow has to quote it.
            "'!cancelled()'",
            "${{ !cancelled() }}",
            "${{!cancelled()}}",
            "\"  ${{   always()   }}  \"",
        ] {
            assert_eq!(
                conditional_under(condition),
                (false, false),
                "`if: {condition}` was read as narrowing when the step runs"
            );
        }
    }

    /// **Every other condition fails closed**, on the step or its job, and
    /// `continue-on-error:` does whatever its value.
    #[test]
    fn any_other_condition_makes_a_step_conditional() {
        for condition in [
            "success()",
            "${{ success() }}",
            "false",
            "github.event_name == 'push'",
            "${{ !cancelled() && false }}",
            "'!cancelled() && false'",
            "true",
        ] {
            assert_eq!(
                conditional_under(condition),
                (true, true),
                "`if: {condition}` was read as never narrowing when the step runs"
            );
        }
        let tolerant = [
            "jobs:",
            "  plain:",
            "    steps:",
            "      - continue-on-error: false",
            "        run: echo tolerated",
            "  tolerant:",
            "    continue-on-error: true",
            "    steps:",
            "      - run: echo tolerated",
        ]
        .join("\n");
        let conditional: Vec<bool> = steps(&tolerant).iter().map(|s| !s.vouches).collect();
        assert_eq!(conditional, vec![true, true]);
    }

    /// Whether each step of `workflow` vouches for what it runs.
    fn vouching(workflow: &str) -> Vec<bool> {
        steps(workflow).iter().map(|step| step.vouches).collect()
    }

    /// One step running `x`, with `step`, `job` and `workflow` lines spliced
    /// in at each level — each written at that level's indentation already.
    fn one_step(workflow: &[&str], job: &[&str], step: &[&str]) -> String {
        let mut lines: Vec<&str> = workflow.to_vec();
        lines.extend(["jobs:", "  j:"]);
        lines.extend(job);
        lines.extend(["    steps:", "      - run: x"]);
        lines.extend(step);
        lines.join("\n") + "\n"
    }

    /// **A step whose shell is not `bash` or `sh` vouches for nothing**, the
    /// step's `shell:` first, then its job's and its workflow's
    /// `defaults.run.shell`: any other shell runs the command as something
    /// other than one process with the arguments it spells.
    #[test]
    fn a_shell_other_than_bash_or_sh_vouches_for_nothing() {
        for shell in [
            "'true {0}'",
            "'echo {0}'",
            "bash -c true {0}",
            "pwsh",
            "python",
            "${{ matrix.shell }}",
            "''",
            "[bash]",
            "",
        ] {
            let step_line = format!("        shell: {shell}");
            let job_lines = [
                "    defaults:",
                "      run:",
                &format!("        shell: {shell}"),
            ];
            let workflow_lines = ["defaults:", "  run:", &format!("    shell: {shell}")];
            for (placement, workflow) in [
                ("step", one_step(&[], &[], &[&step_line])),
                ("job", one_step(&[], &job_lines, &[])),
                ("workflow", one_step(&workflow_lines, &[], &[])),
            ] {
                assert_eq!(
                    vouching(&workflow),
                    vec![false],
                    "a step under a {placement} `shell: {shell}` vouched\n{workflow}"
                );
            }
        }
        for unreadable in [
            one_step(&["defaults: a string"], &[], &[]),
            one_step(&["defaults:", "  run: a string"], &[], &[]),
            one_step(&[], &["    defaults: a string"], &[]),
            one_step(&[], &["    defaults:", "      run: [a, list]"], &[]),
        ] {
            assert_eq!(vouching(&unreadable), vec![false], "{unreadable}");
        }
        for read in [
            one_step(&[], &[], &[]),
            one_step(&[], &[], &["        shell: bash"]),
            one_step(&[], &[], &["        shell: sh"]),
            one_step(
                &[],
                &["    defaults:", "      run:", "        shell: sh"],
                &[],
            ),
            one_step(&["defaults:", "  run:", "    shell: bash"], &[], &[]),
            // The nearer shell wins, so a step's own `bash` is the shell it
            // runs under whatever its job's default names.
            one_step(
                &[],
                &["    defaults:", "      run:", "        shell: pwsh"],
                &["        shell: bash"],
            ),
            // A `defaults.run` naming only a working directory names no shell.
            one_step(
                &["defaults:", "  run:", "    working-directory: crates"],
                &[],
                &[],
            ),
        ] {
            assert_eq!(vouching(&read), vec![true], "{read}");
        }
    }

    /// **A step whose environment sets any key but the project's own vouches
    /// for nothing**, at the step, its job or its workflow: a runner, a
    /// wrapper, a compiler, a search path or a preloaded library named there
    /// can leave the test binaries unbuilt, unrun, or run by something else.
    /// An `env` this cannot read whole may set any of them, so it fails
    /// closed too.
    #[test]
    fn a_step_whose_environment_sets_a_key_not_the_projects_own_vouches_for_nothing() {
        let at_each_level = |key: &str| {
            [
                (
                    "step",
                    one_step(
                        &[],
                        &[],
                        &["        env:", &format!("          {key}: 'true'")],
                    ),
                ),
                (
                    "job",
                    one_step(&[], &["    env:", &format!("      {key}: 'true'")], &[]),
                ),
                (
                    "workflow",
                    one_step(&["env:", &format!("  {key}: 'true'")], &[], &[]),
                ),
            ]
        };
        for key in [
            "PATH",
            "LD_PRELOAD",
            "RUSTC_WRAPPER",
            "CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUNNER",
            "FOO",
            // The project's spellings in another case are other keys.
            "lane_features",
            "norn_x",
        ] {
            for (placement, workflow) in at_each_level(key) {
                assert_eq!(
                    vouching(&workflow),
                    vec![false],
                    "a step under a {placement} `{key}` vouched\n{workflow}"
                );
            }
        }
        for key in [LANE_FEATURES, "NORN_X"] {
            for (placement, workflow) in at_each_level(key) {
                assert_eq!(
                    vouching(&workflow),
                    vec![true],
                    "a step under a {placement} `{key}` was refused\n{workflow}"
                );
            }
        }
        for unreadable in [
            one_step(&["env: ${{ fromJSON(vars.ENV) }}"], &[], &[]),
            one_step(&[], &["    env: [NORN_X]"], &[]),
            one_step(&[], &[], &["        env:", "          NORN_X: [a, list]"]),
            one_step(&[], &[], &["        env:", "          1: one"]),
        ] {
            assert_eq!(vouching(&unreadable), vec![false], "{unreadable}");
        }
    }

    /// **A workflow carrying a YAML merge key anywhere vouches for nothing**:
    /// the parser leaves `<<` a key like any other, so the mapping this reads
    /// is not the one the runner reads, and what it merges in — a condition,
    /// a tolerance, a shell, an environment — goes unseen.
    #[test]
    fn a_merge_key_anywhere_in_a_workflow_vouches_for_nothing() {
        for merged in [
            one_step(
                &[],
                &[],
                &["        <<: {if: 'false', continue-on-error: true}"],
            ),
            one_step(&[], &["    <<: {continue-on-error: true}"], &[]),
            one_step(&["<<: {defaults: {run: {shell: 'true {0}'}}}"], &[], &[]),
            one_step(
                &[],
                &[],
                &["        env:", "          <<: {RUSTC_WRAPPER: 'true'}"],
            ),
            one_step(
                &["x-skip: &skip", "  if: 'false'"],
                &[],
                &["        <<: *skip"],
            ),
            [
                "jobs:",
                "  j:",
                "    steps:",
                "      - run: x",
                "  other:",
                "    steps:",
                "      - <<: {if: 'false'}",
                "        run: y",
            ]
            .join("\n"),
        ] {
            assert!(
                vouching(&merged).iter().all(|vouches| !vouches),
                "a step vouched in\n{merged}"
            );
        }
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
