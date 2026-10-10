//! Routes: the folder a placement rule's `allowed_paths` sends a misplaced
//! document to.
//!
//! **The candidates** for a selected `document/misplaced` finding are the
//! destinations the placement rules it cites — read where the document stands
//! — declare a route to: each rule's route filled
//! ([`Rule::fill_route`](norn_config::schema::Rule::fill_route), the one way a
//! route is bound to its path and filled) — a `{{path.<name>}}` from what the
//! rule's own `match.path` binds where the document stands, a clock token from
//! the plan's one reading — with the document's own file name after it. A rule
//! declaring no route proposes nothing. The candidates agree as one place,
//! compared as the root reads case, so on a root folding case `Tasks/a.md` and
//! `tasks/a.md` agree, written as the first rule in name order spells it. No
//! candidate is [`SkipReason::NoDeclaredFix`]; candidates naming different
//! places are [`SkipReason::Tie`], with each destination and the rule
//! proposing it; a route reading a capture its rule's `match.path` binds
//! several ways is [`SkipReason::AmbiguousCapture`], noting two of the
//! bindings; a route filling to no folder — a capture holding `:` or slugging
//! to nothing — or to no document path is [`SkipReason::JudgeWouldRefuse`],
//! noting why; and a route reading a clock that gives no reading is no skip:
//! its move is left unresolved, naming the clock, as a default's set is.
//!
//! **The destination is the document path a document made there takes**:
//! each folder above it that stands spelled as its parent lists it, as the
//! move's own planning spells it. A destination something stands at — a
//! document, a folder, or anything else — or beneath a document that stands is
//! [`SkipReason::DestinationTaken`]; a document that stands includes one a
//! route of the batch moves away, so a destination another route vacates is
//! taken too. A place the vault reads no document at is
//! [`SkipReason::JudgeWouldRefuse`].
//!
//! **The move is judged where it lands**, first, before any other fix of the
//! document: its bytes at the destination against its before-state, by the
//! applier's one judge, since `match.path`, `exclude.path` and
//! `allowed_paths` each judge the same bytes differently in another place. A
//! destination that brings in required fields is
//! [`SkipReason::BringsInRequiredFields`], naming each and its declared
//! default; one introducing any other violation, or where the document is
//! still misplaced, is [`SkipReason::JudgeWouldRefuse`]. A route passing its
//! own judgment is one `move_document`, cited at the declared level, a route
//! filled from the clock noting the destination is the repair's time.
//!
//! **Its link cascade is the one planner's**: every link the text layer can
//! respell is rewritten, and every other is advised on in the forecast with
//! the reason a Layer 4 move gives it, as any move's is; an unrespellable
//! link never holds a route back. The cascade rewrites other documents, which
//! the batch judges together ([`super::respells`]), and a destination is
//! claimed among the batch's routes ([`colliding`]).

use std::collections::{BTreeMap, BTreeSet};

use norn_config::schema::{FillRefusal, PathBindings, RuleWork};
use norn_fs::NormalizedPath;
use norn_wire::{
    CitedFinding, Confidence, DocumentPath, FindingRow, OperationKind, SkipReason,
    SkippedCandidates, SkippedFinding, UnresolvedReason,
};

use super::{Draft, Fixing, Made, Reckoned, State, skip, spelled, spelled_candidates};
use crate::evidence::count_rule_work;
use crate::planner::repair::{Destination, Reading};

/// What became of a document's route, judged alone.
pub(super) enum Routed {
    /// It passed its own judgment.
    Passed(Box<Passed>),
    /// It is skipped, for the reason and with the data it names.
    Skipped(Box<SkippedFinding>),
    /// It reads a clock that gives no reading: its move, as the route writes
    /// it, left unresolved.
    Unresolved(Box<Made>),
}

/// A route its document's own judgment admits.
pub(super) struct Passed {
    /// Where the document stands.
    pub(super) from: DocumentPath,
    /// Where it lands, spelled as a document made there is.
    pub(super) to: DocumentPath,
    /// The place it lands at, and each folder above it, by the root's
    /// identity rule.
    pub(super) place: NormalizedPath,
    pub(super) folders: Vec<NormalizedPath>,
    /// The move, cited for the misplaced finding.
    pub(super) moved: Made,
    /// The document where it lands, judged there.
    pub(super) state: State,
    /// The misplaced finding the route answers, and the destination
    /// candidates a skip of it carries.
    pub(super) row: FindingRow,
    pub(super) candidates: SkippedCandidates,
}

impl Passed {
    /// The skip of the misplaced finding the route answers, for `reason`,
    /// with its destination and `note`.
    pub(super) fn skipped(&self, reason: SkipReason, note: String) -> SkippedFinding {
        skip(&self.row, reason)
            .with_candidates(self.candidates.clone())
            .with_note(note)
    }
}

/// The route of the misplaced finding at `at` among `reckoned`'s rows, which
/// its document holds where it stands, judged alone against what `vault`
/// holds at its destination.
pub(super) fn route<R: Reading>(
    reckoned: &Reckoned<'_>,
    at: usize,
    vault: &R,
) -> Result<Routed, R::Error> {
    let row = &reckoned.rows[at];
    let state = &reckoned.start;
    let finding = reckoned.found[at]
        .as_ref()
        .expect("a route answers a finding the document holds");
    let rules = state
        .holding(finding)
        .expect("a route answers a finding the document holds")
        .rules
        .clone();
    let skipped = |finding| Ok(Routed::Skipped(Box::new(finding)));
    let proposals = match proposed(reckoned, row, &rules) {
        Ok(proposals) => proposals,
        Err(Unproposed::Skipped(finding)) => return Ok(Routed::Skipped(finding)),
        Err(Unproposed::NoClockReading { written }) => {
            return Ok(Routed::Unresolved(Box::new(Made::Unresolved {
                operation: OperationKind::move_document(state.at.clone(), written),
                reason: UnresolvedReason::no_longer_resolves(crate::clock::cannot_fill(&format!(
                    "the route for `{}`",
                    state.at
                ))),
            })));
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
    let place = |to: &DocumentPath| vault.place(to).ok_or_else(|| to.clone());
    if rest
        .iter()
        .any(|proposal| place(&proposal.to) != place(&first.to))
    {
        return skipped(skip(row, SkipReason::Tie).with_candidates(candidates));
    }
    let refused = |note: String| {
        skip(row, SkipReason::JudgeWouldRefuse)
            .with_candidates(candidates.clone())
            .with_note(note)
    };
    let (to, place, folders) = match vault.destination(&first.to)? {
        Destination::Free { at, place, folders } => (at, place, folders),
        Destination::Taken => {
            return skipped(
                skip(row, SkipReason::DestinationTaken)
                    .with_candidates(candidates.clone())
                    .with_note(format!(
                        "something stands at `{}`, or a document above it",
                        first.to
                    )),
            );
        }
        Destination::Closed(detail) => {
            return skipped(refused(format!(
                "no document can be moved to `{}`: {detail}",
                first.to
            )));
        }
    };
    let landed = match reckoned.document.judged(
        state,
        &to,
        std::sync::Arc::clone(&state.bytes),
        Fixing {
            row,
            finding,
            candidates: &candidates,
        },
        &format!("at `{to}` the document still stands where its rules do not allow"),
    ) {
        Ok(landed) => landed,
        // A destination that brings in required fields is named as the
        // route's other refusals are: by the one candidate it was.
        Err(skipped) if skipped.reason == SkipReason::BringsInRequiredFields => {
            return Ok(Routed::Skipped(Box::new(
                skipped.with_candidates(candidates),
            )));
        }
        Err(skipped) => return Ok(Routed::Skipped(skipped)),
    };
    let clocked = proposals.iter().any(|proposal| proposal.clocked);
    Ok(Routed::Passed(Box::new(Passed {
        from: state.at.clone(),
        moved: Made::Fixed {
            operations: vec![OperationKind::move_document(state.at.clone(), to.clone())],
            cited: vec![cited(row, &to, clocked)],
        },
        to,
        place,
        folders,
        state: landed,
        row: row.clone(),
        candidates,
    })))
}

/// One destination a rule's route proposes.
struct Proposal {
    to: DocumentPath,
    rule: String,
    clocked: bool,
}

/// Why no destination is proposed.
enum Unproposed {
    /// The finding is left alone, for the reason and with the data it names.
    Skipped(Box<SkippedFinding>),
    /// A route reading the clock, which gives no reading a route can fill:
    /// the destination as the route writes it.
    NoClockReading { written: DocumentPath },
}

/// The destination each of `rules` routes the document `reckoned` holds to,
/// standing where it stands, in rule name order, the clock read only for a
/// route reading it.
fn proposed(
    reckoned: &Reckoned<'_>,
    row: &FindingRow,
    rules: &BTreeSet<String>,
) -> Result<Vec<Proposal>, Unproposed> {
    let repairing = reckoned.document.repairing;
    let at = &reckoned.start.at;
    let name = at
        .as_str()
        .rsplit('/')
        .next()
        .expect("a path has a last segment");
    let mut proposals = Vec::new();
    let mut bindings = PathBindings::default();
    for rule_name in rules {
        let Some(rule) = reckoned.document.schema().rule(rule_name) else {
            continue;
        };
        let Some(route) = rule.allowed_paths().and_then(|allowed| allowed.route()) else {
            continue;
        };
        let mut clocked = false;
        let mut work = RuleWork::default();
        let filled = rule.fill_route(
            at.as_str(),
            repairing.case,
            &mut || {
                clocked = true;
                repairing.clock.get()
            },
            &mut bindings,
            &mut work,
        );
        count_rule_work(work);
        let Some(filled) = filled else {
            continue;
        };
        let unfillable = |why: String| {
            Unproposed::Skipped(Box::new(skip(row, SkipReason::JudgeWouldRefuse).with_note(
                format!(
                    "the rule `{rule_name}` routes `{at}` by `{}` to no folder a document can \
                     be moved into: {why}",
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
                         binds several ways in `{at}`: {} and {}",
                        route.as_str(),
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
/// break its segment
/// ([`Rule::fill_route`](norn_config::schema::Rule::fill_route)); the whole
/// path is judged here as well, since a value stands beside literal text.
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

/// The routes of `drafts` whose destination collides with an earlier route
/// of the batch the plan makes, each with its skip, by the draft's
/// position; `refused` names the routes skipped already, which claim no
/// place.
///
/// **A destination is claimed by the first route of the batch to it that the
/// plan makes, and so is every place above and beneath it**, in batch order:
/// a route to a place an earlier route moves a document to, to a folder
/// above one, or to a place beneath one, is [`SkipReason::DestinationTaken`],
/// since no two documents of a vault stand one inside the other. Places are
/// compared as the root reads case.
pub(in crate::planner::repair) fn colliding(
    drafts: &[Draft<'_>],
    refused: &BTreeMap<usize, SkippedFinding>,
) -> BTreeMap<usize, SkippedFinding> {
    let mut claimed = Claimed::default();
    let mut taken = BTreeMap::new();
    for (at, draft) in drafts.iter().enumerate() {
        let Some((passed, _)) = draft.passed() else {
            continue;
        };
        if refused.contains_key(&at) {
            continue;
        }
        match claimed.collides(&passed.place, &passed.folders) {
            Some(collision) => {
                taken.insert(
                    at,
                    passed.skipped(
                        SkipReason::DestinationTaken,
                        format!("{collision} `{}`", passed.to),
                    ),
                );
            }
            None => claimed.claim(passed.place.clone(), &passed.folders),
        }
    }
    taken
}

/// The destinations the batch's routes claimed so far: each document's
/// place, and every folder above one, by the identity the root's case rule
/// gives them.
#[derive(Default)]
struct Claimed {
    documents: BTreeSet<NormalizedPath>,
    folders: BTreeSet<NormalizedPath>,
}

impl Claimed {
    /// Why a document cannot be moved to `at`, beneath `folders`, beside the
    /// routes claimed so far; `None` where it can.
    fn collides(&self, at: &NormalizedPath, folders: &[NormalizedPath]) -> Option<&'static str> {
        if self.documents.contains(at) {
            Some("an earlier route of the batch moves a document to")
        } else if self.folders.contains(at) {
            Some("an earlier route of the batch moves a document beneath")
        } else if folders.iter().any(|folder| self.documents.contains(folder)) {
            Some("an earlier route of the batch moves a document above")
        } else {
            None
        }
    }

    /// Claim `at`, beneath `folders`, for a route of the batch.
    fn claim(&mut self, at: NormalizedPath, folders: &[NormalizedPath]) {
        self.documents.insert(at);
        self.folders.extend(folders.iter().cloned());
    }
}
