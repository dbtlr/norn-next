//! Rewriting a document's links: every link of one family whose target is
//! `from` is respelled `to`, and nothing else in the document moves.
//!
//! This is the one seam a link cascade composes a document's after-bytes
//! through. A cascade keys each of its per-document operations by a family
//! and a target as the index stored it, so the match here is exactly the
//! parse's [`Link::target`]: what the index holds is what is matched. What a
//! single token preserves is `rewrite_fidelity.rs`; what a whole document
//! preserves, skips and refuses is here.

use norn_text::{Document, LinkFamily, RewriteSkip, RewrittenLinks};

fn rewrite(source: &str, family: LinkFamily, from: &str, to: &str) -> RewrittenLinks {
    Document::parse(source).rewrite_links(family, from, to)
}

fn reasons(rewritten: &RewrittenLinks) -> Vec<RewriteSkip> {
    rewritten.skipped.iter().map(|skip| skip.reason).collect()
}

// ── Body wikilinks ───────────────────────────────────────────────────────

/// Only the stem's bytes change, so the embed marker, the title, both anchor
/// forms and the author's padding survive byte for byte — and a link with
/// another target, or a Markdown link with the same text, is another family's
/// or another target's link and is not touched.
#[test]
fn a_body_wikilink_to_from_is_respelled_and_every_other_byte_stays() {
    let source = "prose [[Old]] and ![[ Old | Shown ]] and [[Old#Heading]]\n\
                  and ![[ Old #^blk|t ]] and [[Older]] and [[keep|Old]]\n\
                  and [t](Old) too\n";
    let out = rewrite(source, LinkFamily::Wikilink, "Old", "New");
    assert_eq!(
        out.text,
        "prose [[New]] and ![[ New | Shown ]] and [[New#Heading]]\n\
         and ![[ New #^blk|t ]] and [[Older]] and [[keep|Old]]\n\
         and [t](Old) too\n"
    );
    assert_eq!(out.rewritten, 4);
    assert!(out.skipped.is_empty(), "{:?}", reasons(&out));
}

/// The family is part of the key both ways round: a Markdown rewrite leaves a
/// wikilink spelling the same target exactly as written.
#[test]
fn a_markdown_rewrite_leaves_a_wikilink_with_the_same_target_alone() {
    let source = "[[old.md]] and [t](old.md)\n";
    let out = rewrite(source, LinkFamily::Markdown, "old.md", "new.md");
    assert_eq!(out.text, "[[old.md]] and [t](new.md)\n");
    assert_eq!(out.rewritten, 1);
}

// ── Code is opaque ───────────────────────────────────────────────────────

/// NRN-432: a fenced sample was the first occurrence in the file, so a textual
/// rewriter changed it and left the prose link dangling. A link inside fenced
/// code, indented code or an inline code span is not a link, of either family,
/// and is neither rewritten nor reported.
#[test]
fn a_link_written_inside_code_is_never_rewritten() {
    let source = "```\n[[Old]] [t](Old)\n```\n\n    [[Old]] [t](Old)\n\n\
                  `[[Old]]` and `[t](Old)` and [[Old]] and [t](Old)\n";
    for (family, rewritten) in [
        (
            LinkFamily::Wikilink,
            "`[[Old]]` and `[t](Old)` and [[New]] and [t](Old)\n",
        ),
        (
            LinkFamily::Markdown,
            "`[[Old]]` and `[t](Old)` and [[Old]] and [t](New)\n",
        ),
    ] {
        let out = rewrite(source, family, "Old", "New");
        assert_eq!(
            out.text,
            format!("```\n[[Old]] [t](Old)\n```\n\n    [[Old]] [t](Old)\n\n{rewritten}"),
            "{family:?}"
        );
        assert_eq!(out.rewritten, 1, "{family:?}");
        assert!(out.skipped.is_empty(), "{family:?}");
    }
}

// ── Frontmatter wikilinks ────────────────────────────────────────────────

/// A wikilink written in a frontmatter value is rewritten where it stands —
/// in a plain scalar, a single- or double-quoted one, and a block sequence's
/// items — and every byte outside its stem stays: the quotes, the comments,
/// the key order, the other links.
#[test]
fn a_frontmatter_wikilink_is_respelled_in_place() {
    let source = "---\n\
                  title: '[[Old]]'  # kept\n\
                  up: \"[[Old|Parent]]\"\n\
                  plain: see [[Old]] and [[Old#Part]] and [[Other]]\n\
                  related:\n  - \"[[Old]]\"\n  - see [[ Old ]]\n  - '[[Other]]'\n\
                  ---\n\
                  body [[Old]]\n";
    let out = rewrite(source, LinkFamily::Wikilink, "Old", "New");
    assert_eq!(
        out.text,
        "---\n\
         title: '[[New]]'  # kept\n\
         up: \"[[New|Parent]]\"\n\
         plain: see [[New]] and [[New#Part]] and [[Other]]\n\
         related:\n  - \"[[New]]\"\n  - see [[ New ]]\n  - '[[Other]]'\n\
         ---\n\
         body [[New]]\n"
    );
    assert_eq!(out.rewritten, 7);
    assert!(out.skipped.is_empty(), "{:?}", reasons(&out));
}

/// A frontmatter value is read as a wikilink's home and never as a Markdown
/// link's: a `[title](target)` string in a property is inert text.
#[test]
fn a_markdown_rewrite_never_reaches_into_the_frontmatter() {
    let source = "---\nsee: '[t](old.md)'\n---\n[t](old.md)\n";
    let out = rewrite(source, LinkFamily::Markdown, "old.md", "new.md");
    assert_eq!(out.text, "---\nsee: '[t](old.md)'\n---\n[t](new.md)\n");
    assert_eq!(out.rewritten, 1);
}

/// A target the value's own spelling cannot hold is never forced into it: a
/// quote character inside a scalar quoted with it, an escape inside a
/// double-quoted one, and a `: ` a plain scalar reads as a mapping. The link
/// stays as written, the rest of the document is still rewritten, and the
/// skip names the link where the document holds it.
#[test]
fn a_target_the_frontmatter_value_cannot_hold_is_skipped() {
    for (source, to) in [
        ("---\nup: \"[[Old]]\"\n---\n[[Old]]\n", "a\"b"),
        ("---\nup: \"[[Old]]\"\n---\n[[Old]]\n", "a\\tb"),
        ("---\nup: '[[Old]]'\n---\n[[Old]]\n", "it's"),
        ("---\nup: see [[Old]]\n---\n[[Old]]\n", "a: b"),
        ("---\nup:\n  - see [[Old]]\n---\n[[Old]]\n", "a: b"),
    ] {
        let out = rewrite(source, LinkFamily::Wikilink, "Old", to);
        let (block, _) = source.split_at(source.find("---\n[[").expect("a body") + 4);
        assert_eq!(
            out.text,
            format!("{block}[[{to}]]\n"),
            "{source:?} to {to:?}"
        );
        assert_eq!(out.rewritten, 1, "{source:?} to {to:?}");
        assert_eq!(
            reasons(&out),
            [RewriteSkip::WouldCorruptFrontmatter],
            "{source:?} to {to:?}"
        );
        let skipped = &out.skipped[0].link;
        assert_eq!(&source[skipped.range()], "[[Old]]", "{source:?} to {to:?}");
    }
}

/// The index holds a frontmatter wikilink only where the value's bytes carry
/// it literally, and a rewrite reaches exactly what the index holds: a link
/// inside a flow sequence or an escaped scalar names no bytes, is no link the
/// index could have keyed a rewrite by, and stays as written.
#[test]
fn a_frontmatter_link_with_no_span_is_not_a_rewrite_target() {
    let source = "---\nflow: [\"[[Old]]\"]\nescaped: \"\\x5B[Old]]\"\n---\n";
    let out = rewrite(source, LinkFamily::Wikilink, "Old", "New");
    assert_eq!(out.text, source);
    assert_eq!(out.rewritten, 0);
    assert!(out.skipped.is_empty());
}
