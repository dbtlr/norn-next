//! Document-local anchor and tag edits prove the bytes they write.

use norn_text::{AnchorRewrite, Document, LinkFamily, RewriteSkip, TagRewrite};

/// Every exact anchor match is respelled without changing another link.
#[test]
fn an_anchor_rewrite_respells_every_matching_link_and_keeps_the_rest() {
    let source = "![[vault://a#Old|Shown]] [[vault://a#Old]] [[a#Old]] [[vault://b#Old]] [Shown](vault://a#Old) [[vault://a#Other]] [[vault://a#^Old]]\n";
    let rewritten = Document::parse(source)
        .rewrite_anchors(&[AnchorRewrite::new(
            LinkFamily::Wikilink,
            "vault://a",
            "Old",
            "New",
        )])
        .expect("a proven edit");
    assert_eq!(rewritten.rewritten, [Ok(2)]);
    assert_eq!(
        rewritten.text,
        "![[vault://a#New|Shown]] [[vault://a#New]] [[a#Old]] [[vault://b#Old]] [Shown](vault://a#Old) [[vault://a#Other]] [[vault://a#^Old]]\n"
    );
}

/// Encoded Markdown fragments keep their written spelling while proving their decoded anchor.
#[test]
fn a_markdown_anchor_rewrite_reads_back_a_percent_encoded_replacement() {
    let rewritten = Document::parse("[x](t.md#a%20b)")
        .rewrite_anchors(&[AnchorRewrite::new(
            LinkFamily::Markdown,
            "t.md",
            "a%20b",
            "c%20d",
        )])
        .expect("a proven edit");
    assert_eq!(rewritten.rewritten, [Ok(1)]);
    assert_eq!(rewritten.text, "[x](t.md#c%20d)");
}

/// An anchor whose bytes become link syntax cannot be proven by reading back.
#[test]
fn an_anchor_rewrite_that_cannot_read_back_is_refused_with_its_reason() {
    assert_eq!(
        Document::parse("[[a#Old|Shown]]").rewrite_anchors(&[AnchorRewrite::new(
            LinkFamily::Wikilink,
            "a",
            "Old",
            "New|Other"
        )]),
        Err(RewriteSkip::Unrepresentable)
    );
}

/// The whole folded name selects body and frontmatter occurrences together.
#[test]
fn a_tag_rewrite_respells_body_and_frontmatter_matches_and_keeps_other_tags() {
    let source = "---\ntags:\n  - Work\n  - '#WORK'\n  - work/child\n  - prefix-work\n---\n#work #WORK #work/child #prefix-work\n";
    let rewritten = Document::parse(source)
        .rewrite_tags(&[TagRewrite::new("work", "Place")], str::to_lowercase)
        .expect("a proven edit");
    assert_eq!(rewritten.rewritten, [Ok(4)]);
    assert_eq!(
        rewritten.text,
        "---\ntags:\n  - Place\n  - '#Place'\n  - work/child\n  - prefix-work\n---\n#Place #Place #work/child #prefix-work\n"
    );
}

/// A flow-list tags entry is re-rendered whole beside its body tag's in-place
/// edit, so a near tag written in both places is respelled in both.
#[test]
fn a_flow_list_tag_and_its_body_tag_are_both_respelled() {
    let rewritten = Document::parse("---\ntags: [darft, other]\n---\n#darft\n")
        .rewrite_tags(&[TagRewrite::new("darft", "draft")], str::to_lowercase)
        .expect("a proven edit");
    assert_eq!(rewritten.rewritten, [Ok(2)]);
    assert_eq!(rewritten.text, "---\ntags: [draft, other]\n---\n#draft\n");
}

/// A tags entry with no literal place is rewritten whole only where the field
/// reads back: a folded entry, and an escaped block item another entry follows,
/// refuse the batch.
#[test]
fn a_folded_or_followed_escaped_tags_entry_refuses_the_rewrite() {
    for source in [
        "---\ntags:\n  - >-\n    darft\n  - other\n---\n#darft\n",
        "---\ntags:\n  - \"darf\\u0074\"\n  - other\n---\n#darft\n",
    ] {
        assert_eq!(
            Document::parse(source)
                .rewrite_tags(&[TagRewrite::new("darft", "draft")], str::to_lowercase),
            Err(RewriteSkip::Unrepresentable),
            "{source:?}"
        );
    }
}

/// A plain scalar that reads as a boolean cannot carry the requested tag.
#[test]
fn a_tag_rewrite_that_cannot_read_back_is_refused_with_its_reason() {
    assert_eq!(
        Document::parse("---\ntags: work\n---\n#work")
            .rewrite_tags(&[TagRewrite::new("work", "true")], str::to_lowercase),
        Err(RewriteSkip::WouldCorruptFrontmatter)
    );
}

/// Each tag splice carries its own heading position past similar non-tag text.
#[test]
fn a_tag_rewrite_carries_each_heading_edit_at_its_own_position() {
    for (source, expected) in [
        ("# #darftx and #darft\n", "# #darftx and #draft\n"),
        ("# C#darft and #darft\n", "# C#darft and #draft\n"),
    ] {
        let rewritten = Document::parse(source)
            .rewrite_tags(&[TagRewrite::new("darft", "draft")], str::to_lowercase)
            .expect("a proven edit");
        assert_eq!(rewritten.text, expected);
        assert_eq!(rewritten.rewritten, [Ok(1)]);
    }
}

/// An anchor must keep the line boundaries the read-back proof depends on.
#[test]
fn an_anchor_rewrite_refuses_a_newline_in_the_written_anchor() {
    assert_eq!(
        Document::parse("[[a#Old]]")
            .rewrite_anchors(&[AnchorRewrite::new(
                LinkFamily::Wikilink,
                "a",
                "Old",
                "New\r\nz"
            )])
            .unwrap()
            .rewritten,
        [Err(RewriteSkip::Unrepresentable)]
    );
}

/// A Markdown anchor cannot introduce a new GFM table cell boundary.
#[test]
fn a_markdown_anchor_rewrite_refuses_a_pipe_that_splits_a_table_cell() {
    assert_eq!(
        Document::parse("| [x](t.md#old) | b |\n")
            .rewrite_anchors(&[AnchorRewrite::new(
                LinkFamily::Markdown,
                "t.md",
                "old",
                "a|b"
            )])
            .unwrap()
            .rewritten,
        [Err(RewriteSkip::Unrepresentable)]
    );
}

/// The rewritten occurrences must no longer name the old folded tag.
#[test]
fn a_tag_rewrite_refuses_a_case_only_respelling_of_the_same_folded_name() {
    assert_eq!(
        Document::parse("#darft")
            .rewrite_tags(&[TagRewrite::new("darft", "DARFT")], str::to_lowercase)
            .unwrap()
            .rewritten,
        [Err(RewriteSkip::Unrepresentable)]
    );
}

/// Anchor batches match the original document and isolate scan-time refusals.
#[test]
fn an_anchor_batch_does_not_chain_and_reports_each_scan_refusal() {
    use norn_text::AnchorRewrite;
    let result = Document::parse("[[a#old]] [[a#new]] [[a#bad]] [[a#conflict]]")
        .rewrite_anchors(&[
            AnchorRewrite::new(LinkFamily::Wikilink, "a", "old", "new"),
            AnchorRewrite::new(LinkFamily::Wikilink, "a", "new", "done"),
            AnchorRewrite::new(LinkFamily::Wikilink, "a", "bad", "bad\nline"),
            AnchorRewrite::new(LinkFamily::Wikilink, "a", "conflict", "one"),
            AnchorRewrite::new(LinkFamily::Wikilink, "a", "conflict", "two"),
        ])
        .unwrap();
    assert_eq!(result.text, "[[a#new]] [[a#done]] [[a#bad]] [[a#conflict]]");
    assert_eq!(
        result.rewritten,
        [
            Ok(1),
            Ok(1),
            Err(RewriteSkip::Unrepresentable),
            Err(RewriteSkip::ConflictingRewrites),
            Err(RewriteSkip::ConflictingRewrites)
        ]
    );
    assert_eq!(
        Document::parse("[x](t#[[a#old]])").rewrite_anchors(&[
            AnchorRewrite::new(LinkFamily::Markdown, "t", "[[a#old]]", "clean"),
            AnchorRewrite::new(LinkFamily::Wikilink, "a", "old", "new"),
        ]),
        Err(RewriteSkip::Unrepresentable)
    );
}

/// Tag batches match whole original names and isolate conflicting or case-only changes.
#[test]
fn a_tag_batch_does_not_chain_and_reports_each_scan_refusal() {
    use norn_text::TagRewrite;
    let result = Document::parse("#old #new #bad #conflict")
        .rewrite_tags(
            &[
                TagRewrite::new("old", "new"),
                TagRewrite::new("new", "done"),
                TagRewrite::new("bad", "BAD"),
                TagRewrite::new("conflict", "one"),
                TagRewrite::new("conflict", "two"),
            ],
            str::to_lowercase,
        )
        .unwrap();
    assert_eq!(result.text, "#new #done #bad #conflict");
    assert_eq!(
        result.rewritten,
        [
            Ok(1),
            Ok(1),
            Err(RewriteSkip::Unrepresentable),
            Err(RewriteSkip::ConflictingRewrites),
            Err(RewriteSkip::ConflictingRewrites)
        ]
    );
}
