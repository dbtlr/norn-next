//! The coverage corpus suite.
//!
//! `tests/corpus/` holds recorded command invocations — argv, the fixture
//! they ran against, and the bytes and exit class that came back — beside
//! the behavior rulings and the help prose recorded with them. All of it is
//! **evidence with zero authority**. A recording says what a program did
//! once; it makes no claim about what this program should do, and nothing
//! here is a specification.
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
//! in order. The procedure lives in
//! [`norn_testkit::corpus::Activation`]; what follows is what activation
//! costs in this suite.
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
//! The nine commands in the manifest's `unseeded` list carry no behavior at
//! the recording pin and have no activation path at all. Whether they exist
//! is a question for the verb charter, not for this suite, and
//! [`corpus_is_structurally_sound`] refuses any corpus that offers one of
//! them as activatable.
//!
//! # What activation still needs
//!
//! Running a case means materializing its fixture vault, spawning the built
//! binary with its argv under the recorded environment, and judging what
//! comes back. Two of those three do not exist yet:
//!
//! - **The fixture vault.** Every case names a generator profile and seed
//!   (`tests/corpus/environment.json` records the environment the recordings
//!   ran under). The generator that turns a profile and seed into a tree
//!   arrives with the `norn-fixtures` crate.
//! - **The binary's behavior.** The composition root is a stub; there is no
//!   command to invoke.
//!
//! Until both land, [`activated_cases_reach_a_runner`] fails loudly for any
//! case that is activated, naming what is missing. That is the honest
//! failure: the gate is real, and what sits behind it is not built.

use std::path::PathBuf;

use norn_testkit::corpus::Corpus;

fn corpus() -> Corpus {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/corpus");
    Corpus::load(&dir).unwrap_or_else(|e| panic!("the corpus did not load: {e}"))
}

/// The data set holds together: the gate cannot be bypassed, no unseeded
/// command is offered for activation, no case went missing, and every ruling
/// and manifest entry names something real.
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
/// file offers one for activation, so no list edit can make one run.
#[test]
fn unseeded_commands_have_no_activation_path() {
    let corpus = corpus();
    let activatable: Vec<&str> = corpus.activatable_commands().collect();
    for command in &corpus.activation.unseeded {
        assert!(
            !activatable.contains(&command.as_str()),
            "`{command}` carries no behavior at the pin, yet it is offered for activation"
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
         vault from the deterministic generator, and its argv needs a \
         composition root that does something. Activate a command once both \
         exist. Activated cases:\n  {}",
        unrunnable.join("\n  ")
    );
}
