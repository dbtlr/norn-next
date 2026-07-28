//! The coverage corpus and its activation gate.
//!
//! The corpus is a set of recorded command invocations — argv, the fixture
//! they ran against, and the bytes and exit class that came back. It is
//! **evidence with zero authority**: a recording says what a program did
//! once, never what this program should do. Nothing in the corpus runs until
//! a command is activated, and activating a command is an approval act that
//! judges whether the recorded output is *good* — see
//! [`Activation`] for the procedure.
//!
//! This module is the loader and the gate. The suite it gates lives in the
//! `norn` bin package, which is the one place cargo makes the built binary
//! reachable from an integration test.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::{Path, PathBuf};

use serde::Deserialize;

/// Where a recorded fact came from. Every corpus file carries one.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Source {
    pub line: String,
    pub branch: String,
    pub commit: String,
}

/// The activation manifest: the data file that decides what runs.
///
/// # The activation procedure
///
/// Activation is per command and it is an approval act. A command's cases
/// run only after its name is added to `activated`, and adding it means a
/// person answered four questions in order:
///
/// 0. **The goodness preamble.** Is the recorded output independently good —
///    judged against the surface as it is being re-derived, not against the
///    recording? A recording is evidence that a program once emitted these
///    bytes. It carries no claim that the bytes were right. If the answer is
///    no, the case is retired by a recorded ruling instead of activated, and
///    the command's contract is authored fresh.
/// 1. **The shape gate.** Does the command's output shape change? If not,
///    the cases enter as they stand.
/// 2. **Mechanical migration.** If the shape changes but the change is
///    mechanical, the recorded bytes are rewritten to the new shape and the
///    command is activated.
/// 3. **Semantic divergence.** If the change is semantic, the ruling that
///    decides it is recorded first, and activation follows the ruling.
///
/// An inconsistency that spans commands is decided **once, as a class
/// ruling**, and swept across every command it touches — one grammar rather
/// than per-command drift. A class ruling is recorded where it binds and the
/// commands it sweeps are activated together.
///
/// # The unseeded commands
///
/// `unseeded` names the commands that carry no behavior at the recording
/// pin. They have no activation path at all: their disposition is decided by
/// the verb charter, which asks whether the command should exist, and the
/// audit refuses any corpus that offers one of them as activatable.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Activation {
    pub source: Source,
    /// The framing, carried with the data.
    pub note: String,
    /// Commands whose cases run. Adding a name here is the approval act.
    pub activated: Vec<String>,
    /// Commands that have cases and could be activated.
    pub activatable: Vec<String>,
    /// Commands that carry no behavior at the pin and can never be
    /// activated — judged at the verb charter instead.
    pub unseeded: Vec<String>,
    /// Every recorded case, activatable and unseeded alike. Pinned so a case
    /// cannot go missing quietly.
    pub case_total: usize,
}

/// The exit contract the recordings were classified under: success,
/// operational failure, usage error. The mutation commands specialize the
/// failure split — a refusal left the vault untouched, a partial apply did
/// not — keyed on whether any write landed.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub enum ExitClass {
    /// `0` — the request succeeded.
    Ok,
    /// `1` — a well-formed invocation that could not be carried out.
    Operational,
    /// `2` — an argv the parser could not accept.
    Usage,
    /// `2` on a mutation command — refused before any write landed.
    Refusal,
    /// `1` on a mutation command — some writes landed and some did not.
    PartialApply,
}

/// The vault a case ran against, named by the generator profile and seed
/// that produce it.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Fixture {
    pub profile: String,
    pub seed: u64,
}

/// What came back.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Recorded {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
    pub exit_class: ExitClass,
}

/// One recorded invocation.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Case {
    pub id: String,
    /// The grouping the recording was organized under. A label, not a
    /// contract.
    pub suite: String,
    /// The activation unit this case belongs to.
    pub command: String,
    pub source: Source,
    pub argv: Vec<String>,
    pub fixture: Fixture,
    /// Whether the invocation writes to the vault.
    pub mutating: bool,
    /// For a case driven over stdin, the request frames in the order they
    /// were written.
    #[serde(default)]
    pub stdin_frames: Vec<String>,
    /// For a case that takes a plan file, the plan text, with
    /// `plan_vault_root_token` standing in for the vault root.
    #[serde(default)]
    pub plan_template: Option<String>,
    #[serde(default)]
    pub plan_argv_placeholder: Option<String>,
    #[serde(default)]
    pub plan_vault_root_token: Option<String>,
    /// A document the argv depends on being present in the fixture.
    #[serde(default)]
    pub requires_doc: Option<String>,
    /// A validation finding code the argv depends on being present.
    #[serde(default)]
    pub requires_code: Option<String>,
    /// Substitutions applied to the output before it was recorded.
    pub normalizations: Vec<String>,
    /// Placeholders standing in for values that vary run to run.
    pub volatile_masks: Vec<String>,
    pub recorded: Recorded,
    /// For a mutating case, what the invocation did to the vault tree.
    #[serde(default)]
    pub recorded_tree_delta: Option<serde_json::Value>,
}

/// Every case recorded for one command.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandCases {
    pub source: Source,
    /// The framing, carried with the data.
    pub note: String,
    pub command: String,
    pub case_count: usize,
    pub cases: Vec<Case>,
}

/// One behavior ruling, attached to the commands whose cases it covers.
///
/// A ruling enters as contract only when the command it attaches to is
/// activated and the ruling is re-judged affirmatively against the surface
/// being re-derived. A command the verb charter deletes takes its rulings
/// with it.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Ruling {
    pub id: String,
    pub surface: String,
    /// The activation units this ruling attaches to.
    pub commands: Vec<String>,
    /// The recorded cases the ruling covers.
    pub cases: Vec<String>,
    pub recorded_behavior: String,
    pub prior_behavior: String,
    pub reason: String,
    pub source_decision: String,
    pub source: Source,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Ledger {
    pub source: Source,
    /// The framing, carried with the data.
    pub note: String,
    pub source_file: String,
    pub ruling_count: usize,
    pub rulings: Vec<Ruling>,
}

/// One command path's recorded help prose. Rendered nowhere: it is raw
/// material for the prose a command earns when its contract is authored.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HelpEntry {
    pub command: String,
    pub path: Vec<String>,
    pub source: Source,
    pub long: String,
    pub short: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HelpCatalog {
    pub source: Source,
    /// The framing, carried with the data.
    pub note: String,
    pub entry_count: usize,
    pub entries: Vec<HelpEntry>,
}

#[derive(Debug)]
pub enum CorpusError {
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    Parse {
        path: PathBuf,
        source: serde_json::Error,
    },
}

impl fmt::Display for CorpusError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CorpusError::Read { path, source } => {
                write!(f, "could not read {}: {source}", path.display())
            }
            CorpusError::Parse { path, source } => {
                write!(f, "could not parse {}: {source}", path.display())
            }
        }
    }
}

impl std::error::Error for CorpusError {}

/// The loaded corpus: the manifest, the cases, the rulings, and the prose.
pub struct Corpus {
    pub activation: Activation,
    pub ledger: Ledger,
    pub help: HelpCatalog,
    /// Cases for commands that can be activated, keyed by command.
    activatable: BTreeMap<String, CommandCases>,
    /// Cases for commands that carry no behavior at the pin. Kept as
    /// evidence; never reachable through the gate.
    unseeded: BTreeMap<String, CommandCases>,
}

impl Corpus {
    /// Load every corpus file under `dir`.
    pub fn load(dir: &Path) -> Result<Self, CorpusError> {
        Ok(Corpus {
            activation: read_json(&dir.join("activation.json"))?,
            ledger: read_json(&dir.join("behavior-ledger.json"))?,
            help: read_json(&dir.join("help-prose.json"))?,
            activatable: read_command_dir(&dir.join("cases"))?,
            unseeded: read_command_dir(&dir.join("unseeded"))?,
        })
    }

    /// **The gate.** The cases that run: those belonging to a command named
    /// in the manifest's `activated` list, and no others. An empty
    /// `activated` list means nothing runs.
    pub fn activated_cases(&self) -> Vec<&Case> {
        let activated: BTreeSet<&str> = self
            .activation
            .activated
            .iter()
            .map(String::as_str)
            .collect();
        self.activatable
            .values()
            .filter(|group| activated.contains(group.command.as_str()))
            .flat_map(|group| group.cases.iter())
            .collect()
    }

    /// The cases that do not run: recorded, awaiting the approval act.
    pub fn dormant_cases(&self) -> Vec<&Case> {
        let activated: BTreeSet<&str> = self
            .activation
            .activated
            .iter()
            .map(String::as_str)
            .collect();
        self.activatable
            .values()
            .filter(|group| !activated.contains(group.command.as_str()))
            .flat_map(|group| group.cases.iter())
            .collect()
    }

    /// Every recorded case, gate or no gate — for counting, never for
    /// running.
    pub fn all_cases(&self) -> impl Iterator<Item = &Case> {
        self.activatable
            .values()
            .chain(self.unseeded.values())
            .flat_map(|group| group.cases.iter())
    }

    /// The commands that have recorded cases and could be activated.
    pub fn activatable_commands(&self) -> impl Iterator<Item = &str> {
        self.activatable.keys().map(String::as_str)
    }

    /// **The ledger attachment point.** The rulings that come up for
    /// re-judgment when `command` is activated.
    pub fn rulings_for(&self, command: &str) -> Vec<&Ruling> {
        self.ledger
            .rulings
            .iter()
            .filter(|ruling| ruling.commands.iter().any(|c| c == command))
            .collect()
    }

    /// The recorded help prose for `command` and every path beneath it.
    pub fn help_for(&self, command: &str) -> Vec<&HelpEntry> {
        self.help
            .entries
            .iter()
            .filter(|entry| entry.command == command)
            .collect()
    }

    /// Structural violations, one line each. A sound corpus produces none.
    ///
    /// This is not a judgment about any recorded output — the gate makes
    /// those, one command at a time, with a person behind it. It checks only
    /// that the data set holds together: that the gate cannot be bypassed,
    /// that an unseeded command cannot be activated, that no case went
    /// missing, and that every ruling and every activation entry names
    /// something real.
    pub fn audit(&self) -> Vec<String> {
        let mut problems = Vec::new();

        let unseeded: BTreeSet<&str> = self
            .activation
            .unseeded
            .iter()
            .map(String::as_str)
            .collect();
        let activatable: BTreeSet<&str> = self.activatable.keys().map(String::as_str).collect();

        for command in &self.activation.activatable {
            if !activatable.contains(command.as_str()) {
                problems.push(format!(
                    "manifest offers `{command}` as activatable, but no case file records it"
                ));
            }
            if unseeded.contains(command.as_str()) {
                problems.push(format!(
                    "manifest offers unseeded command `{command}` as activatable"
                ));
            }
        }
        for command in activatable.iter() {
            if unseeded.contains(command) {
                problems.push(format!(
                    "unseeded command `{command}` has an activatable case file"
                ));
            }
            if !self
                .activation
                .activatable
                .contains(&(*command).to_string())
            {
                problems.push(format!(
                    "case file records `{command}`, which the manifest does not list as activatable"
                ));
            }
        }
        for command in &self.activation.activated {
            if !activatable.contains(command.as_str()) {
                problems.push(format!(
                    "manifest activates `{command}`, which has no activatable case file"
                ));
            }
        }
        for command in self.unseeded.keys() {
            if !unseeded.contains(command.as_str()) {
                problems.push(format!(
                    "`{command}` is held as unseeded evidence but the manifest does not name it unseeded"
                ));
            }
        }

        let mut total = 0;
        let mut known_ids: BTreeSet<&str> = BTreeSet::new();
        for group in self.activatable.values().chain(self.unseeded.values()) {
            if group.case_count != group.cases.len() {
                problems.push(format!(
                    "`{}` declares {} cases and carries {}",
                    group.command,
                    group.case_count,
                    group.cases.len()
                ));
            }
            total += group.cases.len();
            for case in &group.cases {
                if case.command != group.command {
                    problems.push(format!(
                        "case `{}` is filed under `{}` but names command `{}`",
                        case.id, group.command, case.command
                    ));
                }
                if !known_ids.insert(case.id.as_str()) {
                    problems.push(format!("duplicate case id `{}`", case.id));
                }
            }
        }
        if total != self.activation.case_total {
            problems.push(format!(
                "manifest pins {} recorded cases; {total} are present",
                self.activation.case_total
            ));
        }

        if self.ledger.ruling_count != self.ledger.rulings.len() {
            problems.push(format!(
                "ledger declares {} rulings and carries {}",
                self.ledger.ruling_count,
                self.ledger.rulings.len()
            ));
        }
        for ruling in &self.ledger.rulings {
            for case_id in &ruling.cases {
                if !known_ids.contains(case_id.as_str()) {
                    problems.push(format!(
                        "ruling `{}` cites case `{case_id}`, which no case file records",
                        ruling.id
                    ));
                }
            }
        }

        if self.help.entry_count != self.help.entries.len() {
            problems.push(format!(
                "help catalog declares {} entries and carries {}",
                self.help.entry_count,
                self.help.entries.len()
            ));
        }

        problems
    }
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T, CorpusError> {
    let text = std::fs::read_to_string(path).map_err(|source| CorpusError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    serde_json::from_str(&text).map_err(|source| CorpusError::Parse {
        path: path.to_path_buf(),
        source,
    })
}

fn read_command_dir(dir: &Path) -> Result<BTreeMap<String, CommandCases>, CorpusError> {
    let mut out = BTreeMap::new();
    let entries = std::fs::read_dir(dir).map_err(|source| CorpusError::Read {
        path: dir.to_path_buf(),
        source,
    })?;
    let mut paths: Vec<PathBuf> = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|source| CorpusError::Read {
            path: dir.to_path_buf(),
            source,
        })?;
        let path = entry.path();
        if path.extension().is_some_and(|ext| ext == "json") {
            paths.push(path);
        }
    }
    paths.sort();
    for path in paths {
        let group: CommandCases = read_json(&path)?;
        out.insert(group.command.clone(), group);
    }
    Ok(out)
}
