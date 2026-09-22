//! A finding as a row, with the bounded head of what it could not tell apart.
//!
//! [`finding`](crate::finding) holds the two closed lists a finding is filed
//! under — its kind and its severity. This module holds the row those lists
//! label: what stands at a path, what it is about, and, where it is about a
//! target that resolves to more than one document, the head of the documents
//! it resolves to.
//!
//! **The candidate list is a head, and the head is one type.**
//! [`CandidateHead`] is five candidates and a total, in the resolution
//! ladder's deterministic order, and it is what both carriers hold: the
//! finding row here, and the ambiguous-target refusal. One type is what makes
//! the bound hold everywhere — a payload bounded only where it is rendered is
//! a payload the second renderer emits unbounded, and a bound stated twice is
//! a bound one of the two spellings will outgrow. It holds at rest in the
//! findings table for the same reason. What makes the head a head is its
//! total, so a total below the candidates it heads describes no vault and is
//! refused where one is built and where one is read alike.
//!
//! **A hint names the request that enumerates the rest.** The head answers
//! "which documents", not "all of them", and [`Hint`] carries the machine-
//! readable way to ask for all of them: the `find` that resolves the same
//! target. A client renders it as an offer rather than deriving a request of
//! its own, so the enumeration a person is pointed at is the one the answer
//! meant. A hint is minted from the finding's class key — an anchor-free
//! address that always parses — so building one from a stored row asks nothing
//! that can fail.
//!
//! **The row's identity and its subject are two fields.** `id` is the
//! finding's identity in the findings pillar, minted vault-wide, and the store
//! exposes it with the `validate` builder (NORN-229), which is what pages
//! these rows. `target` is the finding's subject as written, which is not the
//! hint: the hint is a request a client can send, and the subject is the text
//! the document holds.

use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize, de::Error as _};

use crate::document::{DocumentPath, Span, TotalBelowHead};
use crate::finding::{FindingKind, Severity};
use crate::target::ResolutionTarget;

/// How many resolution candidates a finding carries.
///
/// The head is the first five in deterministic resolution-ladder order, and
/// the total beside them is how many there were. The bound is wire shape: the
/// store holds its candidate head to the same number, and a surface renders
/// the head it was handed rather than choosing a bound of its own.
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

/// The bounded head of the documents a target could have named, with how many
/// there were.
///
/// On the wire a head is a plain object:
/// `{"candidates":[…],"total":9}`. The candidates are the first
/// [`CANDIDATE_HEAD`] in the resolution ladder's deterministic order, and
/// `total` beside them is how many there were. A head longer than the bound,
/// or a total below the candidates beside it, describes no vault and is
/// refused.
#[derive(Clone, Debug, Eq, JsonSchema, PartialEq, Serialize)]
#[non_exhaustive]
pub struct CandidateHead {
    /// The documents the target could have named, in the resolution ladder's
    /// order, at most five of them, and empty where the subject is not a
    /// resolution.
    candidates: Vec<Candidate>,
    /// How many documents the target could have named, which is what makes
    /// the candidates a head.
    total: u64,
}

impl CandidateHead {
    /// The first [`CANDIDATE_HEAD`] of `candidates`, out of `total` the target
    /// named, or the reason `total` heads nothing.
    ///
    /// `candidates` is truncated to the bound here, so a producer handing over
    /// more does not widen the head it is building. The read path refuses a
    /// longer one instead: bytes carrying a wider head are bytes this
    /// vocabulary never minted.
    pub fn new(
        candidates: impl IntoIterator<Item = Candidate>,
        total: u64,
    ) -> Result<Self, TotalBelowHead> {
        let candidates: Vec<Candidate> = candidates.into_iter().take(CANDIDATE_HEAD).collect();
        TotalBelowHead::check(candidates.len(), total)?;
        Ok(CandidateHead { candidates, total })
    }

    /// The documents the target could have named, as far as the head goes.
    pub fn candidates(&self) -> &[Candidate] {
        &self.candidates
    }

    /// How many documents the target could have named.
    pub const fn total(&self) -> u64 {
        self.total
    }

    /// Whether the target names documents this head does not carry.
    pub fn is_truncated(&self) -> bool {
        (self.candidates.len() as u64) < self.total
    }
}

/// The head as it arrives, before its length and its total are checked. The
/// field names and order are the head's, so the bytes a reader accepts are the
/// bytes a writer produces.
#[derive(Deserialize)]
struct CandidateHeadFields {
    candidates: Vec<Candidate>,
    total: u64,
}

impl<'de> Deserialize<'de> for CandidateHead {
    /// A head arrives as its candidates and its total and is read back through
    /// the bound the constructor holds: a head longer than
    /// [`CANDIDATE_HEAD`] is a head nothing here mints, and a total below the
    /// candidates beside it heads nothing, so either refuses the read rather
    /// than landing as a payload no bound covers.
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let fields = CandidateHeadFields::deserialize(deserializer)?;
        if fields.candidates.len() > CANDIDATE_HEAD {
            return Err(D::Error::custom(format!(
                "a candidate head holds at most {CANDIDATE_HEAD} candidates, and this one holds {}",
                fields.candidates.len()
            )));
        }
        TotalBelowHead::check(fields.candidates.len(), fields.total).map_err(D::Error::custom)?;
        Ok(CandidateHead {
            candidates: fields.candidates,
            total: fields.total,
        })
    }
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
    /// The finding's identity in the vault's findings, which is what a
    /// continuation stops at and what a later report names the same finding
    /// by.
    pub id: u64,
    /// The cause class it is filed under.
    pub kind: FindingKind,
    /// How urgently it is reported.
    pub severity: Severity,
    /// The path it stands at, whether or not a document is derived there.
    pub path: DocumentPath,
    /// What it is about inside that path, as the document writes it — a
    /// resolution target, a tag, a field key — and `null` where it is about
    /// the whole of it.
    pub target: Option<String>,
    /// Where in the document body it stands, and `null` where it names no
    /// position.
    pub span: Option<Span>,
    /// The documents the target could have named, and how many there were.
    /// Empty, out of none, for a finding that is not about resolution.
    pub head: CandidateHead,
    /// What to ask next to see the whole of the class, and `null` where there
    /// is no wider answer to ask for.
    pub hint: Option<Hint>,
    /// The finding in words, for a person reading a report.
    pub message: String,
    /// The write generation the finding was derived at.
    pub generation: u64,
}

impl FindingRow {
    /// The finding `id` of `kind` at `path`, over `head`.
    #[allow(clippy::too_many_arguments)] // A finding row is the finding's own facts; grouping them would mint a shape nothing else holds.
    pub fn new(
        id: u64,
        kind: FindingKind,
        severity: Severity,
        path: DocumentPath,
        target: Option<String>,
        span: Option<Span>,
        head: CandidateHead,
        hint: Option<Hint>,
        message: impl Into<String>,
        generation: u64,
    ) -> Self {
        FindingRow {
            id,
            kind,
            severity,
            path,
            target,
            span,
            head,
            hint,
            message: message.into(),
            generation,
        }
    }
}
