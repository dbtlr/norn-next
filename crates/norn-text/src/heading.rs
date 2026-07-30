//! Markdown headings and their anchor slugs.
//!
//! A heading is produced by the document's one CommonMark pass
//! ([`crate::BodyScan`]), so a `#` inside a fenced or inline code span is never
//! mistaken for one. Both written forms — ATX (`## Title`) and setext (a title
//! line over `---`/`===`) — produce the same record.

use std::collections::HashMap;

use crate::span::SourceSpan;
use crate::tag::is_mark;

/// A Markdown heading located in a document body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Heading {
    pub level: u8,
    /// The heading's text with inline markup flattened: `## Use \`norn\`` is
    /// `Use norn`.
    pub text: String,
    /// The GFM anchor form of [`Heading::text`] — [`slugify`], plus the
    /// document-order suffix that tells repeated headings apart. This is what
    /// an inline Markdown link's `#fragment` addresses; a wikilink's addresses
    /// [`Heading::text`] instead.
    pub slug: String,
    /// Where the heading construct begins — the `#` of an ATX heading, the
    /// first byte of a setext title line.
    pub span: SourceSpan,
    /// The byte offset just past the whole heading construct, which is where
    /// the section body begins. For an ATX heading that is the byte after its
    /// line's newline; for a setext heading it is the byte after the
    /// underline's newline, because the underline is part of the heading. A
    /// heading at end of file with no trailing newline ends at the end of the
    /// body.
    ///
    /// Always present: it is read from the parser's own heading range, and
    /// there is no way to build a `Heading` without one. A section whose body
    /// silently ran to end of file is the shape that absence produced.
    pub body_offset: usize,
    /// Whether the heading sits inside a blockquote or a list item.
    ///
    /// The bytes below such a heading are the container's before they are the
    /// section's, so replacing the section by byte range lifts them out of the
    /// container and rewrites the document around it. A replace addressed at
    /// one refuses.
    pub inside_container: bool,
}

/// Slugify heading text into a GFM anchor: surrounding whitespace trimmed,
/// lowercased, letters, combining marks and digits kept whatever alphabet they
/// are written in, `_` and `-` kept, a space written as `-`, and everything
/// else — punctuation and symbols — dropped.
///
/// `## What? Really!` slugs to `what-really` and `## 日本語` to `日本語`.
/// Runs are not collapsed, because GFM does not collapse them: `A  B` slugs to
/// `a--b`, and an anchor that agrees with the renderer is the point.
///
/// Marks are kept because dropping them writes a different word: `हिन्दी`
/// would slug to `हिनदी`, and lowercasing `İstanbul` produces an `i` and a
/// combining dot that has to survive for the anchor to match the one a
/// renderer emits.
///
/// This answers for one text. Telling two headings with the same text apart is
/// a property of the document they sit in, so the `-1`, `-2` suffixes are
/// applied by the document's one pass ([`crate::BodyScan`]) and appear in
/// [`Heading::slug`] rather than here.
pub fn slugify(text: &str) -> String {
    let mut slug = String::new();

    for ch in text.trim().chars().flat_map(char::to_lowercase) {
        if ch == ' ' {
            slug.push('-');
        } else if ch.is_alphanumeric() || is_mark(ch) || ch == '_' || ch == '-' {
            slug.push(ch);
        }
    }

    slug
}

/// Document-order slug disambiguation: the first heading with a slug keeps it,
/// and each later one taking the same slug is suffixed `-1`, `-2`, and so on.
///
/// A suffixed slug is itself taken, so a document whose third heading slugs to
/// an already-issued `a-1` gets `a-1-1` rather than a duplicate. Two anchors
/// that address the same heading are the defect this closes.
#[derive(Debug, Clone, Default)]
pub(crate) struct SlugCounter {
    issued: HashMap<String, usize>,
}

impl SlugCounter {
    /// The anchor for the next heading in document order.
    pub(crate) fn issue(&mut self, text: &str) -> String {
        let original = slugify(text);
        let mut slug = original.clone();
        while self.issued.contains_key(&slug) {
            let taken = self.issued.entry(original.clone()).or_default();
            *taken += 1;
            slug = format!("{original}-{taken}");
        }
        self.issued.insert(slug.clone(), 0);
        slug
    }
}
