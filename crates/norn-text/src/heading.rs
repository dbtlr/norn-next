//! Markdown headings and their anchor slugs.
//!
//! A heading is produced by the document's one CommonMark pass
//! ([`crate::BodyScan`]), so a `#` inside a fenced or inline code span is never
//! mistaken for one. Both written forms — ATX (`## Title`) and setext (a title
//! line over `---`/`===`) — produce the same record.

use crate::span::SourceSpan;

/// A Markdown heading located in a document body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Heading {
    pub level: u8,
    /// The heading's text with inline markup flattened: `## Use \`norn\`` is
    /// `Use norn`.
    pub text: String,
    /// The GitHub-ish ASCII anchor form of [`Heading::text`].
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

/// Slugify heading text into an anchor: lowercase, ASCII alphanumerics kept,
/// every other run collapsed to a single `-`, with leading and trailing dashes
/// trimmed.
///
/// ASCII-only: a heading with no ASCII alphanumerics slugs to the empty
/// string, and two headings differing only outside ASCII slug identically.
pub fn slugify(text: &str) -> String {
    let mut slug = String::new();
    let mut previous_dash = false;

    for ch in text.chars().flat_map(char::to_lowercase) {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch);
            previous_dash = false;
        } else if !previous_dash && !slug.is_empty() {
            slug.push('-');
            previous_dash = true;
        }
    }

    slug.trim_end_matches('-').to_string()
}
