//! A finding as a row, with the bounded head of what it could not tell apart.
//!
//! [`finding`](crate::finding) holds the two closed lists a finding is filed
//! under — its kind and its severity. This module holds the row those lists
//! label: what stands at a path, what it is about, and, where it is about a
//! target that resolves to more than one document, the head of the documents
//! it resolves to.
//!
//! **The candidate list is a head, and the head is one type.**
//! [`CandidateHead`] is a bounded head of candidates and a total, in the
//! resolution ladder's deterministic order, and it is what every carrier
//! holds: the finding row here, the link row whose target resolved, and the
//! ambiguous-target refusal. One type is what makes
//! the bound hold everywhere — a payload bounded only where it is rendered is
//! a payload the second renderer emits unbounded, and a bound stated twice is
//! a bound one of the two spellings will outgrow. [`CANDIDATE_HEAD`] is that
//! one spelling: the constructor truncates to it, the read path refuses a
//! wider head than it, and the hand-written schema advertises it as
//! `maxItems`, so a surface validating a head refuses what this crate refuses
//! rather than passing bytes no reader here accepts. It holds at rest in the
//! findings table for the same reason. What makes the head a head is its
//! total, so a total below the candidates it heads describes no vault and is
//! refused where one is built and where one is read alike.
//!
//! **A hint names the request that enumerates the rest.** The head answers
//! "which documents", not "all of them", and [`Hint`] carries the machine-
//! readable way to ask for all of them: the `find` that resolves the same
//! target. A client renders it as an offer rather than deriving a request of
//! its own, so the enumeration a person is pointed at is the one the answer
//! meant. A hint is minted from the finding's class key, read back as the
//! suffix address that opens it, so building one from a stored row resolves
//! nothing. An address holding `#` would read back as a target with an
//! anchor, which names another address, so a class spelled with one carries
//! no hint.
//!
//! **What a finding judged against the schema rules carries is bounded too.**
//! A rule finding cites the set of rules contributing to the constraint it
//! breaches by one number, [`FindingRow::rule_set`], which the response
//! carrying the row resolves once per set rather than once per row, so a
//! row's bytes do not grow with the rules it cites. The value it judged travels as a
//! [`ValueHead`]: the first [`VALUE_HEAD_BYTES`] of its text, its whole length
//! and its hash, so a row's bytes do not grow with the value either. The
//! combined expectation — the closed set, the limit, the paths — never rides
//! a finding; it is a pure function of the cited rules, which `describe`
//! reports.
//!
//! **The row's identity and its subject are two fields.** `id` is the
//! finding's identity in the findings pillar, minted vault-wide, and it is
//! what a `validate` page continues by after the kind and the path. `target`
//! is the finding's subject as written, which is not the
//! hint: the hint is a request a client can send, and the subject is the text
//! the document holds.

use std::borrow::Cow;

use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Deserializer, Serialize, de::Error as _};

use std::fmt;

use crate::document::{DocumentPath, Span, TotalBelowHead};
use crate::finding::{FindingKind, Severity};
use crate::plan::hash::ContentHash;
use crate::target::ResolutionTarget;

/// How many resolution candidates a row or a refusal carries.
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
/// `{"candidates":[…],"total":9}`. The candidates are the first of them in
/// the resolution ladder's deterministic order, bounded at the ceiling the
/// schema advertises, and the total beside them is never below the candidates
/// carried: a smaller total heads nothing, and the read refuses it. A head
/// longer than the advertised ceiling describes no vault and is refused the
/// same way.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[non_exhaustive]
pub struct CandidateHead {
    /// The documents the target could have named, in the resolution ladder's
    /// order, bounded at the ceiling the schema advertises, and empty where
    /// the subject is not a resolution.
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

impl JsonSchema for CandidateHead {
    fn schema_name() -> Cow<'static, str> {
        Cow::Borrowed("CandidateHead")
    }

    fn schema_id() -> Cow<'static, str> {
        Cow::Borrowed("norn_wire::CandidateHead")
    }

    /// The object a derive would describe, with the ceiling the reader keeps
    /// advertised as `maxItems`. A derive over a `Vec` says an array of any
    /// length at all, so a surface validating against it would pass a head
    /// this crate refuses to read, and the bound would be spelled in prose
    /// here rather than in the one number [`CANDIDATE_HEAD`] holds.
    fn json_schema(generator: &mut SchemaGenerator) -> Schema {
        let candidate = generator.subschema_for::<Candidate>();
        json_schema!({
            "type": "object",
            "description": "The bounded head of the documents a target could have named, with how many there were. The candidates are in the resolution ladder's deterministic order and stop at the ceiling this schema advertises, and the total beside them is never below the candidates carried: a smaller total heads nothing, and the read refuses it.",
            "properties": {
                "candidates": {
                    "type": "array",
                    "description": "The documents the target could have named, in the resolution ladder's order, and empty where the subject is not a resolution.",
                    "items": candidate,
                    "maxItems": CANDIDATE_HEAD,
                },
                "total": {
                    "type": "integer",
                    "format": "uint64",
                    "minimum": 0,
                    "description": "How many documents the target could have named, which is what makes the candidates a head.",
                },
            },
            "required": ["candidates", "total"],
        })
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

/// How many bytes of an offending value a finding carries.
///
/// The head is the value's text cut at the last character boundary at or
/// below this many bytes. The bound is wire shape: the store holds a value
/// head to the same number at rest, and a surface renders the head it was
/// handed rather than choosing a bound of its own.
pub const VALUE_HEAD_BYTES: usize = 256;

/// The offending value a finding judged, as its bounded head, its whole
/// length and its hash.
///
/// On the wire a head is a plain object:
/// `{"text":"someday","byte_length":7,"hash":"sha256:…"}`. The value is a
/// scalar or one list element as the document writes it — the same text a
/// field row holds for it — or, for a list or a map, its canonical JSON.
/// `text` is the value's first 256 bytes at most, cut at a character
/// boundary; `byte_length` is the whole value's length in bytes, so
/// a head whose text is shorter was cut; and `hash` is the SHA-256 of the
/// whole value's text, which tells two values apart that share their head.
/// A head that could not have been cut from a value of its length is refused
/// where it is built and where it is read alike: text past the bound, text
/// longer than the length it heads, a value within the bound carried other
/// than whole, or a longer value cut short of the bound by more than one
/// character.
///
/// **What the head alone decides, and what it does not.** Which character
/// follows a cut is not in the head, so a head of a value past the bound that
/// stops one to three bytes short of it is accepted: a character that wide
/// may follow it, and whether one does cannot be told from the head. The wire
/// hashes nothing, so `hash` is not verified here either; it is the writer's
/// declaration of the whole value's SHA-256, which the store takes over the
/// value it writes.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[non_exhaustive]
pub struct ValueHead {
    /// The value's text, at most 256 bytes of it, cut at a character
    /// boundary.
    text: String,
    /// How many bytes the whole value has, which is what makes the text a
    /// head.
    byte_length: u64,
    /// The SHA-256 of the whole value's text.
    hash: ContentHash,
}

impl ValueHead {
    /// The head of the value `full`, whose SHA-256 is `hash`: its first
    /// [`VALUE_HEAD_BYTES`] cut at the last character boundary at or below
    /// them, and its whole length.
    ///
    /// The wire hashes nothing ([`ContentHash`]), so the caller hands the hash
    /// of the very text it hands here; the store's finding write is that
    /// caller, and it hashes the value it is writing. A dormant carrier: that
    /// write takes a value only from a finding judging one, which rule
    /// judgment in derivation files (NORN-358) and nothing files yet.
    pub fn of(full: &str, hash: ContentHash) -> Self {
        let mut cut = full.len().min(VALUE_HEAD_BYTES);
        while !full.is_char_boundary(cut) {
            cut -= 1;
        }
        ValueHead {
            text: full[..cut].to_string(),
            byte_length: full.len() as u64,
            hash,
        }
    }

    /// The head `text` of a value of `byte_length` bytes hashing to `hash`,
    /// or the reason no value's head is that text.
    pub fn new(
        text: impl Into<String>,
        byte_length: u64,
        hash: ContentHash,
    ) -> Result<Self, IllegalValueHead> {
        let text = text.into();
        IllegalValueHead::check(&text, byte_length)?;
        Ok(ValueHead {
            text,
            byte_length,
            hash,
        })
    }

    /// The value's text, as far as the head goes.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// How many bytes the whole value has.
    pub const fn byte_length(&self) -> u64 {
        self.byte_length
    }

    /// The SHA-256 of the whole value's text.
    pub fn hash(&self) -> &ContentHash {
        &self.hash
    }

    /// Whether the value has bytes this head does not carry.
    pub fn is_truncated(&self) -> bool {
        (self.text.len() as u64) < self.byte_length
    }
}

/// A text that is no value's head at the length it claims.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IllegalValueHead {
    /// The text is longer than [`VALUE_HEAD_BYTES`].
    PastTheBound {
        /// The text's length in bytes.
        text: usize,
    },
    /// The text is longer than the value it heads.
    LongerThanTheValue {
        /// The text's length in bytes.
        text: usize,
        /// The length claimed for the whole value.
        byte_length: u64,
    },
    /// The value is within the bound and the text is shorter than it: a
    /// value of at most [`VALUE_HEAD_BYTES`] bytes is carried whole.
    NotWhole {
        /// The text's length in bytes.
        text: usize,
        /// The length claimed for the whole value.
        byte_length: u64,
    },
    /// The text was cut short of the bound by more than one character, which
    /// no cut at the last boundary at or below the bound leaves.
    CutShort {
        /// The text's length in bytes.
        text: usize,
        /// The length claimed for the whole value.
        byte_length: u64,
    },
}

impl IllegalValueHead {
    /// The longest a character is in UTF-8, which is how far short of the
    /// bound a cut at the last character boundary below it can stop.
    const WIDEST_CHARACTER: usize = 4;

    /// Whether `text` can be the head of a value of `byte_length` bytes.
    fn check(text: &str, byte_length: u64) -> Result<(), Self> {
        let length = text.len();
        if length > VALUE_HEAD_BYTES {
            return Err(IllegalValueHead::PastTheBound { text: length });
        }
        if length as u64 > byte_length {
            return Err(IllegalValueHead::LongerThanTheValue {
                text: length,
                byte_length,
            });
        }
        if (length as u64) == byte_length {
            return Ok(());
        }
        if byte_length <= VALUE_HEAD_BYTES as u64 {
            return Err(IllegalValueHead::NotWhole {
                text: length,
                byte_length,
            });
        }
        if length + Self::WIDEST_CHARACTER <= VALUE_HEAD_BYTES {
            return Err(IllegalValueHead::CutShort {
                text: length,
                byte_length,
            });
        }
        Ok(())
    }
}

impl fmt::Display for IllegalValueHead {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            IllegalValueHead::PastTheBound { text } => write!(
                formatter,
                "a value head holds at most {VALUE_HEAD_BYTES} bytes, and this one holds {text}"
            ),
            IllegalValueHead::LongerThanTheValue { text, byte_length } => write!(
                formatter,
                "a head of {text} bytes cannot head a value of {byte_length}"
            ),
            IllegalValueHead::NotWhole { text, byte_length } => write!(
                formatter,
                "a value of {byte_length} bytes is within {VALUE_HEAD_BYTES} bytes and \
                 carried whole, and this head holds {text}"
            ),
            IllegalValueHead::CutShort { text, byte_length } => write!(
                formatter,
                "a value of {byte_length} bytes is cut within one character of \
                 {VALUE_HEAD_BYTES} bytes, and this head stops at {text}"
            ),
        }
    }
}

impl std::error::Error for IllegalValueHead {}

impl JsonSchema for ValueHead {
    fn schema_name() -> Cow<'static, str> {
        Cow::Borrowed("ValueHead")
    }

    fn schema_id() -> Cow<'static, str> {
        Cow::Borrowed("norn_wire::ValueHead")
    }

    /// The object a derive would describe, with the bound the reader keeps
    /// advertised as the text's `maxLength`. A schema counts characters and
    /// the bound counts bytes; a character is at least one byte, so a text
    /// within the byte bound is within the same number of characters, and the
    /// advertised bound is the sound looser one. Whether the text is cut where
    /// its length says is the read's to refuse, as the description states.
    fn json_schema(generator: &mut SchemaGenerator) -> Schema {
        let hash = generator.subschema_for::<ContentHash>();
        json_schema!({
            "type": "object",
            "description": "The offending value a finding judged, as its bounded head, its whole length and its hash. A value within the bound is carried whole and a longer one is cut at the last character boundary at or below it; a head that could not have been cut from a value of its length is refused by the read.",
            "properties": {
                "text": {
                    "type": "string",
                    "description": "The value's text, at most the bound in bytes, cut at a character boundary.",
                    "maxLength": VALUE_HEAD_BYTES,
                },
                "byte_length": {
                    "type": "integer",
                    "format": "uint64",
                    "minimum": 0,
                    "description": "How many bytes the whole value has, which is what makes the text a head.",
                },
                "hash": hash,
            },
            "required": ["text", "byte_length", "hash"],
        })
    }
}

/// The value head as it arrives, before its text is checked against the
/// length it heads. The field names and order are the head's, so the bytes a
/// reader accepts are the bytes a writer produces.
#[derive(Deserialize)]
struct ValueHeadFields {
    text: String,
    byte_length: u64,
    hash: ContentHash,
}

impl<'de> Deserialize<'de> for ValueHead {
    /// A head arrives as its text, its length and its hash and is read back
    /// through the check the constructor holds, so a head no value could have
    /// been cut to refuses the read rather than landing as a payload no bound
    /// covers.
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let fields = ValueHeadFields::deserialize(deserializer)?;
        ValueHead::new(fields.text, fields.byte_length, fields.hash).map_err(D::Error::custom)
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
    /// The set of schema rules the finding cites, by identity, and `null` for
    /// a finding that cites no rule. A finding judged against the rules
    /// selecting its document cites every rule contributing to the constraint
    /// it breaches. Every response carrying finding rows — a validate page, a
    /// get's record or page of findings, a find's or a search's page — carries
    /// the sets its rows cite beside them, as the names of each set's rules,
    /// and the identity resolves only against those: it is where the store
    /// filed the set, so it is not stable across responses, and a schema pin
    /// files the sets again under new ones.
    pub rule_set: Option<u64>,
    /// The offending value the finding judged, as its bounded head, and `null`
    /// for a finding about no value — a missing field, a misplaced document, a
    /// conflict between rules over where a document may stand, and a
    /// conflict over a field the document does not hold. A conflict over a
    /// field the document holds carries that whole value, its canonical JSON
    /// for a list or a map.
    pub value: Option<ValueHead>,
}

impl FindingRow {
    /// The finding `id` of `kind` at `path`, over `head`, citing no rule and
    /// about no value.
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
            rule_set: None,
            value: None,
        }
    }

    /// The same row, citing the rule set `rule_set`.
    ///
    /// A dormant carrier: no finding cites a rule until rule judgment in
    /// derivation files one (NORN-358), so today only the store's own suite
    /// and this crate's reach it.
    #[must_use]
    pub const fn citing(mut self, rule_set: u64) -> Self {
        self.rule_set = Some(rule_set);
        self
    }

    /// The same row, about the offending value `value`.
    ///
    /// A dormant carrier for the same reason as [`FindingRow::citing`]: no
    /// finding judges a value until rule judgment in derivation files one
    /// (NORN-358).
    #[must_use]
    pub fn with_value(mut self, value: ValueHead) -> Self {
        self.value = Some(value);
        self
    }
}
