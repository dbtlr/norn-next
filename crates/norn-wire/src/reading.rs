//! What a vault answer was answered under.
//!
//! **Every vault answer carries its own reading.** A consumer judges an answer
//! without a second request: where the entry stood, which database the rows
//! came from, how far that database's writes had got, and — where the answer
//! is a ranking — which rungs ranked it, which models ran, and how far each
//! one's derived state trailed. A reading that had to be asked for separately
//! would be a reading taken at another instant from the answer it describes.
//! The first three are every answer's [`AnswerReading`]; the ladder is a
//! search's alone, so it is the [`SearchReport`](crate::SearchReport)'s
//! required [`LadderDeclaration`] and has that one home.
//!
//! **A ladder is declared, not inferred.** Which rungs ran is a fact about the
//! answer, and a rung that was enabled and did not run is not in the
//! declaration. Every search answer declares its ladder, the lexical floor
//! alone included — as `[lexical]`, repeatable — so a consumer never infers
//! the floor from an absence, and a verb that ranks nothing has no ladder to
//! declare. A declaration names its rungs in ladder order, each once, and
//! holds a retrieval rung, so the rung set it names is always a ladder. A
//! score is on the scale of the ladder that ranked it, and no ladder's scale
//! is normalized into another's. `repeatable` is what says the same request
//! against the same reading produces the same rows: a rung that ranks by a
//! request-time model is still repeatable, and one whose order depends on
//! state that drains underneath it is not.
//!
//! **Freshness belongs to a rung that holds state, and to no other.** The
//! lexical floor runs no model and has nothing to trail. A request-time rung
//! holds no derived state, so it names its model and reports no lag. A
//! stateful rung has both: the model it derived under, and how far that
//! derivation trails the store — in generations while it is draining the same
//! database, and as a rescan while its epoch is not the store's, because a
//! generation count across two databases compares nothing. A rung report is a
//! sum for that reason rather than a struct with optional halves: which halves
//! a rung has is decided by which rung it is, so a shape that admits a
//! request-time rung with a lag admits a report no answer can produce.
//!
//! **A rung's declaration order is ladder order, and `Ord` is derived from
//! it.** The floor is declared first and each rung after it is declared where
//! it stands on the ladder, so the derived ordering a [`RungSet`](crate::RungSet)
//! sorts by is the order the ladder runs in rather than an alphabet. A rung
//! added later is declared at its ladder position, not appended: appending it
//! would leave the derived order saying it runs last whatever the ladder does.
//!
//! **The engine section is what the host was delivered, not what a file
//! says.** It is the reading the status verb reports and the reading a vector
//! refusal is composed against, so "the vault has no engine" and "the vault's
//! engine section is malformed" are two answers rather than one.
//!
//! [`EngineSection`] is plain rather than `#[non_exhaustive]`, on the same
//! terms as [`FindingScope`](crate::FindingScope): every reading here composes
//! with an engine's own refusal to say what a client should do about it, and a
//! composer that has not decided what a new reading means should fail to
//! compile rather than fall into a default arm.
//!
//! The host retains the reading at config dispatch, beside the engine slot the
//! dispatch delivered to. No serving surface renders one yet: the `search` and
//! `status` handlers are what carry a retained section onto the wire.

use std::borrow::Cow;
use std::fmt;

use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Deserializer, Serialize, de::Error as _};

use crate::read::search::RungSet;
use crate::trust::TrustState;

/// A rung of a vault's search ladder, in the order the ladder runs them.
///
/// On the wire a rung is the flat string itself: `"lexical"`, `"vector"`,
/// `"expansion"`, `"rerank"`. A rung the ladder gains is spelled at the
/// position it runs at.
#[derive(
    Clone, Copy, Debug, Deserialize, Eq, JsonSchema, Ord, PartialEq, PartialOrd, Serialize,
)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Rung {
    /// The model-free floor: full-text matching over the store.
    Lexical,
    /// Nearest neighbours over the vault's derived vectors.
    Vector,
    /// Query expansion by a model at request time.
    Expansion,
    /// Re-ordering of the candidates by a model at request time.
    Rerank,
}

impl Rung {
    /// Whether this rung retrieves: finds candidates of its own, rather than
    /// expanding the query or re-ordering what another rung found. A ladder
    /// holds at least one retrieval rung.
    ///
    /// The match carries no wildcard, so a rung the ladder gains is classed
    /// here before it compiles.
    pub const fn retrieves(self) -> bool {
        match self {
            Rung::Lexical | Rung::Vector => true,
            Rung::Expansion | Rung::Rerank => false,
        }
    }
}

/// Every rung, in ladder order.
const EVERY_RUNG: [Rung; 4] = [Rung::Lexical, Rung::Vector, Rung::Expansion, Rung::Rerank];

/// The retrieval rungs, in ladder order: what a schema advertises a ladder
/// holds one of.
pub(crate) fn retrieval_rungs() -> Vec<Rung> {
    EVERY_RUNG
        .into_iter()
        .filter(|rung| rung.retrieves())
        .collect()
}

/// Which model ran, and which build of it.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[non_exhaustive]
pub struct ModelIdentity {
    /// The model's identifier.
    pub id: String,
    /// The version of that model this answer ran.
    pub version: String,
}

impl ModelIdentity {
    /// The model `id` at `version`.
    pub fn new(id: impl Into<String>, version: impl Into<String>) -> Self {
        ModelIdentity {
            id: id.into(),
            version: version.into(),
        }
    }
}

/// How far a rung's derived state trails the store it is derived from.
///
/// On the wire freshness is an object tagged `state`:
/// `{"state":"trailing","generations":3}`, `{"state":"rescanning"}`.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
#[non_exhaustive]
pub enum Freshness {
    /// The rung is draining the same database the answer was read from, and
    /// this is how far behind it is.
    #[non_exhaustive]
    Trailing {
        /// Write generations the rung's derived state is behind the store, the
        /// worse of what its documents and its tombstones have drained.
        generations: u64,
    },
    /// The rung's derived state is being built over a database that is not the
    /// one the answer was read from, so there is no generation count that
    /// compares the two.
    Rescanning {},
}

impl Freshness {
    /// A rung trailing the store by `generations`.
    pub const fn trailing(generations: u64) -> Self {
        Freshness::Trailing { generations }
    }

    /// A rung deriving over another database entirely.
    pub const fn rescanning() -> Self {
        Freshness::Rescanning {}
    }
}

/// What one rung of the ladder contributed to this answer.
///
/// On the wire a report is an object tagged `rung`:
/// `{"rung":"lexical"}`, `{"rung":"rerank","model":{"id":"…","version":"…"}}`.
/// Each rung carries exactly what that rung has: the floor runs no model and
/// holds no state, a request-time rung names its model, and the one stateful
/// rung names its model and how far its derived state trails.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(tag = "rung", rename_all = "snake_case")]
#[non_exhaustive]
pub enum RungReport {
    /// The model-free floor ran: full-text matching over the store.
    Lexical {},
    /// Nearest neighbours over the vault's derived vectors ran.
    #[non_exhaustive]
    Vector {
        /// The model the vectors were derived under.
        model: ModelIdentity,
        /// How far that derivation trails the store.
        freshness: Freshness,
    },
    /// Query expansion by a model at request time ran.
    #[non_exhaustive]
    Expansion {
        /// The model that expanded the query.
        model: ModelIdentity,
    },
    /// Re-ordering of the candidates by a model at request time ran.
    #[non_exhaustive]
    Rerank {
        /// The model that re-ordered the candidates.
        model: ModelIdentity,
    },
}

impl RungReport {
    /// The model-free floor ran.
    pub const fn lexical() -> Self {
        RungReport::Lexical {}
    }

    /// The vector rung ran, derived under `model`, at `freshness`.
    pub const fn vector(model: ModelIdentity, freshness: Freshness) -> Self {
        RungReport::Vector { model, freshness }
    }

    /// Query expansion ran, under `model`.
    pub const fn expansion(model: ModelIdentity) -> Self {
        RungReport::Expansion { model }
    }

    /// Re-ranking ran, under `model`.
    pub const fn rerank(model: ModelIdentity) -> Self {
        RungReport::Rerank { model }
    }

    /// Which rung this is a report of.
    ///
    /// The match carries no wildcard, so a report minted without a rung does
    /// not compile: [`Rung`] stays the flat selector a request names a rung
    /// by, and this is the one place the two lists are held together.
    pub const fn rung(&self) -> Rung {
        match self {
            RungReport::Lexical {} => Rung::Lexical,
            RungReport::Vector { .. } => Rung::Vector,
            RungReport::Expansion { .. } => Rung::Expansion,
            RungReport::Rerank { .. } => Rung::Rerank,
        }
    }
}

/// Why a list of rung reports declares no ladder.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum MalformedLadder {
    /// No rung it declares retrieves, so there was nothing to rank: the
    /// empty list, or enhancers alone.
    NoRetrievalRung,
    /// A rung is declared twice, or before a rung the ladder runs ahead of it.
    OutOfLadderOrder,
}

impl fmt::Display for MalformedLadder {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            MalformedLadder::NoRetrievalRung => {
                "a ladder declares at least one retrieval rung: lexical or vector"
            }
            MalformedLadder::OutOfLadderOrder => {
                "a ladder declares each rung once, in ladder order"
            }
        })
    }
}

impl std::error::Error for MalformedLadder {}

/// The ladder a search answer ran.
///
/// Every search answer declares one, the lexical floor alone included:
/// `{"rungs":[{"rung":"lexical"}],"repeatable":true}`. A hit's score, and a
/// request's floor on it, are on the scale this ladder ranks on, and no
/// ladder's scale is normalized into another's.
///
/// The rungs are reports rather than a repeated flat shape: each one carries
/// what that rung has and nothing it does not, so a request-time rung cannot
/// be spelled with a lag it never held. They are in ladder order, each rung
/// once, and at least one of them retrieves: a list that is not is refused
/// where a declaration is built and where one is read alike, so the rung set
/// a declaration names ([`LadderDeclaration::rung_set`]) is always a ladder.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[non_exhaustive]
pub struct LadderDeclaration {
    /// The rungs that ran, in the order they ran, each carrying what that
    /// rung contributed.
    rungs: Vec<RungReport>,
    /// Whether the same request against the same reading produces the same
    /// rows in the same order.
    repeatable: bool,
}

impl LadderDeclaration {
    /// A ladder that ran `rungs`, repeatable or not, or why `rungs` is no
    /// ladder.
    pub fn new(rungs: Vec<RungReport>, repeatable: bool) -> Result<Self, MalformedLadder> {
        if !rungs.iter().any(|report| report.rung().retrieves()) {
            return Err(MalformedLadder::NoRetrievalRung);
        }
        if !rungs.windows(2).all(|pair| pair[0].rung() < pair[1].rung()) {
            return Err(MalformedLadder::OutOfLadderOrder);
        }
        Ok(LadderDeclaration { rungs, repeatable })
    }

    /// The lexical floor alone, which runs no model and is repeatable.
    pub fn lexical() -> Self {
        LadderDeclaration {
            rungs: vec![RungReport::lexical()],
            repeatable: true,
        }
    }

    /// The rungs that ran, in ladder order.
    pub fn rungs(&self) -> &[RungReport] {
        &self.rungs
    }

    /// Whether the same request against the same reading produces the same
    /// rows in the same order.
    pub const fn repeatable(&self) -> bool {
        self.repeatable
    }

    /// The set of rungs this ladder ran: what a hit cursor minted from this
    /// answer names its ranking by.
    pub fn rung_set(&self) -> RungSet {
        RungSet::of(self.rungs.iter().map(RungReport::rung))
            .expect("a declared ladder holds a retrieval rung")
    }
}

/// A declaration as it arrives, before it is held to the ladder's grammar.
/// The field names are the declaration's, so the bytes a reader accepts are
/// the bytes a writer produces.
#[derive(Deserialize)]
struct LadderFields {
    rungs: Vec<RungReport>,
    repeatable: bool,
}

impl<'de> Deserialize<'de> for LadderDeclaration {
    /// A declaration is read back through the grammar the constructor holds:
    /// a list out of ladder order, or holding no retrieval rung, refuses the
    /// read.
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let fields = LadderFields::deserialize(deserializer)?;
        LadderDeclaration::new(fields.rungs, fields.repeatable).map_err(D::Error::custom)
    }
}

impl JsonSchema for LadderDeclaration {
    fn schema_name() -> Cow<'static, str> {
        Cow::Borrowed("LadderDeclaration")
    }

    fn schema_id() -> Cow<'static, str> {
        Cow::Borrowed("norn_wire::LadderDeclaration")
    }

    /// The object a derive would describe, with the rule the reader keeps
    /// advertised on its rungs: `contains` a report of a retrieval rung,
    /// which implies at least one. Ladder order is the reader's alone, since
    /// no JSON Schema keyword states an order over a tag.
    fn json_schema(generator: &mut SchemaGenerator) -> Schema {
        let report = generator.subschema_for::<RungReport>();
        let retrieval = retrieval_rungs();
        json_schema!({
            "type": "object",
            "description": "The ladder a search answer ran.",
            "properties": {
                "rungs": {
                    "type": "array",
                    "description": "The rungs that ran, in ladder order, each once, each carrying what that rung contributed. At least one is a retrieval rung: the lexical floor or vectors.",
                    "items": report,
                    "minItems": 1,
                    "contains": {
                        "type": "object",
                        "properties": { "rung": { "enum": retrieval } },
                        "required": ["rung"],
                    },
                },
                "repeatable": {
                    "type": "boolean",
                    "description": "Whether the same request against the same reading produces the same rows in the same order.",
                },
            },
            "required": ["rungs", "repeatable"],
        })
    }
}

/// What a vault answer was answered under.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[non_exhaustive]
pub struct AnswerReading {
    /// Where the vault entry stood when the answer was taken.
    pub trust: TrustState,
    /// The database the answer was read from.
    pub epoch: String,
    /// The last write generation committed to that database when the answer
    /// was established.
    pub generation: u64,
}

impl AnswerReading {
    /// The reading an answer was taken under.
    pub fn new(trust: TrustState, epoch: impl Into<String>, generation: u64) -> Self {
        AnswerReading {
            trust,
            epoch: epoch.into(),
            generation,
        }
    }
}

/// What the host was delivered as a vault's engine section.
///
/// On the wire a section is an object tagged `state`:
/// `{"state":"absent"}`, `{"state":"malformed","detail":"…"}`.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum EngineSection {
    /// The vault's config states no engine section.
    Absent {},
    /// The section is stated and turns the engine off.
    Disabled {},
    /// The section is stated and could not be read.
    #[non_exhaustive]
    Malformed {
        /// What could not be read, in words, for a person reading a message or
        /// a log. Clients never match on it.
        detail: String,
    },
    /// The section is stated and turns the engine on.
    Enabled {},
}

impl EngineSection {
    /// No engine section is stated.
    pub const fn absent() -> Self {
        EngineSection::Absent {}
    }

    /// The section turns the engine off.
    pub const fn disabled() -> Self {
        EngineSection::Disabled {}
    }

    /// The section could not be read, described by `detail`.
    pub fn malformed(detail: impl Into<String>) -> Self {
        EngineSection::Malformed {
            detail: detail.into(),
        }
    }

    /// The section turns the engine on.
    pub const fn enabled() -> Self {
        EngineSection::Enabled {}
    }
}
