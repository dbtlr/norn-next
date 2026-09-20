//! A 1-based line/column plus 0-based byte offset into a source string.
//!
//! The text layer's single position type — headings and wikilink tokens both
//! carry one so a caller can point a human (or a diagnostic) at the exact byte
//! a construct begins.
//!
//! This module also owns the crate's one definition of a line break: `\n`,
//! `\r\n`, or a lone `\r`. [`LineCursor`] counts positions by it,
//! [`split_lines_inclusive`] cuts lines by it, and [`lf_normalized`] presents
//! it to a parser that reads a narrower rule. Every line-shaped question in
//! the crate goes through one of the three, so no two answers can disagree
//! about where a line ends.

use std::borrow::Cow;

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

/// Split `content` into lines, terminator included, on [`LineCursor`]'s break
/// rule: `\n`, `\r\n`, or a lone `\r`.
///
/// The cursor and this splitter read the same rule, so a scan that walks lines
/// and asks the cursor where they are cannot disagree with it about what a line
/// is. `str::split_inclusive('\n')` is the shape this exists in place of: it
/// folds a lone-`\r` line into the one after it, handing its caller a chunk
/// that spans a break. A per-line test then answers for two lines at once —
/// masking or matching the pair on whichever one the test happens to read.
pub(crate) fn split_lines_inclusive(content: &str) -> impl Iterator<Item = &str> {
    let bytes = content.as_bytes();
    let mut start = 0;
    std::iter::from_fn(move || {
        if start >= bytes.len() {
            return None;
        }
        let end = match bytes[start..]
            .iter()
            .position(|&b| b == b'\n' || b == b'\r')
        {
            // `\r\n` is one break, so it terminates one line rather than
            // opening an empty second one.
            Some(offset) => {
                let brk = start + offset;
                match bytes[brk] {
                    b'\r' if bytes.get(brk + 1) == Some(&b'\n') => brk + 2,
                    _ => brk + 1,
                }
            }
            None => bytes.len(),
        };
        let line = &content[start..end];
        start = end;
        Some(line)
    })
}

/// `content` with every lone `\r` rewritten to `\n`, for a reader whose own
/// line rule is narrower than [`LineCursor`]'s.
///
/// The rewrite is **byte-length preserving and one byte wide**: `\r` and `\n`
/// are both single ASCII bytes, a `\r\n` pair is left alone, and nothing is
/// inserted or removed. So every byte offset into the returned string is the
/// same byte offset into `content`, and a range a reader reports over the copy
/// slices the original to the same text. That is the whole reason the
/// normalization is admissible: it changes what a narrower reader sees a line
/// break as, and changes no position.
///
/// Content carrying no lone `\r` is returned borrowed, so the common document
/// costs one scan and no allocation.
pub(crate) fn lf_normalized(content: &str) -> Cow<'_, str> {
    let bytes = content.as_bytes();
    let is_lone_cr = |index: usize| bytes[index] == b'\r' && bytes.get(index + 1) != Some(&b'\n');
    if !(0..bytes.len()).any(is_lone_cr) {
        return Cow::Borrowed(content);
    }
    let mut out = bytes.to_vec();
    for index in 0..out.len() {
        if is_lone_cr(index) {
            out[index] = b'\n';
        }
    }
    // One ASCII byte swapped for another leaves every multi-byte sequence
    // untouched, so the bytes are still the UTF-8 they arrived as.
    Cow::Owned(String::from_utf8(out).expect("swapping ASCII for ASCII preserves UTF-8"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The allocation is the thing being contracted, so the test reads the
    /// variant rather than the text: content with no lone `\r` is handed back
    /// borrowed, and only content that has one is copied.
    #[test]
    fn content_without_a_lone_cr_is_borrowed_rather_than_copied() {
        for borrowed in [
            "",
            "plain",
            "a\nb\n",
            "a\r\nb\r\n",
            "a\r\n\r\nb",
            "trailing\r\n",
        ] {
            assert!(
                matches!(lf_normalized(borrowed), Cow::Borrowed(_)),
                "{borrowed:?} was copied"
            );
        }
        for owned in ["a\rb", "a\r", "\r", "a\r\nb\rc"] {
            assert!(
                matches!(lf_normalized(owned), Cow::Owned(_)),
                "{owned:?} was not copied"
            );
        }
    }

    /// The rewrite is one byte wide and touches lone `\r` alone, so lengths
    /// match, `\r\n` survives, and every other byte is where it was.
    #[test]
    fn the_rewrite_preserves_every_byte_position() {
        for content in ["a\rb\r\nc\rd", "\r\r\r", "é\rß\r\n", "no breaks at all"] {
            let normalized = lf_normalized(content);
            assert_eq!(normalized.len(), content.len(), "{content:?}");
            for (index, (was, now)) in content.bytes().zip(normalized.bytes()).enumerate() {
                let expected = if was == b'\r' && content.as_bytes().get(index + 1) != Some(&b'\n')
                {
                    b'\n'
                } else {
                    was
                };
                assert_eq!(now, expected, "byte {index} of {content:?}");
            }
        }
    }
}
