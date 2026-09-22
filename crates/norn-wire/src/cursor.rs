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
//! **Opaque is not encrypted, and it is not a promise of privacy.** Anyone may
//! decode one; what the rendering buys is that nobody *builds* one. A position
//! spelled by a client is a position no answer minted, and the parts a cursor
//! carries — the epoch, the generation, the order's fingerprint — are exactly
//! the parts a continuation is judged against.
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
//! consumer can say how the ground shifted. The one thing that is refused is a
//! change of *order* — a cursor minted under one schema fingerprint continued
//! against another — because the rows a key names are in a sequence that no
//! longer exists. A cursor minted under no fingerprint was ordered rawly, and
//! a raw order does not change with the schema.

use std::borrow::Cow;

use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error as _};

use crate::base64url;
use crate::finding::FindingKind;

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
    /// A folder in the vault.
    Folder,
    /// A path rule the vault's schema states.
    PathRule,
}

/// Where one page of rows stopped, as the parts that row's order sorts by.
///
/// On the wire a key is an object tagged `row`:
/// `{"row":"document","sort":"2026-01-01","path":"notes/a.md"}`.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(tag = "row", rename_all = "snake_case")]
#[non_exhaustive]
pub enum CursorKey {
    /// A document row: the sort field's value, then the path.
    #[non_exhaustive]
    Document {
        /// The sort field's value as the store's string sort key, and `null`
        /// for a document that does not carry the sort field. A document
        /// missing it orders before every document that has it.
        sort: Option<String>,
        /// The document's path.
        path: String,
    },
    /// A ranked hit: relevance descending, then the path.
    #[non_exhaustive]
    Hit {
        /// The hit's relevance score.
        score: f64,
        /// The document's path.
        path: String,
    },
    /// A tally row: the grouping tuple.
    #[non_exhaustive]
    Tally {
        /// The values the row is grouped by, in the grouping's own order.
        group: Vec<String>,
    },
    /// A finding row: the kind, then the path, then the finding's identifier.
    #[non_exhaustive]
    Finding {
        /// The kind the finding is filed under.
        kind: FindingKind,
        /// The path the finding stands at.
        path: String,
        /// The finding's identifier within that path.
        id: u64,
    },
    /// A facet row: the kind, then the key.
    #[non_exhaustive]
    Facet {
        /// What the row is a facet of.
        kind: FacetKind,
        /// The facet's key.
        key: String,
    },
    /// A row of a nested collection, paged by its position in that collection.
    #[non_exhaustive]
    Ordinal {
        /// The position the page stopped at.
        index: u64,
    },
}

impl CursorKey {
    /// A document row stopped at `path`, whose sort field held `sort`.
    pub fn document(sort: Option<String>, path: impl Into<String>) -> Self {
        CursorKey::Document {
            sort,
            path: path.into(),
        }
    }

    /// A ranked hit stopped at `path`, scored `score`.
    pub fn hit(score: f64, path: impl Into<String>) -> Self {
        CursorKey::Hit {
            score,
            path: path.into(),
        }
    }

    /// A tally row stopped at the grouping tuple `group`.
    pub fn tally(group: impl IntoIterator<Item = String>) -> Self {
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

    /// A nested collection stopped at `index`.
    pub const fn ordinal(index: u64) -> Self {
        CursorKey::Ordinal { index }
    }
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
    /// Writes landed in the same database after the cursor was minted.
    Generation,
    /// The sidecar the answer reads is at another revision.
    SidecarRevision,
}

/// A continuation whose order no longer exists.
///
/// The cursor was minted under one schema fingerprint and is being continued
/// under another, so the sequence its key names a position in is not the
/// sequence the answer would walk. The page is refused rather than answered
/// from a position that means something else, and the two fingerprints say
/// which order was asked for and which one stands.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[non_exhaustive]
pub struct CursorOrderChanged {
    /// The fingerprint the cursor was minted under.
    pub minted_under: String,
    /// The fingerprint the establishment reads now.
    pub current: String,
}

impl CursorOrderChanged {
    /// An order that changed between `minted_under` and `current`.
    pub fn new(minted_under: impl Into<String>, current: impl Into<String>) -> Self {
        CursorOrderChanged {
            minted_under: minted_under.into(),
            current: current.into(),
        }
    }
}

/// Where a page stopped.
///
/// On the wire a cursor is one opaque string a client passes back unchanged.
#[derive(Clone, Debug, PartialEq)]
pub struct Cursor {
    epoch: String,
    generation: u64,
    schema_fingerprint: Option<String>,
    sidecar_revision: Option<u64>,
    key: CursorKey,
}

impl Cursor {
    /// The cursor a page minted at `key`, under the establishment it was
    /// answered from.
    ///
    /// `schema_fingerprint` is present where the order is typed and `None`
    /// where it is raw; `sidecar_revision` is present where the answer read a
    /// sidecar, sampled with the answer.
    pub fn new(
        epoch: impl Into<String>,
        generation: u64,
        schema_fingerprint: Option<String>,
        sidecar_revision: Option<u64>,
        key: CursorKey,
    ) -> Self {
        Cursor {
            epoch: epoch.into(),
            generation,
            schema_fingerprint,
            sidecar_revision,
            key,
        }
    }

    /// The database the page was read from.
    pub fn epoch(&self) -> &str {
        &self.epoch
    }

    /// How far that database's writes had got.
    pub const fn generation(&self) -> u64 {
        self.generation
    }

    /// The fingerprint of the schema the order was taken under, where the
    /// order is typed.
    pub fn schema_fingerprint(&self) -> Option<&str> {
        self.schema_fingerprint.as_deref()
    }

    /// The sidecar revision the answer was read against, where it read one.
    pub const fn sidecar_revision(&self) -> Option<u64> {
        self.sidecar_revision
    }

    /// Where the page stopped.
    pub const fn key(&self) -> &CursorKey {
        &self.key
    }

    /// What has moved between this cursor being minted and `now`, or the
    /// refusal that the order it names no longer exists.
    ///
    /// The list is in a fixed order — epoch, generation, sidecar revision —
    /// so a consumer reads it as a report rather than as a set it has to sort.
    /// A generation is only compared within one database: a rebuild restarts
    /// the count, so a generation read against another epoch says nothing.
    pub fn continuation(&self, now: &Snapshot) -> Result<Vec<Moved>, CursorOrderChanged> {
        if let (Some(minted_under), Some(current)) =
            (&self.schema_fingerprint, &now.schema_fingerprint)
            && minted_under != current
        {
            return Err(CursorOrderChanged::new(minted_under, current));
        }

        let mut moved = Vec::new();
        if self.epoch != now.epoch {
            moved.push(Moved::Epoch);
        } else if now.generation > self.generation {
            moved.push(Moved::Generation);
        }
        if self.sidecar_revision.is_some() && self.sidecar_revision != now.sidecar_revision {
            moved.push(Moved::SidecarRevision);
        }
        Ok(moved)
    }
}

/// The cursor's fields, which are what the opaque string is an encoding of.
///
/// The wire shape stays a derive: the field names, the tag of the key and the
/// treatment of an absent fingerprint are all serde's reading of this struct,
/// and the only thing written by hand is the wrapping around it.
#[derive(Deserialize, Serialize)]
struct CursorFields {
    epoch: String,
    generation: u64,
    schema_fingerprint: Option<String>,
    sidecar_revision: Option<u64>,
    key: CursorKey,
}

/// The characters a cursor is spelled in.
const CURSOR_PATTERN: &str = "^[A-Za-z0-9_-]+$";

impl Serialize for Cursor {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let fields = CursorFields {
            epoch: self.epoch.clone(),
            generation: self.generation,
            schema_fingerprint: self.schema_fingerprint.clone(),
            sidecar_revision: self.sidecar_revision,
            key: self.key.clone(),
        };
        let json = serde_json::to_vec(&fields).map_err(serde::ser::Error::custom)?;
        serializer.serialize_str(&base64url::encode(&json))
    }
}

impl<'de> Deserialize<'de> for Cursor {
    /// A cursor arrives as the opaque string a page handed out, and is read
    /// back by decoding it and parsing the fields. A string that does not
    /// decode, or that decodes to something these fields do not parse, is not
    /// a position any answer minted, so it refuses the read.
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let text = String::deserialize(deserializer)?;
        let bytes = base64url::decode(&text)
            .map_err(|error| D::Error::custom(format!("a cursor is one opaque string: {error}")))?;
        let fields: CursorFields = serde_json::from_slice(&bytes)
            .map_err(|error| D::Error::custom(format!("the cursor names no position: {error}")))?;
        Ok(Cursor {
            epoch: fields.epoch,
            generation: fields.generation,
            schema_fingerprint: fields.schema_fingerprint,
            sidecar_revision: fields.sidecar_revision,
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
