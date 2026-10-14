//! Document-local anchor and tag edits prove the bytes they write.

use norn_text::{Document, LinkFamily, RewriteSkip};

/// Every exact anchor match is respelled without changing another link.
#[test]
fn an_anchor_rewrite_respells_every_matching_link_and_keeps_the_rest() {
    let source = "![[vault://a#Old|Shown]] [[vault://a#Old]] [[a#Old]] [[vault://b#Old]] [Shown](vault://a#Old) [[vault://a#Other]] [[vault://a#^Old]]\n";
    let rewritten = Document::parse(source)
        .rewrite_anchors(LinkFamily::Wikilink, "vault://a", "Old", "New")
        .expect("a proven edit");
    assert_eq!(rewritten.rewritten, 2);
    assert_eq!(
        rewritten.text,
        "![[vault://a#New|Shown]] [[vault://a#New]] [[a#Old]] [[vault://b#Old]] [Shown](vault://a#Old) [[vault://a#Other]] [[vault://a#^Old]]\n"
    );
}

/// An anchor whose bytes become link syntax cannot be proven by reading back.
#[test]
fn an_anchor_rewrite_that_cannot_read_back_is_refused_with_its_reason() {
    assert_eq!(
        Document::parse("[[a#Old|Shown]]").rewrite_anchors(
            LinkFamily::Wikilink,
            "a",
            "Old",
            "New|Other"
        ),
        Err(RewriteSkip::Unrepresentable)
    );
}

/// The whole folded name selects body and frontmatter occurrences together.
#[test]
fn a_tag_rewrite_respells_body_and_frontmatter_matches_and_keeps_other_tags() {
    let source = "---\ntags:\n  - Work\n  - '#WORK'\n  - work/child\n  - prefix-work\n---\n#work #WORK #work/child #prefix-work\n";
    let rewritten = Document::parse(source)
        .rewrite_tags("work", "Place", str::to_lowercase)
        .expect("a proven edit");
    assert_eq!(rewritten.rewritten, 4);
    assert_eq!(
        rewritten.text,
        "---\ntags:\n  - Place\n  - '#Place'\n  - work/child\n  - prefix-work\n---\n#Place #Place #work/child #prefix-work\n"
    );
}

/// A plain scalar that reads as a boolean cannot carry the requested tag.
#[test]
fn a_tag_rewrite_that_cannot_read_back_is_refused_with_its_reason() {
    assert_eq!(
        Document::parse("---\ntags: work\n---\n#work").rewrite_tags(
            "work",
            "true",
            str::to_lowercase
        ),
        Err(RewriteSkip::WouldCorruptFrontmatter)
    );
}
