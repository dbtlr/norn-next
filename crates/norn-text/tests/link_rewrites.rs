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
