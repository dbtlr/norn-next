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
//! **An advisory is not an unsatisfied part.** An [`AnswerAdvisory`] says what
//! a part that *was* applied had to assume to answer: the part decided the
//! answer's order or membership, and the answer is complete, but a comparison
//! it made read something the vault's text leaves open. It sits beside the
//! unsatisfied parts rather than among them, so an answer carrying one is
//! still [`VaultAnswer::is_complete`], and on the answer rather than inside a
//! verb's report, so every verb that compares values says it one way.
//!
//! **A suggestion is advice, never a decision.** `did_you_mean` is drawn from
//! the vault's field universe by the handler that met the unknown key. How
//! near a candidate has to be, and how many are offered, are that handler's,
//! so a client renders the list and never re-derives it.

use schemars::JsonSchema;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::finding_row::CandidateHead;
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
    /// A count grouped by a field key the vault's field universe does not
    /// hold, so every document's member for that key is `null`.
    #[non_exhaustive]
    UnknownGroupKey {
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
    /// A match part named a full-text query the engine cannot parse, so no
    /// document satisfies that part.
    #[non_exhaustive]
    MalformedQuery {
        /// The query the request named.
        query: String,
        /// What about it does not parse, in the engine's words.
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
    /// The request asked for a block the document does not define.
    #[non_exhaustive]
    MissingBlock {
        /// The block identifier the request named, without its `^`.
        id: String,
    },
    /// The request carried a resolution part on a verb that answers no
    /// resolution. It is meaningful on `find` alone; `count`, `validate` and
    /// `search` report it here.
    #[non_exhaustive]
    ResolvesNotApplicable {
        /// The target the request asked to resolve.
        target: ResolutionTarget,
    },
    /// A search's query held no word, as the full-text index's tokenizer
    /// reads words, so no document is a hit.
    #[non_exhaustive]
    QueryNamesNoWord {
        /// The query the request named.
        query: String,
    },
    /// A links-to part named a target that resolves to more than one
    /// document, so it names no one document a link could reach and no
    /// document satisfies that part.
    #[non_exhaustive]
    LinksToAmbiguous {
        /// The target the request named.
        target: ResolutionTarget,
        /// The first of the documents it resolves to, in the resolution
        /// ladder's order, with how many there were.
        candidates: CandidateHead,
    },
    /// A links-to part named a target that resolves to no document, so no
    /// document satisfies that part.
    #[non_exhaustive]
    LinksToUnknown {
        /// The target the request named.
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

    /// A count grouped by `key`, which the field universe does not hold.
    pub fn unknown_group_key(key: impl Into<String>, did_you_mean: Vec<String>) -> Self {
        Unsatisfied::UnknownGroupKey {
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

    /// A match part named `query`, which the full-text engine cannot parse,
    /// for `problem`.
    pub fn malformed_query(query: impl Into<String>, problem: impl Into<String>) -> Self {
        Unsatisfied::MalformedQuery {
            query: query.into(),
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

    /// The document defines no block `id`.
    pub fn missing_block(id: impl Into<String>) -> Self {
        Unsatisfied::MissingBlock { id: id.into() }
    }

    /// This verb answers no resolution of `target`.
    pub const fn resolves_not_applicable(target: ResolutionTarget) -> Self {
        Unsatisfied::ResolvesNotApplicable { target }
    }

    /// A search named `query`, which holds no word.
    pub fn query_names_no_word(query: impl Into<String>) -> Self {
        Unsatisfied::QueryNamesNoWord {
            query: query.into(),
        }
    }

    /// A links-to part named `target`, which resolves to the documents
    /// `candidates` heads.
    pub const fn links_to_ambiguous(target: ResolutionTarget, candidates: CandidateHead) -> Self {
        Unsatisfied::LinksToAmbiguous { target, candidates }
    }

    /// A links-to part named `target`, which resolves to no document.
    pub const fn links_to_unknown(target: ResolutionTarget) -> Self {
        Unsatisfied::LinksToUnknown { target }
    }
}

/// Something a part of the request that was applied had to assume to answer.
///
/// On the wire an advisory is an object tagged `advisory`:
/// `{"advisory":"mixed_offset","key":"due","compared_by":"sort"}`.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(tag = "advisory", rename_all = "snake_case")]
#[non_exhaustive]
pub enum AnswerAdvisory {
    /// A comparison that decided the answer compared a date stating an offset
    /// from UTC against one stating none, and read the one stating none at
    /// offset zero. It speaks for every value the key holds in the vault the
    /// answer was read from, not only for the rows the answer carries: an
    /// order places each row among those values, a grouping decides which of
    /// them are one group, and a predicate compares its own value against
    /// each of them to decide which documents it keeps. So it can stand
    /// beside rows that all write one spelling. `find`, `count`, `validate`
    /// and `search` each raise it where their order, grouping or conjunction
    /// made such a comparison.
    #[non_exhaustive]
    MixedOffset {
        /// The date key whose values were compared.
        key: String,
        /// Where the request compared them.
        compared_by: ComparedBy,
    },
}

impl AnswerAdvisory {
    /// A comparison of `key`'s dates, made by `compared_by`, read an unstated
    /// offset as zero against a stated one.
    pub fn mixed_offset(key: impl Into<String>, compared_by: ComparedBy) -> Self {
        AnswerAdvisory::MixedOffset {
            key: key.into(),
            compared_by,
        }
    }
}

/// Where a request compared a key's values.
///
/// On the wire the flat string itself: `"sort"`, `"group"`, `"predicate"`.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ComparedBy {
    /// The order the rows are sorted in.
    Sort,
    /// A count's grouping, which makes one group of the values that compare
    /// equal and orders the groups. It is its own place rather than a sort
    /// because it decides which values are one tally, not only their order.
    Group,
    /// A predicate comparing the key's values against a value the request
    /// names: an equality, an inequality, a membership, or a before or after
    /// bound.
    Predicate,
}

/// What a read verb answers with: the reading it was taken under, the parts of
/// the request that were not applied, what the parts that were applied had to
/// assume, and the verb's own report.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(bound(serialize = "R: Serialize", deserialize = "R: DeserializeOwned"))]
#[non_exhaustive]
pub struct VaultAnswer<R: JsonSchema + Serialize + DeserializeOwned> {
    /// What the answer was answered under.
    pub reading: AnswerReading,
    /// The parts of the request that could not be applied. Empty when the
    /// whole request was.
    pub unsatisfied: Vec<Unsatisfied>,
    /// What the parts that were applied had to assume to answer. Empty when
    /// they assumed nothing.
    pub advisories: Vec<AnswerAdvisory>,
    /// The verb's own report.
    pub report: R,
}

impl<R: JsonSchema + Serialize + DeserializeOwned> VaultAnswer<R> {
    /// The answer `report`, taken under `reading`, with `unsatisfied` parts of
    /// the request not applied and no advisory.
    pub fn new(reading: AnswerReading, unsatisfied: Vec<Unsatisfied>, report: R) -> Self {
        VaultAnswer {
            reading,
            unsatisfied,
            advisories: Vec::new(),
            report,
        }
    }

    /// The same answer, carrying `advisories`.
    #[must_use]
    pub fn with_advisories(mut self, advisories: Vec<AnswerAdvisory>) -> Self {
        self.advisories = advisories;
        self
    }

    /// Whether every part of the request was applied. An advisory does not
    /// make an answer incomplete.
    pub fn is_complete(&self) -> bool {
        self.unsatisfied.is_empty()
    }
}
