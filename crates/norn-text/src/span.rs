//! A 1-based line/column plus 0-based byte offset into a source string.
//!
//! The text layer's single position type — headings and wikilink tokens both
//! carry one so a caller can point a human (or a diagnostic) at the exact byte
//! a construct begins.
//!
//! This module also owns the crate's definition of a line break: `\n`,
//! `\r\n`, or a lone `\r`. [`LineCursor`] counts positions by it,
//! [`split_lines_inclusive`] cuts lines by it, and [`lf_normalized`] presents
//! it to a parser that reads a narrower rule.
//!
//! The rest of the crate decides where a line ends by asking one of the three,
//! or by a `\n` test written beside a `\r` test — never by `\n` alone. That
//! is a checked property rather than a claim: a test walks every source file
//! of the crate for a `\n` literal standing without a `\r`, and the
//! exemptions it grants are named one by one with the reason each is not a
//! line rule. [`crate::line_ending::LineEnding`] is the one other module that
//! decides anything about `\n`, and what it decides is which of two
//! terminator spellings an edit writes, not where a line ends.

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
pub(crate) fn split_lines_inclusive(content: &str) -> LinesInclusive<'_> {
    LinesInclusive {
        content,
        start: 0,
        end: content.len(),
    }
}

/// The lines of a string, terminator included, walkable from either end.
///
/// Both ends exist because trimming a run of blank lines off the back of a
/// slice is as ordinary a question as reading one off the front, and a caller
/// that has to collect the whole slice to ask it backwards pays the slice in
/// memory to answer a question about its last few bytes.
///
/// The two directions cut at the same places. `next` and `next_back` each
/// consume a whole line with its whole terminator, so the unconsumed middle is
/// always a run of whole lines and neither end can leave the other standing
/// inside a `\r\n` pair.
pub(crate) struct LinesInclusive<'a> {
    content: &'a str,
    /// The unconsumed range, `[start, end)`. Both bounds sit on a line
    /// boundary.
    start: usize,
    end: usize,
}

impl<'a> LinesInclusive<'a> {
    /// Where the break at `at` ends, given that `at` is a break byte inside
    /// the unconsumed range.
    fn past_break(&self, at: usize) -> usize {
        let bytes = self.content.as_bytes();
        // `\r\n` is one break, so it terminates one line rather than opening
        // an empty second one.
        if bytes[at] == b'\r' && at + 1 < self.end && bytes[at + 1] == b'\n' {
            at + 2
        } else {
            at + 1
        }
    }
}

impl<'a> Iterator for LinesInclusive<'a> {
    type Item = &'a str;

    fn next(&mut self) -> Option<&'a str> {
        if self.start >= self.end {
            return None;
        }
        let bytes = self.content.as_bytes();
        let stop = match bytes[self.start..self.end]
            .iter()
            .position(|&byte| byte == b'\n' || byte == b'\r')
        {
            Some(offset) => self.past_break(self.start + offset),
            None => self.end,
        };
        let line = &self.content[self.start..stop];
        self.start = stop;
        Some(line)
    }
}

impl<'a> DoubleEndedIterator for LinesInclusive<'a> {
    fn next_back(&mut self) -> Option<&'a str> {
        if self.start >= self.end {
            return None;
        }
        let bytes = self.content.as_bytes();
        // The last line runs to `end`. Step onto the first byte of whatever
        // terminates it, if anything does: the `\r` of a pair rather than its
        // `\n`, so the pair is not mistaken for two breaks.
        let mut last = self.end - 1;
        if bytes[last] == b'\n' && last > self.start && bytes[last - 1] == b'\r' {
            last -= 1;
        }
        // A line the slice ends inside carries no terminator, so the search
        // for the break above it covers those bytes too.
        let above = if bytes[last] == b'\n' || bytes[last] == b'\r' {
            last
        } else {
            self.end
        };
        let begin = bytes[self.start..above]
            .iter()
            .rposition(|&byte| byte == b'\n' || byte == b'\r')
            .map_or(self.start, |offset| self.past_break(self.start + offset));
        let line = &self.content[begin..self.end];
        self.end = begin;
        Some(line)
    }
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
    let lone_cr_at =
        |bytes: &[u8], index: usize| bytes[index] == b'\r' && bytes.get(index + 1) != Some(&b'\n');
    // The scan that decides whether to copy is the whole cost a document with
    // no lone `\r` pays.
    if !(0..content.len()).any(|index| lone_cr_at(content.as_bytes(), index)) {
        return Cow::Borrowed(content);
    }
    // One buffer from here on: the test and the write read the same bytes, so
    // a rewritten `\r` cannot change the answer for the byte after it — the
    // test looks at `bytes[index + 1]`, which the loop has not reached.
    let mut bytes = content.as_bytes().to_vec();
    for index in 0..bytes.len() {
        if lone_cr_at(&bytes, index) {
            bytes[index] = b'\n';
        }
    }
    // One ASCII byte swapped for another leaves every multi-byte sequence
    // untouched, so the bytes are still the UTF-8 they arrived as.
    Cow::Owned(String::from_utf8(bytes).expect("swapping ASCII for ASCII preserves UTF-8"))
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

    /// The two ends cut at the same places, so a caller that walks a slice
    /// backwards sees the lines a caller walking forwards sees, reversed.
    ///
    /// The pair is what makes a backwards walk safe to trust: `next_back`
    /// standing inside a `\r\n` would report a line the forward walk never
    /// produced, and nothing at a call site would show it.
    #[test]
    fn the_two_directions_cut_a_slice_at_the_same_places() {
        for content in [
            "",
            "abc",
            "a\nb\n",
            "a\r\nb\r\n",
            "a\rb\r",
            "\r\n",
            "\n\n",
            "\r\r",
            "a\r\nb\rc\nd",
            "\r\n\r\n\r",
            "é\rß\r\n",
        ] {
            let forward: Vec<&str> = split_lines_inclusive(content).collect();
            let mut backward: Vec<&str> = split_lines_inclusive(content).rev().collect();
            backward.reverse();
            assert_eq!(forward, backward, "{content:?}");
            // The lines partition the content: nothing duplicated, nothing
            // dropped, in order.
            assert_eq!(forward.concat(), content, "{content:?}");
        }
    }

    /// Cutting from both ends at once meets in the middle without either end
    /// producing a line the other already did.
    #[test]
    fn walking_from_both_ends_partitions_the_content_once() {
        let content = "one\ntwo\r\nthree\rfour";
        let mut lines = split_lines_inclusive(content);
        assert_eq!(lines.next(), Some("one\n"));
        assert_eq!(lines.next_back(), Some("four"));
        assert_eq!(lines.next(), Some("two\r\n"));
        assert_eq!(lines.next_back(), Some("three\r"));
        assert_eq!(lines.next(), None);
        assert_eq!(lines.next_back(), None);
    }

    /// A `\r\n` is one break from either direction, so it terminates one line
    /// rather than opening an empty second one.
    #[test]
    fn a_crlf_pair_is_one_break_from_either_end() {
        assert_eq!(
            split_lines_inclusive("a\r\n\r\nb").collect::<Vec<_>>(),
            ["a\r\n", "\r\n", "b"]
        );
        assert_eq!(
            split_lines_inclusive("a\r\n\r\nb")
                .rev()
                .collect::<Vec<_>>(),
            ["b", "\r\n", "a\r\n"]
        );
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
