//! The coverage corpus suite.
//!
//! `tests/corpus/` holds recorded command invocations — argv, the fixture
//! tree they ran against, and the bytes and exit class that came back —
//! beside the input trees those fixtures produced, the behavior rulings, and
//! the help prose recorded with them. All of it is **evidence with zero
//! authority**. A recording says what a program did once; it makes no claim
//! about what this program should do, and nothing here is a specification.
//!
//! **Every case is dormant by default, and dormancy is structural.** The
//! cases that run are exactly the cases of the commands named in
//! `tests/corpus/activation.json`. That list is empty. Nothing else gates
//! them: there is no ignore attribute to remove and no environment variable
//! to set, because approving a command's recorded output is a judgment a
//! person makes and records, and the manifest is where that record lives.
//!
//! # Activating a command
//!
//! Adding a command to `activated` means these four questions were answered,
//! in order. The procedure lives in [`norn_testkit::corpus::Activation`];
//! what follows is what activation costs in this suite.
//!
//! 0. **Is the recorded output good?** Judged against the surface as it is
//!    being re-derived, never against the recording. A recording that is not
//!    good is retired by a recorded ruling and the contract is authored
//!    fresh — activation is not the only exit.
//! 1. **Does the output shape change?** If not, the cases enter as they
//!    stand.
//! 2. **Mechanical migration** rewrites the recorded bytes to the new shape.
//! 3. **Semantic divergence** records its ruling first, and activation
//!    follows it.
//!
//! A cross-command inconsistency is decided **once, as a class ruling**, and
//! swept across every command it touches — one grammar, not per-command
//! drift. The commands a class ruling sweeps are activated together.
//!
//! The behavior rulings attach per command:
//! [`Corpus::rulings_for`](norn_testkit::corpus::Corpus::rulings_for)
//! returns the ones that come up for re-judgment when a command is
//! activated. A ruling becomes contract only on an affirmative judgment
//! against the re-derived surface. A command that is deleted takes its
//! rulings with it.
//!
//! # The three categories
//!
//! Every command the binary had at the recording pin sits in exactly one of
//! three disjoint categories — `activatable` (has cases), `unseeded` (no
//! behavior at the pin, so no activation path) and `unrecorded` (behavior at
//! the pin, but no case exercises it, so its contract is authored with no
//! evidence at all). [`corpus_is_structurally_sound`] reconciles the set
//! against the recorded top-level help page so none can go uncategorized, and
//! refuses a command that appears in two.
//!
//! `activated` is not a fourth category: it is the subset of `activatable`
//! whose cases run.
//!
//! # What activation still needs
//!
//! Running a case means materializing its fixture vault, spawning the built
//! binary with its argv under the recorded environment, and judging what
//! comes back. One of those three does not exist yet:
//!
//! - **The fixture vault** is recorded in full. `tests/corpus/fixtures.json`
//!   holds every entry's exact bytes, so a case materializes its tree by
//!   writing each entry out at its path — code that is not written, against
//!   data that is complete. **No generator is in that path**: the profile and
//!   seed name a recording rather than instruct a regeneration, and the
//!   `norn-fixtures` generator binds only its own profiles.
//! - **The binary's behavior.** The composition root is a stub; there is no
//!   command to invoke.
//!
//! Until the runner and the composition root land,
//! [`activated_cases_reach_a_runner`] fails loudly for any case that is
//! activated, naming what is missing. That is the honest failure: the gate is
//! real, and what sits behind it is not built.
//!
//! **Activation is deferred to Layer 6.** A case judges rendered bytes, and
//! the renderings land at Layer 6, so no command is activated before then.
//! The corpus README records the deferral, and the rulings that activation
//! re-judges are already in the ledger.

use std::collections::BTreeSet;
use std::path::PathBuf;

use norn_testkit::corpus::{Corpus, UNSEEDED_COMMANDS};

fn corpus() -> Corpus {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/corpus");
    Corpus::load(&dir).unwrap_or_else(|e| panic!("the corpus did not load: {e}"))
}

/// The data set holds together: the gate cannot be bypassed, the unseeded and
/// unrecorded lists match the ones the harness pins, every command the binary
/// listed sits in exactly one category, no case went missing, every case is
/// filed under the command its own argv selects, every exit class agrees with
/// its code and with the mutation specialization, every placeholder in a
/// recording is declared and every declaration appears, and every ruling,
/// prose entry and fixture reference names something real.
#[test]
fn corpus_is_structurally_sound() {
    let problems = corpus().audit();
    assert!(
        problems.is_empty(),
        "the corpus is not structurally sound:\n  {}",
        problems.join("\n  ")
    );
}

/// The gate: a case runs only if its command was activated. Every other
/// recorded case is dormant, and the two sets together account for every
/// activatable case — a case cannot fall out of both and quietly disappear.
#[test]
fn only_activated_commands_run() {
    let corpus = corpus();
    let activated = corpus.activated_cases();
    let dormant = corpus.dormant_cases();

    let activated_commands = &corpus.activation.activated;
    for case in &activated {
        assert!(
            activated_commands.contains(&case.command),
            "case `{}` would run, but `{}` is not activated",
            case.id,
            case.command
        );
    }
    for case in &dormant {
        assert!(
            !activated_commands.contains(&case.command),
            "case `{}` is dormant, but `{}` is activated",
            case.id,
            case.command
        );
    }

    let unseeded_total: usize = corpus
        .all_cases()
        .filter(|case| corpus.activation.unseeded.contains(&case.command))
        .count();
    assert_eq!(
        activated.len() + dormant.len() + unseeded_total,
        corpus.activation.case_total,
        "the activated, dormant, and unseeded sets do not account for every recorded case"
    );
}

/// The unseeded commands are absent from the gate by construction: no case
/// file offers one for activation, and the list itself is pinned in the
/// harness, so narrowing the manifest fails the audit rather than opening a
/// path.
#[test]
fn unseeded_commands_have_no_activation_path() {
    let corpus = corpus();
    let activatable: BTreeSet<&str> = corpus.activatable_commands().collect();
    for command in &corpus.activation.unseeded {
        assert!(
            !activatable.contains(command.as_str()),
            "`{command}` carries no behavior at the pin, yet it is offered for activation"
        );
    }
    let declared: BTreeSet<&str> = corpus
        .activation
        .unseeded
        .iter()
        .map(String::as_str)
        .collect();
    let pinned: BTreeSet<&str> = UNSEEDED_COMMANDS.iter().copied().collect();
    assert_eq!(
        declared, pinned,
        "the manifest's unseeded list differs from the one the harness pins"
    );
}

/// Every case names a tree that is recorded. A case whose fixture tree went
/// unrecorded could never be judged — there would be nothing to materialize
/// it from, and a difference in output would be unattributable.
#[test]
fn every_case_has_a_recorded_input_tree() {
    let corpus = corpus();
    for case in corpus.all_cases() {
        let manifest = corpus.fixture(&case.fixture).unwrap_or_else(|| {
            panic!(
                "case `{}` runs against fixture `{}`, whose tree is not recorded",
                case.id, case.fixture
            )
        });
        assert!(
            !manifest.entries.is_empty(),
            "fixture `{}` records an empty tree",
            case.fixture
        );
    }
}

/// Every recorded entry hands back the bytes of the file it stands for.
///
/// This is the property the activation contract rests on: a case materializes
/// its tree from its manifest, so a manifest that could not rebuild its own
/// tree would put a generator back in the path. Asserting it against the real
/// corpus is what makes "the recording is complete" a checked claim rather
/// than a description — and it exercises the binary entries specifically,
/// which are the ones that used to carry a length instead of contents.
#[test]
fn every_recorded_entry_reconstructs_its_bytes() {
    let corpus = corpus();
    let mut entries = 0;
    let mut files = 0;
    let mut directories = 0;
    let mut binary = 0;
    for manifest in &corpus.fixtures.fixtures {
        for entry in &manifest.entries {
            entries += 1;
            let reconstructed = entry.bytes().unwrap_or_else(|problem| {
                panic!(
                    "fixture `{}` entry `{}` does not reconstruct: {problem}",
                    manifest.fixture_ref(),
                    entry.path
                )
            });
            let Some(bytes) = reconstructed else {
                directories += 1;
                continue;
            };
            files += 1;
            if entry.content_base64.is_some() {
                assert!(
                    std::str::from_utf8(&bytes).is_err(),
                    "fixture `{}` entry `{}` is recorded as binary but decodes to text",
                    manifest.fixture_ref(),
                    entry.path
                );
                binary += 1;
            }
        }
    }
    // Counted apart, so a directory entry appearing in a future recording
    // shows up as one rather than inflating the file total.
    assert_eq!(entries, 422, "the recorded entry total moved");
    assert_eq!(files, 422, "the recorded file total moved");
    assert_eq!(
        directories, 0,
        "a recorded manifest gained a directory entry"
    );
    assert_eq!(binary, 11, "the recorded binary-entry total moved");
}

/// Rulings no activation will ever bring up, because every command they
/// attach to has no activation path. They are not defects, but nobody is
/// prompted to judge them, so the set is pinned here: a change to it is a
/// change somebody should see.
///
/// `PD-134` is the whole set today. It rules on a `--vault` flag local to
/// `service`, which carries no behavior at the pin. `service` is reserved in
/// the architecture and not yet built; it is decided to be re-derived at
/// installation scope over the one host an installation supervises, with no
/// per-vault form, so the flag has no successor. The ruling stays here until
/// `service`'s recorded cases retire at Layer 6, and the corpus retires it
/// with them.
#[test]
fn rulings_without_an_activation_path_are_known() {
    let corpus = corpus();
    let ids: Vec<&str> = corpus
        .rulings_without_an_activation_path()
        .iter()
        .map(|ruling| ruling.id.as_str())
        .collect();
    assert_eq!(
        ids,
        vec!["PD-134"],
        "the set of rulings with no activation path changed"
    );
}

/// The line this repository records rulings on. A ruling whose source names
/// it was judged here, against this repository's surface; every other ruling
/// was transcribed from the recording checkout.
const THIS_LINE: &str = "norn";

/// The class ruling that strikes help generated from a vault.
const HELP_CLASS_RULING: &str = "CR-help-reads-no-vault";

/// Whether this repository holds a file at `relative`, a path from its root.
///
/// Only a path made of plain components counts: an absolute path, or one that
/// climbs with `..`, names a file outside the tree whatever it resolves to.
#[allow(clippy::disallowed_methods)] // Harness scaffolding: a ledger citation checked against this repository's own tree.
fn repository_holds(relative: &str) -> bool {
    let within = std::path::Path::new(relative)
        .components()
        .all(|component| matches!(component, std::path::Component::Normal(_)));
    within
        && PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join(relative)
            .is_file()
}

#[test]
fn a_citation_outside_the_repository_is_not_held() {
    assert!(repository_holds("crates/norn-wire/src/verb.rs"));
    assert!(!repository_holds("/etc/hosts"));
    assert!(!repository_holds("../norn/crates/norn-wire/src/verb.rs"));
    assert!(!repository_holds("crates/../crates/norn-wire/src/verb.rs"));
}

/// The rulings recorded on this line are pinned, and each one cites a
/// decision this repository holds.
///
/// Two namespaces share one field. A transcribed ruling's `source_decision`
/// is a path inside the recording checkout, whose decision records are
/// numbered from `0001` as this repository's are, so it resolves here to the
/// wrong document or to none. A ruling recorded on this line names this
/// repository in its `source` and cites a path that resolves here. A ruling
/// whose source names neither is refused, so a reader always knows which
/// namespace a path is read in.
#[test]
fn rulings_recorded_on_this_line_are_known_and_cite_this_repository() {
    let corpus = corpus();
    let recording = &corpus.ledger.source;
    let mut recorded_here = Vec::new();
    for ruling in &corpus.ledger.rulings {
        let source = &ruling.source;
        if source.line == THIS_LINE {
            assert!(
                repository_holds(&ruling.source_decision),
                "ruling `{}` is recorded on this line but cites `{}`, which this repository does not hold",
                ruling.id,
                ruling.source_decision
            );
            recorded_here.push(ruling.id.as_str());
        } else {
            assert!(
                source.line == recording.line
                    && source.branch == recording.branch
                    && source.commit == recording.commit,
                "ruling `{}` names neither this line nor the recording checkout as its source",
                ruling.id
            );
        }
    }
    assert_eq!(
        recorded_here,
        vec![
            HELP_CLASS_RULING,
            "SR-find",
            "SR-get",
            "SR-count",
            "SR-validate",
            "SR-describe",
            "SR-top-level",
        ],
        "the set of rulings recorded on this line changed"
    );
}

/// The help class ruling is swept across every command, as a class ruling
/// is, and covers every recorded help page.
///
/// A help page never reads a vault, on any command, so the ruling attaches to
/// the whole command universe rather than to the commands whose pages happen
/// to be recorded; and every recorded `--help` or `-h` invocation is one of
/// the cases it re-judges.
#[test]
fn the_help_class_ruling_sweeps_every_command() {
    let corpus = corpus();
    let ruling = corpus
        .ledger
        .rulings
        .iter()
        .find(|ruling| ruling.id == HELP_CLASS_RULING)
        .unwrap_or_else(|| panic!("the ledger records no ruling `{HELP_CLASS_RULING}`"));

    let swept: BTreeSet<&str> = ruling.commands.iter().map(String::as_str).collect();
    assert_eq!(
        swept,
        corpus.command_universe(),
        "the help class ruling does not attach to every command"
    );

    let covered: BTreeSet<&str> = ruling.cases.iter().map(String::as_str).collect();
    let help_pages: BTreeSet<&str> = corpus
        .all_cases()
        .filter(|case| case.argv.iter().any(|arg| arg == "--help" || arg == "-h"))
        .map(|case| case.id.as_str())
        .collect();
    assert_eq!(
        covered, help_pages,
        "the help class ruling does not cover exactly the recorded help pages"
    );
}

/// The top-level shape ruling.
const TOP_LEVEL_RULING: &str = "SR-top-level";

/// The wire registry the top-level ruling cites.
const VERB_REGISTRY: &str = "crates/norn-wire/src/verb.rs";

/// The machine-local verbs the top-level ruling names as reserved, not built.
const RESERVED_MACHINE_LOCAL_VERBS: [&str; 4] =
    ["self-update", "service", "completions", "manpage"];

/// The commands the top-level ruling names as decided and not in the wire
/// registry, spelled as the registry would spell them.
const DECIDED_NOT_REGISTERED: [&str; 1] = ["model_fetch"];

/// Whether `docs/architecture.md` marks `verb` reserved on a `norn-client`
/// row: the client that owns the machine-local verbs is not written yet.
#[allow(clippy::disallowed_methods)] // Harness scaffolding: this repository's own architecture document.
fn architecture_reserves_client_verb(verb: &str) -> bool {
    let architecture = std::fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../docs/architecture.md"),
    )
    .expect("reading the architecture document");
    architecture.lines().any(|line| {
        line.starts_with("| `norn-client`") && line.contains("*Reserved.*") && line.contains(verb)
    })
}

/// The top-level ruling names what exists as what exists.
///
/// No command line is built on this line, so the ruling separates what the
/// code holds from what is decided. It cites the wire verb registry, names
/// every verb the registry holds, names the client's machine-local verbs only
/// where the architecture marks them reserved, and names the decided commands
/// the registry does not hold yet as absent from it. A verb added to or
/// removed from the registry fails here until the ruling says so.
#[test]
fn the_top_level_ruling_names_what_the_registry_holds() {
    let corpus = corpus();
    let ruling = corpus
        .ledger
        .rulings
        .iter()
        .find(|ruling| ruling.id == TOP_LEVEL_RULING)
        .unwrap_or_else(|| panic!("the ledger records no ruling `{TOP_LEVEL_RULING}`"));
    assert_eq!(
        ruling.source_decision, VERB_REGISTRY,
        "the top-level ruling does not cite the wire verb registry"
    );

    let names = |spelling: &str| ruling.recorded_behavior.contains(&format!("`{spelling}`"));
    let registered: BTreeSet<&str> = norn_wire::Verb::ALL
        .iter()
        .map(|verb| verb.as_str())
        .collect();
    for verb in &registered {
        assert!(
            names(verb),
            "the wire registry holds `{verb}`, which the top-level ruling does not name"
        );
    }
    for verb in RESERVED_MACHINE_LOCAL_VERBS {
        assert!(
            names(verb),
            "the top-level ruling does not name the reserved verb `{verb}`"
        );
        assert!(
            architecture_reserves_client_verb(verb),
            "the top-level ruling names `{verb}` as reserved, and the architecture reserves no such client verb"
        );
    }
    for verb in DECIDED_NOT_REGISTERED {
        assert!(
            !registered.contains(verb),
            "the top-level ruling names `{verb}` as not yet registered, and the wire registry holds it"
        );
    }
}

/// Every activated case must reach a runner. None is activated, so nothing
/// runs; the first command to be activated meets the missing execution seam
/// here rather than in a silent skip.
#[test]
fn activated_cases_reach_a_runner() {
    let corpus = corpus();
    let unrunnable: Vec<String> = corpus
        .activated_cases()
        .iter()
        .map(|case| format!("`{}` (`{}`)", case.id, case.command))
        .collect();
    assert!(
        unrunnable.is_empty(),
        "no runner can execute an activated case yet: a case needs its fixture \
         vault written out from its recorded manifest, and its argv needs a \
         composition root that does something. Activate a command once both \
         exist. Activated cases:\n  {}",
        unrunnable.join("\n  ")
    );
}
