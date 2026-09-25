//! `search`: ranked hits for a query, answered from the vault's ladder.
//!
//! **The wire carries a selection, never a preset and never a resolved set.**
//! A request selects its rungs one of two ways: the vault's enabled set less
//! the rungs it names, or exactly the rungs it names. The wire spells a bare
//! search, one that selects nothing, as the enabled selection with an empty
//! `without`. The host resolves a selection against the
//! vault's configuration when it answers, so a request is not a claim about a
//! configuration it cannot see. The spellings a person types — `lexical`,
//! `semantic`, `hybrid` — and the `--no-<rung>` subtraction that trims them are
//! surface renderings: `hybrid` is the enabled set whole, a named preset is an
//! exact set, and a subtraction is the enabled set less the rungs it names.
//! **A preset combined with a subtraction has no spelling here**: an exact
//! selection holds no subtraction and the enabled selection holds no set, and
//! each refuses a field it does not hold, so that combination is a surface's
//! argument error and never a request.
//!
//! [`RungSelection`] is plain rather than `#[non_exhaustive]`: the host
//! resolves every selection to the ladder it runs, and a resolver that has not
//! decided what a new selection resolves to should fail to compile rather than
//! fall into a default arm.
//!
//! **A ladder holds a retrieval rung.** A retrieval rung — the lexical floor,
//! or vectors — finds candidates of its own; expansion and re-ranking only
//! expand the query for, or re-order, what a retrieval rung found. A set
//! holding no retrieval rung, the empty set or expansion and re-ranking alone,
//! would answer nothing at all, so it is refused where a set is built, where
//! one is read, and in the schema a surface validates against alike, and so is
//! a subtraction that leaves out every retrieval rung. Which retrieval rungs a
//! vault enables and has an engine for is a fact the wire cannot see: a
//! selection that resolves to no retrieval rung there is the host's refusal,
//! described on [`RungSelection`].
//!
//! **A set names each rung once.** Its schema says `uniqueItems`, and the
//! reader holds it to that: a set naming a rung twice is refused rather than
//! folded into one, so a surface's validator and this reader accept the same
//! bytes.
//!
//! **A score is on the scale of the ladder that ran.** A hit's `score` and a
//! request's `min_score` are read on the scale the answer's ladder ranks on,
//! and no scale is normalized into another: a floor that suits one ladder says
//! nothing about another, which is why a hit cursor names its ladder and a
//! continuation under another ladder is refused.
//!
//! **`PartialEq` alone on [`Hit`].** A hit carries a relevance score, and a
//! score is a number two of which may be near without being one value.
//!
//! **`search` never sorts by a field.** A ranked answer is ordered by its
//! ranking, so there is no sort on [`SearchParams`] and no spelling of one: a
//! request that wants a field order wants `find`.

use std::borrow::Cow;
use std::collections::BTreeSet;
use std::fmt;

use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error as _};

use crate::address::VaultAddress;
use crate::cursor::{Cursor, Page, Score};
use crate::document::{Column, DocumentPath, DocumentRow};
use crate::predicate::Predicate;
use crate::reading::{LadderDeclaration, Rung, retrieval_rungs};

/// The most candidates one rung hands the ranking an answer is fused from:
/// its depth.
///
/// A rung finds its candidates up to this depth and no further, so an answer
/// ranks none beyond it, and one whose rung reached it is advised as having
/// done so ([`AnswerAdvisory::RungDepthReached`](crate::AnswerAdvisory::RungDepthReached));
/// one whose rung delivered fewer with more beyond what it read is advised
/// how many it delivered ([`AnswerAdvisory::RungShortOfDepth`](crate::AnswerAdvisory::RungShortOfDepth)).
/// It is the most rows one page holds, so one rung's candidates are one page
/// of that rung.
pub const RUNG_DEPTH: u32 = 1024;

/// A set of rungs, or what a subtraction leaves, holding no retrieval rung.
///
/// Every search runs at least one retrieval rung
/// ([`Rung::retrieves`]): expansion and re-ranking expand or re-order the
/// candidates a retrieval rung found, so a set holding none — the empty set, or
/// expansion and re-ranking alone — has nothing to answer from.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NoRetrievalRung;

impl fmt::Display for NoRetrievalRung {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a search runs at least one retrieval rung: lexical or vector")
    }
}

impl std::error::Error for NoRetrievalRung {}

/// Rungs as they arrive, each named once. A set naming a rung twice is refused
/// rather than folded, which is what the schema's `uniqueItems` says.
fn distinct_rungs<'de, D>(deserializer: D) -> Result<BTreeSet<Rung>, D::Error>
where
    D: Deserializer<'de>,
{
    let named = Vec::<Rung>::deserialize(deserializer)?;
    let mut rungs = BTreeSet::new();
    for rung in named {
        if !rungs.insert(rung) {
            return Err(D::Error::custom("a set of rungs names a rung twice"));
        }
    }
    Ok(rungs)
}

/// A set of rungs a search runs: one an exact selection names, or the one a
/// selection resolved to and a hit cursor names.
///
/// On the wire a rung set is the array of its rungs, in ladder order:
/// `["lexical","vector"]`. The set holds at least one retrieval rung, and
/// names each rung once: a set holding no retrieval rung, or read naming a
/// rung twice, is refused rather than read.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub struct RungSet {
    /// The rungs to run, in ladder order.
    rungs: BTreeSet<Rung>,
}

impl RungSet {
    /// The model-free floor alone.
    pub fn lexical() -> Self {
        RungSet {
            rungs: BTreeSet::from([Rung::Lexical]),
        }
    }

    /// The set `rungs` names, or the refusal that it holds no retrieval rung.
    /// A rung named twice is held once.
    pub fn of(rungs: impl IntoIterator<Item = Rung>) -> Result<Self, NoRetrievalRung> {
        let rungs: BTreeSet<Rung> = rungs.into_iter().collect();
        if !rungs.iter().any(|rung| rung.retrieves()) {
            return Err(NoRetrievalRung);
        }
        Ok(RungSet { rungs })
    }

    /// The rungs, in ladder order.
    pub const fn rungs(&self) -> &BTreeSet<Rung> {
        &self.rungs
    }
}

impl Serialize for RungSet {
    /// The rungs themselves, in ladder order.
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        self.rungs.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for RungSet {
    /// A set arrives as the rungs it names, each once, and is read back
    /// through the same grammar the constructor holds: a set holding no
    /// retrieval rung is no ladder, so it refuses the read rather than
    /// landing as a request that answers nothing.
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        RungSet::of(distinct_rungs(deserializer)?).map_err(D::Error::custom)
    }
}

impl JsonSchema for RungSet {
    fn schema_name() -> Cow<'static, str> {
        Cow::Borrowed("RungSet")
    }

    fn schema_id() -> Cow<'static, str> {
        Cow::Borrowed("norn_wire::RungSet")
    }

    /// The array a derive over a `BTreeSet` would describe, with the rule the
    /// reader keeps advertised: `contains` a retrieval rung, which implies at
    /// least one rung. A derive says an array of rungs with no rule at all,
    /// so a surface validating against it would pass a set this crate refuses
    /// to read.
    fn json_schema(generator: &mut SchemaGenerator) -> Schema {
        let rung = generator.subschema_for::<Rung>();
        json_schema!({
            "type": "array",
            "description": "A set of rungs, in ladder order, each once. It holds at least one retrieval rung: every search runs the lexical floor or vectors, and expansion or re-ranking expands or re-orders what a retrieval rung found.",
            "items": rung,
            "minItems": 1,
            "uniqueItems": true,
            "contains": { "enum": retrieval_rungs() },
        })
    }
}

/// The rungs an enabled selection leaves out of the vault's enabled set.
///
/// On the wire the array of those rungs, in ladder order: `["vector"]`, and
/// `[]` for none. It never leaves out every retrieval rung: that subtraction
/// selects no ladder whatever the vault enables, so it is refused where one is
/// built and where one is read alike, and a rung named twice is refused on
/// read.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub struct RungSubtraction {
    /// The rungs left out, in ladder order.
    rungs: BTreeSet<Rung>,
}

impl RungSubtraction {
    /// Leaving out no rung.
    pub const fn none() -> Self {
        RungSubtraction {
            rungs: BTreeSet::new(),
        }
    }

    /// Leaving out `rungs`, or the refusal that they are every retrieval rung.
    pub fn of(rungs: impl IntoIterator<Item = Rung>) -> Result<Self, NoRetrievalRung> {
        let rungs: BTreeSet<Rung> = rungs.into_iter().collect();
        if retrieval_rungs().iter().all(|rung| rungs.contains(rung)) {
            return Err(NoRetrievalRung);
        }
        Ok(RungSubtraction { rungs })
    }

    /// The rungs left out, in ladder order.
    pub const fn rungs(&self) -> &BTreeSet<Rung> {
        &self.rungs
    }
}

impl Serialize for RungSubtraction {
    /// The rungs left out, in ladder order.
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        self.rungs.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for RungSubtraction {
    /// A subtraction arrives as the rungs it names, each once, and is read
    /// back through the grammar the constructor holds.
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        RungSubtraction::of(distinct_rungs(deserializer)?).map_err(D::Error::custom)
    }
}

impl JsonSchema for RungSubtraction {
    fn schema_name() -> Cow<'static, str> {
        Cow::Borrowed("RungSubtraction")
    }

    fn schema_id() -> Cow<'static, str> {
        Cow::Borrowed("norn_wire::RungSubtraction")
    }

    /// An array of rungs, each once, that does not contain every retrieval
    /// rung: the rule the reader keeps, advertised so a surface validating a
    /// request refuses the subtraction this crate refuses to read.
    fn json_schema(generator: &mut SchemaGenerator) -> Schema {
        let rung = generator.subschema_for::<Rung>();
        let every_retrieval_rung: Vec<_> = retrieval_rungs()
            .into_iter()
            .map(|retrieval| json_schema!({ "contains": { "const": retrieval } }))
            .collect();
        json_schema!({
            "type": "array",
            "description": "The rungs to leave out of the vault's enabled set, in ladder order, each once. Never every retrieval rung: a search runs the lexical floor or vectors.",
            "items": rung,
            "uniqueItems": true,
            "not": { "allOf": every_retrieval_rung },
        })
    }
}

/// Which rungs a search asks for, which the host resolves against the vault's
/// configuration when it answers.
///
/// On the wire a selection is an object tagged `select`:
/// `{"select":"enabled","without":["vector"]}`,
/// `{"select":"exactly","rungs":["lexical"]}`. Each refuses a field the other
/// holds, so a selection that both names a set and subtracts from one has no
/// spelling.
///
/// **A selection that resolves to no retrieval rung is refused, never
/// answered empty.** Whether a retrieval rung is enabled, and whether an
/// engine stands for it, is the vault's configuration and the host's state,
/// which a request cannot see; so this type refuses only what no vault could
/// answer, and the host refuses the rest when it resolves. Where resolution
/// leaves no retrieval rung, the host refuses with the code of a retrieval
/// rung that failed it — `engine/not-enabled` or `engine/unavailable`, naming
/// that rung — under either selection.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(tag = "select", rename_all = "snake_case", deny_unknown_fields)]
pub enum RungSelection {
    /// The vault's enabled set less the rungs named. Naming none is the
    /// enabled set whole, which is how a search that selects nothing is
    /// spelled.
    ///
    /// A rung the enabled set holds and no engine stands for is left out and
    /// advised as `rung_skipped` while a retrieval rung remains to answer
    /// from. Where the rungs left
    /// leave no retrieval rung, the answer is refused with the code of the
    /// retrieval rung that failed it, never answered empty.
    #[non_exhaustive]
    Enabled {
        /// The rungs to leave out. A rung the vault has not enabled is left
        /// out already, and naming it changes nothing. It never names every
        /// retrieval rung.
        without: RungSubtraction,
    },
    /// Exactly the rungs named, whatever the vault enables. A rung the vault
    /// has not enabled, or that no engine stands for, is refused.
    #[non_exhaustive]
    Exactly {
        /// The rungs to run.
        rungs: RungSet,
    },
}

impl RungSelection {
    /// The vault's enabled set, whole.
    pub const fn enabled() -> Self {
        RungSelection::Enabled {
            without: RungSubtraction::none(),
        }
    }

    /// The vault's enabled set, less `without`, or the refusal that `without`
    /// names every retrieval rung.
    pub fn enabled_without(
        without: impl IntoIterator<Item = Rung>,
    ) -> Result<Self, NoRetrievalRung> {
        Ok(RungSelection::Enabled {
            without: RungSubtraction::of(without)?,
        })
    }

    /// Exactly `rungs`.
    pub const fn exactly(rungs: RungSet) -> Self {
        RungSelection::Exactly { rungs }
    }
}

/// One ranked hit: a document the ladder ranked, at the relevance it ranked
/// it with.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[non_exhaustive]
pub struct Hit {
    /// The document the hit is for.
    pub path: DocumentPath,
    /// How relevant the ladder judged it: higher is more relevant. It is on
    /// the scale of the ladder the answer ran, which the search report
    /// declares, and is not normalized across ladders. Hits are ordered by
    /// score descending, then by path in byte order, and a request's
    /// `min_score` floors this same scale.
    pub score: Score,
    /// The document's row, projected onto the columns the request asked for,
    /// and `null` where the request projected no column.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub document: Option<DocumentRow>,
}

impl Hit {
    /// The document at `path`, scored `score`, carrying no projected row.
    pub const fn new(path: DocumentPath, score: Score) -> Self {
        Hit {
            path,
            score,
            document: None,
        }
    }

    /// The hit carrying the projected row `document`.
    #[must_use]
    pub fn with_document(mut self, document: DocumentRow) -> Self {
        self.document = Some(document);
        self
    }
}

/// What `search` answers with: the ladder that ranked it, and one page of
/// ranked hits.
///
/// On the wire an object of the two:
/// `{"ladder":{"rungs":[{"rung":"lexical"}],"repeatable":true},"page":{"rows":[],"next":null,"moved":[]}}`.
/// The ladder is required, so a search answer always declares it, the lexical
/// floor alone included, and a consumer never infers a ladder from an absence.
/// The page is a field of its own rather than flattened beside the ladder, as
/// every report holding a page holds it, so the page's shape is the one every
/// paged verb answers with.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[non_exhaustive]
pub struct SearchReport {
    /// The ladder the answer ran. A hit's score is on its scale, and a hit
    /// cursor this page mints names its rung set.
    pub ladder: LadderDeclaration,
    /// The hits, most relevant first, and where the next page begins.
    pub page: Page<Hit>,
}

impl SearchReport {
    /// The page `page`, ranked by `ladder`.
    pub const fn new(ladder: LadderDeclaration, page: Page<Hit>) -> Self {
        SearchReport { ladder, page }
    }
}

/// What a `search` request carries.
///
/// A search answers the documents the ladder ranked for the query, in
/// relevance order.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[non_exhaustive]
pub struct SearchParams {
    /// The vault to answer from.
    pub vault: VaultAddress,
    /// The query to rank against, carried as written. It is plain text, and
    /// no character of it is query syntax.
    ///
    /// Terms are split at whitespace — every character Unicode reads as
    /// whitespace — and at NUL. A word is what the full-text index's tokenizer
    /// reads as one: a run of the characters its Unicode tables class as
    /// letters, digits or private use, or class not at all. Those tables
    /// predate recent Unicode versions, so a character assigned since, such as
    /// a newer emoji, reads as a word. A term holding no word, such as
    /// punctuation alone, is dropped. A hit is a
    /// document holding every other term, and a term holding several words,
    /// such as `foo-bar`, matches them adjacent and in order. Matching folds
    /// case and diacritics as the tokenizer does, and compares a word by its
    /// first 32768 bytes, in the index and in the query alike, so two words
    /// that share those bytes match each other. A query holding no word
    /// answers no hit and is reported as the unsatisfied part
    /// `query_names_no_word`.
    pub query: String,
    /// The conjunction a hit must also satisfy. Empty filters nothing. A
    /// `resolves` part answers which documents a target names, which is a
    /// `find`, so a search reports it as not applicable.
    pub predicates: Vec<Predicate>,
    /// Which rungs to run: the vault's enabled set less the rungs named, or
    /// exactly the rungs named. The enabled set whole unless the request
    /// selects otherwise.
    pub rungs: RungSelection,
    /// The relevance a hit must reach, on the scale a hit's `score` is, which
    /// is the scale of the ladder the selection resolves to: a hit scored at
    /// or above it is answered. A floor that suits one ladder says nothing
    /// about another. `null` returns every hit the ladder
    /// ranked.
    pub min_score: Option<Score>,
    /// The columns each hit's document row carries. Empty hydrates no row.
    pub columns: Vec<Column>,
    /// How many hits at most. `null` leaves the ceiling to the host.
    pub limit: Option<u32>,
    /// Where to continue from. `null` starts at the first hit.
    pub after: Option<Cursor>,
}

impl SearchParams {
    /// A `search` of `vault` for `query`, over the vault's enabled set whole,
    /// hydrating no document row.
    pub fn new(vault: VaultAddress, query: impl Into<String>) -> Self {
        SearchParams {
            vault,
            query: query.into(),
            predicates: Vec::new(),
            rungs: RungSelection::enabled(),
            min_score: None,
            columns: Vec::new(),
            limit: None,
            after: None,
        }
    }

    /// The request filtered by `predicates`.
    #[must_use]
    pub fn with_predicates(mut self, predicates: impl IntoIterator<Item = Predicate>) -> Self {
        self.predicates = predicates.into_iter().collect();
        self
    }

    /// The request selecting `rungs`.
    #[must_use]
    pub fn with_rungs(mut self, rungs: RungSelection) -> Self {
        self.rungs = rungs;
        self
    }

    /// The request floored at `min_score`.
    #[must_use]
    pub const fn with_min_score(mut self, min_score: Score) -> Self {
        self.min_score = Some(min_score);
        self
    }

    /// The request projecting `columns` onto each hit's document row.
    #[must_use]
    pub fn with_columns(mut self, columns: impl IntoIterator<Item = Column>) -> Self {
        self.columns = columns.into_iter().collect();
        self
    }

    /// The request bounded at `limit` hits.
    #[must_use]
    pub const fn with_limit(mut self, limit: u32) -> Self {
        self.limit = Some(limit);
        self
    }

    /// The request continuing from `after`.
    #[must_use]
    pub fn with_after(mut self, after: Cursor) -> Self {
        self.after = Some(after);
        self
    }
}
