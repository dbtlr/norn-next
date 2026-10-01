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

// ── What never matches, and what is skipped ──────────────────────────────

/// A `to` no wikilink can spell — a delimiter, a line break, nothing at all,
/// padding, a protocol prefix — would re-read as another link or none, so
/// every matching wikilink is skipped as unrepresentable, in the body and the
/// frontmatter alike, and the document is returned as it was.
#[test]
fn a_target_no_wikilink_can_spell_skips_every_match() {
    let source = "---\nup: '[[Old]]'\n---\n[[Old]] and ![[ Old | t ]]\n";
    for to in ["a|b", "a#b", "a[b", "a]b", "a\nb", "", " New ", "https://x"] {
        let out = rewrite(source, LinkFamily::Wikilink, "Old", to);
        assert_eq!(out.text, source, "to {to:?}");
        assert_eq!(out.rewritten, 0, "to {to:?}");
        assert_eq!(
            reasons(&out),
            [RewriteSkip::Unrepresentable; 3],
            "to {to:?}"
        );
        let at: Vec<&str> = out
            .skipped
            .iter()
            .map(|skip| &source[skip.link.range()])
            .collect();
        assert_eq!(at, ["[[Old]]", "[[Old]]", "![[ Old | t ]]"], "to {to:?}");
    }
}

/// A wikilink token spanning a line break is recognized and never written:
/// splicing over it would reflow whatever text it swallowed. One whose target
/// is `from` is reported as a link no target can be written into.
#[test]
fn a_wikilink_spanning_a_line_break_is_skipped_whatever_the_target() {
    let source = "[[Old|Shown\nmore]] and [[Old]]\n";
    let out = rewrite(source, LinkFamily::Wikilink, "Old", "New");
    assert_eq!(out.text, "[[Old|Shown\nmore]] and [[New]]\n");
    assert_eq!(out.rewritten, 1);
    assert_eq!(reasons(&out), [RewriteSkip::LinkNotRewritable]);
    assert_eq!(out.skipped[0].link.span.byte_offset, 0);
}

/// A link written with a protocol other than `vault` addresses something
/// outside the vault, so its stem spelling `from` is a coincidence and it
/// never matches. The reserved `vault://` addresses the vault and is
/// rewritten with its prefix standing.
#[test]
fn only_a_link_addressing_the_vault_matches() {
    let source = "[[https://Old|Docs]] [[vault://Old]] [[Old]]\n";
    let out = rewrite(source, LinkFamily::Wikilink, "Old", "New");
    assert_eq!(out.text, "[[https://Old|Docs]] [[vault://New]] [[New]]\n");
    assert_eq!(out.rewritten, 2);
    assert!(out.skipped.is_empty());

    let source = "[a](https://x/old.md) [b](vault://x/old.md) [c](x/old.md)\n";
    let out = rewrite(source, LinkFamily::Markdown, "x/old.md", "y/new.md");
    assert_eq!(
        out.text,
        "[a](https://x/old.md) [b](vault://y/new.md) [c](y/new.md)\n"
    );
    assert_eq!(out.rewritten, 2);
}

/// A target-less link addresses the document holding it, and is never what a
/// cascade renames: an empty `from` matches nothing, an anchor-only link
/// included.
#[test]
fn an_empty_from_matches_nothing() {
    let source = "[[#Heading]] and [t](#frag) and [[Old]]\n";
    for family in [LinkFamily::Wikilink, LinkFamily::Markdown] {
        let out = rewrite(source, family, "", "New");
        assert_eq!(out.text, source, "{family:?}");
        assert_eq!(out.rewritten, 0, "{family:?}");
        assert!(out.skipped.is_empty(), "{family:?}");
    }
}

// ── Body Markdown links ──────────────────────────────────────────────────

/// An inline Markdown link's destination is respelled over its stem alone, so
/// the bracket text, the fragment in both its forms and the optional quoted
/// title stand as written — and so does a link written inside an image's alt
/// text, which is a link like any other.
#[test]
fn a_markdown_destination_is_respelled_and_its_fragment_and_title_stay() {
    let source = "[text](old.md \"Tip\") and [t](old.md#Heading) and [t]( old.md#^blk )\n\
                  and ![see [here](old.md)](pic.png) and [t](older.md)\n";
    let out = rewrite(source, LinkFamily::Markdown, "old.md", "../new.md");
    assert_eq!(
        out.text,
        "[text](../new.md \"Tip\") and [t](../new.md#Heading) and [t]( ../new.md#^blk )\n\
         and ![see [here](../new.md)](pic.png) and [t](older.md)\n"
    );
    assert_eq!(out.rewritten, 4);
    assert!(out.skipped.is_empty(), "{:?}", reasons(&out));
}

/// The target is the destination as written, encoding and all, and so is the
/// target written in its place: a percent-encoded destination is matched in
/// its encoded spelling and stays encoded when `to` is spelled so. A space no
/// bare destination can hold is skipped rather than encoded behind the
/// caller's back, because the target that would re-read is not `to`.
#[test]
fn a_percent_encoded_destination_is_matched_and_written_as_spelled() {
    let source = "[t](my%20note.md#Part)\n";
    let out = rewrite(
        source,
        LinkFamily::Markdown,
        "my%20note.md",
        "your%20note.md",
    );
    assert_eq!(out.text, "[t](your%20note.md#Part)\n");
    assert_eq!(out.rewritten, 1);

    let out = rewrite(source, LinkFamily::Markdown, "my note.md", "x.md");
    assert_eq!(out.text, source, "a decoded from is another target");
    assert_eq!(out.rewritten, 0);

    let out = rewrite(source, LinkFamily::Markdown, "my%20note.md", "your note.md");
    assert_eq!(out.text, source);
    assert_eq!(reasons(&out), [RewriteSkip::Unrepresentable]);
}

/// An angle-bracketed destination stays angle-bracketed, so a space it holds
/// is written as a space; a `to` that would close the brackets early cannot be
/// written there and is skipped.
#[test]
fn an_angle_bracketed_destination_stays_bracketed() {
    let source = "[t](<my note.md#Part> \"Tip\")\n";
    let out = rewrite(source, LinkFamily::Markdown, "my note.md", "your note.md");
    assert_eq!(out.text, "[t](<your note.md#Part> \"Tip\")\n");
    assert_eq!(out.rewritten, 1);

    for to in ["a>b", "a<b"] {
        let out = rewrite(source, LinkFamily::Markdown, "my note.md", to);
        assert_eq!(out.text, source, "to {to:?}");
        assert_eq!(reasons(&out), [RewriteSkip::Unrepresentable], "to {to:?}");
    }
}

/// A `to` that re-reads as some other destination in a bare one — a space
/// ending it, a parenthesis closing the link, a hash opening a fragment, an
/// escape or an entity the parse resolves, a protocol prefix — is skipped, and
/// the link keeps its bytes.
#[test]
fn a_target_a_bare_destination_cannot_spell_is_skipped() {
    let source = "[t](old.md) and [u](old.md)\n";
    for to in [
        "a b",
        "a)b",
        "a#b",
        "a\\_b",
        "a&amp;b",
        "<a",
        "https://x",
        "",
        "a\nb",
    ] {
        let out = rewrite(source, LinkFamily::Markdown, "old.md", to);
        assert_eq!(out.text, source, "to {to:?}");
        assert_eq!(out.rewritten, 0, "to {to:?}");
        assert_eq!(
            reasons(&out),
            [RewriteSkip::Unrepresentable; 2],
            "to {to:?}"
        );
    }
}

/// A destination written with an escape is read through it, so its target is
/// not the bytes it was written as and there is no stem to write over.
#[test]
fn an_escaped_destination_is_skipped_as_not_rewritable() {
    let source = "[t](note\\#draft.md)\n";
    let out = rewrite(source, LinkFamily::Markdown, "note#draft.md", "new.md");
    assert_eq!(out.text, source);
    assert_eq!(reasons(&out), [RewriteSkip::LinkNotRewritable]);
}

/// Only the inline form is a Markdown link here. An image, an autolink, and a
/// reference-style link with its definition produce no link fact, so the index
/// holds none of them and a rewrite leaves them as written.
#[test]
fn a_form_that_produces_no_link_fact_is_never_rewritten() {
    let source = "![alt](old.md) <old.md> [t][ref] [ref]\n\n[ref]: old.md\n";
    let out = rewrite(source, LinkFamily::Markdown, "old.md", "new.md");
    assert_eq!(out.text, source);
    assert_eq!(out.rewritten, 0);
    assert!(out.skipped.is_empty());
}

// ── Exact, and proven by reading it back ─────────────────────────────────

/// Read back, the rewritten document holds the same links in the same order,
/// each rewritten one now targeting `to` and every other one as it was — and
/// a second rewrite of `from` finds nothing left to do.
#[test]
fn a_rewritten_document_reads_back_with_to_at_every_rewritten_link() {
    let source = "---\nup: '[[Old]]'\nsee: '[[Other]]'\n---\n\
                  # Head\n[[Old#Head|t]] [t](Old) [[#Head]] ![[Old]] [[Other]]\n";
    let before = Document::parse(source);
    let out = before.rewrite_links(LinkFamily::Wikilink, "Old", "Sub/New");
    assert_eq!(out.rewritten, 3);

    let after = Document::parse(&out.text);
    let was: Vec<_> = before
        .frontmatter_wikilinks()
        .into_iter()
        .chain(before.links())
        .collect();
    let is: Vec<_> = after
        .frontmatter_wikilinks()
        .into_iter()
        .chain(after.links())
        .collect();
    assert_eq!(was.len(), is.len());
    for (was, is) in was.iter().zip(&is) {
        let expected = if was.family == LinkFamily::Wikilink && was.target == "Old" {
            "Sub/New"
        } else {
            was.target.as_str()
        };
        assert_eq!(is.target, expected, "{was:?}");
        assert_eq!(
            (is.family, is.embed, &is.anchor, &is.block_ref, &is.title),
            (
                was.family,
                was.embed,
                &was.anchor,
                &was.block_ref,
                &was.title
            )
        );
    }

    let again = after.rewrite_links(LinkFamily::Wikilink, "Old", "Sub/New");
    assert_eq!(again.text, out.text);
    assert_eq!(again.rewritten, 0);
}

/// What a target's bytes mean depends on what surrounds them: a backtick in a
/// stem pairs with the next one in the paragraph and turns the text between
/// into code, which would swallow the link. The rewrite is proven position by
/// position, so the one place the bytes would read otherwise is skipped and
/// the place they read as written is rewritten.
#[test]
fn a_target_whose_bytes_read_otherwise_where_written_is_skipped_there_alone() {
    let source = "[[Old]] then `code` and [[Old]]\n";
    let out = rewrite(source, LinkFamily::Wikilink, "Old", "a`b");
    assert_eq!(out.text, "[[Old]] then `code` and [[a`b]]\n");
    assert_eq!(out.rewritten, 1);
    assert_eq!(reasons(&out), [RewriteSkip::Unrepresentable]);
    assert_eq!(out.skipped[0].link.span.byte_offset, 0);
}

/// Two families' links may share bytes: `[[Old]](x.md)` is a wikilink inside
/// a Markdown link's bracket text. Rewriting the wikilink changes the other
/// link's display text, which is that link's own bytes being what they are,
/// and not a change in what it addresses.
#[test]
fn a_wikilink_inside_a_markdown_links_text_is_rewritten() {
    let source = "[[Old]](x.md)\n";
    let out = rewrite(source, LinkFamily::Wikilink, "Old", "New");
    assert_eq!(out.text, "[[New]](x.md)\n");
    assert_eq!(out.rewritten, 1);
}

/// A rewrite that changes nothing returns the document's own bytes — a mark,
/// CRLF breaks and all — whether nothing matched or every match was a link
/// already spelled `to`.
#[test]
fn a_rewrite_that_changes_nothing_returns_the_input_unchanged() {
    let source = "\u{feff}---\r\nup: \"[[Old]]\"\r\n---\r\n[[Old]] [t](Old)\r\n";
    let out = rewrite(source, LinkFamily::Wikilink, "Missing", "New");
    assert_eq!(out.text, source);
    assert_eq!(out.rewritten, 0);
    assert!(out.skipped.is_empty());

    let out = rewrite(source, LinkFamily::Wikilink, "Old", "Old");
    assert_eq!(out.text, source);
    assert!(out.skipped.is_empty());
}

/// A CRLF document with a byte-order mark is rewritten in place like any
/// other: the mark, the breaks and the quoting all stand.
#[test]
fn a_crlf_document_keeps_its_breaks_and_mark() {
    let source = "\u{feff}---\r\nup: \"[[Old]]\"\r\n---\r\n[[Old]] [t](Old)\r\n";
    let out = rewrite(source, LinkFamily::Wikilink, "Old", "New");
    assert_eq!(
        out.text,
        "\u{feff}---\r\nup: \"[[New]]\"\r\n---\r\n[[New]] [t](Old)\r\n"
    );
    assert_eq!(out.rewritten, 2);
}
