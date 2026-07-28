//! The coverage corpus and its activation gate.
//!
//! The corpus is a set of recorded command invocations — argv, the fixture
//! tree they ran against, and the bytes and exit class that came back. It is
//! **evidence with zero authority**: a recording says what a program did
//! once, never what this program should do. Nothing in the corpus runs until
//! a command is activated, and activating a command is an approval act that
//! judges whether the recorded output is *good* — see [`Activation`] for the
//! procedure.
//!
//! This module is the loader and the gate. The suite it gates lives in the
//! `norn` bin package, which is the one place cargo makes the built binary
//! reachable from an integration test.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::{Path, PathBuf};

use serde::Deserialize;

/// The commands that carry no behavior at the recording pin.
///
/// Pinned here rather than taken from the manifest so the list is a fact of
/// the harness, not a field an edit can narrow. [`Corpus::audit`] requires
/// the manifest to name exactly these, which is what makes "an unseeded
/// command cannot be activated by editing the manifest" true: moving a case
/// file into the activatable set no longer suffices, because the manifest
/// would also have to drop the name from `unseeded`, and that fails the
/// audit.
pub const UNSEEDED_COMMANDS: &[&str] = &[
    "audit",
    "cache",
    "completions",
    "config",
    "init",
    "manpage",
    "self-update",
    "serve",
    "service",
];

/// Global options that consume the token after them as their value, so that
/// value is never mistaken for the command.
pub const VALUE_TAKING_GLOBALS: &[&str] = &["-C", "--cwd", "--color", "--vault"];

/// The activation unit a bare top-level invocation belongs to. Not a
/// command: the top-level page is its own surface, so it is named rather
/// than left to fall into whichever verb happened to appear first.
pub const TOP_LEVEL: &str = "top-level";

/// Commands the binary accepts at the pin but does not list in its own help
/// output, so the recorded help catalog has no entry for them.
///
/// `manpage` is the whole list: it is a hidden generator verb. It matters
/// because [`Corpus::audit`] reconciles the manifest against the recorded
/// top-level help page, and without this allowance an accurate manifest
/// would fail that reconciliation.
pub const COMMANDS_ABSENT_FROM_HELP: &[&str] = &["manpage"];

/// The activation unit for an argv: the first token that is neither a flag
/// nor a value-taking global's value. An argv with no such token (the bare
/// `--help` page) belongs to [`TOP_LEVEL`].
///
/// This is the rule the corpus was grouped by, and [`Corpus::audit`] holds
/// every case to it — a case filed under a command its own argv does not
/// select would activate under the wrong approval.
pub fn command_of(argv: &[String]) -> String {
    let mut i = 0;
    while i < argv.len() {
        let token = argv[i].as_str();
        if VALUE_TAKING_GLOBALS.contains(&token) {
            i += 2;
            continue;
        }
        if token.starts_with('-') {
            i += 1;
            continue;
        }
        return token.to_string();
    }
    TOP_LEVEL.to_string()
}

/// Where a recorded fact came from. Every corpus file carries one.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Source {
    pub line: String,
    pub branch: String,
    pub commit: String,
}

/// A command with behavior at the pin that no recorded case exercises.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Unrecorded {
    pub command: String,
    /// Why nothing was recorded. A category with no reason is a gap someone
    /// stopped writing down halfway.
    pub reason: String,
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
/// # The four categories
///
/// Every command the binary had at the pin falls in exactly one, and
/// [`Corpus::audit`] reconciles the set against the recorded top-level help
/// page so a command cannot go uncategorized:
///
/// - **`activatable`** — has recorded cases, and can be approved.
/// - **`activated`** — the subset of `activatable` whose cases run.
/// - **`unseeded`** — carries no behavior at the pin, so there is nothing to
///   approve and no activation path exists. Whether the command should exist
///   is a question for the verb charter. Pinned by [`UNSEEDED_COMMANDS`].
/// - **`unrecorded`** — has behavior at the pin, but no case exercises it.
///   Its contract is authored from scratch; the corpus offers it no
///   evidence, and saying so is the point of the category.
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
    /// Commands with behavior at the pin that no case exercises.
    pub unrecorded: Vec<Unrecorded>,
    /// Every recorded case, in every category. Pinned so a case cannot go
    /// missing quietly.
    pub case_total: usize,
}

/// How a recorded exit code was classified.
///
/// The recordings were made under a tri-state contract — `0` success, `1`
/// operational failure, `2` usage error — with one specialization: on a
/// command that writes, the failure split is keyed on whether any write
/// landed, so `2` is a refusal that left the vault untouched and `1` is a
/// partial apply.
///
/// **The mutation specialization takes precedence over [`ExitClass::Usage`].**
/// A command that writes and rejects a malformed payload exits `2` and is
/// classified [`ExitClass::Refusal`], not [`ExitClass::Usage`], because the
/// load-bearing fact for a caller is that nothing was written — not which
/// layer refused. Every recorded `2` on a mutating case is a refusal for
/// this reason.
///
/// This is the classification the recordings were made under. It is not this
/// program's exit contract, which is authored with the verb charter.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub enum ExitClass {
    /// `0` — the request succeeded.
    Ok,
    /// `1` — a well-formed invocation that could not be carried out.
    Operational,
    /// `2` — an argv the parser could not accept.
    Usage,
    /// `2` on a command that writes — refused before any write landed.
    Refusal,
    /// `1` on a command that writes — some writes landed and some did not.
    PartialApply,
}

impl ExitClass {
    /// The exit code this class is recorded against.
    pub fn code(self) -> i32 {
        match self {
            ExitClass::Ok => 0,
            ExitClass::Operational | ExitClass::PartialApply => 1,
            ExitClass::Usage | ExitClass::Refusal => 2,
        }
    }
}

/// The vault a case ran against, named by the generator profile and seed
/// that produce it. The tree itself is recorded in `fixtures.json`, because
/// the name alone does not determine it.
#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd)]
#[serde(deny_unknown_fields)]
pub struct FixtureRef {
    pub profile: String,
    pub seed: u64,
}

impl fmt::Display for FixtureRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.profile, self.seed)
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub enum TreeEntryKind {
    File,
    EmptyDir,
}

/// One node of a vault tree: a file with its bytes, or an empty directory.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TreeEntry {
    pub path: String,
    pub kind: TreeEntryKind,
    /// A text file's contents.
    #[serde(default)]
    pub content: Option<String>,
    /// A file whose bytes are not valid UTF-8 carries its length instead.
    #[serde(default)]
    pub non_utf8_len: Option<usize>,
}

/// What a mutating invocation did to the vault tree.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TreeDelta {
    pub added: Vec<TreeEntry>,
    pub removed: Vec<String>,
    pub modified: Vec<TreeEntry>,
}

/// The tree one `(profile, seed)` produced at the pin.
///
/// This is the input side of every case that names it. A future generator
/// reproduces the fixture when it emits exactly these paths with exactly
/// these bytes; until it does, a case naming this fixture cannot be
/// activated, because a difference in output would read as a defect in this
/// program when it is drift in the generator.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FixtureManifest {
    pub profile: String,
    pub seed: u64,
    pub entry_count: usize,
    pub entries: Vec<TreeEntry>,
}

impl FixtureManifest {
    pub fn fixture_ref(&self) -> FixtureRef {
        FixtureRef {
            profile: self.profile.clone(),
            seed: self.seed,
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Fixtures {
    pub source: Source,
    /// The framing, carried with the data.
    pub note: String,
    pub fixture_count: usize,
    pub fixtures: Vec<FixtureManifest>,
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
    /// The activation unit this case belongs to, per [`command_of`].
    pub command: String,
    pub source: Source,
    pub argv: Vec<String>,
    pub fixture: FixtureRef,
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
    /// Substitutions the recording harness applied before the extraction saw
    /// the bytes. A different thing from `volatile_masks`; `environment.json`
    /// describes both and why they are not interchangeable.
    pub normalizations: Vec<String>,
    /// Placeholders the extraction wrote in place of values that vary run to
    /// run. Empty when nothing volatile appeared — including when a field
    /// that *can* hold a volatile value was empty, which is itself a fact
    /// about the invocation and is recorded verbatim.
    pub volatile_masks: Vec<String>,
    pub recorded: Recorded,
    /// For a mutating case, what the invocation did to the vault tree.
    #[serde(default)]
    pub recorded_tree_delta: Option<TreeDelta>,
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
    /// A path inside the recording checkout, **not this repository**. The
    /// two namespaces overlap — both number their decision records from
    /// `0001` — so reading this as a local path resolves to the wrong
    /// document.
    pub source_decision: String,
    pub source: Source,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Ledger {
    pub source: Source,
    /// The framing, carried with the data.
    pub note: String,
    /// A path inside the recording checkout, **not this repository**.
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

impl HelpCatalog {
    /// The entry for the bare top-level page.
    pub fn top_level(&self) -> Option<&HelpEntry> {
        self.entries.iter().find(|entry| entry.path.is_empty())
    }

    /// The command names the top-level page lists, in render order.
    pub fn top_level_commands(&self) -> Vec<String> {
        self.top_level()
            .map(|entry| parse_command_rows(&entry.long))
            .unwrap_or_default()
    }
}

/// The subcommand names in a help page's `COMMANDS` section: the first token
/// of each row indented exactly four spaces, until the next unindented
/// section header.
fn parse_command_rows(help: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut in_commands = false;
    for line in help.lines() {
        if line.starts_with("COMMANDS") {
            in_commands = true;
            continue;
        }
        if !in_commands {
            continue;
        }
        if !line.starts_with(' ') && !line.trim().is_empty() {
            break;
        }
        let Some(rest) = line.strip_prefix("    ") else {
            continue;
        };
        if rest.starts_with(' ') || rest.is_empty() {
            continue;
        }
        let name = rest.split_whitespace().next().unwrap_or("");
        if !name.is_empty() && name.chars().all(|c| c.is_ascii_lowercase() || c == '-') {
            names.push(name.to_string());
        }
    }
    names
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
    /// Two files claim the same command. Keeping the last would silently
    /// drop the other's cases.
    DuplicateCommand {
        command: String,
        first: PathBuf,
        second: PathBuf,
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
            CorpusError::DuplicateCommand {
                command,
                first,
                second,
            } => write!(
                f,
                "two case files claim command `{command}`: {} and {}",
                first.display(),
                second.display()
            ),
        }
    }
}

impl std::error::Error for CorpusError {}

/// The loaded corpus: the manifest, the fixtures, the cases, the rulings,
/// and the prose.
pub struct Corpus {
    pub activation: Activation,
    pub fixtures: Fixtures,
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
            fixtures: read_json(&dir.join("fixtures.json"))?,
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
        let activated = self.activated_set();
        self.activatable
            .values()
            .filter(|group| activated.contains(group.command.as_str()))
            .flat_map(|group| group.cases.iter())
            .collect()
    }

    /// The cases that do not run: recorded, awaiting the approval act.
    pub fn dormant_cases(&self) -> Vec<&Case> {
        let activated = self.activated_set();
        self.activatable
            .values()
            .filter(|group| !activated.contains(group.command.as_str()))
            .flat_map(|group| group.cases.iter())
            .collect()
    }

    fn activated_set(&self) -> BTreeSet<&str> {
        self.activation
            .activated
            .iter()
            .map(String::as_str)
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

    /// Every command the manifest accounts for, in any category, plus the
    /// synthetic top-level unit — which is always a member, because the
    /// top-level page is a surface the binary always has.
    pub fn command_universe(&self) -> BTreeSet<&str> {
        std::iter::once(TOP_LEVEL)
            .chain(self.activation.activatable.iter().map(String::as_str))
            .chain(self.activation.unseeded.iter().map(String::as_str))
            .chain(
                self.activation
                    .unrecorded
                    .iter()
                    .map(|entry| entry.command.as_str()),
            )
            .collect()
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

    /// Rulings no activation will ever bring up, because every command they
    /// attach to is unseeded or unrecorded.
    ///
    /// Not a defect — a ruling about a command with no behavior is still
    /// evidence — but it is a ruling nobody will be prompted to judge, so it
    /// has to be found deliberately rather than waited for. Its disposition
    /// belongs to the verb charter, along with the command it describes.
    pub fn rulings_without_an_activation_path(&self) -> Vec<&Ruling> {
        let activatable: BTreeSet<&str> = self.activatable_commands().collect();
        self.ledger
            .rulings
            .iter()
            .filter(|ruling| {
                !ruling.commands.is_empty()
                    && !ruling
                        .commands
                        .iter()
                        .any(|c| activatable.contains(c.as_str()))
            })
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

    /// The recorded input tree for `fixture`.
    pub fn fixture(&self, fixture: &FixtureRef) -> Option<&FixtureManifest> {
        self.fixtures
            .fixtures
            .iter()
            .find(|manifest| &manifest.fixture_ref() == fixture)
    }

    /// Structural violations, one line each. A sound corpus produces none.
    ///
    /// This is not a judgment about any recorded output — the gate makes
    /// those, one command at a time, with a person behind it. It checks only
    /// that the data set holds together: that the gate cannot be bypassed,
    /// that an unseeded command cannot be activated, that no case or command
    /// went missing, that every case is filed under the command its own argv
    /// selects, and that every ruling, prose entry and fixture reference
    /// names something real.
    pub fn audit(&self) -> Vec<String> {
        let mut problems = Vec::new();
        self.audit_categories(&mut problems);
        self.audit_cases(&mut problems);
        self.audit_attachments(&mut problems);
        problems
    }

    fn audit_categories(&self, problems: &mut Vec<String>) {
        let pinned: BTreeSet<&str> = UNSEEDED_COMMANDS.iter().copied().collect();
        let declared: BTreeSet<&str> = self
            .activation
            .unseeded
            .iter()
            .map(String::as_str)
            .collect();
        for missing in pinned.difference(&declared) {
            problems.push(format!(
                "manifest omits `{missing}` from `unseeded`, which the harness pins as unseeded"
            ));
        }
        for extra in declared.difference(&pinned) {
            problems.push(format!(
                "manifest names `{extra}` unseeded, which the harness does not pin as unseeded"
            ));
        }

        let with_cases: BTreeSet<&str> = self.activatable.keys().map(String::as_str).collect();
        let unrecorded: BTreeSet<&str> = self
            .activation
            .unrecorded
            .iter()
            .map(|entry| entry.command.as_str())
            .collect();

        for command in &self.activation.activatable {
            if !with_cases.contains(command.as_str()) {
                problems.push(format!(
                    "manifest offers `{command}` as activatable, but no case file records it"
                ));
            }
            if declared.contains(command.as_str()) {
                problems.push(format!(
                    "manifest offers unseeded command `{command}` as activatable"
                ));
            }
            if unrecorded.contains(command.as_str()) {
                problems.push(format!(
                    "manifest lists `{command}` as both activatable and unrecorded"
                ));
            }
        }
        for command in &with_cases {
            if declared.contains(command) {
                problems.push(format!(
                    "unseeded command `{command}` has an activatable case file"
                ));
            }
            if !self.activation.activatable.iter().any(|c| c == command) {
                problems.push(format!(
                    "case file records `{command}`, which the manifest does not list as activatable"
                ));
            }
        }
        for command in &self.activation.activated {
            if !with_cases.contains(command.as_str()) {
                problems.push(format!(
                    "manifest activates `{command}`, which has no activatable case file"
                ));
            }
        }
        for command in self.unseeded.keys() {
            if !declared.contains(command.as_str()) {
                problems.push(format!(
                    "`{command}` is held as unseeded evidence but the manifest does not name it unseeded"
                ));
            }
        }
        for entry in &self.activation.unrecorded {
            if entry.reason.trim().is_empty() {
                problems.push(format!(
                    "`{}` is listed as unrecorded with no reason",
                    entry.command
                ));
            }
        }

        // Reconcile the categories against the binary's own command list, so
        // a command cannot sit outside every category unnoticed.
        let universe = self.command_universe();
        let listed = self.help.top_level_commands();
        if listed.is_empty() {
            problems.push(
                "the help catalog carries no top-level page, so the manifest cannot be reconciled \
                 against the command list"
                    .to_string(),
            );
        }
        for command in &listed {
            if !universe.contains(command.as_str()) {
                problems.push(format!(
                    "`{command}` is listed on the top-level help page but sits in no manifest \
                     category"
                ));
            }
        }
        let allowed_absent: BTreeSet<&str> = COMMANDS_ABSENT_FROM_HELP.iter().copied().collect();
        for command in &universe {
            if *command == TOP_LEVEL
                || allowed_absent.contains(command)
                || listed.iter().any(|c| c == command)
            {
                continue;
            }
            problems.push(format!(
                "the manifest accounts for `{command}`, which the top-level help page does not \
                 list and which is not a known hidden command"
            ));
        }
    }

    fn audit_cases(&self, problems: &mut Vec<String>) {
        let mut total = 0;
        let mut known_ids: BTreeSet<&str> = BTreeSet::new();
        let recorded_fixtures: BTreeSet<FixtureRef> = self
            .fixtures
            .fixtures
            .iter()
            .map(FixtureManifest::fixture_ref)
            .collect();

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
                let selected = command_of(&case.argv);
                if selected != case.command {
                    problems.push(format!(
                        "case `{}` names command `{}` but its argv selects `{selected}`",
                        case.id, case.command
                    ));
                }
                if !known_ids.insert(case.id.as_str()) {
                    problems.push(format!("duplicate case id `{}`", case.id));
                }
                if case.recorded.exit_class.code() != case.recorded.exit_code {
                    problems.push(format!(
                        "case `{}` recorded exit {} classified as `{:?}`, which is the class for \
                         exit {}",
                        case.id,
                        case.recorded.exit_code,
                        case.recorded.exit_class,
                        case.recorded.exit_class.code()
                    ));
                }
                if !recorded_fixtures.contains(&case.fixture) {
                    problems.push(format!(
                        "case `{}` runs against fixture `{}`, whose tree is not recorded",
                        case.id, case.fixture
                    ));
                }
                if case.mutating != case.recorded_tree_delta.is_some() {
                    problems.push(format!(
                        "case `{}` is marked mutating={} yet {} a recorded tree delta",
                        case.id,
                        case.mutating,
                        if case.recorded_tree_delta.is_some() {
                            "carries"
                        } else {
                            "lacks"
                        }
                    ));
                }
            }
        }
        if total != self.activation.case_total {
            problems.push(format!(
                "manifest pins {} recorded cases; {total} are present",
                self.activation.case_total
            ));
        }

        if self.fixtures.fixture_count != self.fixtures.fixtures.len() {
            problems.push(format!(
                "fixtures file declares {} manifests and carries {}",
                self.fixtures.fixture_count,
                self.fixtures.fixtures.len()
            ));
        }
        for manifest in &self.fixtures.fixtures {
            if manifest.entry_count != manifest.entries.len() {
                problems.push(format!(
                    "fixture `{}` declares {} entries and carries {}",
                    manifest.fixture_ref(),
                    manifest.entry_count,
                    manifest.entries.len()
                ));
            }
        }
    }

    fn audit_attachments(&self, problems: &mut Vec<String>) {
        let known_ids: BTreeSet<&str> = self.all_cases().map(|case| case.id.as_str()).collect();
        let universe = self.command_universe();

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
            for command in &ruling.commands {
                if !universe.contains(command.as_str()) {
                    problems.push(format!(
                        "ruling `{}` attaches to `{command}`, which sits in no manifest category",
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
        for entry in &self.help.entries {
            if !universe.contains(entry.command.as_str()) {
                problems.push(format!(
                    "help entry `{}` belongs to `{}`, which sits in no manifest category",
                    entry.path.join(" "),
                    entry.command
                ));
            }
        }
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
    let mut out: BTreeMap<String, CommandCases> = BTreeMap::new();
    let mut seen: BTreeMap<String, PathBuf> = BTreeMap::new();
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
        if let Some(first) = seen.get(&group.command) {
            return Err(CorpusError::DuplicateCommand {
                command: group.command,
                first: first.clone(),
                second: path,
            });
        }
        seen.insert(group.command.clone(), path);
        out.insert(group.command.clone(), group);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source() -> Source {
        Source {
            line: "line".into(),
            branch: "branch".into(),
            commit: "0000000".into(),
        }
    }

    fn case(id: &str, command: &str, argv: &[&str]) -> Case {
        Case {
            id: id.into(),
            suite: "suite".into(),
            command: command.into(),
            source: source(),
            argv: argv.iter().map(|s| (*s).to_string()).collect(),
            fixture: FixtureRef {
                profile: "clean".into(),
                seed: 1,
            },
            mutating: false,
            stdin_frames: Vec::new(),
            plan_template: None,
            plan_argv_placeholder: None,
            plan_vault_root_token: None,
            requires_doc: None,
            requires_code: None,
            normalizations: vec!["vault-root".into()],
            volatile_masks: Vec::new(),
            recorded: Recorded {
                stdout: String::new(),
                stderr: String::new(),
                exit_code: 0,
                exit_class: ExitClass::Ok,
            },
            recorded_tree_delta: None,
        }
    }

    fn group(command: &str, cases: Vec<Case>) -> CommandCases {
        CommandCases {
            source: source(),
            note: "note".into(),
            command: command.into(),
            case_count: cases.len(),
            cases,
        }
    }

    /// A corpus that audits clean: one activatable command with one case,
    /// every pinned unseeded name declared, one unrecorded command, a
    /// fixture tree, a ruling, and a top-level help page listing exactly the
    /// commands the manifest accounts for (bar the hidden one).
    fn sound() -> Corpus {
        let help_long = "COMMANDS\n    find    Find\n    vault   Vault\n    audit   Audit\n    \
                         cache   Cache\n    completions  Completions\n    config  Config\n    \
                         init    Init\n    self-update  Self update\n    serve   Serve\n    \
                         service Service\n\nGLOBAL OPTIONS\n";
        Corpus {
            activation: Activation {
                source: source(),
                note: "note".into(),
                activated: Vec::new(),
                activatable: vec!["find".into()],
                unseeded: UNSEEDED_COMMANDS.iter().map(|s| (*s).to_string()).collect(),
                unrecorded: vec![Unrecorded {
                    command: "vault".into(),
                    reason: "no case exercises it".into(),
                }],
                case_total: 1,
            },
            fixtures: Fixtures {
                source: source(),
                note: "note".into(),
                fixture_count: 1,
                fixtures: vec![FixtureManifest {
                    profile: "clean".into(),
                    seed: 1,
                    entry_count: 1,
                    entries: vec![TreeEntry {
                        path: "a.md".into(),
                        kind: TreeEntryKind::File,
                        content: Some("a".into()),
                        non_utf8_len: None,
                    }],
                }],
            },
            ledger: Ledger {
                source: source(),
                note: "note".into(),
                source_file: "elsewhere.toml".into(),
                ruling_count: 1,
                rulings: vec![Ruling {
                    id: "R-1".into(),
                    surface: "find".into(),
                    commands: vec!["find".into()],
                    cases: vec!["find-1".into()],
                    recorded_behavior: "new".into(),
                    prior_behavior: "old".into(),
                    reason: "decided".into(),
                    source_decision: "elsewhere.md".into(),
                    source: source(),
                }],
            },
            help: HelpCatalog {
                source: source(),
                note: "note".into(),
                entry_count: 1,
                entries: vec![HelpEntry {
                    command: TOP_LEVEL.into(),
                    path: Vec::new(),
                    source: source(),
                    long: help_long.into(),
                    short: help_long.into(),
                }],
            },
            activatable: BTreeMap::from([(
                "find".to_string(),
                group("find", vec![case("find-1", "find", &["find", "--all"])]),
            )]),
            unseeded: BTreeMap::new(),
        }
    }

    fn audit_mentions(corpus: &Corpus, needle: &str) -> bool {
        let problems = corpus.audit();
        let hit = problems.iter().any(|p| p.contains(needle));
        assert!(
            hit,
            "expected a violation containing {needle:?}, got: {problems:?}"
        );
        hit
    }

    #[test]
    fn the_reference_corpus_audits_clean() {
        assert_eq!(sound().audit(), Vec::<String>::new());
    }

    #[test]
    fn command_of_skips_flags_and_their_values() {
        let argv = |tokens: &[&str]| tokens.iter().map(|s| (*s).to_string()).collect::<Vec<_>>();
        assert_eq!(command_of(&argv(&["find", "--all"])), "find");
        assert_eq!(
            command_of(&argv(&["--vault", "name", "service"])),
            "service"
        );
        assert_eq!(command_of(&argv(&["-C", "somewhere", "find"])), "find");
        assert_eq!(command_of(&argv(&["--color", "never", "get", "a"])), "get");
        assert_eq!(command_of(&argv(&["--verbose", "count"])), "count");
        assert_eq!(command_of(&argv(&["--help"])), TOP_LEVEL);
    }

    #[test]
    fn a_narrowed_unseeded_list_is_caught() {
        let mut corpus = sound();
        corpus.activation.unseeded.retain(|c| c != "service");
        audit_mentions(&corpus, "which the harness pins as unseeded");
    }

    #[test]
    fn an_invented_unseeded_name_is_caught() {
        let mut corpus = sound();
        corpus.activation.unseeded.push("find".into());
        audit_mentions(&corpus, "which the harness does not pin as unseeded");
    }

    /// The whole point of pinning the list: moving a case file into the
    /// activatable set and narrowing the manifest no longer audits clean.
    #[test]
    fn an_unseeded_command_cannot_be_activated_by_editing_the_manifest() {
        let mut corpus = sound();
        corpus.activation.unseeded.retain(|c| c != "service");
        corpus.activation.activatable.push("service".into());
        corpus.activation.activated.push("service".into());
        corpus.activation.case_total = 2;
        corpus.activatable.insert(
            "service".into(),
            group(
                "service",
                vec![case("service-1", "service", &["service", "status"])],
            ),
        );
        audit_mentions(&corpus, "which the harness pins as unseeded");
    }

    #[test]
    fn an_activatable_command_with_no_case_file_is_caught() {
        let mut corpus = sound();
        corpus.activation.activatable.push("count".into());
        audit_mentions(&corpus, "but no case file records it");
    }

    #[test]
    fn a_case_file_the_manifest_does_not_list_is_caught() {
        let mut corpus = sound();
        corpus.activatable.insert(
            "count".into(),
            group("count", vec![case("count-1", "count", &["count"])]),
        );
        corpus.activation.case_total = 2;
        audit_mentions(&corpus, "which the manifest does not list as activatable");
    }

    #[test]
    fn activating_a_command_with_no_cases_is_caught() {
        let mut corpus = sound();
        corpus.activation.activated.push("count".into());
        audit_mentions(&corpus, "which has no activatable case file");
    }

    #[test]
    fn a_command_in_two_categories_is_caught() {
        let mut corpus = sound();
        corpus.activation.unrecorded.push(Unrecorded {
            command: "find".into(),
            reason: "because".into(),
        });
        audit_mentions(&corpus, "as both activatable and unrecorded");
    }

    #[test]
    fn an_unrecorded_command_with_no_reason_is_caught() {
        let mut corpus = sound();
        corpus.activation.unrecorded[0].reason = "  ".into();
        audit_mentions(&corpus, "listed as unrecorded with no reason");
    }

    #[test]
    fn a_command_in_no_category_is_caught() {
        let mut corpus = sound();
        corpus.activation.unrecorded.clear();
        audit_mentions(
            &corpus,
            "listed on the top-level help page but sits in no manifest category",
        );
    }

    #[test]
    fn a_category_the_help_page_does_not_list_is_caught() {
        let mut corpus = sound();
        corpus.activation.unrecorded.push(Unrecorded {
            command: "invented".into(),
            reason: "because".into(),
        });
        audit_mentions(&corpus, "which the top-level help page does not list");
    }

    /// `manpage` is pinned unseeded and absent from the help page, which is
    /// exactly the allowance the hidden-command list makes.
    #[test]
    fn a_hidden_command_is_allowed_to_be_absent_from_help() {
        assert!(COMMANDS_ABSENT_FROM_HELP.contains(&"manpage"));
        assert!(UNSEEDED_COMMANDS.contains(&"manpage"));
        assert_eq!(sound().audit(), Vec::<String>::new());
    }

    #[test]
    fn a_missing_top_level_help_page_is_caught() {
        let mut corpus = sound();
        corpus.help.entries.clear();
        corpus.help.entry_count = 0;
        audit_mentions(&corpus, "carries no top-level page");
    }

    #[test]
    fn a_miscounted_case_file_is_caught() {
        let mut corpus = sound();
        corpus.activatable.get_mut("find").unwrap().case_count = 7;
        audit_mentions(&corpus, "declares 7 cases and carries 1");
    }

    #[test]
    fn a_case_filed_under_the_wrong_command_is_caught() {
        let mut corpus = sound();
        corpus.activatable.get_mut("find").unwrap().cases[0].command = "count".into();
        audit_mentions(&corpus, "but names command `count`");
    }

    #[test]
    fn a_case_whose_argv_selects_another_command_is_caught() {
        let mut corpus = sound();
        corpus.activatable.get_mut("find").unwrap().cases[0].argv = vec!["count".into()];
        audit_mentions(&corpus, "its argv selects `count`");
    }

    #[test]
    fn a_duplicate_case_id_is_caught() {
        let mut corpus = sound();
        let extra = case("find-1", "find", &["find", "--all"]);
        let group = corpus.activatable.get_mut("find").unwrap();
        group.cases.push(extra);
        group.case_count = 2;
        corpus.activation.case_total = 2;
        audit_mentions(&corpus, "duplicate case id `find-1`");
    }

    #[test]
    fn a_pinned_case_total_that_does_not_match_is_caught() {
        let mut corpus = sound();
        corpus.activation.case_total = 99;
        audit_mentions(&corpus, "manifest pins 99 recorded cases");
    }

    #[test]
    fn an_exit_class_that_contradicts_its_code_is_caught() {
        let mut corpus = sound();
        corpus.activatable.get_mut("find").unwrap().cases[0]
            .recorded
            .exit_class = ExitClass::Usage;
        audit_mentions(&corpus, "which is the class for exit 2");
    }

    #[test]
    fn every_exit_class_agrees_with_its_code() {
        assert_eq!(ExitClass::Ok.code(), 0);
        assert_eq!(ExitClass::Operational.code(), 1);
        assert_eq!(ExitClass::PartialApply.code(), 1);
        assert_eq!(ExitClass::Usage.code(), 2);
        assert_eq!(ExitClass::Refusal.code(), 2);
    }

    #[test]
    fn a_case_naming_an_unrecorded_fixture_is_caught() {
        let mut corpus = sound();
        corpus.activatable.get_mut("find").unwrap().cases[0].fixture = FixtureRef {
            profile: "nowhere".into(),
            seed: 9,
        };
        audit_mentions(&corpus, "whose tree is not recorded");
    }

    #[test]
    fn a_mutating_case_without_a_tree_delta_is_caught() {
        let mut corpus = sound();
        corpus.activatable.get_mut("find").unwrap().cases[0].mutating = true;
        audit_mentions(&corpus, "yet lacks a recorded tree delta");
    }

    #[test]
    fn a_miscounted_fixture_manifest_is_caught() {
        let mut corpus = sound();
        corpus.fixtures.fixtures[0].entry_count = 5;
        audit_mentions(&corpus, "declares 5 entries and carries 1");
    }

    #[test]
    fn a_miscounted_fixtures_file_is_caught() {
        let mut corpus = sound();
        corpus.fixtures.fixture_count = 4;
        audit_mentions(&corpus, "declares 4 manifests and carries 1");
    }

    #[test]
    fn a_ruling_citing_an_unknown_case_is_caught() {
        let mut corpus = sound();
        corpus.ledger.rulings[0].cases = vec!["nope".into()];
        audit_mentions(&corpus, "cites case `nope`");
    }

    #[test]
    fn a_ruling_attached_to_an_uncategorized_command_is_caught() {
        let mut corpus = sound();
        corpus.ledger.rulings[0].commands = vec!["nowhere".into()];
        audit_mentions(
            &corpus,
            "attaches to `nowhere`, which sits in no manifest category",
        );
    }

    #[test]
    fn a_miscounted_ledger_is_caught() {
        let mut corpus = sound();
        corpus.ledger.ruling_count = 3;
        audit_mentions(&corpus, "declares 3 rulings and carries 1");
    }

    #[test]
    fn a_help_entry_for_an_uncategorized_command_is_caught() {
        let mut corpus = sound();
        corpus.help.entries.push(HelpEntry {
            command: "nowhere".into(),
            path: vec!["nowhere".into()],
            source: source(),
            long: String::new(),
            short: String::new(),
        });
        corpus.help.entry_count = 2;
        audit_mentions(
            &corpus,
            "belongs to `nowhere`, which sits in no manifest category",
        );
    }

    #[test]
    fn a_miscounted_help_catalog_is_caught() {
        let mut corpus = sound();
        corpus.help.entry_count = 8;
        audit_mentions(&corpus, "declares 8 entries and carries 1");
    }

    #[test]
    fn rulings_for_returns_only_the_commands_rulings() {
        let corpus = sound();
        let found = corpus.rulings_for("find");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].id, "R-1");
        assert_eq!(found[0].cases, vec!["find-1".to_string()]);
        assert_eq!(found[0].surface, "find");
        assert!(corpus.rulings_for("count").is_empty());
    }

    #[test]
    fn help_for_returns_only_the_commands_entries() {
        let corpus = sound();
        let found = corpus.help_for(TOP_LEVEL);
        assert_eq!(found.len(), 1);
        assert!(found[0].path.is_empty());
        assert!(found[0].long.contains("COMMANDS"));
        assert!(corpus.help_for("find").is_empty());
    }

    #[test]
    fn top_level_commands_reads_the_command_rows() {
        let corpus = sound();
        let listed = corpus.help.top_level_commands();
        assert!(listed.contains(&"find".to_string()));
        assert!(listed.contains(&"vault".to_string()));
        assert!(!listed.contains(&"manpage".to_string()));
    }

    #[test]
    fn a_ruling_only_touching_unseeded_commands_has_no_activation_path() {
        let mut corpus = sound();
        corpus.ledger.rulings[0].commands = vec!["service".into()];
        let orphaned = corpus.rulings_without_an_activation_path();
        assert_eq!(orphaned.len(), 1);
        assert_eq!(orphaned[0].id, "R-1");
    }

    #[test]
    fn a_ruling_touching_an_activatable_command_has_an_activation_path() {
        assert!(sound().rulings_without_an_activation_path().is_empty());
    }

    #[test]
    fn the_gate_yields_nothing_until_a_command_is_activated() {
        let corpus = sound();
        assert!(corpus.activated_cases().is_empty());
        assert_eq!(corpus.dormant_cases().len(), 1);
    }

    #[test]
    fn the_gate_yields_an_activated_commands_cases() {
        let mut corpus = sound();
        corpus.activation.activated.push("find".into());
        assert_eq!(corpus.activated_cases().len(), 1);
        assert!(corpus.dormant_cases().is_empty());
    }

    #[test]
    fn two_files_claiming_one_command_are_refused() {
        let dir = std::env::temp_dir().join(format!("norn-testkit-dup-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let body = |command: &str| {
            format!(
                r#"{{"source":{{"line":"l","branch":"b","commit":"c"}},"note":"n",
                    "command":"{command}","case_count":0,"cases":[]}}"#
            )
        };
        std::fs::write(dir.join("a.json"), body("find")).unwrap();
        std::fs::write(dir.join("b.json"), body("find")).unwrap();
        let result = read_command_dir(&dir);
        std::fs::remove_dir_all(&dir).ok();
        let message = match result {
            Err(e) => e.to_string(),
            Ok(loaded) => panic!("a duplicate command must be refused, but {loaded:?} loaded"),
        };
        assert!(
            message.contains("two case files claim command `find`"),
            "{message}"
        );
    }
}
