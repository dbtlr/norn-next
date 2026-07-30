//! A parsed document, and the edits that keep the rest of it byte-identical.

use std::fmt;
use std::ops::Range;

use crate::body::BodyScan;
use crate::diagnostic::Diagnostic;
use crate::frontmatter::extract::{BOM, extract};
use crate::frontmatter::fields::{Field, ValueStyle, classify_value, field_spans, reparse};
use crate::frontmatter::render::{
    RenderError, ScalarStyle, render_flow_sequence, render_key, render_scalar_entry,
    render_scalar_in_span, render_sequence_entry,
};
use crate::heading::Heading;
use crate::line_ending::LineEnding;
use crate::link::{Link, parse_wikilinks_in_text};
use crate::section::{SectionAddress, SectionError, SectionSpan};
use crate::span::LineCursor;
use crate::tag::{Tag, frontmatter_tag_name};
use crate::value::{Mapping, Value};

/// The one frontmatter field whose strings are read as tags.
const TAGS_FIELD: &str = "tags";

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
    /// addressable. Reads are unaffected; no edit is attempted.
    FrontmatterNotEditable,
    /// This field's value cannot be named as one span. Reads are unaffected;
    /// the edit is not attempted.
    FieldNotEditable {
        field: String,
    },
    FieldAbsent {
        field: String,
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
    /// The document a section replace produced does not read back as
    /// intended — the frontmatter moved, the addressed heading no longer
    /// resolves to the content it was given, or a heading the body already had
    /// is gone. Nothing is returned.
    SectionPostImageMismatch {
        heading: String,
    },
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
            EditError::FrontmatterNotEditable => f.write_str(
                "the frontmatter block's field spans cannot be trusted, so no field in it can be \
                 edited",
            ),
            EditError::FieldNotEditable { field } => {
                write!(f, "the field {field:?} cannot be edited in place")
            }
            EditError::FieldAbsent { field } => write!(f, "the field {field:?} is not present"),
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
    frontmatter_range: Option<Range<usize>>,
    body: &'a str,
    body_start: usize,
    fields: Vec<Field>,
    /// The block parsed and the field layer refused to split it, so no line in
    /// it is safely attributable to a field and every edit into it refuses.
    spans_untrusted: bool,
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
            _ => Some(Vec::new()),
        };
        let spans_untrusted = located.is_none();
        let fields = located.unwrap_or_default();
        if spans_untrusted {
            diagnostics.push(Diagnostic::warning(
                "frontmatter-not-editable",
                "the frontmatter block's field spans cannot be trusted, so field edits refuse; \
                 reading is unaffected",
            ));
        }
        Document {
            spans_untrusted,
            source,
            byte_order_mark: extraction.byte_order_mark,
            line_ending: LineEnding::of(source),
            frontmatter: extraction.value,
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
        let mut texts = Vec::new();
        for field in &self.fields {
            match map.get(&field.name) {
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

    /// The source bytes of each item of a block-style sequence field, one
    /// entry per item.
    ///
    /// A flow sequence reports no ranges: quotes, nesting and trailing content
    /// defeat a byte scan of one, which is the same reason a flow value has no
    /// value span. A block sequence whose scanned item count disagrees with
    /// the parsed one reports none either — a disagreement is refused, never
    /// guessed at.
    fn sequence_item_ranges(&self, field: &Field, items: &[Value]) -> Vec<Option<Range<usize>>> {
        let absent = vec![None; items.len()];
        if field.style != ValueStyle::BlockSequence {
            return absent;
        }
        let mut scanned = Vec::new();
        let mut line_start = field.line_range.start;
        for line in self.source[field.line_range.clone()].split_inclusive('\n') {
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
    /// **A link is reported when the entry's source bytes carry its token
    /// literally**, which is what makes the span exact and
    /// [`Link::range`] index the source. A value whose bytes and whose parsed
    /// string are different text — a flow sequence's items, which have no
    /// nameable bytes at all, or an escaped scalar — yields nothing here
    /// rather than a span that is not certainly right. Reading one without a
    /// span is what [`Document::field_texts`] and
    /// [`crate::parse_wikilinks_in_text`] are for.
    pub fn frontmatter_wikilinks(&self) -> Vec<Link> {
        let mut cursor = LineCursor::new(self.source);
        let mut links = Vec::new();
        for text in self.field_texts() {
            let Some(range) = text.range else {
                continue;
            };
            let written = &self.source[range.clone()];
            // The tokens are found in ascending order, so the search for each
            // one resumes where the last was found: two identical links in one
            // value are two entries at two offsets.
            let mut at = 0;
            for link in parse_wikilinks_in_text(text.text) {
                let Some(found) = written[at..].find(&link.raw) else {
                    continue;
                };
                at = at + found + link.raw.len();
                links.push(Link {
                    span: cursor.span_at(range.start + at - link.raw.len()),
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
    pub fn set_field(&self, field: &str, value: &Value) -> Result<String, EditError> {
        let edited = self.spliced_set(field, value)?;
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
        if self.spans_untrusted {
            return Err(EditError::FrontmatterNotEditable);
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
    /// `content` empties the section without touching its heading. Every line
    /// the splice writes uses the document's own terminator, `content`'s own
    /// lines included.
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
        let scan = self.scan_body();
        let span = scan.resolve_section(address)?;
        if scan.headings().iter().any(|heading| {
            heading.span.byte_offset == span.heading_start && heading.inside_container
        }) {
            return Err(EditError::SectionInContainer {
                heading: address.heading.to_string(),
            });
        }
        let start = self.body_start + span.content_start;
        let end = self.body_start + span.content_end;

        let terminator = self.line_ending.as_str();
        let mut replacement = String::new();
        if !content.is_empty() {
            // A splice into a point that is not at the start of a line — a
            // heading at end of file with no trailing newline — needs one, or
            // the content welds onto the heading.
            if start > 0 && !self.source[..start].ends_with('\n') {
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

    /// The section a heading owns, in source coordinates.
    pub fn resolve_section(
        &self,
        address: impl Into<SectionAddress<'a>>,
    ) -> Result<SectionSpan, SectionError> {
        let span = self.scan_body().resolve_section(address.into())?;
        let shift = self.body_start;
        Ok(SectionSpan {
            heading_start: span.heading_start + shift,
            body_start: span.body_start + shift,
            content_start: span.content_start + shift,
            content_end: span.content_end + shift,
            end: span.end + shift,
        })
    }

    /// The frontmatter mapping, or `None` for an absent or null block.
    fn mapping(&self) -> Result<Option<Mapping>, EditError> {
        match (&self.frontmatter, &self.frontmatter_range) {
            (Some(Value::Map(map)), _) => Ok(Some(map.clone())),
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
                    .any(|diagnostic| diagnostic.code == "frontmatter-unclosed"))
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

    /// The edited bytes, before they are proven.
    fn spliced_set(&self, field: &str, value: &Value) -> Result<String, EditError> {
        if self.frontmatter_broken() {
            return Err(EditError::FrontmatterUnreadable);
        }
        if self.spans_untrusted {
            return Err(EditError::FrontmatterNotEditable);
        }
        match &self.frontmatter {
            Some(Value::Map(_)) | Some(Value::Null) | None => {}
            Some(other) => return Err(EditError::FrontmatterNotAMapping { kind: other.kind() }),
        }

        if let Some(located) = self.field(field) {
            return self.splice_existing(located, value);
        }

        let entry = self.render_entry(field, value)?;
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

    fn render_entry(&self, field: &str, value: &Value) -> Result<String, EditError> {
        Ok(match value {
            Value::Sequence(items) => render_sequence_entry(field, items, self.line_ending)?,
            Value::Map(_) => {
                return Err(EditError::Render(RenderError::NonScalarValue {
                    kind: "map",
                }));
            }
            scalar => render_scalar_entry(field, scalar, self.line_ending)?,
        })
    }

    fn splice_existing(&self, located: &Field, value: &Value) -> Result<String, EditError> {
        // A sequence replaces the whole entry, keeping the author's flow or
        // block spelling. A stubbed field — `tags:` with nothing after it —
        // becomes a block sequence.
        if let Value::Sequence(items) = value
            && (located.style.is_sequence() || located.style == ValueStyle::EmptyValue)
        {
            let entry = if located.style == ValueStyle::FlowSequence {
                format!(
                    "{}: {}{}",
                    render_key(&located.name)?,
                    render_flow_sequence(items)?,
                    self.line_ending.as_str()
                )
            } else {
                render_sequence_entry(&located.name, items, self.line_ending)?
            };
            return Ok(splice(self.source, located.line_range.clone(), &entry));
        }

        let (Some(range), Some(style)) = (&located.value_range, ScalarStyle::of(located.style))
        else {
            return Err(EditError::FieldNotEditable {
                field: located.name.clone(),
            });
        };
        let mut rendered = render_scalar_in_span(value, style)?;
        if located.style == ValueStyle::EmptyValue {
            // The span is the point just past the colon, so the separating
            // space is part of what the splice writes.
            rendered.insert(0, ' ');
        }
        Ok(splice(self.source, range.clone(), &rendered))
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
        if reread.frontmatter() != self.frontmatter() {
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
}

/// The frontmatter value `source` holds, without locating its fields.
fn frontmatter_of(source: &str) -> Option<Value> {
    extract(source, &mut Vec::new()).value
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
fn same_lines(left: &str, right: &str) -> bool {
    fn lines(text: &str) -> impl Iterator<Item = &str> {
        text.trim_end_matches(['\n', '\r'])
            .split('\n')
            .map(|line| line.trim_end_matches('\r'))
    }
    lines(left).eq(lines(right))
}

/// Append `content` with every line terminated by `line_ending`, and terminate
/// the last line too.
///
/// Content arrives written however its author wrote it. Splicing it verbatim
/// is how a CRLF document ends up with LF lines in the middle of it, which is
/// the same defect as a synthesized line with the wrong terminator and is
/// caught by nothing downstream.
fn append_with_terminator(out: &mut String, content: &str, line_ending: LineEnding) {
    let terminator = line_ending.as_str();
    for line in content.split_inclusive('\n') {
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
