//! A parsed document, and the edits that keep the rest of it byte-identical.

use std::fmt;
use std::ops::Range;

use crate::body::BodyScan;
use crate::diagnostic::{Diagnostic, DiagnosticCode};
use crate::frontmatter::extract::{
    BOM, BlockRefusal, FRONTMATTER_MAX_BYTES, closed_block, extract,
};
use crate::frontmatter::fields::{
    Field, SplitRefusal, ValueStyle, classify_value, field_spans, reparse,
};
use crate::frontmatter::list::{
    BlockItem, block_item_lines, entry_carries_comment, key_line_value_point,
};
use crate::frontmatter::render::{
    RenderError, Scalar, ScalarStyle, is_collection, is_nested, render_block_item, render_entry,
    render_flow_sequence, render_key, render_scalar_in_span,
};
use crate::heading::Heading;
use crate::line_ending::LineEnding;
use crate::link::{Link, parse_wikilinks_in_text};
use crate::section::{SectionAddress, SectionError, SectionSpan, line_start};
use crate::span::{LineCursor, split_lines_inclusive, trailing_break};
use crate::tag::{Tag, frontmatter_tag_name};
use crate::value::{KeyIndex, Mapping, Value};

/// The one frontmatter field whose strings are read as tags.
pub const TAGS_FIELD: &str = "tags";

/// A frontmatter string value and where its bytes are.
///
/// `text` is the decoded value — what the field means. `range` is the bytes
/// that produced it, exactly as written, quotes included; a decoded value need
/// not appear literally anywhere in the document, so the range names the
/// source bytes rather than a substring search for the decoded text. It is
/// `None` when the value's bytes cannot be named as one span the parser agrees
/// with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldText<'a> {
    pub field: &'a str,
    pub text: &'a str,
    pub range: Option<Range<usize>>,
}

/// A frontmatter string written literally in the source: its text is a
/// substring of the document, starting at `start`.
pub(crate) struct LiteralText<'a> {
    pub(crate) text: &'a str,
    pub(crate) start: usize,
}

/// Why an edit was refused.
#[derive(Debug, Clone, PartialEq)]
pub enum EditError {
    /// The document opens a frontmatter block that does not parse, or does not
    /// close. Editing it means guessing what it was meant to say.
    FrontmatterUnreadable,
    /// The frontmatter block parses to something other than a mapping, so it
    /// holds no fields to address.
    FrontmatterNotAMapping {
        kind: &'static str,
    },
    /// The block parses, and its field spans cannot be trusted: the scanner
    /// and the parser disagree somewhere in it, so no field in it is
    /// addressable. The value model still reads whole; the reads built on
    /// field spans report nothing. No edit is attempted.
    ///
    /// `cause` names which disagreement it was, because the three shapes ask
    /// for three different edits to the block.
    FrontmatterNotEditable {
        cause: SplitRefusal,
    },
    /// This field's value cannot be named as one span. Reads are unaffected;
    /// the edit is not attempted.
    FieldNotEditable {
        field: String,
    },
    FieldAbsent {
        field: String,
    },
    /// A list edit addressed a field holding something other than a list.
    FieldNotAList {
        field: String,
        kind: &'static str,
    },
    /// An edit would have to rewrite the field whole — a list edit that cannot
    /// splice an item, or a set changing what the field holds or writing or
    /// replacing a nested value — and the field's entry carries a comment that
    /// rewrite would drop. Nothing is written: a comment is the author's, and no edit
    /// loses one silently.
    CommentWouldBeLost {
        field: String,
    },
    /// A pop named a value the field's list does not hold. A pop that changes
    /// nothing is refused rather than reported as done.
    ListValueAbsent {
        field: String,
        value: Value,
    },
    Render(RenderError),
    Section(SectionError),
    /// The addressed heading sits inside a blockquote or a list item, whose
    /// bytes are the container's before they are the section's. Replacing them
    /// by byte range lifts them out of the container.
    SectionInContainer {
        heading: String,
    },
    /// The edited document does not read back as intended. Nothing is
    /// returned: an unproven write is a refusal.
    PostImageMismatch {
        field: String,
    },
    /// The edit lands a frontmatter block past
    /// [`FRONTMATTER_MAX_BYTES`](crate::FRONTMATTER_MAX_BYTES), which is
    /// longer than the block this crate reads.
    ///
    /// The written bytes would be well-formed and no read would ever turn
    /// them back into fields, so the block is refused before it is written and
    /// the refusal names the bound rather than accusing the splice.
    FrontmatterPastBound {
        bytes: usize,
        bound: usize,
    },
    /// The document a section replace produced does not read back as
    /// intended — the frontmatter moved, the addressed heading no longer
    /// resolves to the content it was given, or a heading the body already had
    /// is gone. Nothing is returned.
    SectionPostImageMismatch {
        heading: String,
    },
    /// The document a body replace produced does not read back with the same
    /// frontmatter and the given body — the new body opens with something
    /// that reads as a block. Nothing is returned.
    BodyPostImageMismatch,
}

impl fmt::Display for EditError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EditError::FrontmatterUnreadable => {
                f.write_str("the document's frontmatter block cannot be read")
            }
            EditError::FrontmatterNotAMapping { kind } => write!(
                f,
                "the frontmatter block holds a {kind}, and only a mapping has fields"
            ),
            EditError::FrontmatterNotEditable { cause } => write!(
                f,
                "the frontmatter block's field spans cannot be trusted, so no field in it can be \
                 edited ({})",
                cause.problem()
            ),
            EditError::FieldNotEditable { field } => {
                write!(f, "the field {field:?} cannot be edited in place")
            }
            EditError::FieldAbsent { field } => write!(f, "the field {field:?} is not present"),
            EditError::FieldNotAList { field, kind } => {
                write!(f, "the field {field:?} holds a {kind}, not a list")
            }
            EditError::CommentWouldBeLost { field } => write!(
                f,
                "the field {field:?} carries a comment this edit would drop, so it was refused"
            ),
            EditError::ListValueAbsent { field, value } => {
                write!(f, "the list {field:?} holds no element equal to {value:?}")
            }
            EditError::Render(error) => write!(f, "{error}"),
            EditError::Section(error) => write!(f, "{error}"),
            EditError::SectionInContainer { heading } => write!(
                f,
                "the heading {heading:?} sits inside a blockquote or list item, whose content is \
                 not separately replaceable"
            ),
            EditError::PostImageMismatch { field } => write!(
                f,
                "the edited document does not read back with {field:?} as intended, so the edit \
                 was refused"
            ),
            EditError::SectionPostImageMismatch { heading } => write!(
                f,
                "the edited document does not read back with the section {heading:?} as intended, \
                 so the edit was refused"
            ),
            EditError::BodyPostImageMismatch => f.write_str(
                "the edited document does not read back with its frontmatter and the new body as \
                 intended, so the edit was refused",
            ),
            EditError::FrontmatterPastBound { bytes, bound } => write!(
                f,
                "the edit writes a frontmatter block of {bytes} bytes, and a block past {bound} \
                 bytes is not read"
            ),
        }
    }
}

impl std::error::Error for EditError {}

impl From<RenderError> for EditError {
    fn from(error: RenderError) -> Self {
        EditError::Render(error)
    }
}

impl From<SectionError> for EditError {
    fn from(error: SectionError) -> Self {
        EditError::Section(error)
    }
}

/// A document read as syntax: its frontmatter block, its body, and where every
/// top-level field's bytes are.
///
/// Reading is forgiving and reports what it worked around in
/// [`Document::diagnostics`]. Editing is not: each edit either returns a whole
/// new document that provably reads back as intended, or refuses.
///
/// # Ask the body once
///
/// The body accessors here — [`Document::headings`], [`Document::links`],
/// [`Document::wikilinks`], [`Document::tags`] — each build their own
/// [`BodyScan`], so asking four questions parses the body four times. They are
/// the convenience for a caller with one question and source coordinates. A
/// caller with several should take [`Document::scan_body`] once and rebase by
/// [`Document::body_start`], which is what the one-pass guarantee is worth.
#[derive(Debug, Clone)]
pub struct Document<'a> {
    source: &'a str,
    byte_order_mark: bool,
    line_ending: LineEnding,
    frontmatter: Option<Value>,
    /// Why the block was read by nothing, when it was: the state that separates
    /// a document carrying no block from one whose fields are unknown.
    frontmatter_refusal: Option<BlockRefusal>,
    frontmatter_range: Option<Range<usize>>,
    body: &'a str,
    body_start: usize,
    fields: Vec<Field>,
    /// Why the field layer refused to split the block, when it did: no line in
    /// it is then safely attributable to a field and every edit into it
    /// refuses, carrying this cause.
    split_refusal: Option<SplitRefusal>,
    diagnostics: Vec<Diagnostic>,
}

impl<'a> Document<'a> {
    /// Read `source`. Never fails: a malformed block is reported as a
    /// diagnostic and the body is still available.
    pub fn parse(source: &'a str) -> Self {
        let mut diagnostics = Vec::new();
        let extraction = extract(source, &mut diagnostics);
        let located = match (&extraction.value, &extraction.range) {
            (Some(value), Some(range)) => {
                field_spans(source, range.clone(), value, extraction.strip)
            }
            _ => Ok(Vec::new()),
        };
        let split_refusal = located.as_ref().err().cloned();
        let fields = located.unwrap_or_default();
        // The message states the contract that holds for every refused split;
        // the detail is this block's own cause, rendered here because prose
        // belongs to the layer that delivers it. One code covers all three
        // causes: a detail string is advisory prose no consumer matches on.
        if let Some(cause) = &split_refusal {
            diagnostics.push(
                Diagnostic::warning(
                    DiagnosticCode::FrontmatterNotEditable,
                    "the frontmatter block's field spans cannot be trusted, so field edits refuse \
                     and no field text is reported; the block still reads whole",
                )
                .with_detail(cause.problem()),
            );
        }
        Document {
            split_refusal,
            source,
            byte_order_mark: extraction.byte_order_mark,
            line_ending: LineEnding::of(source),
            frontmatter: extraction.value,
            frontmatter_refusal: extraction.refusal,
            frontmatter_range: extraction.range,
            body: extraction.body,
            body_start: extraction.body_start,
            fields,
            diagnostics,
        }
    }

    /// Everything after the closing delimiter, or the whole document when
    /// there is no block.
    pub fn body(&self) -> &'a str {
        self.body
    }

    /// Where [`Document::body`] begins in the source.
    pub fn body_start(&self) -> usize {
        self.body_start
    }

    /// The whole text this document was read from.
    pub(crate) fn source(&self) -> &'a str {
        self.source
    }

    /// The line terminator the document is written with. Every line an edit
    /// synthesizes uses it.
    pub fn line_ending(&self) -> LineEnding {
        self.line_ending
    }

    /// Whether the document opens with a byte-order mark. The mark stays where
    /// it is: it is stepped over for recognition and never rewritten, and a
    /// synthesized block lands after it.
    pub fn has_byte_order_mark(&self) -> bool {
        self.byte_order_mark
    }

    /// The parsed frontmatter, or `None` when there is no block or it did not
    /// parse.
    pub fn frontmatter(&self) -> Option<&Value> {
        self.frontmatter.as_ref()
    }

    /// Why this document's frontmatter block was read by nothing, or `None`
    /// where nothing refused it.
    ///
    /// [`Document::frontmatter`] is `None` for two documents that differ: one
    /// carries no block, and one carries a block that contributed no field. A
    /// consumer deriving state from a document has to tell them apart —
    /// answering *this document has no tags, no title, no aliases* about a
    /// document whose block went unread states as fact what was never read —
    /// and this is the state it tells them apart by. It is `Some` exactly when
    /// a block was opened and no value came out of it.
    pub fn frontmatter_refusal(&self) -> Option<&BlockRefusal> {
        self.frontmatter_refusal.as_ref()
    }

    /// The byte range of the YAML between the delimiters. Present even when
    /// the block did not parse.
    pub fn frontmatter_range(&self) -> Option<Range<usize>> {
        self.frontmatter_range.clone()
    }

    /// Every top-level field, in document order. Empty when the block holds no
    /// mapping, or when its spans cannot be trusted.
    pub fn fields(&self) -> &[Field] {
        &self.fields
    }

    /// One top-level field by name.
    pub fn field(&self, name: &str) -> Option<&Field> {
        self.fields.iter().find(|field| field.name == name)
    }

    /// Why the field layer refused to split this document's block, or `None`
    /// where it did not.
    ///
    /// A refused split reaches a reader as absence — no fields, no field
    /// texts, no frontmatter tags or wikilinks — and this is the state that
    /// separates that absence from a block holding nothing, the same way
    /// [`Document::frontmatter_refusal`] separates an unread block from a
    /// document carrying none. It is the cause
    /// [`EditError::FrontmatterNotEditable`] carries, readable without
    /// proposing an edit the caller never meant to make.
    ///
    /// The host's read side reports an unread block from
    /// [`Document::frontmatter_refusal`] and reports a refused split as the
    /// diagnostic alone, so nothing outside this crate reads this yet. The
    /// apply seam that gives the cause a shape of its own is where it is read
    /// from; until then the state is reachable rather than only inferable, so
    /// no consumer has to sham an edit or match on advisory prose to learn it.
    pub fn split_refusal(&self) -> Option<&SplitRefusal> {
        self.split_refusal.as_ref()
    }

    /// What reading this document had to work around.
    pub fn diagnostics(&self) -> &[Diagnostic] {
        &self.diagnostics
    }

    /// Every string held in the frontmatter — scalar field values and the
    /// string items of sequences — with the source bytes that produced each.
    ///
    /// This is the seam a caller scans frontmatter values for syntax through;
    /// the ranges come from the field layer, so an escaped or line-continued
    /// value reports the bytes it was written as rather than nothing.
    pub fn field_texts(&self) -> Vec<FieldText<'_>> {
        let Some(Value::Map(map)) = &self.frontmatter else {
            return Vec::new();
        };
        if self.fields.is_empty() {
            // A block whose split was refused has fields nothing can attribute,
            // and there is no key here to resolve. The mapping still holds every
            // entry the block parsed, so indexing it would be work for no lookup.
            return Vec::new();
        }
        // One lookup per field, so the mapping is indexed once rather than
        // scanned per field: a block at the byte bound holds thousands of
        // fields, and a scan each is quadratic in how many there are.
        let parsed_keys = KeyIndex::of(map);
        let mut texts = Vec::new();
        for field in &self.fields {
            match parsed_keys.get(&field.name) {
                Some(Value::String(text)) => texts.push(FieldText {
                    field: &field.name,
                    text,
                    range: field.value_range.clone(),
                }),
                Some(Value::Sequence(items)) => {
                    let ranges = self.sequence_item_ranges(field, items);
                    for (item, range) in items.iter().zip(ranges) {
                        if let Value::String(text) = item {
                            texts.push(FieldText {
                                field: &field.name,
                                text,
                                range,
                            });
                        }
                    }
                }
                _ => {}
            }
        }
        texts
    }

    /// Every frontmatter string whose source bytes carry it literally, in
    /// document order, each with where its text begins in the source.
    ///
    /// These are the strings a token can be located in by offset, which is
    /// what [`Document::frontmatter_wikilinks`] reports and what a link
    /// rewrite writes into; see the former for which shapes are and are not.
    pub(crate) fn literal_texts(&self) -> Vec<LiteralText<'_>> {
        self.field_texts()
            .into_iter()
            .filter_map(|text| {
                let range = text.range?;
                let offset = literal_text_offset(&self.source[range.clone()], text.text)?;
                Some(LiteralText {
                    text: text.text,
                    start: range.start + offset,
                })
            })
            .collect()
    }

    /// The source bytes of each item of a block-style sequence field, one
    /// entry per item.
    ///
    /// A flow sequence reports no ranges: quotes, nesting and trailing content
    /// defeat a byte scan of one, which is the same reason a flow value has no
    /// value span. A block sequence whose scanned item count disagrees with
    /// the parsed one reports none either — a disagreement is refused, never
    /// guessed at.
    ///
    /// The scan cuts lines on [`crate::span`]'s break rule, so a
    /// `\r`-separated sequence is scanned item by item and agrees with the
    /// parse about how many items it holds, rather than reaching the refusal
    /// above for a sequence that is not ambiguous.
    fn sequence_item_ranges(&self, field: &Field, items: &[Value]) -> Vec<Option<Range<usize>>> {
        let absent = vec![None; items.len()];
        if field.style != ValueStyle::BlockSequence {
            return absent;
        }
        let mut scanned = Vec::new();
        let mut line_start = field.line_range.start;
        for line in split_lines_inclusive(&self.source[field.line_range.clone()]) {
            let trimmed = line.trim_end_matches(['\r', '\n']);
            let indent = trimmed.len() - trimmed.trim_start().len();
            if trimmed.trim_start().starts_with("- ") || trimmed.trim_start() == "-" {
                let after_dash = indent + 1;
                let (range, _, _) = classify_value(line_start, after_dash, &trimmed[after_dash..]);
                scanned.push(range);
            }
            line_start += line.len();
        }
        if scanned.len() != items.len() {
            return absent;
        }
        scanned
            .into_iter()
            .zip(items)
            .map(|(range, item)| {
                let range = range?;
                (reparse(&self.source[range.clone()]).as_ref() == Some(item)).then_some(range)
            })
            .collect()
    }

    /// One CommonMark reading of the body. Offsets it reports are relative to
    /// [`Document::body`]; the accessors on this type report the same
    /// constructs in source coordinates.
    pub fn scan_body(&self) -> BodyScan<'a> {
        BodyScan::new(self.body)
    }

    /// Every heading in the body, in source coordinates.
    ///
    /// There are two origins in this crate and they are one `body_start`
    /// apart: a [`BodyScan`] answers about the body it was built from, and a
    /// `Document` answers about the bytes it was read from. Rebasing by hand
    /// is how an offset from one origin gets used against the other, so the
    /// rebase is here and a caller never adds anything.
    pub fn headings(&self) -> Vec<Heading> {
        let scan = self.scan_body();
        let mut cursor = LineCursor::new(self.source);
        scan.headings()
            .iter()
            .map(|heading| Heading {
                span: cursor.span_at(heading.span.byte_offset + self.body_start),
                body_offset: heading.body_offset + self.body_start,
                ..heading.clone()
            })
            .collect()
    }

    /// Every `[[…]]` token in the body, code excluded, in source coordinates.
    pub fn wikilinks(&self) -> Vec<Link> {
        self.rebased(self.scan_body().wikilinks())
    }

    /// Every link token in the body, both families, in document order, code
    /// excluded, in source coordinates.
    pub fn links(&self) -> Vec<Link> {
        self.rebased(self.scan_body().links())
    }

    /// Every `#tag` in the body, in document order, in source coordinates.
    ///
    /// Frontmatter tags are a separate answer: see
    /// [`Document::frontmatter_tags`].
    pub fn tags(&self) -> Vec<Tag> {
        let mut cursor = LineCursor::new(self.source);
        self.scan_body()
            .tags()
            .into_iter()
            .map(|tag| Tag {
                span: tag
                    .span
                    .map(|span| cursor.span_at(span.byte_offset + self.body_start)),
                ..tag
            })
            .collect()
    }

    /// The tags the frontmatter `tags` field declares, in document order.
    ///
    /// Every string the field holds is read — the items of a sequence, or the
    /// field's own value when it is a scalar string — under the same grammar
    /// body tags follow, with the `#` marker optional: `#foo` and `foo` are
    /// both the tag `foo`. A string the grammar does not describe produces no
    /// tag and no complaint; judging it is validation's venue, not this
    /// crate's. No other field is scanned, and no field is scanned for body
    /// tokens.
    pub fn frontmatter_tags(&self) -> Vec<Tag> {
        let mut cursor = LineCursor::new(self.source);
        self.field_texts()
            .into_iter()
            .filter(|text| text.field == TAGS_FIELD)
            .filter_map(|text| {
                let name = frontmatter_tag_name(text.text)?;
                Some(Tag {
                    name,
                    span: text.range.map(|range| cursor.span_at(range.start)),
                })
            })
            .collect()
    }

    /// Every `[[…]]` token written in a frontmatter string value, in source
    /// coordinates.
    ///
    /// The counterpart to [`Document::frontmatter_tags`], and the other half
    /// of what a frontmatter value is scanned for: every string the block
    /// holds is read — scalar values and the string items of sequences, not
    /// just one field — because writing the wikilink form is what opts a
    /// property into the link graph, whichever property it is. A
    /// `[title](target)` string is inert text here; the Markdown form is body
    /// syntax.
    ///
    /// **A link is reported when the entry's source bytes carry its value
    /// literally** — a plain scalar, or one wrapped in a single pair of quotes
    /// — because that is the only case where an offset in the parsed string is
    /// an offset in the document, which is what makes the span exact and
    /// [`Link::range`] index the source.
    ///
    /// An entry whose bytes and whose parsed string are *different text*
    /// reports nothing here. That refusal covers the flow sequence's items and
    /// the flow value, which have no nameable bytes at all; the escaped scalar,
    /// where `"[[X]]"` and `[[X]]` share no offsets; the doubled quote of
    /// `'it''s'`; the block scalar and the folded scalar (`|`, `>`); the
    /// multi-line quoted scalar; and the nested map, whose strings are not
    /// top-level entries. Locating a token by searching the source for its text
    /// instead is guessing, and guessing wrong is silent: an escaped token
    /// whose text matches a later literal one claims that one's bytes and the
    /// literal link disappears. Reading these shapes without a span is what
    /// [`Document::field_texts`] and [`crate::parse_wikilinks_in_text`] are
    /// for.
    pub fn frontmatter_wikilinks(&self) -> Vec<Link> {
        let mut cursor = LineCursor::new(self.source);
        let mut links = Vec::new();
        for literal in self.literal_texts() {
            // Every token's offset in the entry's text is its offset in the
            // source, one quote apart, so no token is searched for and two
            // identical links in one value are two entries at two offsets.
            for link in parse_wikilinks_in_text(literal.text) {
                links.push(Link {
                    span: cursor.span_at(literal.start + link.span.byte_offset),
                    ..link
                });
            }
        }
        links
    }

    /// Rebase body-relative link spans onto the source.
    fn rebased(&self, links: Vec<Link>) -> Vec<Link> {
        let mut cursor = LineCursor::new(self.source);
        links
            .into_iter()
            .map(|link| Link {
                span: cursor.span_at(link.span.byte_offset + self.body_start),
                ..link
            })
            .collect()
    }

    /// Write `value` at `field`, returning the whole edited document.
    ///
    /// Only the field's own bytes move. An absent field is appended before the
    /// closing delimiter; a document with no block at all gets one, placed
    /// after any byte-order mark. The result is re-read before it is returned,
    /// and an edit that does not read back as intended refuses.
    ///
    /// `value` may be any shape the model holds, over whatever the field
    /// holds. A scalar over a scalar replaces the value's bytes, and a flat
    /// sequence over a flat sequence rewrites the list where it stands in the
    /// author's flow or block spelling. Every other set — one changing what
    /// the field holds, or writing or replacing a nested value — replaces the
    /// field's whole entry, a collection written in block style, and refuses
    /// with [`EditError::CommentWouldBeLost`] where the entry carries a
    /// comment it would drop. A field already holding `value` — equal under
    /// the value model's equality, at every depth — is left as it is written,
    /// and the document comes back unchanged, even from a block whose entries
    /// no other set could locate.
    ///
    /// Growing the block past [`FRONTMATTER_MAX_BYTES`] refuses too, and with
    /// its own error: past the bound no read turns the block back into fields,
    /// which is the bound speaking rather than the splice going wrong.
    pub fn set_field(&self, field: &str, value: &Value) -> Result<String, EditError> {
        let edited = self.spliced_set(field, value)?;
        refuse_past_bound(&edited)?;
        let mut expected = self.mapping()?.unwrap_or_default();
        expected.insert(field, value.clone());
        self.verify(&edited, field, &expected)?;
        Ok(edited)
    }

    /// Remove `field`, returning the whole edited document.
    ///
    /// The field's whole entry goes, continuation lines included. The blank
    /// lines and comments around it stay: they are the document's, not the
    /// field's.
    pub fn remove_field(&self, field: &str) -> Result<String, EditError> {
        if self.frontmatter_broken() {
            return Err(EditError::FrontmatterUnreadable);
        }
        if let Some(cause) = &self.split_refusal {
            return Err(EditError::FrontmatterNotEditable {
                cause: cause.clone(),
            });
        }
        let Some(located) = self.field(field) else {
            return Err(self.absent_or_not_editable(field));
        };
        let mut edited = String::with_capacity(self.source.len() - located.line_range.len());
        edited.push_str(&self.source[..located.line_range.start]);
        edited.push_str(&self.source[located.line_range.end..]);
        let mut expected = self.mapping()?.unwrap_or_default();
        expected.remove(field);
        self.verify(&edited, field, &expected)?;
        Ok(edited)
    }

    /// Replace a section's content, returning the whole edited document.
    ///
    /// The heading and the blank lines separating it from its neighbours are
    /// not the section's content and are left where they are. An empty
    /// `content` empties the section without touching its heading.
    ///
    /// **Every line the splice writes carries the document's terminator**,
    /// `content`'s own lines included: each break in `content` — `\n`, `\r\n`
    /// or a lone `\r` — is rewritten to [`LineEnding::of`]'s classification of
    /// the document, which is the spelling of the document's own first break.
    /// A document holding no break at all classifies as `Lf`.
    ///
    /// **Bytes outside the addressed range keep their spelling**, whatever
    /// they are broken by. The splice rewrites the section's content and
    /// nothing above or below it.
    ///
    /// The result is re-read before it is returned. A replace that moved the
    /// frontmatter, lost a heading the body already had, or produced a section
    /// that does not read back as the content it was given refuses and returns
    /// nothing — the same bargain a field edit makes, for the same reason:
    /// content is arbitrary Markdown, and an unclosed fence in it swallows
    /// everything below.
    pub fn replace_section(
        &self,
        address: impl Into<SectionAddress<'a>>,
        content: &str,
    ) -> Result<String, EditError> {
        let address = address.into();
        let (scan, span) = self.editable_section(address)?;
        let start = self.body_start + span.content_start;
        let end = self.body_start + span.content_end;

        let terminator = self.line_ending.as_str();
        let mut replacement = String::new();
        if !content.is_empty() {
            // A splice into a point that is not at the start of a line — a
            // heading at end of file with no trailing newline — needs one, or
            // the content welds onto the heading. Whether the byte before the
            // splice ends a line is the crate's break rule: a lone `\r` ends
            // one, so a document written with them already has its separator
            // and gains no second one.
            if start > 0 && trailing_break(&self.source[..start]).is_none() {
                replacement.push_str(terminator);
            }
            append_with_terminator(&mut replacement, content, self.line_ending);
            // An empty section's content range collapses onto the next
            // heading, so the separator the section had sits above the splice
            // and none is left below it. Restoring one is what keeps the
            // heading below from being jammed against written content — and,
            // where that heading is setext, from being absorbed into it.
            if span.content_start == span.content_end
                && span.content_start == span.end
                && span.body_start < span.end
                && end < self.source.len()
            {
                replacement.push_str(terminator);
            }
        }

        let mut edited =
            String::with_capacity(self.source.len() - (end - start) + replacement.len());
        edited.push_str(&self.source[..start]);
        edited.push_str(&replacement);
        edited.push_str(&self.source[end..]);
        self.verify_section(
            &edited,
            address,
            content,
            scan.headings(),
            span.content_start..span.content_end,
        )?;
        Ok(edited)
    }

    /// Append `content` to the end of a section, returning the whole edited
    /// document.
    ///
    /// The content lands below the section's last non-blank line — below its
    /// subsections, which are the section's too — and above the blank lines
    /// separating it from the next heading, so the document's blank structure
    /// stands. An empty section is written as [`Document::replace_section`]
    /// writes one, between its separators. Empty `content` changes nothing.
    ///
    /// Every line written carries the document's terminator, as a replace's
    /// do. The result is re-read before it is returned, and refuses unless
    /// the body's headings are exactly the ones it had with the content's own
    /// headings at the end of the section — an underline that makes the
    /// section's last line a heading returns nothing — and the section still
    /// ends with `content`, which content opening a heading at the section's
    /// level or above does not.
    pub fn append_to_section(
        &self,
        address: impl Into<SectionAddress<'a>>,
        content: &str,
    ) -> Result<String, EditError> {
        let address = address.into();
        let (scan, span) = self.editable_section(address)?;
        if span.content_start == span.content_end {
            return self.replace_section(address, content);
        }
        if content.is_empty() {
            return Ok(self.source.to_string());
        }
        let edited = self.proven_insert(&scan, address, span.content_end, content)?;
        let had = &self.body[span.content_start..span.content_end];
        let expected = format!("{}\n{content}", had.trim_end_matches(['\n', '\r']));
        self.verify_section(
            &edited,
            address,
            &expected,
            scan.headings(),
            span.content_end..span.content_end,
        )?;
        Ok(edited)
    }

    /// Replace the body — everything after the frontmatter block — with
    /// `content`, returning the whole edited document.
    ///
    /// The block stays byte-identical. A document without one is all body and
    /// is rewritten whole, except a byte-order mark, which stays its first
    /// bytes. Every line written carries the document's terminator, and empty
    /// `content` leaves the block alone with no body below it.
    ///
    /// A closing delimiter that ends the file without a terminator gains one,
    /// so the body starts on a line of its own.
    ///
    /// A block that cannot be read refuses: where its closing delimiter is
    /// missing, nothing separates its fields from the body. The result is
    /// re-read before it is returned, and refuses unless the block reads as it
    /// did — the same block, as readable as it was — and the body reads as
    /// `content`: content opening with a delimiter would otherwise make the
    /// body a block, closed or not.
    pub fn replace_body(&self, content: &str) -> Result<String, EditError> {
        if self.frontmatter_broken() {
            return Err(EditError::FrontmatterUnreadable);
        }
        let start = if self.byte_order_mark {
            self.body_start.max(BOM.len())
        } else {
            self.body_start
        };
        let mut edited = self.source[..start].to_string();
        edited.push_str(self.break_before(start));
        let written = edited.len();
        append_with_terminator(&mut edited, content, self.line_ending);
        let reread = Document::parse(&edited);
        if !self.same_block(&reread) || !same_lines(&edited[written..], content) {
            return Err(EditError::BodyPostImageMismatch);
        }
        Ok(edited)
    }

    /// The terminator a write at the source offset `at` needs first so that
    /// it starts a line: none where `at` already starts one, and none at the
    /// document's first line, which a byte-order mark does not end.
    fn break_before(&self, at: usize) -> &'static str {
        let first_line = if self.byte_order_mark { BOM.len() } else { 0 };
        if at > first_line && trailing_break(&self.source[..at]).is_none() {
            self.line_ending.as_str()
        } else {
            ""
        }
    }

    /// Whether `reread` — this document after a body or section write —
    /// carries the frontmatter block this one does: the same value, from the
    /// same bytes, read or refused the same way. A body or section write never
    /// creates, removes or breaks a block, and a block broken by the write
    /// reads as `None` exactly as a missing one does, so the value alone
    /// cannot tell.
    fn same_block(&self, reread: &Document<'_>) -> bool {
        reread.frontmatter() == self.frontmatter()
            && reread.frontmatter_range() == self.frontmatter_range()
            && reread.frontmatter_refusal() == self.frontmatter_refusal()
            && reread.frontmatter_broken() == self.frontmatter_broken()
    }

    /// Insert `content` directly above a heading's line, returning the whole
    /// edited document.
    ///
    /// The blank lines above the heading stay above the content, and nothing
    /// is added between the content and the heading: separators the caller
    /// wants are the caller's to write. Empty `content` changes nothing. Every
    /// line written carries the document's terminator.
    ///
    /// The result is re-read before it is returned, and refuses unless the
    /// body's headings are the ones it had with the content's own headings
    /// between them, in order — a line written above a setext heading joins
    /// its title and returns nothing.
    pub fn insert_before_heading(
        &self,
        address: impl Into<SectionAddress<'a>>,
        content: &str,
    ) -> Result<String, EditError> {
        self.insert_at_heading(address.into(), content, |body, span| {
            line_start(body, span.heading_start)
        })
    }

    /// Insert `content` directly below a heading's line — below a setext
    /// heading's underline — returning the whole edited document.
    ///
    /// The blank lines between the heading and the section's content stay
    /// below the inserted content. Otherwise it writes, and refuses, as
    /// [`Document::insert_before_heading`] does.
    pub fn insert_after_heading(
        &self,
        address: impl Into<SectionAddress<'a>>,
        content: &str,
    ) -> Result<String, EditError> {
        self.insert_at_heading(address.into(), content, |_, span| span.body_start)
    }

    /// Insert `content` as whole lines at the body offset `point` picks out
    /// of the addressed section, and prove the headings came through.
    fn insert_at_heading(
        &self,
        address: SectionAddress<'a>,
        content: &str,
        point: impl Fn(&str, &SectionSpan) -> usize,
    ) -> Result<String, EditError> {
        let (scan, span) = self.editable_section(address)?;
        if content.is_empty() {
            return Ok(self.source.to_string());
        }
        self.proven_insert(&scan, address, point(self.body, &span), content)
    }

    /// Insert `content` as whole lines at the body offset `at`, and refuse
    /// unless the result keeps the frontmatter block and its headings are
    /// exactly the body's own with the content's headings at `at`: the one
    /// proof every insert makes, an append included.
    fn proven_insert(
        &self,
        scan: &BodyScan<'_>,
        address: SectionAddress<'_>,
        at: usize,
        content: &str,
    ) -> Result<String, EditError> {
        let edited = self.insert_lines(at, content);
        let (above, below): (Vec<&Heading>, Vec<&Heading>) = scan
            .headings()
            .iter()
            .partition(|heading| heading.span.byte_offset < at);
        let inserted = BodyScan::new(content).headings().to_vec();
        let expected = above
            .into_iter()
            .chain(&inserted)
            .chain(below)
            .map(heading_key)
            .collect();
        self.verify_headings(&edited, address, expected)?;
        Ok(edited)
    }

    /// Delete a section — its heading line and everything it owns, its
    /// subsections included — returning the whole edited document.
    ///
    /// The blank lines below the section go with it, because they sit inside
    /// its range; the ones above its heading are the section before's and
    /// stay. The result is re-read before it is returned, and refuses unless
    /// the body's headings are exactly the ones it had, less the deleted
    /// ones, in order: a delete that fuses the text above it into the heading
    /// below changes that heading and returns nothing.
    pub fn delete_section(
        &self,
        address: impl Into<SectionAddress<'a>>,
    ) -> Result<String, EditError> {
        let address = address.into();
        let (scan, span) = self.editable_section(address)?;
        let deleted = line_start(self.body, span.heading_start)..span.end;
        let edited = splice(
            self.source,
            self.body_start + deleted.start..self.body_start + deleted.end,
            "",
        );
        let expected = scan
            .headings()
            .iter()
            .filter(|heading| !deleted.contains(&heading.span.byte_offset))
            .map(heading_key)
            .collect();
        self.verify_headings(&edited, address, expected)?;
        Ok(edited)
    }

    /// The section `address` names, with the scan it was resolved over,
    /// refused where its heading sits inside a container: every section write
    /// resolves through here, so they agree about where a section is and
    /// which ones they may touch.
    fn editable_section(
        &self,
        address: SectionAddress<'a>,
    ) -> Result<(BodyScan<'a>, SectionSpan), EditError> {
        let scan = self.scan_body();
        let span = scan.resolve_section(address)?;
        if scan.headings()[span.heading].inside_container {
            return Err(EditError::SectionInContainer {
                heading: address.heading.to_string(),
            });
        }
        Ok((scan, span))
    }

    /// The document with `content` inserted as whole lines at `at`, a body
    /// offset, every line carrying the document's terminator.
    ///
    /// A point that does not start a line — the end of a last line with no
    /// terminator — gains one first, so the content never welds onto the line
    /// above. A byte-order mark is the start of the first line, not a line.
    fn insert_lines(&self, at: usize, content: &str) -> String {
        let at = self.body_start + at;
        let mut lines = self.break_before(at).to_string();
        append_with_terminator(&mut lines, content, self.line_ending);
        splice(self.source, at..at, &lines)
    }

    /// The section a heading owns, in source coordinates.
    pub fn resolve_section(
        &self,
        address: impl Into<SectionAddress<'a>>,
    ) -> Result<SectionSpan, SectionError> {
        let span = self.scan_body().resolve_section(address.into())?;
        let shift = self.body_start;
        Ok(SectionSpan {
            heading: span.heading,
            heading_start: span.heading_start + shift,
            body_start: span.body_start + shift,
            content_start: span.content_start + shift,
            content_end: span.content_end + shift,
            end: span.end + shift,
        })
    }

    /// Append `value` to the list `field` holds, returning the whole edited
    /// document.
    ///
    /// A value the list already holds is appended again. A field holding a
    /// scalar or a map refuses with [`EditError::FieldNotAList`]: turning it
    /// into a list is a set.
    ///
    /// `value` may be any shape the model holds; a collection is written in
    /// block style, as [`Document::set_field`] writes one.
    ///
    /// **Every byte the push does not change stays.** A block list whose
    /// items each re-read alone as themselves — on one line or several, a map
    /// or a nested list included — gains one item below the last line of its
    /// last item, at that item's indent and with its line terminator on every
    /// line; comments, the other items' quoting and layout and the key's
    /// spelling are not touched. Every other list — a flow list, a block list
    /// whose items cannot be proven one by one — and an absent or null field,
    /// which becomes a one-element list, are written whole by
    /// [`Document::set_field`], and only where the entry carries no comment: a
    /// comment the rewrite would drop refuses with
    /// [`EditError::CommentWouldBeLost`] instead. Either way the result is
    /// re-read and proven as a set is.
    pub fn push_to_list(&self, field: &str, value: &Value) -> Result<String, EditError> {
        let mut items = self.list_items(field)?.unwrap_or_default();
        let lines = self
            .field(field)
            .and_then(|located| block_item_lines(self.source, located, &items));
        let Some(last) = lines.as_ref().and_then(|lines| lines.last()) else {
            items.push(value.clone());
            return self.rewrite_list(field, items);
        };
        let terminator =
            trailing_break(&self.source[last.lines.clone()]).unwrap_or(self.line_ending.as_str());
        let item = render_block_item(value, &self.source[last.indent.clone()], terminator)?;
        let edited = splice(self.source, last.lines.end..last.lines.end, &item);
        refuse_past_bound(&edited)?;
        items.push(value.clone());
        self.verified_list(edited, field, items)
    }

    /// Write `items` over `field` whole, as [`Document::set_field`] does, where
    /// the field's entry carries no comment the rewrite would drop.
    fn rewrite_list(&self, field: &str, items: Vec<Value>) -> Result<String, EditError> {
        if let Some(located) = self.field(field)
            && self.entry_carries_comment(located)
        {
            return Err(EditError::CommentWouldBeLost {
                field: field.to_string(),
            });
        }
        self.set_field(field, &Value::Sequence(items))
    }

    /// `edited`, where it re-reads with `field` holding exactly `items` and
    /// every other field untouched.
    fn verified_list(
        &self,
        edited: String,
        field: &str,
        items: Vec<Value>,
    ) -> Result<String, EditError> {
        let mut expected = self.mapping()?.unwrap_or_default();
        expected.insert(field, Value::Sequence(items));
        self.verify(&edited, field, &expected)?;
        Ok(edited)
    }

    /// Remove every element equal to `value` from the list `field` holds,
    /// returning the whole edited document.
    ///
    /// Popping the last element leaves the field holding an empty list; the
    /// field stays. Nothing is silently left as it was: a value the list does
    /// not hold refuses with [`EditError::ListValueAbsent`], an absent field
    /// with [`EditError::FieldAbsent`], and a field holding a scalar or a map
    /// with [`EditError::FieldNotAList`].
    ///
    /// **Every byte the pop does not change stays.** From a block list whose
    /// items each re-read alone as themselves, on one line or several, the
    /// lines of the matching items are deleted and nothing else; popping its
    /// last item writes `[]` on the key line, before any comment there,
    /// because a key with nothing under it reads as null and `[]` reads back
    /// as the empty list. A comment on a matching item's own lines — trailing
    /// one of them, or between two of them — goes with those lines: it
    /// annotates the item being removed. Comments on every other line stay.
    /// Every other list is written whole by [`Document::set_field`], only
    /// where its entry carries no comment; a comment that rewrite would drop
    /// refuses with [`EditError::CommentWouldBeLost`]. Either way the result
    /// is re-read and proven as a set is.
    pub fn pop_from_list(&self, field: &str, value: &Value) -> Result<String, EditError> {
        let Some(items) = self.list_items(field)? else {
            return Err(EditError::FieldAbsent {
                field: field.to_string(),
            });
        };
        let kept: Vec<Value> = items
            .iter()
            .filter(|item| *item != value)
            .cloned()
            .collect();
        if kept.len() == items.len() {
            return Err(EditError::ListValueAbsent {
                field: field.to_string(),
                value: value.clone(),
            });
        }
        let Some((located, lines)) = self.field(field).and_then(|located| {
            block_item_lines(self.source, located, &items).map(|lines| (located, lines))
        }) else {
            return self.rewrite_list(field, kept);
        };
        let popped: Vec<&BlockItem> = lines
            .iter()
            .zip(&items)
            .filter(|(_, item)| *item == value)
            .map(|(lines, _)| lines)
            .collect();
        let mut edits: Vec<(Range<usize>, &str)> = Vec::new();
        if kept.is_empty() {
            let Some(point) = key_line_value_point(self.source, located.line_range.start) else {
                return self.rewrite_list(field, kept);
            };
            edits.push((point, " []"));
        }
        edits.extend(popped.iter().map(|item| (item.lines.clone(), "")));
        self.verified_list(splice_all(self.source, &edits), field, kept)
    }

    /// The items of the list `field` holds — none for a field written with no
    /// value — or `None` where the block has no such field. A field holding
    /// anything else refuses.
    fn list_items(&self, field: &str) -> Result<Option<Vec<Value>>, EditError> {
        match self.mapping_ref()?.and_then(|map| map.get(field)) {
            None => Ok(None),
            Some(Value::Null) => Ok(Some(Vec::new())),
            Some(Value::Sequence(items)) => Ok(Some(items.clone())),
            Some(other) => Err(EditError::FieldNotAList {
                field: field.to_string(),
                kind: other.kind(),
            }),
        }
    }

    /// The frontmatter mapping, or `None` for an absent or null block.
    fn mapping(&self) -> Result<Option<Mapping>, EditError> {
        Ok(self.mapping_ref()?.cloned())
    }

    /// The frontmatter mapping, borrowed, or `None` for an absent or null
    /// block.
    fn mapping_ref(&self) -> Result<Option<&Mapping>, EditError> {
        match (&self.frontmatter, &self.frontmatter_range) {
            (Some(Value::Map(map)), _) => Ok(Some(map)),
            (Some(Value::Null), _) | (None, None) if !self.frontmatter_broken() => Ok(None),
            (Some(other), _) => Err(EditError::FrontmatterNotAMapping { kind: other.kind() }),
            _ => Err(EditError::FrontmatterUnreadable),
        }
    }

    /// Whether the document opens a block that cannot be read: an unclosed
    /// delimiter, or content that does not parse.
    fn frontmatter_broken(&self) -> bool {
        self.frontmatter.is_none()
            && (self.frontmatter_range.is_some()
                || self
                    .diagnostics
                    .iter()
                    .any(|diagnostic| diagnostic.code == DiagnosticCode::FrontmatterUnclosed))
    }

    fn absent_or_not_editable(&self, field: &str) -> EditError {
        let present = matches!(&self.frontmatter, Some(Value::Map(map)) if map.contains_key(field));
        if present {
            EditError::FieldNotEditable {
                field: field.to_string(),
            }
        } else {
            EditError::FieldAbsent {
                field: field.to_string(),
            }
        }
    }

    /// Whether `located`'s entry carries a comment a whole-entry rewrite would
    /// drop, on [`entry_carries_comment`]'s reading. A located field always
    /// sits in a block; were one ever to sit in none, nothing could prove it
    /// comment-free, so it would carry one.
    fn entry_carries_comment(&self, located: &Field) -> bool {
        self.frontmatter_range
            .clone()
            .is_none_or(|block| entry_carries_comment(self.source, block, located))
    }

    /// The edited bytes, before they are proven.
    fn spliced_set(&self, field: &str, value: &Value) -> Result<String, EditError> {
        if self.frontmatter_broken() {
            return Err(EditError::FrontmatterUnreadable);
        }
        // A field already holding `value` already says what the set asks for,
        // so nothing is written: a re-spelling would be a change of nothing.
        // Nothing in the way of a rewrite is in the way of no write — not a
        // comment the rewrite would drop, nor a block whose entries cannot be
        // located — so this answers before either refuses.
        if let Some(Value::Map(map)) = &self.frontmatter
            && map.get(field) == Some(value)
        {
            return Ok(self.source.to_string());
        }
        if let Some(cause) = &self.split_refusal {
            return Err(EditError::FrontmatterNotEditable {
                cause: cause.clone(),
            });
        }
        match &self.frontmatter {
            Some(Value::Map(_)) | Some(Value::Null) | None => {}
            Some(other) => return Err(EditError::FrontmatterNotAMapping { kind: other.kind() }),
        }

        if let Some(located) = self.field(field) {
            return self.splice_existing(located, value);
        }

        let entry = render_entry(field, value, self.line_ending)?;
        let terminator = self.line_ending.as_str();
        match &self.frontmatter_range {
            // Append before the closing delimiter. A null block — `---\n---\n`
            // — has an empty range there, so writing a field into it promotes
            // it to a mapping.
            Some(range) => Ok(splice(self.source, range.end..range.end, &entry)),
            // No block at all. It lands after any byte-order mark, never above
            // it, so the mark stays the document's first bytes.
            None => {
                let at = if self.byte_order_mark { BOM.len() } else { 0 };
                let block = format!("---{terminator}{entry}---{terminator}");
                Ok(splice(self.source, at..at, &block))
            }
        }
    }

    fn splice_existing(&self, located: &Field, value: &Value) -> Result<String, EditError> {
        let held = match &self.frontmatter {
            Some(Value::Map(map)) => map.get(&located.name),
            _ => None,
        };

        // A scalar over a scalar replaces the value's bytes, keeping the
        // author's quoting where the new value permits it. A scalar no span
        // names — a block scalar, an anchored or tagged one — is not
        // replaceable in place.
        if let Some(scalar) = Scalar::of(value)
            && !held.is_some_and(is_collection)
        {
            let (Some(range), Some(style)) = (&located.value_range, ScalarStyle::of(located.style))
            else {
                return Err(EditError::FieldNotEditable {
                    field: located.name.clone(),
                });
            };
            let mut rendered = render_scalar_in_span(scalar, style)?;
            if located.style == ValueStyle::EmptyValue {
                // The span is the point just past the colon, so the separating
                // space is part of what the splice writes.
                rendered.insert(0, ' ');
            }
            return Ok(splice(self.source, range.clone(), &rendered));
        }

        // A flat sequence over a flat sequence rewrites the list where it
        // stands, keeping the author's flow or block spelling; a comment
        // inside the entry goes with it.
        let flat_list = |value: &Value| matches!(value, Value::Sequence(_)) && !is_nested(value);
        if let Value::Sequence(items) = value
            && located.style.is_sequence()
            && let Some(scalars) = Scalar::all(items)
            && held.is_some_and(flat_list)
        {
            let entry = if located.style == ValueStyle::FlowSequence {
                format!(
                    "{}: {}{}",
                    render_key(&located.name)?,
                    render_flow_sequence(&scalars)?,
                    self.line_ending.as_str()
                )
            } else {
                render_entry(&located.name, value, self.line_ending)?
            };
            return Ok(splice(self.source, located.line_range.clone(), &entry));
        }

        // Every other set changes what the field holds — a collection over a
        // scalar, a stub or another shape of collection, a scalar over a
        // collection — or writes a nested value, and replaces the whole entry,
        // the collection in block style. A comment anywhere in the entry, its
        // key line included, would be dropped silently, so it refuses instead.
        if self.entry_carries_comment(located) {
            return Err(EditError::CommentWouldBeLost {
                field: located.name.clone(),
            });
        }
        let entry = render_entry(&located.name, value, self.line_ending)?;
        Ok(splice(self.source, located.line_range.clone(), &entry))
    }

    /// Re-read the edited bytes and refuse unless the frontmatter is exactly
    /// the intended mapping — the edited field as asked for, and every other
    /// field, its order included, untouched.
    ///
    /// This is where the fidelity invariant is actually enforced. Every layer
    /// below reasons about where a construct's bytes are; this one asks the
    /// reader what the bytes now say and compares it against what was asked
    /// for. A span computed one line off, a quoting escalation that changed a
    /// neighbour, a splice that closed a quote somewhere else — none of them
    /// can reach a caller through here, because none of them read back as the
    /// mapping that was intended.
    fn verify(&self, edited: &str, field: &str, expected: &Mapping) -> Result<(), EditError> {
        // The check is about the block, so only the block is re-read: locating
        // the edited document's fields would compute spans nothing here asks
        // for, at the cost of the scan that produced this edit in the first
        // place.
        let matches = match frontmatter_of(edited) {
            Some(Value::Map(map)) => &map == expected,
            Some(Value::Null) => expected.is_empty(),
            _ => false,
        };
        if matches {
            Ok(())
        } else {
            Err(EditError::PostImageMismatch {
                field: field.to_string(),
            })
        }
    }

    /// Re-read the bytes a section replace produced and refuse unless all four
    /// of these hold: the frontmatter mapping is the one that was there, the
    /// addressed heading still resolves, it now owns the content it was given,
    /// and every heading the body already had *outside the replaced range* is
    /// still a heading.
    ///
    /// The fourth is the one that catches the class: `content` is arbitrary
    /// Markdown, and an unclosed fence, an indented block or a line that turns
    /// the heading below it into a setext underline all swallow document
    /// structure without touching a byte the splice addressed.
    ///
    /// `replaced` — the addressed section's content range, in body
    /// coordinates — is what bounds it. A section owns its subsections, so
    /// replacing its content is allowed to remove them, and a heading the
    /// splice overwrote is not one this check may demand back. Everything
    /// above the range and below it is the document's, and all of it survives
    /// or the replace refuses.
    fn verify_section(
        &self,
        edited: &str,
        address: SectionAddress<'_>,
        content: &str,
        before: &[Heading],
        replaced: Range<usize>,
    ) -> Result<(), EditError> {
        let refuse = || EditError::SectionPostImageMismatch {
            heading: address.heading.to_string(),
        };
        let reread = Document::parse(edited);
        if !self.same_block(&reread) {
            return Err(refuse());
        }
        let scan = reread.scan_body();
        let span = scan.resolve_section(address).map_err(|_| refuse())?;
        let written = &reread.body()[span.content_start..span.content_end];
        if !same_lines(written, content) {
            return Err(refuse());
        }
        let after = scan.headings();
        let survives = |heading: &Heading| !replaced.contains(&heading.span.byte_offset);
        for heading in before.iter().filter(|heading| survives(heading)) {
            let had = before
                .iter()
                .filter(|other| survives(other) && same_heading(other, heading))
                .count();
            let wanted = after
                .iter()
                .filter(|other| same_heading(other, heading))
                .count();
            if wanted < had {
                return Err(refuse());
            }
        }
        Ok(())
    }

    /// Re-read the bytes a structural section edit produced and refuse unless
    /// the frontmatter is the one that was there and the body's headings are
    /// exactly `expected`, by level and text, in document order.
    ///
    /// An insert or a delete names every heading the result should have: the
    /// ones the body had, less what was deleted, plus what the inserted
    /// content carries at the point it went in. A heading swallowed by an
    /// unclosed fence, fused into a setext title, or conjured by an underline
    /// all break that sequence.
    fn verify_headings(
        &self,
        edited: &str,
        address: SectionAddress<'_>,
        expected: Vec<(u8, String)>,
    ) -> Result<(), EditError> {
        let reread = Document::parse(edited);
        let headings: Vec<(u8, String)> = reread
            .scan_body()
            .headings()
            .iter()
            .map(heading_key)
            .collect();
        if self.same_block(&reread) && headings == expected {
            Ok(())
        } else {
            Err(EditError::SectionPostImageMismatch {
                heading: address.heading.to_string(),
            })
        }
    }
}

/// A heading as a structural edit's post-image check compares it: its level
/// and its text. Position is not part of it, because an edit moves the bytes
/// below it.
fn heading_key(heading: &Heading) -> (u8, String) {
    (heading.level, heading.text.clone())
}

/// Where `text` begins inside the `written` bytes that produced it, when those
/// bytes carry it literally: zero for a plain scalar, one for a scalar wrapped
/// in a single pair of quotes.
///
/// `None` when the two are different text — an escape, a doubled quote, a
/// folded line — and no offset maps the parsed string onto the source at all.
/// The whole entry is refused rather than partly located: the two texts share a
/// prefix up to the first difference and nothing after it, so a token past that
/// point has no source position and the ones before it cannot be told apart
/// from the ones after.
fn literal_text_offset(written: &str, text: &str) -> Option<usize> {
    if written == text {
        return Some(0);
    }
    ['"', '\'']
        .into_iter()
        .filter_map(|quote| written.strip_prefix(quote)?.strip_suffix(quote))
        .any(|inner| inner == text)
        .then_some(1)
}

/// The frontmatter value `source` holds, without locating its fields.
fn frontmatter_of(source: &str) -> Option<Value> {
    extract(source, &mut Vec::new()).value
}

/// Refuse `edited` when the block it carries is past
/// [`FRONTMATTER_MAX_BYTES`].
///
/// The block is measured by its delimiters rather than by re-reading it,
/// because a block past the bound is exactly the one no read produces a value
/// for: asking the reader would only say the fields are gone, and this says
/// which rule took them.
fn refuse_past_bound(edited: &str) -> Result<(), EditError> {
    match closed_block(edited) {
        Some(block) if block.yaml.len() > FRONTMATTER_MAX_BYTES => {
            Err(EditError::FrontmatterPastBound {
                bytes: block.yaml.len(),
                bound: FRONTMATTER_MAX_BYTES,
            })
        }
        _ => Ok(()),
    }
}

/// `source` with `range` replaced by `replacement`, allocated once at the size
/// the result actually is.
fn splice(source: &str, range: Range<usize>, replacement: &str) -> String {
    let mut out = String::with_capacity(source.len() - range.len() + replacement.len());
    out.push_str(&source[..range.start]);
    out.push_str(replacement);
    out.push_str(&source[range.end..]);
    out
}

/// `source` with each range in `edits` replaced by its text. The ranges are in
/// document order and do not overlap.
pub(crate) fn splice_all(source: &str, edits: &[(Range<usize>, &str)]) -> String {
    let mut out = String::with_capacity(source.len());
    let mut copied = 0;
    for (range, replacement) in edits {
        out.push_str(&source[copied..range.start]);
        out.push_str(replacement);
        copied = range.end;
    }
    out.push_str(&source[copied..]);
    out
}

/// Whether two headings are the same heading for survival purposes: same level
/// and same text. Position is deliberately not part of it — a splice moves the
/// bytes below it, and a heading that only moved is a heading that survived.
fn same_heading(left: &Heading, right: &Heading) -> bool {
    left.level == right.level && left.text == right.text
}

/// Whether two runs of text are the same lines.
///
/// Section content is compared the way a splice writes it: terminators are the
/// document's whatever the content arrived with, and the last line is
/// terminated whether or not it asked to be. Neither is a difference in what
/// the section says, so neither is a mismatch.
///
/// Both sides are cut on [`crate::span`]'s break rule, the same rule the splice
/// writes by, so the comparison is over the lines the splice produced rather
/// than over a run it held whole.
fn same_lines(left: &str, right: &str) -> bool {
    fn lines(text: &str) -> impl Iterator<Item = &str> {
        split_lines_inclusive(text.trim_end_matches(['\n', '\r']))
            .map(|line| line.trim_end_matches(['\n', '\r']))
    }
    lines(left).eq(lines(right))
}

/// Append `content` with every line terminated by `line_ending`, and terminate
/// the last line too.
///
/// Content arrives written however its author wrote it, and **the document's
/// own terminator wins**: every break in `content` — `\n`, `\r\n` or a lone
/// `\r` — is rewritten to `line_ending` on the way in. Splicing it verbatim
/// is how a CRLF document ends up with LF lines in the middle of it, which is
/// the same defect as a synthesized line with the wrong terminator and is
/// caught by nothing downstream. Cutting lines on [`crate::span`]'s break rule
/// is what makes the promise cover all three.
fn append_with_terminator(out: &mut String, content: &str, line_ending: LineEnding) {
    let terminator = line_ending.as_str();
    for line in split_lines_inclusive(content) {
        out.push_str(line.trim_end_matches(['\r', '\n']));
        out.push_str(terminator);
    }
}

/// Whether `content` re-reads as the same value it was written from — the
/// round-trip the emission layer proves for itself, exposed so a caller can
/// assert it too.
///
/// A block holding no fields is written `---\n---\n` and reads as null, and
/// the crate promotes that null to a mapping the moment a field is written
/// into it. So null and the empty mapping are the same block written twice,
/// and this predicate says so rather than reporting a round-trip failure for
/// a document that round-tripped.
pub fn frontmatter_reads_back(content: &str, expected: &Value) -> bool {
    match (frontmatter_of(content), expected) {
        (Some(Value::Null), Value::Map(map)) => map.is_empty(),
        (Some(actual), expected) => &actual == expected,
        (None, _) => false,
    }
}
