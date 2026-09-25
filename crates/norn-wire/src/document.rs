//! What a document looks like on a row, and which parts of one a request asked
//! for.
//!
//! **A row is a projection, and the projection is stated.** A read names the
//! [`Column`]s it wants and the row carries those and no others: every column
//! past the path is an `Option` on [`DocumentRow`], `None` is "not projected",
//! and an absent option is left out of the bytes entirely. A shape that always
//! carried every column would make one read of a vault's bodies out of every
//! read of its paths, and a client could not tell a column it did not ask for
//! from a column the document does not have.
//!
//! **A nested collection is a head and a total.** [`Collection`] carries the
//! items that fit and how many there were, so a row whose links were cut says
//! so in band rather than handing back a short list that reads as the whole of
//! one. Where the cut falls is the handler's — it is a bound on the work one
//! row costs, not a fact about the vocabulary — and what is spelled here is
//! only that the cut is reported. A total below the items it heads describes
//! no document, so it is refused where a collection is built and where one is
//! read alike.
//!
//! **A body is a head and a total too.** [`BodyText`] carries the text that
//! fit and how many bytes the whole body has, so a body cut to a per-row
//! ceiling says so in band rather than crossing as a string a client cannot
//! tell a whole document from. The ceiling that cuts it is the handler's, the
//! same way a collection's is; what is spelled here is that the cut is
//! reported.
//!
//! **Link health is computed, never stored.** Resolution runs at read time
//! over syntactic link facts, so the health of a link is a fact about the
//! vault at the instant of the read: one target is healthy, none is broken,
//! and more than one is ambiguous. **A link is judged by resolving it
//! first** ([`LinkHealth::of_link`]): a link addressed elsewhere — a protocol
//! other than `vault`, or a Markdown target opening with a URI scheme — is not
//! judged and names no document, and one that resolves to no document is not
//! judged where its target names an attachment. How a target reaches
//! documents is [`LinkAddress`], the one addressing selector. What the
//! target resolved to crosses as a [`CandidateHead`] — the same bounded head
//! of paths and total that a finding and the ambiguous-target refusal carry —
//! so a short link a large vault resolves many ways puts a bounded head and a
//! count on the row rather than the whole ambiguity class. [`LinkRow`] derives
//! its health in its constructor from the link's addressing and that total,
//! and reads it back the same way, so the two halves of one fact cannot
//! arrive disagreeing.
//!
//! **A frontmatter value crosses as the tree it is written as.** The content
//! model — which fields are numbers, which are dates, how two of them compare
//! — is the vault schema's and lives behind the store. [`FieldValue`] says
//! only what container each value sits in and what text each leaf is written
//! as, because that is decidable from the document alone and a client
//! rendering a row needs it. A map is a map of values and a sequence is a
//! sequence of them, all the way down: nothing here is JSON in a string, so a
//! client reads a nested value the way it reads a flat one rather than parsing
//! a second time.
//!
//! **A present null and an absent field are two leaves.** The projection
//! answers whether a document carries a key at all, so a frontmatter key
//! written with no value is a fact the row can state — `null` for the value
//! the document holds — and a key the document never wrote is a second one.
//! Folding them together would make a client unable to tell a field written
//! empty from a field not written, which is a difference a person editing
//! frontmatter can see and act on.
//!
//! **A field value is filled from the document row's canonical frontmatter
//! projection.** The projection is what a row's values are read off, one read
//! of it per row, which is the projection's purpose rather than a second parse
//! of the document. The projection pillar's presence rows answer whether a
//! document carries a key at all and which container it sits in; the values
//! themselves come from the projection.

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::fmt;

use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer, Serialize, de::Error as _};

use crate::address::IllegalPath;
use crate::finding_row::{CandidateHead, FindingRow};
use crate::target::Anchor;

/// What a document path is called in a refusal that names one.
const DOCUMENT_PATH: &str = "document path";

/// What the grammar here wants: something rather than nothing.
const EMPTY: &str = "a document path names something rather than nothing";

/// What the grammar here wants: a path under the vault root rather than one
/// that starts at a filesystem root.
const ROOTED: &str = "a document path is relative to the vault root";

/// Where a document stands in its vault, as the store spells it.
///
/// On the wire a path is the string itself: `"notes/a.md"`. It is relative to
/// the vault root and it is not empty; what else a path may hold is the
/// store's own grammar, checked where documents are derived rather than here.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct DocumentPath(String);

impl DocumentPath {
    /// The path `text` spells, or the reason it spells none.
    ///
    /// Two rules, and they are the two the schema advertises: a path names
    /// something, and it is relative to the vault root. A path starting at a
    /// filesystem root names a place no vault holds a document at, so the
    /// sentence the schema publishes is true of what this reader accepts.
    pub fn new(text: impl AsRef<str>) -> Result<Self, IllegalPath> {
        let text = text.as_ref();
        if text.is_empty() {
            return Err(IllegalPath::new(text, DOCUMENT_PATH, EMPTY));
        }
        if text.starts_with('/') {
            return Err(IllegalPath::new(text, DOCUMENT_PATH, ROOTED));
        }
        Ok(DocumentPath(text.to_string()))
    }

    /// The path as the string it is.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for DocumentPath {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl AsRef<str> for DocumentPath {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for DocumentPath {
    /// A path arrives as the string it is written as and is read through the
    /// grammar, so a path that crossed is a path that parsed. There is no
    /// second door: the derived read path a `#[serde(transparent)]` newtype
    /// would give takes any string at all, the empty one included.
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let text = String::deserialize(deserializer)?;
        DocumentPath::new(text).map_err(D::Error::custom)
    }
}

impl JsonSchema for DocumentPath {
    fn schema_name() -> Cow<'static, str> {
        Cow::Borrowed("DocumentPath")
    }

    fn schema_id() -> Cow<'static, str> {
        Cow::Borrowed("norn_wire::DocumentPath")
    }

    /// A string with a floor of one character. There is no `pattern`: the two
    /// rules this reader keeps are stated in the description, and the rest of
    /// what a document path may hold is the store's grammar, which a regular
    /// expression here would make a second definition of.
    fn json_schema(_generator: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "string",
            "description": "Where a document stands in its vault, relative to the vault root. Not empty, and never starting with a slash.",
            "minLength": 1,
        })
    }
}

/// A position in a document body: 1-based line and column, 0-based byte
/// offset.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[non_exhaustive]
pub struct Span {
    /// The 1-based line the position is on.
    pub line: u64,
    /// The 1-based column the position is at.
    pub column: u64,
    /// The 0-based byte offset of the position from the start of the body.
    pub byte_offset: u64,
}

impl Span {
    /// The position at `line`, `column` and `byte_offset`.
    pub const fn new(line: u64, column: u64, byte_offset: u64) -> Self {
        Span {
            line,
            column,
            byte_offset,
        }
    }
}

/// One part of a document a read asks to have on the row.
///
/// On the wire a column is an object tagged `col`: `{"col":"path"}`,
/// `{"col":"field","key":"due"}`.
///
/// Six of them are nested collections or whole bodies, and each is bounded on
/// the row it lands on: `body`, `links`, `headings`, `blocks`, `tags` and
/// `findings`. A projection that names none of them is a projection of scalar
/// columns alone.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(tag = "col", rename_all = "snake_case")]
#[non_exhaustive]
pub enum Column {
    /// The document's path, which every row carries whether it is asked for or
    /// not.
    Path {},
    /// One frontmatter field, by key.
    #[non_exhaustive]
    Field {
        /// The frontmatter key.
        key: String,
    },
    /// The document's body text.
    Body {},
    /// The links the document carries.
    Links {},
    /// The headings the document carries.
    Headings {},
    /// The block identifiers the document defines.
    Blocks {},
    /// The tags the document carries.
    Tags {},
    /// The findings standing over the document.
    Findings {},
    /// Every frontmatter field the document carries, as one map.
    Fields {},
}

impl Column {
    /// The document's path.
    pub const fn path() -> Self {
        Column::Path {}
    }

    /// The frontmatter field `key`.
    pub fn field(key: impl Into<String>) -> Self {
        Column::Field { key: key.into() }
    }

    /// The document's body text.
    pub const fn body() -> Self {
        Column::Body {}
    }

    /// The links the document carries.
    pub const fn links() -> Self {
        Column::Links {}
    }

    /// The headings the document carries.
    pub const fn headings() -> Self {
        Column::Headings {}
    }

    /// The block identifiers the document defines.
    pub const fn blocks() -> Self {
        Column::Blocks {}
    }

    /// The tags the document carries.
    pub const fn tags() -> Self {
        Column::Tags {}
    }

    /// The findings standing over the document.
    pub const fn findings() -> Self {
        Column::Findings {}
    }

    /// Every frontmatter field the document carries.
    pub const fn fields() -> Self {
        Column::Fields {}
    }
}

/// A total that is smaller than the head it heads.
///
/// The total is what makes a bounded head a head, so a total below the number
/// of rows kept describes nothing: there is no reading of it under which the
/// head is a head of anything. Every bounded head in the vocabulary — a
/// nested [`Collection`] on a row, the [`BodyText`] a body crosses as, and the
/// [`CandidateHead`](crate::CandidateHead) a finding, a link row and a
/// refusal carry —
/// refuses through this one type, so a person reads one sentence whichever
/// head they cut short.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TotalBelowHead {
    head: usize,
    total: u64,
}

impl TotalBelowHead {
    /// How many rows the head kept.
    pub const fn head(&self) -> usize {
        self.head
    }

    /// The total that was claimed for it.
    pub const fn total(&self) -> u64 {
        self.total
    }

    /// `total`, if it can be the total `head` rows head.
    pub(crate) const fn check(head: usize, total: u64) -> Result<(), Self> {
        if total < head as u64 {
            return Err(TotalBelowHead { head, total });
        }
        Ok(())
    }
}

impl fmt::Display for TotalBelowHead {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "a head of {} cannot be the head of {}",
            self.head, self.total
        )
    }
}

impl std::error::Error for TotalBelowHead {}

/// One nested collection on a row: the rows that fit, and how many there were.
///
/// On the wire a collection is a plain object: `{"items":[…],"total":12}`.
/// `total` is the count in the vault, so a collection whose items are fewer
/// than its total was cut to fit the row. A total below the items beside it is
/// refused: it is what makes the items a head, and a smaller one heads
/// nothing.
#[derive(Clone, Debug, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(bound(serialize = "T: Serialize"))]
#[non_exhaustive]
pub struct Collection<T: JsonSchema + Serialize + DeserializeOwned> {
    /// The rows this collection carries on the row.
    pub items: Vec<T>,
    /// How many rows the document has, which is what makes the items a head.
    pub total: u64,
}

impl<T: JsonSchema + Serialize + DeserializeOwned> Collection<T> {
    /// The `items` that fit on the row, out of `total` the document has, or
    /// the reason `total` heads nothing.
    pub fn new(items: Vec<T>, total: u64) -> Result<Self, TotalBelowHead> {
        TotalBelowHead::check(items.len(), total)?;
        Ok(Collection { items, total })
    }

    /// Whether the document has rows this collection does not carry.
    pub fn is_truncated(&self) -> bool {
        (self.items.len() as u64) < self.total
    }
}

/// The collection as it arrives, before its total is checked against the items
/// it heads. The field names and order are the collection's, so the bytes a
/// reader accepts are the bytes a writer produces.
#[derive(Deserialize)]
#[serde(bound(deserialize = "T: DeserializeOwned"))]
struct CollectionFields<T> {
    items: Vec<T>,
    total: u64,
}

impl<'de, T: JsonSchema + Serialize + DeserializeOwned> Deserialize<'de> for Collection<T> {
    /// A collection arrives as its head and its total and is read back through
    /// the same check the constructor holds: a total below the items beside it
    /// heads nothing, so it refuses the read rather than landing as a row that
    /// claims to have been cut to more than it holds.
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let fields = CollectionFields::<T>::deserialize(deserializer)?;
        Collection::new(fields.items, fields.total).map_err(D::Error::custom)
    }
}

/// A document body, as the text that fit and how many bytes the whole of it
/// has.
///
/// On the wire a body is a plain object:
/// `{"text":"# Design\n","byte_length":4096}`. `byte_length` is the body's
/// length in the document, so a body whose text is shorter was cut to fit the
/// row. A length below the text beside it is refused: it is what makes the
/// text a head, and a smaller one heads nothing.
#[derive(Clone, Debug, Eq, JsonSchema, PartialEq, Serialize)]
#[non_exhaustive]
pub struct BodyText {
    /// The body text this row carries.
    text: String,
    /// How many bytes the whole body has, which is what makes the text a head.
    byte_length: u64,
}

impl BodyText {
    /// The `text` that fit, out of a body of `byte_length` bytes, or the
    /// reason `byte_length` heads nothing.
    pub fn new(text: impl Into<String>, byte_length: u64) -> Result<Self, TotalBelowHead> {
        let text = text.into();
        TotalBelowHead::check(text.len(), byte_length)?;
        Ok(BodyText { text, byte_length })
    }

    /// The body text this row carries.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// How many bytes the whole body has.
    pub const fn byte_length(&self) -> u64 {
        self.byte_length
    }

    /// Whether the body has bytes this text does not carry.
    pub fn is_truncated(&self) -> bool {
        (self.text.len() as u64) < self.byte_length
    }
}

/// The body as it arrives, before its length is checked against the text it
/// heads. The field names and order are the body's, so the bytes a reader
/// accepts are the bytes a writer produces.
#[derive(Deserialize)]
struct BodyTextFields {
    text: String,
    byte_length: u64,
}

impl<'de> Deserialize<'de> for BodyText {
    /// A body arrives as its text and its length and is read back through the
    /// same check the constructor holds: a length below the text beside it
    /// heads nothing, so it refuses the read rather than landing as a body
    /// that claims to have been cut to less than it carries.
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let fields = BodyTextFields::deserialize(deserializer)?;
        BodyText::new(fields.text, fields.byte_length).map_err(D::Error::custom)
    }
}

/// The extension a document's file carries, compared with ASCII case folded.
pub const DOCUMENT_EXTENSION: &str = "md";

/// The one protocol a link's address is read under rather than set aside: a
/// `vault://` stem is read from the vault root.
pub const VAULT_PROTOCOL: &str = "vault";

/// The separator between a path's segments.
const SEPARATOR: char = '/';

/// How a link's target reaches documents: the one addressing selector every
/// reading of a link — the store's keys, a row's health — reads.
///
/// **Protocol first, family second.** A link written with a protocol other
/// than [`VAULT_PROTOCOL`] addresses whatever that protocol addresses. A
/// `vault://` stem is read from the vault root, and its family keeps its own
/// rules: a wikilink's stem is a rooted name, and a Markdown link's a path
/// from the root. A link written with no protocol falls to its family: an
/// empty target names the document holding the link, a wikilink's target is
/// a suffix address, and a Markdown target is a path — unless it opens with a
/// URI scheme, `mailto:` and `tel:` among them, which addresses no document.
///
/// **A path is read by URL rules.** Its query — from the first `?` — is not
/// part of it and is cut off here; the fragment was split off when the link
/// was recognized. Splitting it into segments, percent-decoding each, and
/// joining it to where it is read from is the reader's. A wikilink's target,
/// rooted or not, is read by no URL rule: nothing in it is cut off or
/// decoded.
///
/// Plain rather than `#[non_exhaustive]`: a reader matches every address, and
/// an address nobody chose a reading for should fail to compile.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LinkAddress<'a> {
    /// The link addresses no document of the vault, so no resolution judges
    /// it.
    Elsewhere,
    /// The link names the document holding it: an empty target.
    HoldingDocument,
    /// A suffix address, resolved through the one resolver.
    Suffix(&'a str),
    /// A wikilink's name read from the vault root: it names the document at
    /// exactly the root path one of its reductions spells — mirroring a
    /// suffix wikilink's own two reductions: the stem as written, and, where
    /// its leaf carries an extension, the stem with that extension stripped,
    /// each with [`DOCUMENT_EXTENSION`] appended — and never a suffix of a
    /// deeper path.
    RootedName(&'a str),
    /// A path read from the directory of the document holding the link.
    Relative(&'a str),
    /// A path read from the vault root, its leading separator cut off.
    Rooted(&'a str),
}

impl<'a> LinkAddress<'a> {
    /// How a link of `family`, written with `protocol` and `target`, reaches
    /// documents.
    pub fn of(family: LinkFamily, protocol: Option<&str>, target: &'a str) -> Self {
        match protocol {
            Some(VAULT_PROTOCOL) => match family {
                LinkFamily::Wikilink => LinkAddress::RootedName(target),
                LinkFamily::Markdown => LinkAddress::Rooted(without_query(target)),
            },
            Some(_) => LinkAddress::Elsewhere,
            None if target.is_empty() => LinkAddress::HoldingDocument,
            None => match family {
                LinkFamily::Wikilink => LinkAddress::Suffix(target),
                LinkFamily::Markdown if opens_with_a_scheme(target) => LinkAddress::Elsewhere,
                LinkFamily::Markdown => match without_query(target) {
                    "" => LinkAddress::HoldingDocument,
                    path => match path.strip_prefix(SEPARATOR) {
                        Some(rooted) => LinkAddress::Rooted(rooted),
                        None => LinkAddress::Relative(path),
                    },
                },
            },
        }
    }

    /// Whether the address's last segment carries an extension other than
    /// [`DOCUMENT_EXTENSION`], which names an attachment. An extension is what
    /// follows the last dot inside the last segment: a dot leading the
    /// segment starts a name rather than an extension.
    pub fn names_an_attachment(&self) -> bool {
        let (LinkAddress::Suffix(address)
        | LinkAddress::RootedName(address)
        | LinkAddress::Relative(address)
        | LinkAddress::Rooted(address)) = *self
        else {
            return false;
        };
        let leaf = address.rsplit(SEPARATOR).next().unwrap_or(address);
        match leaf.rfind('.') {
            Some(dot) if dot > 0 => !leaf[dot + 1..].eq_ignore_ascii_case(DOCUMENT_EXTENSION),
            _ => false,
        }
    }
}

/// `target` with its query — from the first `?` — cut off.
fn without_query(target: &str) -> &str {
    target.split_once('?').map_or(target, |(path, _)| path)
}

/// Whether `target` opens with an RFC 3986 scheme and its colon: a letter,
/// then letters, digits, `+`, `-` or `.`, up to the first colon. A separator
/// before that colon makes the colon part of a path.
fn opens_with_a_scheme(target: &str) -> bool {
    let Some((scheme, _)) = target.split_once(':') else {
        return false;
    };
    let mut characters = scheme.chars();
    characters
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic())
        && characters.all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '+' | '-' | '.')
        })
}

/// What resolving a link's target found.
///
/// On the wire a health is the flat string itself: `"healthy"`, `"broken"`,
/// `"ambiguous"`, `"not_judged"`.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum LinkHealth {
    /// The target resolves to exactly one document.
    Healthy,
    /// The target resolves to no document, and names no attachment.
    Broken,
    /// The target resolves to more than one document.
    Ambiguous,
    /// No resolution judges the link: it addresses no document of the vault,
    /// or it resolves to none and its target names an attachment.
    NotJudged,
}

impl LinkHealth {
    /// The health of a link of `family`, written with `protocol` and
    /// `target`, whose target resolved to `targets` documents.
    ///
    /// **A link is judged by resolving it first.** A link addressed elsewhere
    /// ([`LinkAddress::Elsewhere`]) is not judged. Any other link is healthy
    /// where it resolved to one document and ambiguous where it resolved to
    /// more; one that resolved to none is not judged where its target names an
    /// attachment ([`LinkAddress::names_an_attachment`]), and broken
    /// otherwise. The count is the head's total rather than the candidates it
    /// carries, so a head cut at its bound reports the health of the whole
    /// class.
    ///
    /// This is the whole of the derivation, so a producer cannot file one
    /// reading of a link and a consumer another.
    pub fn of_link(family: LinkFamily, protocol: Option<&str>, target: &str, targets: u64) -> Self {
        let address = LinkAddress::of(family, protocol, target);
        match (address, targets) {
            (LinkAddress::Elsewhere, _) => LinkHealth::NotJudged,
            (_, 0) if address.names_an_attachment() => LinkHealth::NotJudged,
            (_, 0) => LinkHealth::Broken,
            (_, 1) => LinkHealth::Healthy,
            _ => LinkHealth::Ambiguous,
        }
    }
}

/// The written form a link was recognized from.
///
/// On the wire a family is the flat string itself: `"wikilink"`, `"markdown"`.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum LinkFamily {
    /// A wikilink, `[[target]]`.
    Wikilink,
    /// An inline Markdown link, `[title](target)`.
    Markdown,
}

/// One link the document carries, with what its target resolves to.
///
/// The syntactic half is what the document says; the resolved half is what the
/// vault held at the instant of the read. `targets` is a bounded head and a
/// total, so a link a vault resolves many ways crosses as the first few of
/// those documents and how many there were, and a link addressed elsewhere
/// crosses with no document. `health` is derived from the link's addressing
/// and that total rather than carried beside them: the constructor computes it
/// and the read path recomputes it, so a row whose health disagrees with the
/// documents it names has no representation on either side of the seam.
#[derive(Clone, Debug, Eq, JsonSchema, PartialEq, Serialize)]
#[non_exhaustive]
pub struct LinkRow {
    /// Which grammar the link was written in.
    pub family: LinkFamily,
    /// Whether the link embeds its target rather than referring to it.
    pub embed: bool,
    /// The `protocol://` prefix the target was written with, and `null` where
    /// it was written with none.
    pub protocol: Option<String>,
    /// The target as written: no normalization and no percent-decoding.
    pub target: String,
    /// The link's title, where its grammar carries one.
    pub title: Option<String>,
    /// The place inside the target the link names, where it names one.
    pub anchor: Option<Anchor>,
    /// Where the link stands in the document body.
    pub span: Span,
    /// The bounded head of the documents the target resolves to, in the
    /// resolution ladder's own order, with how many there were.
    pub targets: CandidateHead,
    /// What resolving the target found, which is read off the documents it
    /// found.
    health: LinkHealth,
}

impl LinkRow {
    /// A link of `family` at `span`, written as `target`, resolving to the
    /// documents `targets` heads.
    ///
    /// The health is computed from the link's addressing and the total
    /// `targets` carries rather than passed in, so the two name one reading of
    /// the vault. A link addressed elsewhere ([`LinkAddress::Elsewhere`])
    /// resolves to none, so its `targets` is the empty head.
    #[allow(clippy::too_many_arguments)] // A link row is the link's own facts; grouping them would mint a shape nothing else holds.
    pub fn new(
        family: LinkFamily,
        embed: bool,
        protocol: Option<String>,
        target: impl Into<String>,
        title: Option<String>,
        anchor: Option<Anchor>,
        span: Span,
        targets: CandidateHead,
    ) -> Self {
        let target = target.into();
        let health = LinkHealth::of_link(family, protocol.as_deref(), &target, targets.total());
        LinkRow {
            family,
            embed,
            protocol,
            target,
            title,
            anchor,
            span,
            targets,
            health,
        }
    }

    /// What resolving this link's target found.
    pub const fn health(&self) -> LinkHealth {
        self.health
    }
}

/// The link row as it arrives, before its health is checked against the
/// documents it names. The field names and order are the row's, so the bytes a
/// reader accepts are the bytes a writer produces.
#[derive(Deserialize)]
struct LinkRowFields {
    family: LinkFamily,
    embed: bool,
    protocol: Option<String>,
    target: String,
    title: Option<String>,
    anchor: Option<Anchor>,
    span: Span,
    targets: CandidateHead,
    health: LinkHealth,
}

impl<'de> Deserialize<'de> for LinkRow {
    /// A row arrives with both halves of one fact and is read back by
    /// recomputing the derived half: a row whose `health` is not the health of
    /// its addressing and the `targets` beside it, or a link addressed
    /// elsewhere that names documents, is refused rather than read into a value
    /// whose fields say different things about one link.
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let fields = LinkRowFields::deserialize(deserializer)?;
        let derived = LinkHealth::of_link(
            fields.family,
            fields.protocol.as_deref(),
            &fields.target,
            fields.targets.total(),
        );
        if derived == LinkHealth::NotJudged && fields.targets.total() != 0 {
            return Err(D::Error::custom(format!(
                "the link addresses no document, and it resolves to {} of them",
                fields.targets.total(),
            )));
        }
        if fields.health != derived {
            return Err(D::Error::custom(format!(
                "the link's health {:?} is not the health of the {} documents it resolves to, {derived:?}",
                fields.health,
                fields.targets.total(),
            )));
        }
        Ok(LinkRow {
            family: fields.family,
            embed: fields.embed,
            protocol: fields.protocol,
            target: fields.target,
            title: fields.title,
            anchor: fields.anchor,
            span: fields.span,
            targets: fields.targets,
            health: fields.health,
        })
    }
}

/// One heading the document carries.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[non_exhaustive]
pub struct HeadingRow {
    /// How deep the heading is, `#` being 1.
    pub level: u8,
    /// The heading's text, with inline markup flattened. A wikilink anchor
    /// addresses this.
    pub text: String,
    /// The heading's anchor form, its document-order dedupe suffix included.
    /// An inline Markdown fragment addresses this.
    pub slug: String,
    /// Where the heading stands in the document body.
    pub span: Span,
}

impl HeadingRow {
    /// The heading `text` at `level`, sluggified as `slug`, standing at
    /// `span`.
    pub fn new(level: u8, text: impl Into<String>, slug: impl Into<String>, span: Span) -> Self {
        HeadingRow {
            level,
            text: text.into(),
            slug: slug.into(),
            span,
        }
    }
}

/// One block identifier the document defines.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[non_exhaustive]
pub struct BlockRow {
    /// The identifier, without its `^`.
    pub id: String,
    /// Where the definition stands, and `null` where its position cannot be
    /// named.
    pub span: Option<Span>,
}

impl BlockRow {
    /// The block `id`, defined at `span`.
    pub fn new(id: impl Into<String>, span: Option<Span>) -> Self {
        BlockRow {
            id: id.into(),
            span,
        }
    }
}

/// Which home a tag was read from.
///
/// On the wire a source is the flat string itself: `"body"`,
/// `"frontmatter"`.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum TagSource {
    /// The tag was written in the document body as a `#tag` marker.
    Body,
    /// The tag was written as a frontmatter entry.
    Frontmatter,
}

/// One tag the document carries.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[non_exhaustive]
pub struct TagRow {
    /// The tag's name, without its `#`, nesting included and case as written.
    pub name: String,
    /// Which home it was read from.
    pub source: TagSource,
    /// Where it stands in the document body, and `null` where its bytes cannot
    /// be named as one span.
    pub span: Option<Span>,
}

impl TagRow {
    /// The tag `name`, read from `source`, standing at `span`.
    pub fn new(name: impl Into<String>, source: TagSource, span: Option<Span>) -> Self {
        TagRow {
            name: name.into(),
            source,
            span,
        }
    }
}

/// What one frontmatter field holds: the container each value sits in, and
/// the text each leaf is written as.
///
/// On the wire a value is an object tagged `kind`:
/// `{"kind":"scalar","raw":"note"}`, `{"kind":"absent"}`. A sequence holds
/// values and a map holds values by key, so a nested value is read the way a
/// flat one is. A field written with no value and a field the document never
/// wrote are two kinds rather than one.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum FieldValue {
    /// One value, as the text it is written as.
    #[non_exhaustive]
    Scalar {
        /// The value as written.
        raw: String,
    },
    /// A sequence of values.
    #[non_exhaustive]
    Sequence {
        /// The values, in the order the document writes them.
        items: Vec<FieldValue>,
    },
    /// A mapping of values, by key.
    #[non_exhaustive]
    Map {
        /// The values the mapping holds, by the key each one is written under.
        entries: BTreeMap<String, FieldValue>,
    },
    /// The document carries the field, and what it holds is null.
    Null {},
    /// The document does not carry the field.
    Absent {},
}

impl FieldValue {
    /// The scalar written as `raw`.
    pub fn scalar(raw: impl Into<String>) -> Self {
        FieldValue::Scalar { raw: raw.into() }
    }

    /// The sequence of `items`.
    pub fn sequence(items: impl IntoIterator<Item = FieldValue>) -> Self {
        FieldValue::Sequence {
            items: items.into_iter().collect(),
        }
    }

    /// The mapping holding `entries`.
    pub fn map(entries: impl IntoIterator<Item = (String, FieldValue)>) -> Self {
        FieldValue::Map {
            entries: entries.into_iter().collect(),
        }
    }

    /// The field the document carries as null.
    pub const fn null() -> Self {
        FieldValue::Null {}
    }

    /// The field the document does not carry.
    pub const fn absent() -> Self {
        FieldValue::Absent {}
    }
}

/// One document, projected onto the columns a read asked for.
///
/// The path is always carried. Every other column is `null` — and left out of
/// the bytes — where the read did not ask for it, so a client tells a column
/// it did not project from a column the document does not have.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[non_exhaustive]
pub struct DocumentRow {
    /// Where the document stands in its vault.
    pub path: DocumentPath,
    /// Every frontmatter field the document carries, by key.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fields: Option<BTreeMap<String, FieldValue>>,
    /// The document's body text, with how many bytes the whole of it has.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<BodyText>,
    /// The links the document carries.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub links: Option<Collection<LinkRow>>,
    /// The headings the document carries.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub headings: Option<Collection<HeadingRow>>,
    /// The block identifiers the document defines.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blocks: Option<Collection<BlockRow>>,
    /// The tags the document carries.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tags: Option<Collection<TagRow>>,
    /// The findings standing over the document.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub findings: Option<Collection<FindingRow>>,
}

impl DocumentRow {
    /// The row of the document at `path`, with no column projected.
    ///
    /// The columns are set by the setters below, so a row carries exactly the
    /// ones the read asked for.
    pub const fn new(path: DocumentPath) -> Self {
        DocumentRow {
            path,
            fields: None,
            body: None,
            links: None,
            headings: None,
            blocks: None,
            tags: None,
            findings: None,
        }
    }

    /// The row with its frontmatter fields projected.
    #[must_use]
    pub fn with_fields(mut self, fields: BTreeMap<String, FieldValue>) -> Self {
        self.fields = Some(fields);
        self
    }

    /// The row with its body projected.
    #[must_use]
    pub fn with_body(mut self, body: BodyText) -> Self {
        self.body = Some(body);
        self
    }

    /// The row with its links projected.
    #[must_use]
    pub fn with_links(mut self, links: Collection<LinkRow>) -> Self {
        self.links = Some(links);
        self
    }

    /// The row with its headings projected.
    #[must_use]
    pub fn with_headings(mut self, headings: Collection<HeadingRow>) -> Self {
        self.headings = Some(headings);
        self
    }

    /// The row with its block identifiers projected.
    #[must_use]
    pub fn with_blocks(mut self, blocks: Collection<BlockRow>) -> Self {
        self.blocks = Some(blocks);
        self
    }

    /// The row with its tags projected.
    #[must_use]
    pub fn with_tags(mut self, tags: Collection<TagRow>) -> Self {
        self.tags = Some(tags);
        self
    }

    /// The row with the findings standing over it projected.
    #[must_use]
    pub fn with_findings(mut self, findings: Collection<FindingRow>) -> Self {
        self.findings = Some(findings);
        self
    }
}
