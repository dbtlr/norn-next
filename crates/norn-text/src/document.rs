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
use crate::line_ending::LineEnding;
use crate::section::{SectionAddress, SectionError, SectionSpan};
use crate::value::{Mapping, Value};

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
    /// The block's field spans cannot be trusted, or this field's value cannot
    /// be named as one span. Reads are unaffected; the edit is not attempted.
    FieldNotEditable {
        field: String,
    },
    FieldAbsent {
        field: String,
    },
    Render(RenderError),
    Section(SectionError),
    /// The edited document does not read back as intended. Nothing is
    /// returned: an unproven write is a refusal.
    PostImageMismatch {
        field: String,
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
            EditError::FieldNotEditable { field } => {
                write!(f, "the field {field:?} cannot be edited in place")
            }
            EditError::FieldAbsent { field } => write!(f, "the field {field:?} is not present"),
            EditError::Render(error) => write!(f, "{error}"),
            EditError::Section(error) => write!(f, "{error}"),
            EditError::PostImageMismatch { field } => write!(
                f,
                "the edited document does not read back with {field:?} as intended, so the edit \
                 was refused"
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
    diagnostics: Vec<Diagnostic>,
}

impl<'a> Document<'a> {
    /// Read `source`. Never fails: a malformed block is reported as a
    /// diagnostic and the body is still available.
    pub fn parse(source: &'a str) -> Self {
        let mut diagnostics = Vec::new();
        let extraction = extract(source, &mut diagnostics);
        let fields = match (&extraction.value, &extraction.range) {
            (Some(value), Some(range)) => {
                field_spans(source, range.clone(), value, extraction.strip)
            }
            _ => Vec::new(),
        };
        if let Some(Value::Map(map)) = &extraction.value
            && fields.len() != map.len()
        {
            diagnostics.push(Diagnostic::warning(
                "frontmatter-not-editable",
                "the frontmatter block's field spans cannot be trusted, so field edits refuse; \
                 reading is unaffected",
            ));
        }
        Document {
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

    /// The bytes this document was read from.
    pub fn source(&self) -> &'a str {
        self.source
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
    /// [`Document::body`]; add [`Document::body_start`] for source offsets.
    pub fn scan_body(&self) -> BodyScan<'a> {
        BodyScan::new(self.body)
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
        let Some(located) = self.field(field) else {
            return Err(self.absent_or_not_editable(field));
        };
        let mut edited = String::with_capacity(self.source.len());
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
    pub fn replace_section(
        &self,
        address: impl Into<SectionAddress<'a>>,
        content: &str,
    ) -> Result<String, EditError> {
        let scan = self.scan_body();
        let span = scan.resolve_section(address.into())?;
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
            replacement.push_str(content);
            if !content.ends_with('\n') {
                replacement.push_str(terminator);
            }
        }

        let mut edited = String::with_capacity(self.source.len());
        edited.push_str(&self.source[..start]);
        edited.push_str(&replacement);
        edited.push_str(&self.source[end..]);
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
        if let Some(Value::Map(map)) = &self.frontmatter
            && map.contains_key(field)
            && self.field(field).is_none()
        {
            return Err(EditError::FieldNotEditable {
                field: field.to_string(),
            });
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
            Some(range) => Ok(format!(
                "{}{entry}{}",
                &self.source[..range.end],
                &self.source[range.end..]
            )),
            // No block at all. It lands after any byte-order mark, never above
            // it, so the mark stays the document's first bytes.
            None => {
                let at = if self.byte_order_mark { BOM.len() } else { 0 };
                Ok(format!(
                    "{}---{terminator}{entry}---{terminator}{}",
                    &self.source[..at],
                    &self.source[at..]
                ))
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
            return Ok(format!(
                "{}{entry}{}",
                &self.source[..located.line_range.start],
                &self.source[located.line_range.end..]
            ));
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
        Ok(format!(
            "{}{rendered}{}",
            &self.source[..range.start],
            &self.source[range.end..]
        ))
    }

    /// Re-read the edited bytes and refuse unless the frontmatter is exactly
    /// the intended mapping — the edited field as asked for, and every other
    /// field, its order included, untouched.
    fn verify(&self, edited: &str, field: &str, expected: &Mapping) -> Result<(), EditError> {
        let reread = Document::parse(edited);
        let matches = match reread.frontmatter() {
            Some(Value::Map(map)) => map == expected,
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
}

/// Whether `content` re-reads as the same value it was written from — the
/// round-trip the emission layer proves for itself, exposed so a caller can
/// assert it too.
pub fn frontmatter_reads_back(content: &str, expected: &Value) -> bool {
    Document::parse(content).frontmatter() == Some(expected)
}

/// Read a value as this crate reads frontmatter, for a caller holding a
/// fragment rather than a document.
pub fn parse_value(text: &str) -> Option<Value> {
    reparse(text)
}
