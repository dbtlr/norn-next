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
//! against the re-derived surface. A command the verb charter deletes takes
//! its rulings with it.
//!
//! # The four categories
//!
//! Every command the binary had at the recording pin sits in exactly one
//! category, and [`corpus_is_structurally_sound`] reconciles the set against
//! the recorded top-level help page so none can go uncategorized:
//! `activatable` (has cases), `activated` (its cases run), `unseeded` (no
//! behavior at the pin, so no activation path), and `unrecorded` (behavior
//! at the pin, but no case exercises it — its contract is authored with no
//! evidence at all).
//!
//! # What activation still needs
//!
//! Running a case means materializing its fixture vault, spawning the built
//! binary with its argv under the recorded environment, and judging what
//! comes back. Two of those three do not exist yet:
//!
//! - **The fixture vault.** Every case names a generator profile and seed,
//!   and `tests/corpus/fixtures.json` records the tree each of those
//!   produced, file by file. The generator that turns a profile and seed
//!   into a tree arrives with the `norn-fixtures` crate, and it is
//!   re-derived with different realism knobs — so **a case is judgeable only
//!   once that generator reproduces its recorded tree exactly.** Until then
//!   a difference in output would read as a defect in this program when it
//!   is drift in the generator.
//! - **The binary's behavior.** The composition root is a stub; there is no
//!   command to invoke.
//!
//! Until both land, [`activated_cases_reach_a_runner`] fails loudly for any
//! case that is activated, naming what is missing. That is the honest
//! failure: the gate is real, and what sits behind it is not built.

use std::collections::BTreeSet;
use std::path::PathBuf;

use norn_testkit::corpus::{Corpus, UNSEEDED_COMMANDS};

fn corpus() -> Corpus {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/corpus");
    Corpus::load(&dir).unwrap_or_else(|e| panic!("the corpus did not load: {e}"))
}

/// The data set holds together: the gate cannot be bypassed, the unseeded
/// list matches the one the harness pins, every command the binary listed
/// sits in a category, no case went missing, every case is filed under the
/// command its own argv selects, every exit class agrees with its code, and
/// every ruling, prose entry and fixture reference names something real.
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

/// Every case's input tree is recorded, so activation has something to hold
/// a future generator to. A case whose fixture tree went unrecorded could
/// never be judged: a difference in output would be unattributable.
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

/// Rulings no activation will ever bring up, because every command they
/// attach to has no activation path. They are not defects, but nobody is
/// prompted to judge them, so the set is pinned here: a change to it is a
/// change somebody should see.
///
/// `PD-134` is the whole set today — it describes a flag on `service`, which
/// carries no behavior at the pin. Its disposition belongs to the verb
/// charter, along with the command it describes.
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
         vault reproduced by the deterministic generator, and its argv needs a \
         composition root that does something. Activate a command once both \
         exist. Activated cases:\n  {}",
        unrunnable.join("\n  ")
    );
}
