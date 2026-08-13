//! Locating a top-level field's bytes in the frontmatter block.
//!
//! A field is addressed by two ranges. `line_range` is the whole entry, key
//! line through continuation lines, and is what a remove deletes.
//! `value_range` is the value's bytes on the key line, and is what a scalar
//! set replaces; it is absent whenever the value's bytes cannot be named as
//! one contiguous span the parser agrees with.
//!
//! # The scanner proposes, the parser decides
//!
//! A byte scanner cannot decide on its own where a YAML value's bytes are. So
//! the parsed mapping is threaded in as the authority and every proposal is
//! validated against it:
//!
//! - **A field exists only if the parser has that key.** A merge key the
//!   extraction folds away, or a quoted key whose decode diverges from the
//!   parser's, produces no field, and a set or remove then reports it absent
//!   rather than editing a line the parser reads differently.
//! - **A value span survives only if its bytes re-parse to exactly the parsed
//!   value.** A mapping value, or a null whose bytes are not a literal null
//!   token — a bare `# comment` re-parses to null — is refused. This makes a
//!   *wrong* value span impossible to emit: disagreement collapses to absence.
//! - **A line boundary is parser-confirmed.** A `key:`-shaped line that is not
//!   a real key — a `- name:` sequence-of-mappings item, a `key:` lookalike
//!   inside a multi-line quoted value — would truncate the preceding field and
//!   orphan its tail on remove, so it is absorbed into the preceding field
//!   instead of ending it.
//!
//! **Ambiguity refuses the whole block.** If any parsed key cannot be located
//! to exactly one candidate line, the scan and the parser disagree somewhere
//! and there is no safe per-field split — the mis-located key's bytes would be
//! absorbed into a neighbour and deleted by an unrelated remove. No field gets
//! a span; reads are unaffected and every field edit refuses. The same holds
//! when the value model had to drop an entry: the scanner still sees the line
//! the parser no longer names.
//!
//! # Blank lines and comments end a field
//!
//! A `line_range` stops before the run of blank lines and column-0 comments
//! that trails it, so removing a field leaves the blank structure and the
//! comments around it standing. Indented tails are still absorbed — a blank
//! line inside a folded scalar is part of that scalar, and the run is measured
//! from the end of the range backwards, so it never reaches one.
//!
//! A block scalar with keep-chomping (`|+`, `>+`) owns trailing blank lines as
//! content, and owns exactly as many as its parsed value carries: the value's
//! own trailing newlines are the proof, and everything past them — a standing
//! comment, the blanks below it — is the document's. Whether the header asks
//! for keep-chomping is read from the header itself, where the chomping
//! indicator sits before any comment, so a `+` written in a trailing comment
//! is a comment.

use std::collections::HashMap;
use std::ops::Range;

use crate::frontmatter::extract::MERGE_KEY;
use crate::value::{KeyIndex, StripReport, Value};

/// How a field's value is written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValueStyle {
    Plain,
    SingleQuoted,
    DoubleQuoted,
    BlockLiteral,
    BlockFolded,
    FlowSequence,
    FlowMapping,
    BlockSequence,
    BlockMapping,
    /// A key with nothing after its colon on the key line.
    EmptyValue,
}

impl ValueStyle {
    /// Whether this style holds a sequence, and so is edited by replacing the
    /// whole entry rather than a value span.
    pub(crate) fn is_sequence(self) -> bool {
        matches!(self, ValueStyle::FlowSequence | ValueStyle::BlockSequence)
    }
}

/// A top-level frontmatter field and the bytes it occupies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Field {
    pub name: String,
    /// The whole entry: key line through continuation lines, trailing blank
    /// lines and column-0 comments excluded.
    pub line_range: Range<usize>,
    /// The value's bytes on the key line, or `None` when they cannot be named
    /// as one span the parser agrees with. A field whose value is empty
    /// carries a zero-width range just past the colon, so setting a stubbed
    /// field is a splice like any other.
    pub value_range: Option<Range<usize>>,
    pub style: ValueStyle,
}

/// One field per top-level key in `content[frontmatter_range]`, in document
/// order, or `None` when the block's spans cannot be trusted.
///
/// `None` and an empty split are different answers and the distinction is
/// load-bearing: a block holding no fields is fully understood and takes an
/// appended one, while a block whose split was refused has lines nothing can
/// attribute and refuses every edit. A block whose only key the value model
/// dropped reaches both descriptions by counting, and only one of them is
/// true.
pub(crate) fn field_spans(
    content: &str,
    frontmatter_range: Range<usize>,
    value: &Value,
    strip: StripReport,
) -> Option<Vec<Field>> {
    let Some(map) = value.as_map() else {
        return Some(Vec::new());
    };
    if !strip.is_clean() {
        return None;
    }

    let candidates = scan_key_lines(content, &frontmatter_range);
    // Every scanned line asks the mapping about its key, so the mapping is
    // indexed once here rather than scanned once per line: the block's byte
    // bound admits thousands of keys, and a scan per key is quadratic in how
    // many of them there are.
    let parsed_keys = KeyIndex::of(map);

    let mut name_counts: HashMap<&str, usize> = HashMap::new();
    for candidate in &candidates {
        *name_counts.entry(candidate.name.as_str()).or_insert(0) += 1;
    }
    let every_key_uniquely_located = map
        .keys()
        .all(|key| name_counts.get(key).copied() == Some(1));
    if !every_key_uniquely_located {
        return None;
    }

    // An entry ends where the next one begins. A merge-key line begins no
    // entry — expansion folded it away, so no parsed key names it — but it
    // still bounds the entry above it: absorbing it would hand a directive's
    // bytes to an unrelated field's remove. Every other unconfirmed candidate
    // is a `key:` lookalike inside the value above it and is absorbed on
    // purpose.
    let boundaries: Vec<&RawKeyLine> = candidates
        .iter()
        .filter(|candidate| {
            parsed_keys.contains_key(&candidate.name) || candidate.name == MERGE_KEY
        })
        .collect();

    let mut fields = Vec::with_capacity(boundaries.len());
    for (index, candidate) in boundaries.iter().enumerate() {
        if candidate.name == MERGE_KEY {
            continue;
        }
        let entry_end = boundaries
            .get(index + 1)
            .map(|next| next.key_line_start)
            .unwrap_or(frontmatter_range.end);

        let parsed = parsed_keys
            .get(&candidate.name)
            .expect("a boundary that is not the merge key is a parsed key");
        let trailing = &content[candidate.key_line_end..entry_end];
        let line_end = if candidate.keeps_trailing_blanks {
            entry_end - unowned_trailing_bytes(trailing, owned_blank_lines(parsed))
        } else {
            entry_end - trailing_separator_bytes(trailing)
        };
        let (value_range, style) = resolve_value(content, candidate, parsed);

        fields.push(Field {
            name: candidate.name.clone(),
            line_range: candidate.key_line_start..line_end.max(candidate.key_line_end),
            value_range,
            style,
        });
    }

    Some(fields)
}

/// The trailing lines of `slice` that are whole blank lines or column-0
/// comment lines, in document order.
fn trailing_separator_run(slice: &str) -> Vec<&str> {
    let mut lines: Vec<&str> = Vec::new();
    for line in slice.split_inclusive('\n').rev() {
        let text = line.trim_end_matches(['\r', '\n']);
        if text.trim().is_empty() || text.starts_with('#') {
            lines.push(line);
        } else {
            break;
        }
    }
    lines.reverse();
    lines
}

/// The trailing bytes of `slice` that are whole blank lines or column-0
/// comment lines.
fn trailing_separator_bytes(slice: &str) -> usize {
    trailing_separator_run(slice)
        .iter()
        .map(|line| line.len())
        .sum()
}

/// The trailing separator bytes a keep-chomped block scalar does not own: the
/// run past the first `owned` blank lines of it.
///
/// A comment is never one of them. Keep-chomping asks for the blank lines
/// below the value, and a column-0 comment ends the scalar rather than
/// extending it, so a standing comment under a `|+` block — and everything
/// after it — belongs to the document and survives the field's removal.
fn unowned_trailing_bytes(slice: &str, owned: usize) -> usize {
    let run = trailing_separator_run(slice);
    let mut kept = 0;
    while kept < owned && run.get(kept).is_some_and(|line| line.trim().is_empty()) {
        kept += 1;
    }
    run[kept..].iter().map(|line| line.len()).sum()
}

/// How many blank lines a keep-chomped value's own bytes prove it owns.
///
/// Every trailing newline past the one terminating the value's last line is an
/// empty line the value carries. A value that is nothing but newlines carries
/// all of them.
fn owned_blank_lines(parsed: &Value) -> usize {
    let Value::String(text) = parsed else {
        return 0;
    };
    let trailing = text.bytes().rev().take_while(|byte| *byte == b'\n').count();
    if trailing == text.len() {
        trailing
    } else {
        trailing - 1
    }
}

/// A column-0 mapping key line the scanner proposes, before validation.
struct RawKeyLine {
    key_line_start: usize,
    /// The byte just past this key line's newline — where its continuation
    /// lines, if any, begin.
    key_line_end: usize,
    name: String,
    proposed_value_range: Option<Range<usize>>,
    proposed_style: ValueStyle,
    /// A block scalar whose header asks for keep-chomping, which makes the
    /// blank lines trailing it content rather than separators.
    keeps_trailing_blanks: bool,
}

/// Decide a candidate's final value range and style against the parsed value.
fn resolve_value(
    content: &str,
    candidate: &RawKeyLine,
    parsed: &Value,
) -> (Option<Range<usize>>, ValueStyle) {
    if candidate.proposed_style == ValueStyle::EmptyValue {
        return match parsed {
            // A key with nothing after its colon really is null: the proposed
            // range is an insertion point, and there are no bytes to validate.
            Value::Null => (
                candidate.proposed_value_range.clone(),
                ValueStyle::EmptyValue,
            ),
            // The value lives on the lines below, so the entry is replaced
            // whole or not at all.
            Value::Sequence(_) => (None, ValueStyle::BlockSequence),
            Value::Map(_) => (None, ValueStyle::BlockMapping),
            // A plain scalar continued on an indented line. Nothing on the key
            // line is the value, so no span can name it.
            _ => (None, ValueStyle::EmptyValue),
        };
    }
    (
        validate_value_span(content, &candidate.proposed_value_range, parsed),
        candidate.proposed_style,
    )
}

/// A proposed span survives only when its bytes reconstitute to exactly the
/// parsed value.
fn validate_value_span(
    content: &str,
    proposed: &Option<Range<usize>>,
    parsed: &Value,
) -> Option<Range<usize>> {
    let range = proposed.clone()?;
    let slice = &content[range.clone()];
    match parsed {
        // A mapping value — an anchored block map, a flow map — is never a
        // scalar span.
        Value::Map(_) => None,
        // Null is the trap: a bare `# comment` re-parses to null and would
        // falsely compare equal, so a literal null token is required.
        Value::Null => is_null_token(slice).then_some(range),
        expected => match reparse(slice) {
            Some(actual) if &actual == expected => Some(range),
            _ => None,
        },
    }
}

fn is_null_token(text: &str) -> bool {
    matches!(text, "null" | "Null" | "NULL" | "~")
}

/// Re-parse a value slice through the same pipeline the block went through, so
/// `12:30`, `1.10` and `yes` are read identically on both sides.
pub(crate) fn reparse(text: &str) -> Option<Value> {
    let parsed = crate::frontmatter::extract::parse_block(text).ok()?;
    let mut discarded = Vec::new();
    let mut strip = StripReport::default();
    let value = crate::value::from_yaml(parsed, &mut String::new(), &mut discarded, &mut strip);
    strip.is_clean().then_some(value)
}

/// Identify every top-level `key:` line and the scanner's candidate value span
/// for it. A multi-line value whose continuation can sit at column 0 — an
/// unclosed flow collection or quoted scalar — is stepped over so its
/// continuation lines are not misread as new keys; block scalars, folds and
/// block collections continue on indented lines the scan already skips.
fn scan_key_lines(content: &str, frontmatter_range: &Range<usize>) -> Vec<RawKeyLine> {
    let yaml = &content[frontmatter_range.clone()];
    let lines: Vec<&str> = yaml.split_inclusive('\n').collect();
    let mut line_starts: Vec<usize> = Vec::with_capacity(lines.len() + 1);
    let mut accumulated = frontmatter_range.start;
    for line in &lines {
        line_starts.push(accumulated);
        accumulated += line.len();
    }
    line_starts.push(accumulated);

    let mut fields = Vec::new();
    let mut index = 0;
    while index < lines.len() {
        let line = lines[index];
        if line.starts_with([' ', '\t']) {
            index += 1;
            continue;
        }
        let line_start = line_starts[index];
        let trimmed_line = line.trim_end_matches(['\r', '\n']);

        let Some((name, after_colon)) = parse_top_level_key(trimmed_line) else {
            index += 1;
            continue;
        };

        let rest = &trimmed_line[after_colon..];
        let (proposed_value_range, proposed_style, ends_on_key_line) =
            classify_value(line_start, after_colon, rest);

        fields.push(RawKeyLine {
            key_line_start: line_start,
            key_line_end: line_starts[index + 1],
            name,
            proposed_value_range,
            proposed_style,
            keeps_trailing_blanks: keeps_trailing_blanks(proposed_style, rest),
        });

        index += 1;
        if !ends_on_key_line {
            index = absorb_open_value(&lines, index, proposed_style, rest);
        }
    }

    fields
}

/// Whether a block scalar's header asks for the blank lines below it to be
/// kept as content (`|+`, `>+`, `|2+`, `|+2`).
///
/// The header is the indicator, then an optional indentation digit and an
/// optional chomping indicator in either order, and it ends at the first byte
/// that is neither — a space, and therefore any comment. A `+` past that point
/// is prose: reading it as keep-chomping hands the field the blank lines and
/// the standing comment below it, and the next remove deletes both.
fn keeps_trailing_blanks(style: ValueStyle, rest: &str) -> bool {
    if !matches!(style, ValueStyle::BlockLiteral | ValueStyle::BlockFolded) {
        return false;
    }
    let mut header = rest.trim_start().bytes();
    if !matches!(header.next(), Some(b'|') | Some(b'>')) {
        return false;
    }
    let mut keep = false;
    for byte in header {
        match byte {
            b'0'..=b'9' => {}
            b'+' => keep = true,
            b'-' => return false,
            _ => break,
        }
    }
    keep
}

/// Advance past the continuation lines of a value that opened on the key line
/// and did not close there — an unclosed flow collection or quoted scalar,
/// whose continuation lines can sit at column 0. Everything else continues on
/// indented lines the main scan already skips.
fn absorb_open_value(
    lines: &[&str],
    index: usize,
    style: ValueStyle,
    key_line_rest: &str,
) -> usize {
    match style {
        ValueStyle::FlowSequence => absorb_flow_value(lines, index, b']', key_line_rest),
        ValueStyle::FlowMapping => absorb_flow_value(lines, index, b'}', key_line_rest),
        ValueStyle::SingleQuoted => absorb_until_line_contains(lines, index, '\''),
        ValueStyle::DoubleQuoted => absorb_until_line_contains(lines, index, '"'),
        _ => index,
    }
}

/// Best-effort absorb for an unclosed quoted scalar: stop after the first line
/// holding the closing quote character.
fn absorb_until_line_contains(lines: &[&str], mut index: usize, closer: char) -> usize {
    while index < lines.len() {
        let contains_closer = lines[index].contains(closer);
        index += 1;
        if contains_closer {
            break;
        }
    }
    index
}

/// Absorb an unclosed flow collection, stopping after the first line holding
/// `closer` **outside** a quoted scalar. A `]` inside `"b]c"` is structural to
/// the item, not the flow, and stopping there would re-expose an interior
/// `key:`-shaped line as a phantom candidate. Quote state is seeded from the
/// key line's own bytes, because a quote opened after the bracket
/// (`foo: ["a,`) is still open on the first continuation line.
fn absorb_flow_value(lines: &[&str], mut index: usize, closer: u8, key_line_rest: &str) -> usize {
    let mut in_single = false;
    let mut in_double = false;
    scan_flow_line(
        key_line_rest.as_bytes(),
        closer,
        &mut in_single,
        &mut in_double,
    );
    while index < lines.len() {
        let found = scan_flow_line(
            lines[index].as_bytes(),
            closer,
            &mut in_single,
            &mut in_double,
        );
        index += 1;
        if found {
            break;
        }
    }
    index
}

/// One line of the flow absorb: advance the quote state and report whether
/// `closer` occurred outside quotes. An unquoted `#` at line start or after
/// whitespace begins a comment, which is legal inside a flow collection, so
/// scanning stops there.
fn scan_flow_line(bytes: &[u8], closer: u8, in_single: &mut bool, in_double: &mut bool) -> bool {
    let mut index = 0;
    while index < bytes.len() {
        let byte = bytes[index];
        if *in_single {
            if byte == b'\'' {
                if bytes.get(index + 1) == Some(&b'\'') {
                    index += 2;
                    continue;
                }
                *in_single = false;
            }
        } else if *in_double {
            if byte == b'\\' {
                index += 2;
                continue;
            }
            if byte == b'"' {
                *in_double = false;
            }
        } else if byte == b'\'' {
            *in_single = true;
        } else if byte == b'"' {
            *in_double = true;
        } else if byte == b'#' && (index == 0 || matches!(bytes[index - 1], b' ' | b'\t')) {
            return false;
        } else if byte == closer {
            return true;
        }
        index += 1;
    }
    false
}

/// Recognize a top-level `key:` mapping entry at column 0 of `line`, whose
/// trailing newline is already stripped. Returns the *decoded* key name — the
/// spelling the parser keys the mapping by — and the byte offset in `line`
/// just past the `:` separator.
///
/// Recognized structurally, with no identifier allowlist: plain keys, single-
/// and double-quoted keys, numeric-leading keys, and the merge key (`<<`).
fn parse_top_level_key(line: &str) -> Option<(String, usize)> {
    if line.starts_with('#') {
        return None;
    }
    match line.as_bytes().first()? {
        b'\'' => {
            let (name, end) = scan_single_quoted_key(line)?;
            Some((name, colon_after(line, end)?))
        }
        b'"' => {
            let (name, end) = scan_double_quoted_key(line)?;
            Some((name, colon_after(line, end)?))
        }
        _ => {
            let separator = find_plain_key_separator(line)?;
            let name = line[..separator].trim_end();
            if name.is_empty() {
                return None;
            }
            Some((name.to_string(), separator + 1))
        }
    }
}

/// The first `:` acting as a block-mapping separator — a colon followed by
/// whitespace or end of line. A `time: 12:30` value is unaffected: the later
/// colon has no following whitespace.
fn find_plain_key_separator(line: &str) -> Option<usize> {
    let bytes = line.as_bytes();
    for (index, byte) in bytes.iter().enumerate() {
        if *byte == b':' {
            match bytes.get(index + 1) {
                None | Some(b' ') | Some(b'\t') => return Some(index),
                _ => {}
            }
        }
    }
    None
}

/// Skip inline whitespace from `from` and return the offset just past a `:`.
fn colon_after(line: &str, from: usize) -> Option<usize> {
    let bytes = line.as_bytes();
    let mut index = from;
    while matches!(bytes.get(index), Some(b' ') | Some(b'\t')) {
        index += 1;
    }
    (bytes.get(index) == Some(&b':')).then_some(index + 1)
}

/// Decode a single-quoted key at byte 0 of `line`, honoring the `''` escape.
fn scan_single_quoted_key(line: &str) -> Option<(String, usize)> {
    let mut out = String::new();
    let mut chars = line[1..].char_indices();
    while let Some((relative, ch)) = chars.next() {
        let absolute = 1 + relative;
        if ch == '\'' {
            if line[absolute + 1..].starts_with('\'') {
                out.push('\'');
                chars.next();
                continue;
            }
            return Some((out, absolute + 1));
        }
        out.push(ch);
    }
    None
}

/// Decode a double-quoted key at byte 0 of `line`, honoring `\"`, `\\` and the
/// common control escapes.
fn scan_double_quoted_key(line: &str) -> Option<(String, usize)> {
    let mut out = String::new();
    let mut chars = line[1..].char_indices();
    while let Some((relative, ch)) = chars.next() {
        let absolute = 1 + relative;
        match ch {
            '\\' => {
                let (_, escaped) = chars.next()?;
                out.push(match escaped {
                    'n' => '\n',
                    't' => '\t',
                    'r' => '\r',
                    '0' => '\0',
                    other => other,
                });
            }
            '"' => return Some((out, absolute + 1)),
            _ => out.push(ch),
        }
    }
    None
}

/// Classify the value portion of a key line.
///
/// `line_start` is the key line's byte offset in the content, `after_colon` is
/// the offset within the trimmed key line just past the `:`, and `rest` is
/// everything after it. Returns the candidate span, the style, and whether the
/// value is complete on the key line.
pub(crate) fn classify_value(
    line_start: usize,
    after_colon: usize,
    rest: &str,
) -> (Option<Range<usize>>, ValueStyle, bool) {
    let value_offset = rest
        .char_indices()
        .find(|(_, ch)| !ch.is_whitespace())
        .map(|(index, _)| index);

    // Nothing after the colon but whitespace, or whitespace and a comment: the
    // key is a stub, and the span is the insertion point a set splices into.
    // With nothing but whitespace it covers that whitespace, so `status:   `
    // does not become `status: draft   `. With a comment it is zero-width at
    // the colon, so the padding and the comment both stay where their author
    // put them.
    let comment_only = value_offset.is_none_or(|index| rest.as_bytes()[index] == b'#');
    if comment_only {
        let start = line_start + after_colon;
        let width = if value_offset.is_some() {
            0
        } else {
            rest.len()
        };
        return (Some(start..start + width), ValueStyle::EmptyValue, false);
    }
    let value_offset = value_offset.expect("a comment-only value returned above");

    let value_start = line_start + after_colon + value_offset;
    let value_text = &rest[value_offset..];
    let first = value_text.chars().next().expect("a non-whitespace char");

    match first {
        '|' => (None, ValueStyle::BlockLiteral, false),
        '>' => (None, ValueStyle::BlockFolded, false),
        // An anchor (`&a`), an alias (`*a`) or a tag (`!!str`, `!foo`) is YAML
        // markup, not value bytes: rewriting or deleting the marker would
        // corrupt the document. The span is refused and the key stays
        // removable through `line_range`. There is no style for it, and none
        // is needed — no value is ever serialized into a refused span.
        '&' | '*' | '!' => (None, ValueStyle::Plain, true),
        '\'' => {
            let bytes = value_text.as_bytes();
            let mut index = 1;
            while index < bytes.len() {
                if bytes[index] == b'\'' {
                    if bytes.get(index + 1) == Some(&b'\'') {
                        index += 2;
                        continue;
                    }
                    return (
                        Some(value_start..value_start + index + 1),
                        ValueStyle::SingleQuoted,
                        true,
                    );
                }
                index += 1;
            }
            (None, ValueStyle::SingleQuoted, false)
        }
        '"' => {
            let bytes = value_text.as_bytes();
            let mut index = 1;
            let mut escaped = false;
            while index < bytes.len() {
                if escaped {
                    escaped = false;
                    index += 1;
                    continue;
                }
                match bytes[index] {
                    b'\\' => escaped = true,
                    b'"' => {
                        return (
                            Some(value_start..value_start + index + 1),
                            ValueStyle::DoubleQuoted,
                            true,
                        );
                    }
                    _ => {}
                }
                index += 1;
            }
            (None, ValueStyle::DoubleQuoted, false)
        }
        // A flow collection is never precisely value-spanned: quotes, nesting
        // and trailing content all defeat a byte scan. It carries no value
        // span and a sequence is edited by replacing the whole entry.
        '[' => (None, ValueStyle::FlowSequence, value_text.contains(']')),
        '{' => (None, ValueStyle::FlowMapping, value_text.contains('}')),
        _ => {
            let bytes = value_text.as_bytes();
            let mut end = bytes.len();
            for index in 0..bytes.len() {
                if bytes[index] == b'#' && index > 0 && matches!(bytes[index - 1], b' ' | b'\t') {
                    end = index;
                    break;
                }
            }
            // A plain scalar has its trailing whitespace stripped by YAML, so
            // the span stops before it — otherwise a set would eat it.
            while end > 0 && matches!(bytes[end - 1], b'\r' | b'\n' | b' ' | b'\t') {
                end -= 1;
            }
            (
                Some(value_start..value_start + end),
                ValueStyle::Plain,
                true,
            )
        }
    }
}
