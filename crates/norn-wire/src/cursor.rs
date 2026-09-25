//! Where a page stopped, and what has moved under it since.
//!
//! **A cursor is a typed envelope rendered as one opaque string.** The fields
//! are a wire type like any other — they derive their serialization and their
//! schema — and the wrapping is what makes the value opaque: URL-safe base64
//! of the canonical JSON of those fields. A client is handed the string,
//! passes it back unchanged, and branches on nothing inside it. That is the
//! whole of the contract, and it is why the shape may grow a field without a
//! client learning anything.
//!
//! **One position has one spelling, and the read path holds it to that.** The
//! fields are parsed and then re-encoded, and a string that is not byte for
//! byte that re-encoding names no position: a spelling that carries a field
//! the fields do not hold, writes them in another order, or leaves an optional
//! one out is a string this crate never minted. Canonicality is what makes the
//! opaque string comparable as a value rather than as a structure a reader
//! would have to normalize first.
//!
//! **Opaque is not encrypted, and it is not a promise of privacy.** Anyone may
//! decode one; what the rendering buys is that nobody *builds* one. A position
//! spelled by a client is a position no answer minted, and the parts a cursor
//! carries — the establishment it was taken under, and the key its order
//! stopped at — are exactly the parts a continuation is judged against.
//!
//! **The key carries the order's parts, not an offset.** Paging by offset
//! re-reads rows under a moving corpus and skips or repeats them; a key is the
//! tuple the order sorts by, so the next page starts after the last row
//! whatever was written in between. One variant per paged row type, each
//! carrying its own total order, because two row types with one key shape
//! would be two orders sharing a spelling.
//!
//! **Movement is reported, never hidden.** A continuation against a changed
//! establishment still answers: what moved rides back with the page so a
//! consumer can say how the ground shifted. Four rules decide a continuation,
//! read against the snapshot the cursor carries:
//!
//! - The epoch differs: the derived state was rebuilt under the vault, and
//!   `epoch` is reported.
//! - The epoch is the same and the generation differs *in either direction*:
//!   `generation` is reported. Writes landing after a cursor was minted move
//!   it forward; a generation that moved backwards inside one epoch is a
//!   database that is not the one the cursor named, and hiding that is worse
//!   than reporting it.
//! - The cursor carries a sidecar revision, and either the epoch differs or
//!   the snapshot's revision differs: `sidecar_revision` is reported. The
//!   revision is epoch-qualified, so it compares nothing across two epochs and
//!   is reported moved wherever the epoch moved.
//! - The cursor carries a schema fingerprint and the snapshot's is absent or
//!   different: the continuation is refused as an order that changed.
//!
//! The report is in one fixed order — epoch, generation, sidecar revision — so
//! a consumer reads it rather than sorting it.
//!
//! **The one thing that is refused is a change of *order*.** The rows a key
//! names are in a sequence that no longer exists, so the page is refused
//! rather than answered from a position that means something else. A cursor
//! minted under no fingerprint was ordered rawly, and a raw order does not
//! change with the schema, so these rules never refuse such a cursor; an
//! answer that also judges a cursor against the order its request names
//! refuses it where that order is typed ([`CursorOrderChanged`]). A document
//! key names the sort key and direction its page was read in, so such an
//! answer refuses a document cursor continued in another key or direction
//! too: the fingerprint says which schema an order is taken under, and the key
//! says which order it is.
//!
//! **Two asymmetries follow from those rules.** A cursor minted without a
//! sidecar revision and continued where a sidecar now answers reports nothing
//! about it: its position was taken without one, so there is no revision it
//! moved from. A typed cursor continued where no fingerprint stands refuses:
//! the sequence its key names a position in is a schema's, and an
//! establishment reading no schema is not walking that sequence.

use std::borrow::Cow;
use std::fmt;

use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error as _};

use crate::base64url;
use crate::finding::FindingKind;
use crate::read::find::Sort;
use crate::read::get::CollectionSelector;

/// What a facet row is a facet of.
///
/// On the wire a kind is the flat string itself: `"declared_field"`,
/// `"folder"`.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum FacetKind {
    /// A field the vault's schema declares.
    DeclaredField,
    /// A field the vault's documents carry, declared or not.
    ObservedField,
    /// A tag the vault's schema declares.
    DeclaredTag,
    /// A folder the vault's schema declares.
    Folder,
    /// A path rule the vault's schema states.
    PathRule,
    /// A pattern the vault's tag facet admits beyond its literal names.
    TagPattern,
    /// What the vault says about a tag its facet does not admit.
    UndeclaredTags,
}

impl FacetKind {
    /// Every kind the vocabulary holds, in declaration order.
    pub const ALL: [FacetKind; 7] = [
        FacetKind::DeclaredField,
        FacetKind::ObservedField,
        FacetKind::DeclaredTag,
        FacetKind::Folder,
        FacetKind::PathRule,
        FacetKind::TagPattern,
        FacetKind::UndeclaredTags,
    ];

    /// Every kind, in the byte order of its code, which is the order a page
    /// of facets reads the kinds in: every facet of one kind before any of the
    /// next. It is the order a finding cursor reads its kinds in too, so every
    /// cursor that names a kind orders kinds one way.
    pub fn in_code_order() -> [FacetKind; 7] {
        let mut kinds = Self::ALL;
        kinds.sort_unstable_by_key(|kind| kind.as_str());
        kinds
    }

    /// The kind as the string it is on the wire.
    pub const fn as_str(&self) -> &'static str {
        match self {
            FacetKind::DeclaredField => "declared_field",
            FacetKind::ObservedField => "observed_field",
            FacetKind::DeclaredTag => "declared_tag",
            FacetKind::Folder => "folder",
            FacetKind::PathRule => "path_rule",
            FacetKind::TagPattern => "tag_pattern",
            FacetKind::UndeclaredTags => "undeclared_tags",
        }
    }
}

/// A number that is no relevance score.
///
/// A score orders the ranked rows a cursor continues, so every value one holds
/// is a number two rows can be compared by. Infinity and a value that is not a
/// number are neither comparable nor spellable as JSON, and are refused here
/// instead.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NonFiniteScore;

impl fmt::Display for NonFiniteScore {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a relevance score is a finite number")
    }
}

impl std::error::Error for NonFiniteScore {}

/// A ranked hit's relevance, as the number the order sorts by.
///
/// On the wire a score is that number itself: `0.5`. It is finite: a score
/// that is infinite, or is not a number at all, has no spelling here and is
/// refused when one is built and when one is read alike.
#[derive(Clone, Copy, Debug, JsonSchema, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct Score(f64);

impl Score {
    /// `score` as a relevance score, if it is finite.
    ///
    /// This is the read path as well as the constructor, so a score that
    /// crossed the seam is a score that parsed: there is no representation of
    /// a non-finite one on either side of it.
    pub fn new(score: f64) -> Result<Score, NonFiniteScore> {
        if score.is_finite() {
            Ok(Score(score))
        } else {
            Err(NonFiniteScore)
        }
    }

    /// The score as the number it is, which is finite by construction.
    pub const fn get(self) -> f64 {
        self.0
    }
}

impl fmt::Display for Score {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, formatter)
    }
}

impl<'de> Deserialize<'de> for Score {
    /// A score arrives as a number and is read back through the same grammar
    /// the constructor holds: a number that is not finite is no score, so it
    /// refuses the read rather than landing as a value nothing can order.
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let score = f64::deserialize(deserializer)?;
        Score::new(score).map_err(D::Error::custom)
    }
}

/// Where one page of rows stopped, as the parts that row's order sorts by.
///
/// On the wire a key is an object tagged `row`:
/// `{"row":"document","order":{"key":{"by":"field","key":"due"},"direction":"ascending"},"sort":"2026-01-01","path":"notes/a.md"}`.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(tag = "row", rename_all = "snake_case")]
#[non_exhaustive]
pub enum CursorKey {
    /// A document row: the order the page was read in, then the sort field's
    /// value, then the path.
    #[non_exhaustive]
    Document {
        /// The order the page was read in: the sort key and direction the
        /// answer ordered its rows by. That is the request's order, except
        /// where the answer read the rows in another one — a sort key the
        /// vault knows nothing of orders by path, ascending — and then it is
        /// the order the rows were actually read in. The value and the path
        /// are a position in this order and in no other.
        order: Sort,
        /// The sort field's value as its string sort key, and `null` for a
        /// document that does not carry the sort field. A document missing it
        /// orders before every document that has it under an ascending sort,
        /// and after every one of them under a descending sort.
        sort: Option<String>,
        /// The document's path.
        path: String,
    },
    /// A ranked hit: relevance descending, then the path.
    #[non_exhaustive]
    Hit {
        /// The hit's relevance score.
        score: Score,
        /// The document's path.
        path: String,
    },
    /// A tally row: the grouping tuple.
    #[non_exhaustive]
    Tally {
        /// The values the row is grouped by, in the grouping's own order, and
        /// `null` where the document carries no scalar value for that key —
        /// or, under a key declared with a typed order, no scalar that reads
        /// as that type.
        group: Vec<Option<String>>,
    },
    /// A finding row: the kind, in the byte order of its code, then the path,
    /// then the finding's identifier.
    #[non_exhaustive]
    Finding {
        /// The kind the finding is filed under.
        kind: FindingKind,
        /// The path the finding stands at.
        path: String,
        /// The finding's identifier within that path.
        id: u64,
    },
    /// A facet row: the kind, in the byte order of its code, then the key in
    /// byte order.
    #[non_exhaustive]
    Facet {
        /// What the row is a facet of.
        kind: FacetKind,
        /// The facet's key.
        key: String,
    },
    /// A row of one nested collection of a document, paged by its position
    /// in that collection. The collection is part of the key, because each
    /// collection is its own row type and its positions are its own order: a
    /// position among a document's headings names no place among its tags.
    #[non_exhaustive]
    Ordinal {
        /// The collection the page read.
        of: CollectionSelector,
        /// The position the page stopped at.
        index: u64,
    },
}

impl CursorKey {
    /// A document row of a page read in `order`, stopped at `path`, whose
    /// sort field held `sort`.
    pub fn document(order: Sort, sort: Option<String>, path: impl Into<String>) -> Self {
        CursorKey::Document {
            order,
            sort,
            path: path.into(),
        }
    }

    /// A ranked hit stopped at `path`, scored `score`.
    pub fn hit(score: Score, path: impl Into<String>) -> Self {
        CursorKey::Hit {
            score,
            path: path.into(),
        }
    }

    /// A tally row stopped at the grouping tuple `group`.
    pub fn tally(group: impl IntoIterator<Item = Option<String>>) -> Self {
        CursorKey::Tally {
            group: group.into_iter().collect(),
        }
    }

    /// A finding row stopped at `id`, under `kind`, at `path`.
    pub fn finding(kind: FindingKind, path: impl Into<String>, id: u64) -> Self {
        CursorKey::Finding {
            kind,
            path: path.into(),
            id,
        }
    }

    /// A facet row stopped at `key`, under `kind`.
    pub fn facet(kind: FacetKind, key: impl Into<String>) -> Self {
        CursorKey::Facet {
            kind,
            key: key.into(),
        }
    }

    /// The nested collection `of` stopped at `index`.
    pub const fn ordinal(of: CollectionSelector, index: u64) -> Self {
        CursorKey::Ordinal { of, index }
    }

    /// The rows this key names a position among.
    pub const fn rows(&self) -> PagedRows {
        match self {
            CursorKey::Document { .. } => PagedRows::Document,
            CursorKey::Hit { .. } => PagedRows::Hit,
            CursorKey::Tally { .. } => PagedRows::Tally,
            CursorKey::Finding { .. } => PagedRows::Finding,
            CursorKey::Facet { .. } => PagedRows::Facet,
            CursorKey::Ordinal { of, .. } => PagedRows::Collection { of: *of },
        }
    }
}

/// The rows a page is read over: the row a cursor names a position among, or
/// the rows a request pages.
///
/// On the wire it is an object tagged `row`, the tag a cursor key carries:
/// `{"row":"document"}`, `{"row":"collection","of":"links"}`. A get paging a
/// document's findings pages `{"row":"finding"}`, the rows a finding's cursor
/// names a position among, and every other collection by its position in it.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(tag = "row", rename_all = "snake_case")]
#[non_exhaustive]
pub enum PagedRows {
    /// Documents, as a find pages them.
    Document,
    /// Ranked hits, as a search pages them.
    Hit,
    /// Tallies, as a count pages them.
    Tally,
    /// Findings, as a validate pages them and a get pages one document's.
    Finding,
    /// Facets, as a describe pages them.
    Facet,
    /// One nested collection of a document, by position in it, as a get
    /// pages every collection but the findings.
    Collection {
        /// The collection.
        of: CollectionSelector,
    },
}

/// What a cursor was minted under, and what an establishment reads now.
///
/// On the wire a snapshot is a plain object: the database the answer came
/// from, how far its writes had got, the fingerprint of the schema the order
/// was taken under where one applies, and the sidecar revision a search was
/// answered against where one applies.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[non_exhaustive]
pub struct Snapshot {
    /// The database the answer was read from.
    pub epoch: String,
    /// The last write generation committed to that database when the answer
    /// was established.
    pub generation: u64,
    /// The fingerprint of the schema the order was taken under, and `null`
    /// where the order was raw.
    pub schema_fingerprint: Option<String>,
    /// The sidecar's epoch-qualified revision, and `null` where the answer
    /// read no sidecar.
    pub sidecar_revision: Option<u64>,
}

impl Snapshot {
    /// The reading an answer was established under.
    pub fn new(
        epoch: impl Into<String>,
        generation: u64,
        schema_fingerprint: Option<String>,
        sidecar_revision: Option<u64>,
    ) -> Self {
        Snapshot {
            epoch: epoch.into(),
            generation,
            schema_fingerprint,
            sidecar_revision,
        }
    }
}

/// What moved between a cursor being minted and its continuation being
/// answered.
///
/// On the wire a movement is the flat string itself: `"epoch"`,
/// `"generation"`, `"sidecar_revision"`.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Moved {
    /// The database is not the one the cursor was minted from: the derived
    /// state was rebuilt under it.
    Epoch,
    /// Writes landed in the same database after the cursor was minted, or the
    /// count they landed under is behind the one the cursor named.
    Generation,
    /// The sidecar the answer reads is at another revision, or at a revision
    /// another database qualifies.
    SidecarRevision,
}

/// A continuation whose order no longer exists.
///
/// The cursor names a position in an order the request does not read: an
/// order taken under another schema's fingerprint, or another sort key or
/// direction. The sequence its key names a position in is not the sequence the
/// answer would walk, so the page is refused rather than answered from a
/// position that means something else.
///
/// The detail says which order was asked for and which one stands. The two
/// fingerprints name the schema each order is taken under: `minted_under` is
/// `null` for a cursor minted in an order no schema gives, and `current` where
/// the order that stands is raw. A page of documents also names the two
/// orders themselves in `orders`, since a document cursor records the sort key
/// and direction its page was read in; `orders` is `null` where the cursor
/// records no such order.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[non_exhaustive]
pub struct CursorOrderChanged {
    /// The fingerprint the cursor was minted under, and `null` where it was
    /// minted in an order no schema gives.
    pub minted_under: Option<String>,
    /// The fingerprint the establishment reads now, and `null` where the order
    /// that stands is raw.
    pub current: Option<String>,
    /// The order the cursor's page was read in and the order the request's
    /// page is read in, and `null` where the cursor records no order.
    pub orders: Option<DocumentOrders>,
}

impl CursorOrderChanged {
    /// An order that changed between `minted_under` and `current`.
    pub fn new(minted_under: impl Into<String>, current: Option<String>) -> Self {
        CursorOrderChanged {
            minted_under: Some(minted_under.into()),
            current,
            orders: None,
        }
    }

    /// An order that changed from one no schema gives to `current`.
    pub fn minted_raw(current: Option<String>) -> Self {
        CursorOrderChanged {
            minted_under: None,
            current,
            orders: None,
        }
    }

    /// This change, between a page of documents read in `cursor` and one the
    /// request reads in `request`.
    #[must_use]
    pub fn in_orders(self, cursor: Sort, request: Sort) -> Self {
        CursorOrderChanged {
            orders: Some(DocumentOrders::new(cursor, request)),
            ..self
        }
    }
}

/// The two document orders a refused continuation stands between.
///
/// On the wire the pair is an object of two orders:
/// `{"cursor":{"key":{"by":"field","key":"due"},"direction":"ascending"},"request":{"key":{"by":"field","key":"due"},"direction":"descending"}}`.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[non_exhaustive]
pub struct DocumentOrders {
    /// The order the cursor's page was read in.
    pub cursor: Sort,
    /// The order the request's page is read in: the request's sort, or the
    /// path ascending where its sort key is outside the field universe.
    pub request: Sort,
}

impl DocumentOrders {
    /// A cursor's page read in `cursor`, continued by a request whose page is
    /// read in `request`.
    pub const fn new(cursor: Sort, request: Sort) -> Self {
        DocumentOrders { cursor, request }
    }
}

/// Where a page stopped.
///
/// On the wire a cursor is one opaque string a client passes back unchanged.
#[derive(Clone, Debug, PartialEq)]
pub struct Cursor {
    snapshot: Snapshot,
    key: CursorKey,
}

impl Cursor {
    /// The cursor a page minted at `key`, under the establishment `snapshot`
    /// it was answered from.
    pub const fn new(snapshot: Snapshot, key: CursorKey) -> Self {
        Cursor { snapshot, key }
    }

    /// The establishment this page was answered from, which is what a
    /// continuation is judged against.
    pub const fn snapshot(&self) -> &Snapshot {
        &self.snapshot
    }

    /// Where the page stopped.
    pub const fn key(&self) -> &CursorKey {
        &self.key
    }

    /// What has moved between this cursor being minted and `now`, or the
    /// refusal that the order it names no longer exists.
    ///
    /// The four rules and the fixed report order are the module's own, stated
    /// once there rather than restated per caller.
    pub fn continuation(&self, now: &Snapshot) -> Result<Vec<Moved>, CursorOrderChanged> {
        if let Some(minted_under) = &self.snapshot.schema_fingerprint
            && now.schema_fingerprint.as_deref() != Some(minted_under.as_str())
        {
            return Err(CursorOrderChanged::new(
                minted_under,
                now.schema_fingerprint.clone(),
            ));
        }

        let epoch_moved = self.snapshot.epoch != now.epoch;
        let mut moved = Vec::new();
        if epoch_moved {
            moved.push(Moved::Epoch);
        } else if self.snapshot.generation != now.generation {
            moved.push(Moved::Generation);
        }
        if self.snapshot.sidecar_revision.is_some()
            && (epoch_moved || self.snapshot.sidecar_revision != now.sidecar_revision)
        {
            moved.push(Moved::SidecarRevision);
        }
        Ok(moved)
    }
}

/// The cursor's fields, which are what the opaque string is an encoding of:
/// the establishment the page was answered from, and the key it stopped at.
///
/// The wire shape stays a derive: the field names, the nesting of the
/// snapshot, the tag of the key and the treatment of an absent fingerprint are
/// all serde's reading of this struct, and the only thing written by hand is
/// the wrapping around it. The snapshot nests rather than flattening, so the
/// bytes carry the same pair the type does and the canonical encoding is one
/// derive's output rather than a merge of two.
#[derive(Deserialize, Serialize)]
struct CursorFields {
    snapshot: Snapshot,
    key: CursorKey,
}

/// The characters a cursor is spelled in.
const CURSOR_PATTERN: &str = "^[A-Za-z0-9_-]+$";

/// What a string that is no position is told. The same sentence answers a
/// spelling that does not parse and a spelling that parses and is not the one
/// this crate mints: both name no position an answer handed out.
const NO_POSITION: &str = "the cursor names no position";

impl Serialize for Cursor {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let fields = CursorFields {
            snapshot: self.snapshot.clone(),
            key: self.key.clone(),
        };
        let json = serde_json::to_vec(&fields).map_err(serde::ser::Error::custom)?;
        serializer.serialize_str(&base64url::encode(&json))
    }
}

impl<'de> Deserialize<'de> for Cursor {
    /// A cursor arrives as the opaque string a page handed out, and is read
    /// back by decoding it, parsing the fields, and re-encoding them. A string
    /// that does not decode, that decodes to something these fields do not
    /// parse, or that is not byte for byte the re-encoding of what it parsed
    /// is not a position any answer minted, so it refuses the read.
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let text = String::deserialize(deserializer)?;
        let bytes = base64url::decode(&text)
            .map_err(|error| D::Error::custom(format!("a cursor is one opaque string: {error}")))?;
        let fields: CursorFields = serde_json::from_slice(&bytes)
            .map_err(|error| D::Error::custom(format!("{NO_POSITION}: {error}")))?;
        let canonical = serde_json::to_vec(&fields).map_err(D::Error::custom)?;
        if canonical != bytes {
            return Err(D::Error::custom(format!(
                "{NO_POSITION}: one position has one spelling, and this is not it"
            )));
        }
        Ok(Cursor {
            snapshot: fields.snapshot,
            key: fields.key,
        })
    }
}

impl JsonSchema for Cursor {
    fn schema_name() -> Cow<'static, str> {
        Cow::Borrowed("Cursor")
    }

    fn schema_id() -> Cow<'static, str> {
        Cow::Borrowed("norn_wire::Cursor")
    }

    /// One opaque string. The pattern is the alphabet it is spelled in, which
    /// is all a validator can say about it: what the string encodes is this
    /// crate's business and never a consumer's.
    fn json_schema(_generator: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "string",
            "description": "Where a page stopped. Opaque: pass it back unchanged to continue, and read nothing out of it.",
            "pattern": CURSOR_PATTERN,
        })
    }
}

/// One page of rows, and where the next one begins.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(bound(serialize = "T: Serialize", deserialize = "T: DeserializeOwned"))]
#[non_exhaustive]
pub struct Page<T: JsonSchema + Serialize + DeserializeOwned> {
    /// The rows this page holds, in the order the request asked for.
    pub rows: Vec<T>,
    /// Where the next page begins, and `null` when this page is the last.
    pub next: Option<Cursor>,
    /// What moved between the cursor this page continued and the reading it
    /// was answered from. Empty when the continuation was exact, and empty on
    /// a first page, which continues nothing.
    pub moved: Vec<Moved>,
}

impl<T: JsonSchema + Serialize + DeserializeOwned> Page<T> {
    /// A page of `rows`, continuing at `next`, having moved by `moved`.
    pub fn new(rows: Vec<T>, next: Option<Cursor>, moved: Vec<Moved>) -> Self {
        Page { rows, next, moved }
    }
}
