//! A 1-based line/column plus 0-based byte offset into a source string.
//!
//! The text layer's single position type — headings and wikilink tokens both
//! carry one so a caller can point a human (or a diagnostic) at the exact byte
//! a construct begins.

/// A location in a source string: 1-based `line` and `column`, 0-based
/// `byte_offset`. Column counts bytes from the start of the line — an
/// editor-agnostic convention.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SourceSpan {
    pub line: usize,
    pub column: usize,
    pub byte_offset: usize,
}

impl SourceSpan {
    /// The span of `byte_offset` within `content`.
    ///
    /// `byte_offset` is clamped into `content`: an offset past the end lands
    /// on the end, and an offset inside a multi-byte character lands on that
    /// character's first byte. The clamped value is what the returned
    /// `byte_offset` carries, so slicing `content` at it is always sound.
    pub fn at(content: &str, byte_offset: usize) -> Self {
        let mut offset = byte_offset.min(content.len());
        while offset > 0 && !content.is_char_boundary(offset) {
            offset -= 1;
        }
        let prefix = &content[..offset];
        let line = prefix.bytes().filter(|byte| *byte == b'\n').count() + 1;
        let column = prefix
            .rsplit_once('\n')
            .map_or(prefix.len() + 1, |(_, tail)| tail.len() + 1);
        SourceSpan {
            line,
            column,
            byte_offset: offset,
        }
    }
}
