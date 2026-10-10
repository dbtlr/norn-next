//! Routes: the folder a placement rule's `allowed_paths` sends a misplaced
//! document to.
//!
//! The candidates for a selected `document/misplaced` finding are the
//! destinations the placement rules it cites declare a route to: each rule's
//! route filled ([`Rule::fill_route`](norn_config::schema::Rule::fill_route),
//! the one way a route is bound to its path and filled) — a
//! `{{path.<name>}}` from what the rule's own `match.path` binds where the
//! document stands, a clock token from the plan's one reading — with the
//! document's own file name after it. A rule declaring no route proposes
//! nothing. The candidates agree only as one destination, spelled alike: on a
//! root folding case, `Tasks/` and `tasks/` tie. No
//! candidate is [`SkipReason::NoDeclaredFix`]; candidates that disagree are
//! [`SkipReason::Tie`], with each destination and the rule proposing it; a
//! route reading a capture its rule's `match.path` binds several ways is
//! [`SkipReason::AmbiguousCapture`], noting two of the bindings.
//!
//! **The fix is one `move_document`**, whose link cascade the one planner
//! generates as it generates any move's: every link the text layer can
//! respell is rewritten, and every other is advised on in the forecast with
//! the reason a Layer 4 move gives it. A destination something stands at in
//! the vault, as the root reads its case, or beneath a document that stands,
//! or that an earlier route of the batch moves a document to, above or
//! beneath, is [`SkipReason::DestinationTaken`]; one where no document can be
//! made, and a route filling to no folder — a capture holding `:` or slugging
//! to nothing ([`Rule::fill_route`](norn_config::schema::Rule::fill_route)) — is
//! [`SkipReason::JudgeWouldRefuse`], noting why. **The move is
//! judged at its destination**: the document's bytes there against its
//! before-state, since `match.path`, `exclude.path` and `allowed_paths` can
//! each judge the same bytes differently in another place — a destination
//! that brings in required fields, that introduces any other violation, or
//! where the document is still misplaced, skips through the judge as any fix
//! does.
//!
//! **A route its document's judgment admits moves only where the judgment of
//! the whole plan admitted it** ([`Repairing::admitted`]): its link cascade
//! rewrites documents that judgment alone sees. Until then it is withheld —
//! its document composed as though it did not move, its finding skipped as
//! one the judge would refuse, with its destination — and it claims no
//! destination, so a route the whole plan refuses takes no place from a
//! later one.

use std::collections::BTreeSet;

use norn_config::schema::{FillRefusal, PathBindings, RuleWork};
use norn_wire::{
    CitedFinding, Confidence, DocumentPath, FindingRow, Operation, OperationId, OperationKind,
    SkipReason, SkippedFinding, UnresolvedOperation, UnresolvedReason,
};

use super::{Document, Fix, Fixing, Routing, State, Unmade, skip, spelled, spelled_candidates};
use crate::applier::Held;
use crate::evidence::count_rule_work;
#[cfg(doc)]
use crate::planner::repair::Repairing;
use crate::planner::repair::{Destination, Reading};

/// One destination a rule's route proposes.
struct Proposal {
    to: DocumentPath,
    rule: String,
    clocked: bool,
}

/// A route its document's own judgment admits.
pub(super) enum Route {
    /// The move, which the judgment of the whole plan admitted
    /// ([`Repairing::admitted`]).
    Moved(Fix),
    /// The move withheld until the judgment of the whole plan admits it: the
    /// skip its finding carries meanwhile, as one the judge would refuse with
    /// its destination, and the destination.
    Withheld {
        skip: Box<SkippedFinding>,
        to: DocumentPath,
    },
}

/// The fix of the misplaced document `row` names — `finding`, as the
/// before-state holds it — standing as `state` composes it, that `rules`,
/// the placement rules its finding cites, route it by, judged against what
/// `routing` reads; or why it is not made. `id` is the operation's id, which
/// an operation left unresolved takes.
pub(super) fn fix<R: Reading>(
    document: &Document<'_>,
    state: &State,
    row: &FindingRow,
    finding: &Held,
    rules: &BTreeSet<String>,
    id: OperationId,
    routing: &mut Routing<'_, R>,
) -> Result<Result<Route, Unmade>, R::Error> {
    let skipped = |finding| Ok(Err(Unmade::Skipped(Box::new(finding))));
    let proposals = match proposed(document, state, row, rules) {
        Ok(proposals) => proposals,
        Err(Unproposed::Skipped(finding)) => return Ok(Err(Unmade::Skipped(finding))),
        Err(Unproposed::NoClockReading { written }) => {
            let reason = UnresolvedReason::no_longer_resolves(crate::clock::cannot_fill(&format!(
                "the route for `{}`",
                state.at
            )));
            let operation =
                Operation::new(OperationKind::move_document(state.at.clone(), written)).with_id(id);
            return Ok(Err(Unmade::ClockUnread(Box::new(
                UnresolvedOperation::new(operation, reason),
            ))));
        }
    };
    let candidates = spelled_candidates(proposals.iter().map(|proposal| {
        (
            norn_store::value_head(proposal.to.as_str()),
            proposal.rule.as_str(),
        )
    }));
    let Some((first, rest)) = proposals.split_first() else {
        return skipped(skip(row, SkipReason::NoDeclaredFix));
    };
    if rest.iter().any(|proposal| proposal.to != first.to) {
        return skipped(skip(row, SkipReason::Tie).with_candidates(candidates));
    }
    let to = &first.to;
    let refused = |note: String| {
        skip(row, SkipReason::JudgeWouldRefuse)
            .with_candidates(candidates.clone())
            .with_note(note)
    };
    let taken = |note: String| {
        skip(row, SkipReason::DestinationTaken)
            .with_candidates(candidates.clone())
            .with_note(note)
    };
    let (identity, folders) = match routing.vault.destination(to)? {
        Destination::Free { at, folders } => (at, folders),
        Destination::Taken => {
            return skipped(taken(format!(
                "something stands at `{to}`, or a document above it"
            )));
        }
        Destination::Closed(detail) => {
            return skipped(refused(format!(
                "no document can be moved to `{to}`: {detail}"
            )));
        }
    };
    if let Some(collision) = routing.claimed.collides(&identity, &folders) {
        return skipped(taken(format!("{collision} `{to}`")));
    }
    let moved = match document.judged(
        state,
        to,
        std::sync::Arc::clone(&state.bytes),
        Fixing {
            row,
            finding,
            candidates: &candidates,
        },
        &format!("at `{to}` the document still stands where its rules do not allow"),
    ) {
        Ok(moved) => moved,
        Err(skip) => {
            // A destination that brings in required fields is named as the
            // route's other refusals are: the one candidate it was.
            let skip = if skip.reason == SkipReason::BringsInRequiredFields {
                Box::new(skip.with_candidates(candidates))
            } else {
                skip
            };
            return Ok(Err(Unmade::Skipped(skip)));
        }
    };
    if !document.repairing.admitted.contains(&row.id) {
        return Ok(Ok(Route::Withheld {
            skip: Box::new(skip(row, SkipReason::JudgeWouldRefuse).with_candidates(candidates)),
            to: to.clone(),
        }));
    }
    routing.claimed.claim(identity, folders);
    let clocked = proposals.iter().any(|proposal| proposal.clocked);
    Ok(Ok(Route::Moved(Fix {
        operations: vec![OperationKind::move_document(state.at.clone(), to.clone())],
        cited: vec![cited(row, to, clocked)],
        state: moved,
    })))
}

/// Why no destination is proposed.
enum Unproposed {
    /// The finding is left alone, for the reason and with the data it names.
    Skipped(Box<norn_wire::SkippedFinding>),
    /// A route reading the clock, which gives no reading a route can fill:
    /// the destination as the route writes it.
    NoClockReading { written: DocumentPath },
}

/// The destination each of `rules` routes the document standing as `state`
/// composes it to, in rule name order, the clock read only for a route
/// reading it.
fn proposed(
    document: &Document<'_>,
    state: &State,
    row: &FindingRow,
    rules: &BTreeSet<String>,
) -> Result<Vec<Proposal>, Unproposed> {
    let repairing = document.repairing;
    let name = state
        .at
        .as_str()
        .rsplit('/')
        .next()
        .expect("a path has a last segment");
    let mut proposals = Vec::new();
    for rule_name in rules {
        let Some(rule) = document.schema().rule(rule_name) else {
            continue;
        };
        let Some(route) = rule.allowed_paths().and_then(|allowed| allowed.route()) else {
            continue;
        };
        let mut clocked = false;
        let mut work = RuleWork::default();
        let filled = rule.fill_route(
            state.at.as_str(),
            repairing.case,
            &mut || {
                clocked = true;
                repairing.clock.get()
            },
            &mut PathBindings::default(),
            &mut work,
        );
        count_rule_work(work);
        let Some(filled) = filled else {
            continue;
        };
        let unfillable = |why: String| {
            Unproposed::Skipped(Box::new(skip(row, SkipReason::JudgeWouldRefuse).with_note(
                format!(
                    "the rule `{rule_name}` routes `{}` by `{}` to no folder a document can \
                     be moved into: {why}",
                    state.at,
                    route.as_str(),
                ),
            )))
        };
        let folder = match filled {
            Ok(folder) => folder,
            Err(FillRefusal::AmbiguousCapture { bindings }) => {
                return Err(Unproposed::Skipped(Box::new(
                    skip(row, SkipReason::AmbiguousCapture).with_note(format!(
                        "the rule `{rule_name}` routes to `{}` from a capture its `match.path` \
                         binds several ways in `{}`: {} and {}",
                        route.as_str(),
                        state.at,
                        spelled(&bindings[0]),
                        spelled(&bindings[1]),
                    )),
                )));
            }
            Err(FillRefusal::NoClockReading(_)) => {
                return Err(Unproposed::NoClockReading {
                    written: in_folder(route.as_str(), name).expect(
                        "schema read refuses a route whose text, each token standing as plain \
                         text, is no folder path",
                    ),
                });
            }
            Err(FillRefusal::Unfillable(error)) => return Err(unfillable(error.to_string())),
        };
        let to = in_folder(&folder, name).map_err(|problem| {
            unfillable(format!("`{folder}{name}` is no document path: {problem}"))
        })?;
        proposals.push(Proposal {
            to,
            rule: rule_name.clone(),
            clocked,
        });
    }
    Ok(proposals)
}

/// The document named `name` in `folder`, a route's folder ending in `/`;
/// or why that is no document path.
///
/// Schema read refuses a route whose text, each token standing as plain
/// text, is no folder path, and a route's fill refuses a value that would
/// break its segment ([`Rule::fill_route`](norn_config::schema::Rule::fill_route)); the
/// whole path is judged here as well, since a value stands beside literal
/// text.
fn in_folder(folder: &str, name: &str) -> Result<DocumentPath, norn_wire::PathProblem> {
    let path = format!("{folder}{name}");
    match norn_wire::PathProblem::of_document(&path) {
        Some(problem) => Err(problem),
        None => Ok(DocumentPath::new(path).expect("a path the document grammar admits")),
    }
}

/// The citation of the misplaced finding `row` fixed by moving it to `to`,
/// noting where the destination is the repair's own time.
fn cited(row: &FindingRow, to: &DocumentPath, clocked: bool) -> CitedFinding {
    let cited = CitedFinding::new(row.id, Confidence::Declared);
    if clocked {
        cited.with_notes(vec![format!(
            "`{to}` is filled from a route reading the clock, so the destination is the \
             repair's time"
        )])
    } else {
        cited
    }
}
