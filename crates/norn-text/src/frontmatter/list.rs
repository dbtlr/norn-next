//! The bytes of a list field: where a block list's items are, where its key
//! line takes a value, and whether an entry carries a comment.
//!
//! A push or a pop splices one item's line rather than rewriting the field,
//! so every byte it does not change — comments, the other items' quoting, the
//! key's spelling — stays. What this module names is only the bytes; the
//! document's re-read after the splice is what proves them.

use std::ops::Range;

use crate::frontmatter::fields::{Field, ValueStyle, classify_value, parse_top_level_key, reparse};
use crate::span::split_lines_inclusive;
use crate::value::Value;

/// One item of a block list, written on a line of its own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BlockItem {
    /// The item's whole line, its terminator included.
    pub(crate) line: Range<usize>,
    /// The bytes before the item's `-`.
    pub(crate) indent: Range<usize>,
    /// Whether a comment trails the item on its line.
    pub(crate) commented: bool,
}

/// The items of the block-list `field` in `content`, one per parsed item, or
/// `None` where the entry is not a list whose items can be spliced line by
/// line.
///
/// Below the key line, every line of the entry must be blank, a comment, or
/// one `- item` whose value ends on that line and re-reads as the parsed item
/// at its position; and there must be exactly as many item lines as parsed
/// items. A multi-line item, a nested collection written over several lines
/// or a scan that disagrees with the parse answers `None`, and the caller
/// does not splice.
pub(crate) fn block_item_lines(
    content: &str,
    field: &Field,
    items: &[Value],
) -> Option<Vec<BlockItem>> {
    if field.style != ValueStyle::BlockSequence {
        return None;
    }
    let mut lines = split_lines_inclusive(&content[field.line_range.clone()]);
    let mut line_start = field.line_range.start + lines.next()?.len();
    let mut found = Vec::new();
    for line in lines {
        let start = line_start;
        line_start += line.len();
        let text = line.trim_end_matches(['\r', '\n']);
        let body = text.trim_start_matches([' ', '\t']);
        if body.is_empty() || body.starts_with('#') {
            continue;
        }
        let item = block_item(content, start, line, text.len() - body.len())?;
        let value = reparse(&content[item.value.clone()])?;
        if items.get(found.len()) != Some(&value) {
            return None;
        }
        found.push(item.line_item);
    }
    (found.len() == items.len()).then_some(found)
}

/// A `- item` line's item and the bytes of its value.
struct ScannedItem {
    line_item: BlockItem,
    value: Range<usize>,
}

/// Read `line` — its terminator included, starting at `start` in `content`
/// and indented by `indent` bytes — as one block-list item whose value ends
/// on the line.
fn block_item(content: &str, start: usize, line: &str, indent: usize) -> Option<ScannedItem> {
    let text = line.trim_end_matches(['\r', '\n']);
    let rest = text[indent..].strip_prefix('-')?;
    if !(rest.is_empty() || rest.starts_with([' ', '\t'])) {
        return None;
    }
    let after_dash = indent + 1;
    let (value, _, complete) = classify_value(start, after_dash, &text[after_dash..]);
    let value = value.filter(|_| complete)?;
    let after_value = &content[value.end..start + text.len()];
    Some(ScannedItem {
        line_item: BlockItem {
            line: start..start + line.len(),
            indent: start..start + indent,
            commented: !after_value.trim().is_empty(),
        },
        value,
    })
}

/// Where the key line starting at `line_start` takes a value when it holds
/// none: the insertion point a stubbed key's value is spliced into, past any
/// trailing space and before any comment. `None` where the line is not a
/// top-level key line or already holds a value.
pub(crate) fn key_line_value_point(content: &str, line_start: usize) -> Option<Range<usize>> {
    let (line, after_colon) = key_line(content, line_start)?;
    match classify_value(line_start, after_colon, &line[after_colon..]) {
        (range, ValueStyle::EmptyValue, _) => range,
        _ => None,
    }
}

/// Whether the entry of `field` carries a comment anywhere past its key: on
/// the key line, inside a multi-line value, or between a block list's items.
///
/// Read conservatively, because the answer decides whether a whole-field
/// rewrite may run, and a comment it misses is a comment that rewrite loses:
/// a `#` opening a line or following a space or tab is a comment unless it
/// sits inside a quoted scalar, and a quote is only one where a scalar can
/// begin. A quote left open, or a key line that cannot be read, answers yes.
pub(crate) fn entry_carries_comment(content: &str, field: &Field) -> bool {
    let Some((_, after_colon)) = key_line(content, field.line_range.start) else {
        return true;
    };
    let value_start = (field.line_range.start + after_colon).min(field.line_range.end);
    carries_comment(&content[value_start..field.line_range.end])
}

/// The key line starting at `line_start`, without its terminator, and the
/// offset in it just past the key's `:`.
fn key_line(content: &str, line_start: usize) -> Option<(&str, usize)> {
    let line = split_lines_inclusive(&content[line_start..])
        .next()?
        .trim_end_matches(['\r', '\n']);
    let (_, after_colon, _) = parse_top_level_key(line)?;
    Some((line, after_colon))
}

/// Whether `text` — a value, from just past its key's `:` — carries a
/// comment, on [`entry_carries_comment`]'s conservative reading.
fn carries_comment(text: &str) -> bool {
    let bytes = text.as_bytes();
    let mut quote: Option<u8> = None;
    // The byte before the one being read, and the last byte outside a quote
    // that was not a space: the first says whether a `#` follows a space, the
    // second whether a quote stands where a scalar can begin.
    let mut before = b' ';
    let mut last_mark = b':';
    let mut index = 0;
    while index < bytes.len() {
        let byte = bytes[index];
        match quote {
            Some(b'\'') if byte == b'\'' => {
                if bytes.get(index + 1) == Some(&b'\'') {
                    index += 1;
                } else {
                    quote = None;
                    last_mark = byte;
                }
            }
            Some(b'"') if byte == b'\\' => index += 1,
            Some(b'"') if byte == b'"' => {
                quote = None;
                last_mark = byte;
            }
            Some(_) => {}
            None => match byte {
                b'#' if matches!(before, b' ' | b'\t' | b'\n' | b'\r') => return true,
                b'\'' | b'"' if opens_scalar(before, last_mark) => quote = Some(byte),
                b' ' | b'\t' => {}
                b'\n' | b'\r' => last_mark = LINE_START,
                _ => last_mark = byte,
            },
        }
        before = bytes[index];
        index += 1;
    }
    quote.is_some()
}

/// The mark [`carries_comment`] records for a line break, whichever of the
/// three spellings made it: no byte of a value is NUL, so it stands for the
/// start of a line and nothing else.
const LINE_START: u8 = 0;

/// Whether a quote read after the byte `before`, with `last_mark` the last
/// byte that was not a space, stands where a scalar can begin: straight after
/// a flow indicator, or after a space that follows one, a key's `:`, an
/// item's `-` or the start of a line.
fn opens_scalar(before: u8, last_mark: u8) -> bool {
    matches!(before, b'[' | b',' | b'{')
        || (matches!(before, b' ' | b'\t' | b'\n' | b'\r')
            && matches!(
                last_mark,
                b'[' | b',' | b'{' | b':' | b'-' | b'?' | LINE_START
            ))
}

#[cfg(test)]
mod tests {
    use super::carries_comment;

    /// **A `#` is a comment only outside a quoted scalar, and only after a
    /// space or at a line's start.** An apostrophe inside a plain scalar is
    /// not a quote, so a comment after it is still found; a quote left open
    /// is read as hiding one.
    #[test]
    fn a_comment_is_found_outside_quotes_and_only_there() {
        for (text, carries) in [
            (" [a, b]", false),
            (" [a] # keep", true),
            (" [a,\n  # inside\n  b]", true),
            (" ['a #b', \"c # d\"]", false),
            (" [it's, b] # keep", true),
            (" [a#b]", false),
            (" [a, 'b", true),
            ("\n  - a # c1\n  - b\n", true),
            ("\n  - 'x '' # y'\n", false),
        ] {
            assert_eq!(carries_comment(text), carries, "for {text:?}");
        }
    }
}
