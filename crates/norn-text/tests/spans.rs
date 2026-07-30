//! Positions: the source span type, and where a frontmatter string's bytes
//! really are.

use norn_text::{Document, SourceSpan, Value};

// ── SourceSpan ───────────────────────────────────────────────────────────

#[test]
fn a_span_reports_a_one_based_line_and_column() {
    let content = "hello\nworld\n";
    assert_eq!(
        SourceSpan::at(content, 0),
        SourceSpan {
            line: 1,
            column: 1,
            byte_offset: 0
        }
    );
    let offset = content.find("world").expect("world");
    assert_eq!(
        SourceSpan::at(content, offset),
        SourceSpan {
            line: 2,
            column: 1,
            byte_offset: offset
        }
    );
    let column = "abc def\n";
    assert_eq!(SourceSpan::at(column, 4).column, 5);
}

/// The clamp is real, not a doc claim. An offset past the end lands on the
/// end, and the reported offset is the clamped one — so a caller that trusts
/// the documentation and slices at it cannot panic.
#[test]
fn an_offset_past_the_end_is_clamped_to_the_end() {
    let content = "ab";
    let span = SourceSpan::at(content, 999);
    assert_eq!(span.byte_offset, content.len());
    assert_eq!(span.line, 1);
    assert_eq!(span.column, 3);
    assert_eq!(&content[span.byte_offset..], "");
}

/// An offset inside a multi-byte character lands on that character's first
/// byte, for the same reason.
#[test]
fn an_offset_inside_a_character_is_clamped_to_its_first_byte() {
    let content = "aé b";
    // `é` occupies bytes 1 and 2.
    let span = SourceSpan::at(content, 2);
    assert_eq!(span.byte_offset, 1);
    assert_eq!(&content[span.byte_offset..], "é b");
    assert_eq!(span.column, 2);
}

#[test]
fn an_empty_string_has_one_position() {
    let span = SourceSpan::at("", 0);
    assert_eq!(
        span,
        SourceSpan {
            line: 1,
            column: 1,
            byte_offset: 0
        }
    );
    assert_eq!(SourceSpan::at("", 7), span);
}

// ── The two origins (NORN-25) ────────────────────────────────────────────

/// There are two coordinate origins in this crate, one `body_start` apart, and
/// a caller adding the difference by hand is how a body offset gets used
/// against a source. A `Document` reports both headings and wikilinks in its
/// own coordinates, with the line and column recomputed against the whole
/// document rather than shifted.
#[test]
fn a_document_reports_its_constructs_in_source_coordinates() {
    let source =
        "---\ntitle: t\ntags:\n  - a\n---\n# One\n\nsee [[Target]] and [[Other]]\n\n## Two\n";
    let document = Document::parse(source);
    let scan = document.scan_body();

    let headings = document.headings();
    assert_eq!(headings.len(), 2);
    for (source_side, body_side) in headings.iter().zip(scan.headings()) {
        assert_eq!(
            source_side.span.byte_offset,
            body_side.span.byte_offset + document.body_start()
        );
        assert_eq!(
            source_side.body_offset,
            body_side.body_offset + document.body_start()
        );
        assert!(source[source_side.span.byte_offset..].starts_with('#'));
    }
    // The heading's line is its line in the document, not in the body.
    assert_eq!(headings[0].text, "One");
    assert_eq!(headings[0].span.line, 6);
    assert_eq!(headings[0].span.column, 1);
    assert_eq!(headings[1].span.line, 10);

    let links = document.wikilinks();
    assert_eq!(links.len(), 2);
    assert_eq!(&source[links[0].range()], "[[Target]]");
    assert_eq!(&source[links[1].range()], "[[Other]]");
    assert_eq!(links[0].span.line, 8);
    assert_eq!(links[0].span.column, 5);
}

/// A lone `\r` ends a line. Counting only `\n` reported line 1 for every
/// construct in a CR-only document, which is a position that points at nothing.
#[test]
fn a_carriage_return_alone_ends_a_line() {
    let content = "one\rtwo\rthree";
    assert_eq!(SourceSpan::at(content, 4).line, 2);
    assert_eq!(SourceSpan::at(content, 4).column, 1);
    assert_eq!(SourceSpan::at(content, 8).line, 3);
    // And a CRLF pair is one break, not two.
    assert_eq!(SourceSpan::at("one\r\ntwo", 5).line, 2);
    assert_eq!(SourceSpan::at("one\r\ntwo", 5).column, 1);
}

/// One break is one position. An offset landing on the `\n` of a `\r\n` is
/// inside a break rather than at a place in the text, so it is clamped to the
/// break's first byte — the same clamp a multi-byte character gets. Without it
/// the interior of a break is a position a walk that stops there and a walk
/// that resumes there answer differently.
#[test]
fn an_offset_inside_a_crlf_pair_is_clamped_to_the_carriage_return() {
    let content = "one\r\ntwo\r\n";
    let span = SourceSpan::at(content, 4);
    assert_eq!(span.byte_offset, 3);
    assert_eq!(span.line, 1);
    assert_eq!(span.column, 4);
    // Every construct after it still reports the line it is on.
    assert_eq!(SourceSpan::at(content, 5).line, 2);
    assert_eq!(SourceSpan::at(content, 9).line, 2);
    assert_eq!(SourceSpan::at(content, content.len()).line, 3);
}

// ── Frontmatter strings and their bytes (NRN-499) ────────────────────────

fn texts(source: &str) -> Vec<(String, String, Option<String>)> {
    Document::parse(source)
        .field_texts()
        .into_iter()
        .map(|entry| {
            (
                entry.field.to_string(),
                entry.text.to_string(),
                entry.range.map(|range| source[range].to_string()),
            )
        })
        .collect()
}

/// NRN-499: offsets were found by searching the key's line for the *decoded*
/// value, so a value whose text also appears in its own key reported the key's
/// offset. The range comes from the field layer now, and names the value.
#[test]
fn a_value_whose_text_repeats_its_key_reports_the_value_not_the_key() {
    let source = "---\nname: name\n---\n";
    let document = Document::parse(source);
    let entry = document
        .field_texts()
        .into_iter()
        .next()
        .expect("one string");
    let range = entry.range.expect("a range");
    assert_eq!(&source[range.clone()], "name");
    assert_eq!(range.start, source.find(": name").expect("the value") + 2);
}

/// The same search matched a value's text in a neighbouring field's line. Each
/// field's range comes from its own span.
#[test]
fn each_field_reports_its_own_bytes() {
    assert_eq!(
        texts("---\nname: foo\ntitle: name\n---\n"),
        [
            (
                "name".to_string(),
                "foo".to_string(),
                Some("foo".to_string())
            ),
            (
                "title".to_string(),
                "name".to_string(),
                Some("name".to_string())
            ),
        ]
    );
}

/// The search did not know what a comment was, so a value whose text also
/// appeared in a trailing comment could report the comment's offset. The span
/// stops before the comment.
#[test]
fn a_trailing_comment_is_not_part_of_the_value_bytes() {
    assert_eq!(
        texts("---\ntitle: hello # hello\n---\n"),
        [(
            "title".to_string(),
            "hello".to_string(),
            Some("hello".to_string())
        )]
    );
}

/// A decoded value need not appear literally in the document at all. A search
/// for it found nothing and reported no offset; the span reports the bytes
/// that produced it, quotes and escapes included.
#[test]
fn an_escaped_value_reports_the_bytes_it_was_written_as() {
    assert_eq!(
        texts("---\ntitle: \"a\\tb\"\n---\n"),
        [(
            "title".to_string(),
            "a\tb".to_string(),
            Some("\"a\\tb\"".to_string())
        )]
    );
}

/// A value continued across lines has no single span to name, so the text is
/// reported without one rather than with a guess.
#[test]
fn a_value_spanning_lines_reports_its_text_and_no_range() {
    assert_eq!(
        texts("---\ntitle: one\n  two\n---\n"),
        [("title".to_string(), "one two".to_string(), None)]
    );
}

#[test]
fn each_item_of_a_block_sequence_reports_its_own_bytes() {
    let source = "---\naliases:\n  - one\n  - \"t\\two\"\n  - 'three'\n---\n";
    assert_eq!(
        texts(source),
        [
            (
                "aliases".to_string(),
                "one".to_string(),
                Some("one".to_string())
            ),
            (
                "aliases".to_string(),
                "t\two".to_string(),
                Some("\"t\\two\"".to_string())
            ),
            (
                "aliases".to_string(),
                "three".to_string(),
                Some("'three'".to_string())
            ),
        ]
    );
}

/// A flow sequence's items are not byte-scannable for the same reason its
/// value is not: quotes, nesting and trailing content defeat the scan. The
/// items are reported without ranges.
#[test]
fn flow_sequence_items_report_their_text_and_no_range() {
    assert_eq!(
        texts("---\naliases: [one, two]\n---\n"),
        [
            ("aliases".to_string(), "one".to_string(), None),
            ("aliases".to_string(), "two".to_string(), None),
        ]
    );
}

#[test]
fn non_string_values_contribute_no_text() {
    assert_eq!(
        texts(
            "---\ncount: 3\nflag: true\nnothing:\nnested:\n  inner: x\nmixed:\n  - a\n  - 2\n---\n"
        ),
        [("mixed".to_string(), "a".to_string(), Some("a".to_string()))]
    );
}

/// The ranges are absolute in the document, mark and all, so a caller scanning
/// frontmatter values can report a position in the file it read.
#[test]
fn ranges_are_absolute_in_the_document() {
    let source = "\u{feff}---\ntitle: \"[[Target]]\"\n---\nbody\n";
    let document = Document::parse(source);
    let entry = document
        .field_texts()
        .into_iter()
        .next()
        .expect("one string");
    let range = entry.range.expect("a range");
    assert_eq!(&source[range.clone()], "\"[[Target]]\"");
    assert_eq!(entry.text, "[[Target]]");
    assert!(
        range.start > 3,
        "the range must sit past the byte-order mark"
    );
}

#[test]
fn a_block_whose_spans_are_untrusted_reports_no_strings() {
    let document = Document::parse("---\n1: x\ntitle: t\n---\n");
    assert!(document.field_texts().is_empty());
    // Reading the value still works; only the byte ranges are withheld.
    assert_eq!(
        document.frontmatter(),
        Some(&Value::Map([("title", "t")].into_iter().collect()))
    );
}

/// Frontmatter values are handed out as text plus the bytes that produced
/// them, and this crate reads no syntax inside them. A caller that wants the
/// links in a frontmatter value scans the text itself, through the one token
/// parser, and maps the result back through the range.
#[test]
fn frontmatter_values_are_text_this_crate_reads_no_syntax_in() {
    let source = "---\nsee: \"[[Target|Shown]]\"\nmd: \"[text](target.md)\"\n---\nbody\n";
    let document = Document::parse(source);
    let entries = document.field_texts();
    assert_eq!(entries.len(), 2);
    // Neither value is decomposed here; both are strings.
    assert_eq!(entries[0].text, "[[Target|Shown]]");
    assert_eq!(entries[1].text, "[text](target.md)");
    // A caller scans the text and locates the result through the range.
    let links = norn_text::parse_wikilinks_in_text(entries[0].text);
    assert_eq!(links.len(), 1);
    assert_eq!(links[0].target, "Target");
    assert_eq!(links[0].title.as_deref(), Some("Shown"));
    // The Markdown-link value holds no wikilink, and this crate reads no other
    // link syntax.
    assert!(norn_text::parse_wikilinks_in_text(entries[1].text).is_empty());
}
