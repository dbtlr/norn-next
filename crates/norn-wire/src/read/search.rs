//! `search`: ranked hits for a query, answered from the vault's ladder.
//!
//! **The wire carries the resolved rung set, never a preset.** The spellings a
//! person types — `lexical`, `semantic`, `hybrid` — and the `--no-<rung>`
//! subtraction that trims them are Layer 6 renderings: a surface expands them
//! into the rungs they name before it builds a request, so two surfaces cannot
//! expand one preset two ways and a preset arriving here would be a second
//! vocabulary for the one the ladder already has.
//!
//! **A search runs at least the lexical floor.** There is nothing an empty set
//! could mean short of answering nothing at all, so it is refused where a set
//! is built, where one is read, and in the schema a surface validates against
//! alike.
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
use serde::{Deserialize, Deserializer, Serialize, de::Error as _};

use crate::address::VaultAddress;
use crate::cursor::{Cursor, Page, Score};
use crate::document::{Column, DocumentPath, DocumentRow};
use crate::predicate::Predicate;
use crate::reading::Rung;

/// A ladder that runs no rung.
///
/// Every search runs at least the lexical floor, so a request naming no rung
/// names no search: there is nothing for the empty set to mean short of
/// answering nothing at all.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EmptyLadder;

impl fmt::Display for EmptyLadder {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a search runs at least the lexical floor")
    }
}

impl std::error::Error for EmptyLadder {}

/// The rungs a search is asked to run.
///
/// On the wire a rung set is a plain object holding the rungs themselves:
/// `{"rungs":["lexical","vector"]}`. The set is not empty: a set naming no
/// rung is refused rather than read.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[non_exhaustive]
pub struct RungSet {
    /// The rungs to run, in ladder order.
    pub rungs: BTreeSet<Rung>,
}

impl RungSet {
    /// The model-free floor alone, which is what every search runs at least.
    pub fn lexical() -> Self {
        RungSet {
            rungs: BTreeSet::from([Rung::Lexical]),
        }
    }

    /// The set `rungs` names, or the reason it names no search.
    pub fn of(rungs: impl IntoIterator<Item = Rung>) -> Result<Self, EmptyLadder> {
        let rungs: BTreeSet<Rung> = rungs.into_iter().collect();
        if rungs.is_empty() {
            return Err(EmptyLadder);
        }
        Ok(RungSet { rungs })
    }
}

impl JsonSchema for RungSet {
    fn schema_name() -> Cow<'static, str> {
        Cow::Borrowed("RungSet")
    }

    fn schema_id() -> Cow<'static, str> {
        Cow::Borrowed("norn_wire::RungSet")
    }

    /// The object a derive would describe, with the floor the reader keeps
    /// advertised as `minItems`. A derive over a `BTreeSet` says an array of
    /// rungs with no members at all, so a surface validating against it would
    /// pass a request this crate refuses to read.
    fn json_schema(generator: &mut SchemaGenerator) -> Schema {
        let rung = generator.subschema_for::<Rung>();
        json_schema!({
            "type": "object",
            "description": "The rungs a search is asked to run.",
            "properties": {
                "rungs": {
                    "type": "array",
                    "description": "The rungs to run, in ladder order. At least one: every search runs at least the lexical floor.",
                    "items": rung,
                    "minItems": 1,
                    "uniqueItems": true,
                },
            },
            "required": ["rungs"],
        })
    }
}

/// The rung set as it arrives, before it is checked for naming a search at
/// all. The field name is the set's, so the bytes a reader accepts are the
/// bytes a writer produces.
#[derive(Deserialize)]
struct RungSetFields {
    rungs: BTreeSet<Rung>,
}

impl<'de> Deserialize<'de> for RungSet {
    /// A set arrives as the rungs it names and is read back through the same
    /// grammar the constructor holds: a set naming no rung is no ladder, so it
    /// refuses the read rather than landing as a request that answers nothing.
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let fields = RungSetFields::deserialize(deserializer)?;
        RungSet::of(fields.rungs).map_err(D::Error::custom)
    }
}

/// One ranked hit: a document the ladder ranked, at the relevance it ranked
/// it with.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[non_exhaustive]
pub struct Hit {
    /// The document the hit is for.
    pub path: DocumentPath,
    /// How relevant the ladder judged it: higher is more relevant. Hits are
    /// ordered by score descending, then by path in byte order, and a
    /// request's `min_score` floors this same scale.
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

/// What `search` answers with: one page of ranked hits.
pub type SearchReport = Page<Hit>;

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
    /// case and diacritics as the tokenizer does. A query holding no word
    /// answers no hit and is reported as the unsatisfied part
    /// `query_names_no_word`.
    pub query: String,
    /// The conjunction a hit must also satisfy. Empty filters nothing. A
    /// `resolves` part answers which documents a target names, which is a
    /// `find`, so a search reports it as not applicable.
    pub predicates: Vec<Predicate>,
    /// The rungs to run. The lexical floor alone unless more are asked for.
    pub rungs: RungSet,
    /// The relevance a hit must reach, on the scale a hit's `score` is: a hit
    /// scored at or above it is answered. `null` returns every hit the ladder
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
    /// A `search` of `vault` for `query`, over the lexical floor, hydrating no
    /// document row.
    pub fn new(vault: VaultAddress, query: impl Into<String>) -> Self {
        SearchParams {
            vault,
            query: query.into(),
            predicates: Vec::new(),
            rungs: RungSet::lexical(),
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

    /// The request running `rungs`.
    #[must_use]
    pub fn with_rungs(mut self, rungs: RungSet) -> Self {
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
