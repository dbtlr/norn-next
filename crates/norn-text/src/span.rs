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
    /// on the end, an offset inside a multi-byte character lands on that
    /// character's first byte, and an offset on the `\n` of a `\r\n` lands on
    /// the `\r`. The clamped value is what the returned `byte_offset` carries,
    /// so slicing `content` at it is always sound.
    ///
    /// Counting is from the start of `content`, so this costs the offset. The
    /// crate's own scans ask for many spans in ascending order and carry the
    /// count forward across them instead, which is linear in the content.
    pub fn at(content: &str, byte_offset: usize) -> Self {
        LineCursor::new(content).span_at(byte_offset)
    }
}

/// A forward-only walk over a string's line breaks.
///
/// Line and column are counted from the start of the content, so asking for
/// one span costs the offset and asking for every construct in a document
/// costs the document once per construct. A scan visits constructs in
/// ascending order, so this carries the count forward: each span resumes where
/// the last one stopped, and the whole pass is linear in the content.
///
/// A line break is `\n`, `\r\n`, or a lone `\r`. The last of those is why the
/// cursor exists as a type rather than as a loop: a CR-only document reported
/// line 1 everywhere when only `\n` counted, and one definition of a break is
/// the only way the single-span and the many-span paths cannot drift apart.
pub(crate) struct LineCursor<'a> {
    content: &'a str,
    offset: usize,
    line: usize,
    line_start: usize,
}

impl<'a> LineCursor<'a> {
    pub(crate) fn new(content: &'a str) -> Self {
        LineCursor {
            content,
            offset: 0,
            line: 1,
            line_start: 0,
        }
    }

    /// The span of `byte_offset`, which must not precede the last one asked
    /// for. Clamping is [`SourceSpan::at`]'s.
    pub(crate) fn span_at(&mut self, byte_offset: usize) -> SourceSpan {
        let mut target = byte_offset.min(self.content.len());
        while target > 0 && !self.content.is_char_boundary(target) {
            target -= 1;
        }
        // `\r\n` is one break, so an offset landing on its `\n` lands on the
        // `\r` — the same clamp a multi-byte character gets, for the same
        // reason. Without it the interior of a break is a position the two
        // paths answer differently: stopping there ends the line, and resuming
        // there reads the `\n` as a second break the one-shot walk never
        // counted.
        let bytes = self.content.as_bytes();
        if target > 0 && bytes.get(target) == Some(&b'\n') && bytes[target - 1] == b'\r' {
            target -= 1;
        }
        debug_assert!(
            target >= self.offset,
            "a line cursor only advances: {target} is behind {}",
            self.offset
        );

        while self.offset < target {
            let byte = bytes[self.offset];
            self.offset += 1;
            // The `\n` of a pair is consumed with its `\r` unconditionally: the
            // clamp above puts no target between them, so there is no offset
            // the cursor can be asked to stop at mid-break.
            if byte == b'\r' && bytes.get(self.offset) == Some(&b'\n') {
                self.offset += 1;
            } else if byte != b'\r' && byte != b'\n' {
                continue;
            }
            self.line += 1;
            self.line_start = self.offset;
        }

        SourceSpan {
            line: self.line,
            column: target - self.line_start + 1,
            byte_offset: target,
        }
    }
}
