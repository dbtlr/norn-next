//! The bytes of a list field: where a block list's items are, where its key
//! line takes a value, and whether an entry carries a comment.
//!
//! A push or a pop splices one item's lines rather than rewriting the field,
//! so every byte it does not change — comments, the other items' quoting and
//! layout, the key's spelling — stays. What this module names is only the
//! bytes; the document's re-read after the splice is what proves them.

use std::ops::Range;

use crate::frontmatter::fields::{Field, ValueStyle, classify_value, parse_top_level_key, reparse};
use crate::span::split_lines_inclusive;
use crate::value::Value;

/// One item of a block list: the lines it is written on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BlockItem {
    /// The item's lines, from its `-` line through its last line of content,
    /// terminators included. A comment or blank line inside that run is the
    /// item's; one after it is not.
    pub(crate) lines: Range<usize>,
    /// The bytes before the item's `-`.
    pub(crate) indent: Range<usize>,
}

/// The items of the block-list `field` in `content`, one per parsed item, or
/// `None` where the entry is not a list whose items can be spliced as runs of
/// whole lines.
///
/// Below the key line, every line of the entry must be blank, a comment, a
/// `- item` line at the indent the first item sets, or a line indented past
/// it, which continues the item above; an item may span any number of lines,
/// a map or a nested list written over several included. Each item's lines,
/// re-read alone, must be a list holding exactly the parsed item at its
/// position, and there must be exactly as many items as parsed ones. A line
/// that fits none of these, or a run that does not re-read as its item —
/// one naming an anchor written outside it, a block scalar whose kept blank
/// lines trail it — answers `None`, and the caller does not splice.
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
    let mut item_indent = None;
    let mut found: Vec<BlockItem> = Vec::new();
    for line in lines {
        let start = line_start;
        line_start += line.len();
        let text = line.trim_end_matches(['\r', '\n']);
        let body = text.trim_start_matches([' ', '\t']);
        if body.is_empty() || body.starts_with('#') {
            continue;
        }
        let indent = text.len() - body.len();
        if item_indent.is_some_and(|column| indent > column) {
            found.last_mut()?.lines.end = line_start;
            continue;
        }
        if item_indent.is_some_and(|column| indent != column) || !opens_item(body) {
            return None;
        }
        item_indent = Some(indent);
        found.push(BlockItem {
            lines: start..line_start,
            indent: start..start + indent,
        });
    }
    let proven = found.len() == items.len()
        && found.iter().zip(items).all(|(item, value)| {
            matches!(reparse(&content[item.lines.clone()]),
                Some(Value::Sequence(read)) if read.len() == 1 && &read[0] == value)
        });
    proven.then_some(found)
}

/// Whether `body` — a line from its first non-blank byte — opens a block-list
/// item: a `-` followed by a space, a tab or the end of the line.
fn opens_item(body: &str) -> bool {
    body.strip_prefix('-')
        .is_some_and(|rest| rest.is_empty() || rest.starts_with([' ', '\t']))
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

/// Whether the entry of `field` carries a comment anywhere: on its key line,
/// inside a multi-line value, or between a block list's items. `block` is
/// the range of the frontmatter's YAML in `content`.
///
/// **A `#` is a comment exactly when deleting it leaves the block reading as
/// the same value.** Each `#` in the entry that opens a line or follows a
/// space or tab is a candidate; the block is re-read with that `#`, the
/// spaces before it and the rest of its line deleted — the whole line where
/// nothing else is on it — and compared with the block as written. A
/// deletion that reads back unchanged removed only a comment; one that
/// changes the value or does not parse removed content: a `#` inside a
/// quoted scalar or on a block scalar's content line. No lexer decides it,
/// so a quote that opens nothing — one inside a plain or a block scalar —
/// cannot hide a comment from it. A block that does not re-read at all
/// answers yes, because nothing it holds can be proven not a comment.
///
/// The answer decides whether a whole-entry rewrite may run, and a comment
/// it misses is one that rewrite drops silently. Its cost is a re-read per
/// candidate, bounded by [`FRONTMATTER_MAX_BYTES`](crate::FRONTMATTER_MAX_BYTES),
/// and an entry with no candidate is not re-read at all.
pub(crate) fn entry_carries_comment(content: &str, block: Range<usize>, field: &Field) -> bool {
    let yaml = &content[block.clone()];
    let entry = field.line_range.start - block.start..field.line_range.end - block.start;
    let mut candidates = comment_candidates(yaml, entry).peekable();
    if candidates.peek().is_none() {
        return false;
    }
    let Some(written) = reparse(yaml) else {
        return true;
    };
    candidates.any(|span| {
        let mut without = String::with_capacity(yaml.len() - span.len());
        without.push_str(&yaml[..span.start]);
        without.push_str(&yaml[span.end..]);
        reparse(&without).as_ref() == Some(&written)
    })
}

/// The span each comment candidate in `yaml[entry]` would delete: a `#` that
/// opens a line or follows a space or tab, from the spaces and tabs before it
/// through the end of its line, and through the line's terminator too where
/// nothing precedes it on the line.
fn comment_candidates(yaml: &str, entry: Range<usize>) -> impl Iterator<Item = Range<usize>> {
    let bytes = yaml.as_bytes();
    let is_break = |byte: u8| matches!(byte, b'\n' | b'\r');
    entry.filter(|&at| bytes[at] == b'#').filter_map(move |at| {
        let mut start = at;
        while start > 0 && matches!(bytes[start - 1], b' ' | b'\t') {
            start -= 1;
        }
        let opens_line = start == 0 || is_break(bytes[start - 1]);
        if start == at && !opens_line {
            return None;
        }
        let mut end = bytes[at..]
            .iter()
            .position(|&byte| is_break(byte))
            .map_or(bytes.len(), |offset| at + offset);
        if opens_line {
            end += match &bytes[end..] {
                [b'\r', b'\n', ..] => 2,
                [b'\n' | b'\r', ..] => 1,
                _ => 0,
            };
        }
        Some(start..end)
    })
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

#[cfg(test)]
mod tests {
    use crate::Document;

    use super::entry_carries_comment;

    /// Whether `field`'s entry in `source` carries a comment.
    fn carries(source: &str, field: &str) -> bool {
        let document = Document::parse(source);
        let block = document.frontmatter_range().expect("a block");
        let located = document.field(field).expect("a located field");
        entry_carries_comment(source, block, located)
    }

    /// **A `#` is a comment exactly when deleting it leaves the block reading
    /// as the same value.** A quote inside a plain or a block scalar opens
    /// nothing, so a comment after it is still found; a `#` inside a quoted
    /// scalar, on a block scalar's content line or with no space before it is
    /// content.
    #[test]
    fn a_comment_is_a_hash_whose_deletion_changes_no_value() {
        for (source, field, carries_one) in [
            ("---\nk: [a, b]\n---\n", "k", false),
            ("---\nk: [a] # keep\n---\n", "k", true),
            ("---\nk: [a,\n  # inside\n  b]\n---\n", "k", true),
            ("---\nk: ['a #b', \"c # d\"]\n---\n", "k", false),
            ("---\nk: [it's, b] # keep\n---\n", "k", true),
            ("---\nk: [a#b]\n---\n", "k", false),
            ("---\nk:\n  - a # c1\n  - b\n---\n", "k", true),
            ("---\nk:\n  - 'x '' # y'\n---\n", "k", false),
            ("---\nk: a#b\nn: 1\n---\n", "k", false),
            ("---\nk: |\n  a\n  # content\nn: 1\n---\n", "k", false),
            ("---\nk: |+\n    a\n  # comment\nn: 1\n---\n", "k", true),
            ("---\nk: v\r\n  # crlf\r\nn: 1\r\n---\r\n", "k", true),
            (
                "---\nname: Lovelace, 'Ada # don't rename\nn: 1\n---\n",
                "name",
                true,
            ),
            ("---\nk: a - 'b # c'\nn: 1\n---\n", "k", true),
            ("---\nk: a ? 'b # c'\nn: 1\n---\n", "k", true),
            ("---\nk: a\n  'b # c'\nn: 1\n---\n", "k", true),
            ("---\nk: [x, a - 'b] # c'\n---\n", "k", true),
            (
                "---\nmeta:\n  k: |\n    \"hello\n  # keep this \"\n  n: 1\nafter: x\n---\n",
                "meta",
                true,
            ),
            (
                "---\nmeta:\n  k: hello - 'world # keep '\n  n: 1\n---\n",
                "meta",
                true,
            ),
            (
                "---\nrows:\n  - &x\n    k: |\n      \"hello\n    # keep this \"\n    n: 1\n  - *x\n---\n",
                "rows",
                true,
            ),
        ] {
            assert_eq!(carries(source, field), carries_one, "for {source:?}");
        }
    }
}
