//! One line break, everywhere: `\n`, `\r\n`, or a lone `\r`.
//!
//! A lone `\r` is a line ending in CommonMark and in YAML, so it is one here,
//! and the crate answers the same questions about a document however its
//! author terminated the lines. Two things make that true and are checked
//! here: the CommonMark pass is handed input whose lone `\r` breaks read as
//! `\n` without any byte moving, and every line-shaped scan in the crate cuts
//! on the one break rule rather than on `\n` alone.
//!
//! The reading half is the offset property — what a scan reports still
//! addresses the bytes the caller handed in.

use norn_text::BodyScan;

// ── The offset property ──────────────────────────────────────────────────

/// A document with something for every family to find: a heading, both link
/// grammars, a tag, a block id, and a fenced sample holding one of each that
/// none of them may reach.
const TEMPLATE: &str = "# Alpha\n\nprose [[Note|shown]] and [text](./t.md) and #atag ^aid\n\n```\nfenced [[Hidden]] #hidden ^hid\n```\n\n## Beta\ntail #btag\n";

/// `TEMPLATE` rewritten so its breaks cycle through the three styles, so the
/// mixed document is mixed at every line rather than in one place.
///
/// One adjustment keeps the rewrite line-count-preserving: a blank line
/// written as `\n` directly after a `\r` would weld the two into a single
/// `\r\n` break and lose a line, so a blank line in that position is written
/// `\r` instead.
fn mixed(template: &str) -> String {
    let styles = ["\n", "\r\n", "\r"];
    let mut out = String::new();
    for (index, line) in template.split_inclusive('\n').enumerate() {
        let content = line.trim_end_matches('\n');
        let mut style = styles[index % styles.len()];
        if content.is_empty() && style == "\n" && out.ends_with('\r') {
            style = "\r";
        }
        out.push_str(content);
        out.push_str(style);
    }
    out
}

/// Every construct `document` carries, named by what the document's **own
/// bytes** say at the span the scan reported.
///
/// Nothing here quotes a fact's parsed text alone: each line is built by
/// slicing `document` at the reported offset, so a span that drifted off its
/// construct produces a different fact rather than the same one. Line and
/// column ride along, since a position that addresses the right byte under the
/// wrong line number is still wrong.
fn facts_by_slicing(document: &str) -> Vec<String> {
    let scan = BodyScan::new(document);
    let mut facts = Vec::new();
    let mut record = |kind: &str, span: norn_text::SourceSpan, expected: &str| {
        let at = span.byte_offset;
        assert!(
            document[at..].starts_with(expected),
            "a {kind} span points at {:?}, not at {expected:?}",
            &document[at..(at + expected.len()).min(document.len())]
        );
        facts.push(format!("{kind} {expected} @{}:{}", span.line, span.column));
    };
    for heading in scan.headings() {
        record(
            "heading",
            heading.span,
            &"#".repeat(usize::from(heading.level)),
        );
    }
    for link in scan.links() {
        let raw = link.raw.clone();
        record("link", link.span, &raw);
    }
    for tag in scan.tags() {
        let name = format!("#{}", tag.name);
        record("tag", tag.span.expect("a body tag names its bytes"), &name);
    }
    for block_id in scan.block_ids() {
        let id = format!("^{}", block_id.id);
        record("block-id", block_id.span, &id);
    }
    facts
}

/// The offset property: for a document written with lone `\r` breaks, every
/// span the scan reports slices the **original** bytes to the same text, on
/// the same line and column, as the `\n` twin of the same document.
///
/// This is what makes the normalization the CommonMark pass applies
/// admissible. The pass reads a copy whose lone `\r` bytes are `\n`; because
/// that rewrite is one byte for one byte, the ranges it reports are offsets
/// into the caller's bytes and nothing downstream has to know a copy existed.
#[test]
fn every_span_addresses_the_original_bytes_on_every_break_style() {
    let lf = TEMPLATE.to_string();
    let crlf = TEMPLATE.replace('\n', "\r\n");
    let cr = TEMPLATE.replace('\n', "\r");
    let mixed = mixed(TEMPLATE);

    let expected = facts_by_slicing(&lf);
    // The facts are the document's, not an accident of an empty scan.
    assert_eq!(
        expected,
        [
            "heading # @1:1",
            "heading ## @9:1",
            "link [[Note|shown]] @3:7",
            "link [text](./t.md) @3:26",
            "tag #atag @3:45",
            "tag #btag @10:6",
            "block-id ^aid @3:51",
        ]
    );
    assert_eq!(facts_by_slicing(&cr), expected, "a lone-`\\r` document");
    assert_eq!(facts_by_slicing(&crlf), expected, "a `\\r\\n` document");
    assert_eq!(facts_by_slicing(&mixed), expected, "a mixed document");
}

/// The fence is opaque on a lone `\r` too, which is the leak the normalization
/// closes: the fenced `[[Hidden]]`, `#hidden` and `^hid` of `TEMPLATE` reach
/// no family on any break style.
///
/// `a_lone_cr_backtick_fence_is_opaque` in `wikilinks.rs` states the same
/// contract against a bare fence; this one states it inside a document that
/// also carries constructs the scan **must** find, so an over-wide mask fails
/// it as loudly as a missing one.
#[test]
fn a_fenced_sample_reaches_no_family_on_any_break_style() {
    for document in [
        TEMPLATE.to_string(),
        TEMPLATE.replace('\n', "\r\n"),
        TEMPLATE.replace('\n', "\r"),
        mixed(TEMPLATE),
    ] {
        let facts = facts_by_slicing(&document);
        for hidden in ["[[Hidden]]", "#hidden", "^hid"] {
            assert!(
                !facts.iter().any(|fact| fact.contains(hidden)),
                "{hidden} leaked out of the fence: {facts:?}"
            );
        }
    }
}
