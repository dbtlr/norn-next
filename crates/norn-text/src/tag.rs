//! `#tag` syntax — one grammar, two homes.
//!
//! A tag is a `#` marker followed by a run of tag characters: unicode letters,
//! digits, `_`, `-` and `/`, the last of which nests (`#area/project`). The
//! run ends at the first character outside that set, at least one character in
//! it is not a digit — `#123` is a number somebody wrote down, not a tag — and
//! the case is recorded as it was written, because deciding that `#Work` and
//! `#work` are the same tag is a matching question and matching is not syntax.
//!
//! # Where a marker counts
//!
//! In a body, a `#` opens a tag only at the start of the text or after a
//! character that is not part of a word — so `foo#bar` carries no tag, while
//! `(#tag)` and a `#tag` at the start of a line both do. A line-leading `#tag`
//! is unambiguous in the other direction too: CommonMark wants a space after
//! the hashes, so `#tag` is a paragraph and `# tag` is a heading, and the two
//! grammars partition rather than compete.
//!
//! In frontmatter, the `tags` field's strings follow the identical grammar
//! with the marker optional: `#foo` and `foo` are both the tag `foo`, so a
//! hand-written marker is read as the syntax it is instead of becoming part of
//! the name. Nothing else in frontmatter is scanned for tags — a `#tag` inside
//! some other string value is text in a string, not a tag.
//!
//! Whether a string that is *not* a tag under this grammar is an error is a
//! validation question with its own venue. Here it is simply not a tag, and no
//! diagnostic is raised about it.

use std::ops::Range;

use crate::body::overlaps_any;
use crate::span::{LineCursor, SourceSpan};

/// A recognized tag, marker excluded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tag {
    /// The tag name as written, without the `#`. Nesting stays in it:
    /// `#area/project` is the one name `area/project`.
    pub name: String,
    /// Where the tag begins — the `#` of a body token, the first byte of the
    /// entry that produced a frontmatter tag.
    ///
    /// `None` only for a frontmatter tag whose entry's bytes cannot be named
    /// as one span, which is the same refusal the field layer makes about a
    /// value it cannot locate exactly.
    pub span: Option<SourceSpan>,
}

/// Whether `ch` may appear in a tag name.
fn is_tag_char(ch: char) -> bool {
    ch.is_alphanumeric() || matches!(ch, '_' | '-' | '/')
}

/// Whether `ch` is part of a word, and so cannot sit in front of a marker.
fn is_word_char(ch: char) -> bool {
    ch.is_alphanumeric() || ch == '_'
}

/// Whether `name` is a tag name: non-empty, every character in the set, and at
/// least one of them not a digit.
fn is_tag_name(name: &str) -> bool {
    !name.is_empty() && name.chars().all(is_tag_char) && name.chars().any(|ch| !ch.is_numeric())
}

/// Read one frontmatter `tags` entry as a tag name, marker optional.
///
/// `None` for a string the grammar does not describe. Judging that string is
/// validation's job, not this crate's.
pub(crate) fn frontmatter_tag_name(entry: &str) -> Option<String> {
    let name = entry.strip_prefix('#').unwrap_or(entry);
    is_tag_name(name).then(|| name.to_string())
}

/// Every `#tag` in `body`, in document order, skipping the ranges in
/// `ignored`.
///
/// `ignored` masks the text a tag may not be read out of: code, where the
/// document is a different document, and link tokens, where a `#` is the
/// link's own fragment syntax rather than a marker. Ranges must be ascending,
/// non-overlapping and non-empty.
pub(crate) fn scan_tags(body: &str, ignored: &[Range<usize>]) -> Vec<Tag> {
    let mut tags = Vec::new();
    // Tokens are found in ascending order, so their positions are counted
    // once across the body rather than once per tag.
    let mut cursor = LineCursor::new(body);
    let mut at = 0;

    while let Some(found) = body[at..].find('#') {
        let marker = at + found;
        let mut end = marker + 1;
        for ch in body[marker + 1..].chars() {
            if !is_tag_char(ch) {
                break;
            }
            end += ch.len_utf8();
        }
        // The character the run stopped on is the next candidate's opener, so
        // the walk resumes there rather than past it.
        at = end.max(marker + 1);

        let name = &body[marker + 1..end];
        let opens = body[..marker]
            .chars()
            .next_back()
            .is_none_or(|before| !is_word_char(before));
        if opens && is_tag_name(name) && !overlaps_any(ignored, &(marker..end)) {
            tags.push(Tag {
                name: name.to_string(),
                span: Some(cursor.span_at(marker)),
            });
        }
    }

    tags
}
