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
/// **A `#` is a comment exactly when truncating the text after it leaves the
/// block reading as the same value.** Each `#` in the entry that opens a line
/// or follows a space or tab is a candidate; the block is re-read with the
/// rest of that `#`'s line deleted — the `#` and the line's terminator stay
/// — and compared with the block as written. A comment truncated is still a
/// comment, so the layout around it, blank lines a keep-chomping scalar owns
/// included, reads as it did; content truncated loses text, so the value
/// changes or the block stops parsing: a `#` inside a quoted scalar or on a
/// block scalar's content line. No lexer decides it, so a quote that opens
/// nothing — one inside a plain or a block scalar — cannot hide a comment
/// from it.
///
/// The answer decides whether a whole-entry rewrite may run, and a comment
/// it misses is one that rewrite drops silently, so every case it cannot
/// judge answers yes and the rewrite refuses instead:
///
/// - a bare `#`, with nothing after it to truncate;
/// - more candidates than [`MAX_JUDGED_CANDIDATES`], which bounds what the
///   answer costs to that many re-reads of a block itself bounded by
///   [`FRONTMATTER_MAX_BYTES`](crate::FRONTMATTER_MAX_BYTES);
/// - a block that does not re-read as written at all.
///
/// Every candidate is truncated at once first, and a block that still reads
/// the same holds a comment; only otherwise is each judged alone, stopping at
/// the first comment. An entry with no candidate is not re-read.
pub(crate) fn entry_carries_comment(content: &str, block: Range<usize>, field: &Field) -> bool {
    let yaml = &content[block.clone()];
    let entry = field.line_range.start - block.start..field.line_range.end - block.start;
    let candidates: Vec<Range<usize>> = comment_candidates(yaml, entry).collect();
    if candidates.is_empty() {
        return false;
    }
    if candidates.len() > MAX_JUDGED_CANDIDATES || candidates.iter().any(Range::is_empty) {
        return true;
    }
    let Some(written) = reparse(yaml) else {
        return true;
    };
    let reads_as_written = |cuts: &[Range<usize>]| {
        let mut truncated = String::with_capacity(yaml.len());
        let mut kept_from = 0;
        // A later `#` on a line an earlier one truncates is inside that cut.
        for cut in cuts {
            if cut.start < kept_from {
                continue;
            }
            truncated.push_str(&yaml[kept_from..cut.start]);
            kept_from = cut.end;
        }
        truncated.push_str(&yaml[kept_from..]);
        reparse(&truncated).as_ref() == Some(&written)
    };
    reads_as_written(&candidates)
        || candidates
            .iter()
            .any(|candidate| reads_as_written(std::slice::from_ref(candidate)))
}

/// The most comment candidates [`entry_carries_comment`] judges one by one.
/// An entry holding more is taken to carry a comment, because each judgment
/// is a re-read of the whole block: past this many the cost would grow with
/// the block rather than stay bounded, and a refused rewrite costs less than
/// a dropped comment.
pub(crate) const MAX_JUDGED_CANDIDATES: usize = 32;

/// The text each comment candidate in `yaml[entry]` would truncate: for a `#`
/// that opens a line or follows a space or tab, the rest of its line, the
/// terminator excluded. A bare `#` truncates an empty range.
fn comment_candidates(yaml: &str, entry: Range<usize>) -> impl Iterator<Item = Range<usize>> {
    let bytes = yaml.as_bytes();
    entry
        .filter(|&at| bytes[at] == b'#')
        .filter(move |&at| at == 0 || matches!(bytes[at - 1], b' ' | b'\t' | b'\n' | b'\r'))
        .map(move |at| {
            let end = bytes[at..]
                .iter()
                .position(|&byte| matches!(byte, b'\n' | b'\r'))
                .map_or(bytes.len(), |offset| at + offset);
            at + 1..end
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

    use super::{MAX_JUDGED_CANDIDATES, entry_carries_comment};

    /// Whether `field`'s entry in `source` carries a comment.
    fn carries(source: &str, field: &str) -> bool {
        let document = Document::parse(source);
        let block = document.frontmatter_range().expect("a block");
        let located = document.field(field).expect("a located field");
        entry_carries_comment(source, block, located)
    }

    /// **A `#` is a comment exactly when truncating the text after it leaves
    /// the block reading as the same value.** A quote inside a plain or a
    /// block scalar opens nothing, so a comment after it is still found, and
    /// so is one among the blank lines a keep-chomping scalar owns; a `#`
    /// inside a quoted scalar, on a block scalar's content line or with no
    /// space before it is content. A bare `#` cannot be truncated, so it is
    /// taken for a comment.
    #[test]
    fn a_comment_is_a_hash_whose_truncation_changes_no_value() {
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
            ("---\nk: |+\n    a\n  # c\n\nn: 1\n---\n", "k", true),
            (
                "---\nk:\n  m: |+\n    a\n  # c\n\n  o: 1\nn: 1\n---\n",
                "k",
                true,
            ),
            ("---\nk: |+\n    a\n\n  # c\n\nn: 1\n---\n", "k", true),
            ("---\nk: v #\nn: 1\n---\n", "k", true),
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

    /// **Past the cap on re-reads, an entry is taken to carry a comment.**
    /// Judging more candidates one by one would cost a re-read each, so an
    /// entry holding more than the cap refuses its rewrite even where none of
    /// them is a comment: a refusal the author can act on, never a comment
    /// lost.
    #[test]
    fn an_entry_with_more_candidates_than_the_cap_carries_a_comment() {
        let content: String = (0..=MAX_JUDGED_CANDIDATES)
            .map(|index| format!("\n  x #{index}"))
            .collect();
        let source = format!("---\nk: '{content}'\nn: 1\n---\n");
        assert!(carries(&source, "k"));
        let within: String = (0..MAX_JUDGED_CANDIDATES)
            .map(|index| format!("\n  x #{index}"))
            .collect();
        assert!(!carries(&format!("---\nk: '{within}'\nn: 1\n---\n"), "k"));
    }
}
