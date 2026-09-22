//! What a read verb answers with.
//!
//! **A read has three exits, and they are three shapes.** A [`VaultAnswer`]
//! with no unsatisfied parts is the whole answer the request asked for. A
//! `VaultAnswer` carrying unsatisfied parts is an answer to the part of the
//! request that could be honoured, saying plainly which parts were not. An
//! [`ErrorEnvelope`](crate::ErrorEnvelope) is no answer at all. A read verb's
//! result type is therefore `Result<VaultAnswer<R>, ErrorEnvelope>`, and the
//! middle exit lives inside the success rather than beside it.
//!
//! **An unsatisfied part is not a refusal.** A sort key the vault has never
//! held, a glob nothing can match, a section no document carries — each of
//! them makes one part of the request unanswerable while the rest stays
//! answerable, and a request that named one gets the rows it earned plus the
//! reason the rest were not applied. Refusing the whole request would make a
//! typo in one flag indistinguishable from a vault that cannot be read.
//!
//! **A suggestion is advice, never a decision.** `did_you_mean` is drawn from
//! the vault's field universe by the handler that met the unknown key. How
//! near a candidate has to be, and how many are offered, are that handler's,
//! so a client renders the list and never re-derives it.

use schemars::JsonSchema;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::reading::AnswerReading;
use crate::target::ResolutionTarget;

/// One part of a request that could not be applied to the answer.
///
/// On the wire a part is an object tagged `part`:
/// `{"part":"unknown_sort_key","key":"due","did_you_mean":["date"]}`.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(tag = "part", rename_all = "snake_case")]
#[non_exhaustive]
pub enum Unsatisfied {
    /// The order named a key the vault's field universe does not hold, so the
    /// rows are in the verb's default order.
    #[non_exhaustive]
    UnknownSortKey {
        /// The key the request named.
        key: String,
        /// Keys from the vault's field universe that are near it.
        did_you_mean: Vec<String>,
    },
    /// The projection named a key the vault's field universe does not hold, so
    /// the rows carry nothing under it.
    #[non_exhaustive]
    UnknownProjectionKey {
        /// The key the request named.
        key: String,
        /// Keys from the vault's field universe that are near it.
        did_you_mean: Vec<String>,
    },
    /// A predicate named a key the vault's field universe does not hold, so
    /// that part filtered nothing.
    #[non_exhaustive]
    UnknownPredicateKey {
        /// The key the request named.
        key: String,
        /// Keys from the vault's field universe that are near it.
        did_you_mean: Vec<String>,
    },
    /// A path part named a directory rather than a pattern. The grammar
    /// matches globs, so a bare directory matches the directory alone.
    #[non_exhaustive]
    BareDirectory {
        /// The path the request named.
        path: String,
    },
    /// A path part named a glob that does not parse.
    #[non_exhaustive]
    MalformedGlob {
        /// The glob the request named.
        glob: String,
        /// What about it does not parse, in words.
        problem: String,
    },
    /// A path part named a path no document in this vault can have.
    #[non_exhaustive]
    ImpossiblePath {
        /// The path the request named.
        path: String,
    },
    /// The request asked for a section the document does not carry.
    #[non_exhaustive]
    MissingSection {
        /// The section the request named.
        section: String,
    },
    /// The request carried a resolution part on a verb that answers no
    /// resolution. It is meaningful on `find` alone; `count` and `validate`
    /// report it here.
    #[non_exhaustive]
    ResolvesNotApplicable {
        /// The target the request asked to resolve.
        target: ResolutionTarget,
    },
}

impl Unsatisfied {
    /// The order named `key`, which the field universe does not hold.
    pub fn unknown_sort_key(key: impl Into<String>, did_you_mean: Vec<String>) -> Self {
        Unsatisfied::UnknownSortKey {
            key: key.into(),
            did_you_mean,
        }
    }

    /// The projection named `key`, which the field universe does not hold.
    pub fn unknown_projection_key(key: impl Into<String>, did_you_mean: Vec<String>) -> Self {
        Unsatisfied::UnknownProjectionKey {
            key: key.into(),
            did_you_mean,
        }
    }

    /// A predicate named `key`, which the field universe does not hold.
    pub fn unknown_predicate_key(key: impl Into<String>, did_you_mean: Vec<String>) -> Self {
        Unsatisfied::UnknownPredicateKey {
            key: key.into(),
            did_you_mean,
        }
    }

    /// A path part named the directory `path` rather than a pattern.
    pub fn bare_directory(path: impl Into<String>) -> Self {
        Unsatisfied::BareDirectory { path: path.into() }
    }

    /// A path part named `glob`, which does not parse, for `problem`.
    pub fn malformed_glob(glob: impl Into<String>, problem: impl Into<String>) -> Self {
        Unsatisfied::MalformedGlob {
            glob: glob.into(),
            problem: problem.into(),
        }
    }

    /// A path part named `path`, which no document here can have.
    pub fn impossible_path(path: impl Into<String>) -> Self {
        Unsatisfied::ImpossiblePath { path: path.into() }
    }

    /// The document carries no `section`.
    pub fn missing_section(section: impl Into<String>) -> Self {
        Unsatisfied::MissingSection {
            section: section.into(),
        }
    }

    /// This verb answers no resolution of `target`.
    pub const fn resolves_not_applicable(target: ResolutionTarget) -> Self {
        Unsatisfied::ResolvesNotApplicable { target }
    }
}

/// What a read verb answers with: the reading it was taken under, the parts of
/// the request that were not applied, and the verb's own report.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(bound(serialize = "R: Serialize", deserialize = "R: DeserializeOwned"))]
#[non_exhaustive]
pub struct VaultAnswer<R: JsonSchema + Serialize + DeserializeOwned> {
    /// What the answer was answered under.
    pub reading: AnswerReading,
    /// The parts of the request that could not be applied. Empty when the
    /// whole request was.
    pub unsatisfied: Vec<Unsatisfied>,
    /// The verb's own report.
    pub report: R,
}

impl<R: JsonSchema + Serialize + DeserializeOwned> VaultAnswer<R> {
    /// The answer `report`, taken under `reading`, with `unsatisfied` parts of
    /// the request not applied.
    pub fn new(reading: AnswerReading, unsatisfied: Vec<Unsatisfied>, report: R) -> Self {
        VaultAnswer {
            reading,
            unsatisfied,
            report,
        }
    }

    /// Whether every part of the request was applied.
    pub fn is_complete(&self) -> bool {
        self.unsatisfied.is_empty()
    }
}
