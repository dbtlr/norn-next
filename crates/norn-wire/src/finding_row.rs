//! A finding as a row, with the bounded head of what it could not tell apart.
//!
//! [`finding`](crate::finding) holds the two closed lists a finding is filed
//! under — its kind and its severity. This module holds the row those lists
//! label: what stands at a path, what it is about, and, where it is about a
//! target that resolves to more than one document, the head of the documents
//! it resolves to.
//!
//! **The candidate list is a head, and the head is bounded here.** Five
//! candidates and a total, in the resolution ladder's deterministic order.
//! The bound is the vocabulary's rather than a renderer's, because a payload
//! bounded only where it is rendered is a payload the second renderer emits
//! unbounded; it holds at rest in the findings table for the same reason. What
//! makes the head a head is `candidates_total`, so a total below the head it
//! heads describes no vault and is refused where one is built.
//!
//! **A hint names the request that enumerates the rest.** The head answers
//! "which documents", not "all of them", and [`Hint`] carries the machine-
//! readable way to ask for all of them: the `find` that resolves the same
//! target. A client renders it as an offer rather than deriving a request of
//! its own, so the enumeration a person is pointed at is the one the answer
//! meant.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::document::{DocumentPath, Span};
use crate::finding::{FindingKind, Severity};
use crate::target::ResolutionTarget;

/// How many resolution candidates a finding carries.
///
/// The head is the first five in deterministic resolution-ladder order, and
/// `candidates_total` beside it is how many there were. The bound is wire
/// shape: the store holds its candidate head to the same number, and a
/// surface renders the head it was handed rather than choosing a bound of its
/// own.
pub const CANDIDATE_HEAD: usize = 5;

/// One document a target could have named.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[non_exhaustive]
pub struct Candidate {
    /// The document's path.
    pub path: DocumentPath,
    /// The minimal suffix that names this candidate and no other.
    pub suffix: String,
}

impl Candidate {
    /// The candidate at `path`, named apart by `suffix`.
    pub fn new(path: DocumentPath, suffix: impl Into<String>) -> Self {
        Candidate {
            path,
            suffix: suffix.into(),
        }
    }
}

/// A total that is smaller than the head it heads.
///
/// The total is what makes a bounded head a head, so a total below the number
/// of candidates kept describes no vault: there is no reading of it under
/// which the head is a head of anything.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TotalBelowHead {
    head: usize,
    total: u64,
}

impl TotalBelowHead {
    /// How many candidates the head kept.
    pub const fn head(&self) -> usize {
        self.head
    }

    /// The total that was claimed for it.
    pub const fn total(&self) -> u64 {
        self.total
    }
}

impl std::fmt::Display for TotalBelowHead {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "a candidate head of {} cannot be the head of {} candidates",
            self.head, self.total
        )
    }
}

impl std::error::Error for TotalBelowHead {}

/// The first [`CANDIDATE_HEAD`] of `candidates`, if `total` can be the total
/// they head.
///
/// The truncation and the check are one function so that the finding row and
/// the ambiguous-target refusal — the two shapes that carry a bounded head —
/// bound it one way.
pub(crate) fn bounded_head(
    candidates: impl IntoIterator<Item = Candidate>,
    total: u64,
) -> Result<Vec<Candidate>, TotalBelowHead> {
    let head: Vec<Candidate> = candidates.into_iter().take(CANDIDATE_HEAD).collect();
    if total < head.len() as u64 {
        return Err(TotalBelowHead {
            head: head.len(),
            total,
        });
    }
    Ok(head)
}

/// What a client can ask next to see the whole of what a bounded head heads.
///
/// On the wire a hint is an object tagged `hint`:
/// `{"hint":"resolves","target":"glossary"}`.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(tag = "hint", rename_all = "snake_case")]
#[non_exhaustive]
pub enum Hint {
    /// Every document the target resolves to is what a `find` resolving that
    /// same target answers.
    #[non_exhaustive]
    Resolves {
        /// The target to resolve.
        target: ResolutionTarget,
    },
}

impl Hint {
    /// A `find` resolving `target` enumerates the class.
    pub const fn resolves(target: ResolutionTarget) -> Self {
        Hint::Resolves { target }
    }
}

/// One finding, as the row a report pages.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[non_exhaustive]
pub struct FindingRow {
    /// The finding's identifier within its path.
    pub id: u64,
    /// The cause class it is filed under.
    pub kind: FindingKind,
    /// How urgently it is reported.
    pub severity: Severity,
    /// The path it stands at, whether or not a document is derived there.
    pub path: DocumentPath,
    /// What it is about inside that path — a resolution target, a tag, a field
    /// key — and `null` where it is about the whole of it.
    pub target: Option<String>,
    /// Where in the document body it stands, and `null` where it names no
    /// position.
    pub span: Option<Span>,
    /// The head of the documents the target could have named, in the
    /// resolution ladder's order, and empty for a finding that is not about
    /// resolution.
    pub candidates: Vec<Candidate>,
    /// How many documents the target could have named, which is what makes the
    /// candidates a head.
    pub candidates_total: u64,
    /// What to ask next to see the whole of the class, and `null` where there
    /// is no wider answer to ask for.
    pub hint: Option<Hint>,
    /// The finding in words, for a person reading a report.
    pub message: String,
    /// The write generation the finding was derived at.
    pub generation: u64,
}

impl FindingRow {
    /// The finding `id` of `kind` at `path`, or the reason its candidate head
    /// is no head.
    ///
    /// `candidates` is truncated to [`CANDIDATE_HEAD`] here, so a producer
    /// handing over more does not widen the row, and `candidates_total` is
    /// refused where it is below the head that was kept.
    #[allow(clippy::too_many_arguments)] // A finding row is the finding's own facts; grouping them would mint a shape nothing else holds.
    pub fn new(
        id: u64,
        kind: FindingKind,
        severity: Severity,
        path: DocumentPath,
        target: Option<String>,
        span: Option<Span>,
        candidates: impl IntoIterator<Item = Candidate>,
        candidates_total: u64,
        hint: Option<Hint>,
        message: impl Into<String>,
        generation: u64,
    ) -> Result<Self, TotalBelowHead> {
        Ok(FindingRow {
            id,
            kind,
            severity,
            path,
            target,
            span,
            candidates: bounded_head(candidates, candidates_total)?,
            candidates_total,
            hint,
            message: message.into(),
            generation,
        })
    }

    /// Whether the target names documents this row does not carry.
    pub fn is_truncated(&self) -> bool {
        (self.candidates.len() as u64) < self.candidates_total
    }
}
