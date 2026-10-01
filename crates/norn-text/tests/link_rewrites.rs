//! Rewriting a document's links: every link of one family whose address is
//! `from` is respelled `to`, and nothing else in the document moves.
//!
//! This is the one seam a link cascade composes a document's after-bytes
//! through. A cascade keys each of its per-document operations by a family,
//! a protocol and a target as the index stored them, so the match here is
//! exactly the parse's [`Link::protocol`] and [`Link::target`]: what the
//! index holds is what is matched. What a
//! single token preserves is `rewrite_fidelity.rs`; what a whole document
//! preserves, skips and refuses is here.

use norn_text::{Document, FRONTMATTER_MAX_BYTES, LinkFamily, RewriteSkip, RewrittenLinks};

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

/// Two frontmatter strings can each hold `to` alone and not together: the
/// block has a byte bound, and a block past it reads as nothing. The strings
/// are kept in document order while they read back, so the first is
/// rewritten and the second skipped as corrupting the frontmatter — and the
/// skips are reported in document order, the frontmatter's before a body
/// link refused earlier for its own bytes.
#[test]
fn frontmatter_strings_that_fit_alone_but_not_together_keep_the_first() {
    let to = "b".repeat(200);
    // The block is `pad: "…"\n` and two `aN: "[[a]]"\n` lines, 32 bytes
    // around the padding: one respelling leaves it 100 bytes inside the bound,
    // and a second takes it past.
    let pad = "x".repeat(FRONTMATTER_MAX_BYTES - 32 - (to.len() - 1) - 100);
    let source = format!("---\npad: \"{pad}\"\na1: \"[[a]]\"\na2: \"[[a]]\"\n---\n[[a|x\ny]]\n");
    let out = rewrite(&source, LinkFamily::Wikilink, "a", &to);
    assert_eq!(
        out.text,
        format!("---\npad: \"{pad}\"\na1: \"[[{to}]]\"\na2: \"[[a]]\"\n---\n[[a|x\ny]]\n")
    );
    assert_eq!(out.rewritten, 1);
    assert_eq!(
        reasons(&out),
        [
            RewriteSkip::WouldCorruptFrontmatter,
            RewriteSkip::LinkNotRewritable
        ]
    );
    let at: Vec<&str> = out
        .skipped
        .iter()
        .map(|skip| &source[skip.link.range()])
        .collect();
    assert_eq!(at, ["[[a]]", "[[a|x\ny]]"]);
    assert!(source[..out.skipped[0].link.span.byte_offset].ends_with("a2: \""));
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
/// outside the vault, so it never matches, even an address spelling it whole.
#[test]
fn a_link_addressed_outside_the_vault_never_matches() {
    let source = "[[https://Old|Docs]] [[Old]]\n";
    for from in ["Old", "https://Old"] {
        let out = rewrite(source, LinkFamily::Wikilink, from, "New");
        assert_eq!(out.rewritten, usize::from(from == "Old"), "from {from:?}");
        assert!(
            out.text.starts_with("[[https://Old|Docs]]"),
            "from {from:?}"
        );
        assert!(out.skipped.is_empty(), "from {from:?}");
    }
}

/// `from` is the address as written, protocol prefix and all, because a
/// `vault://` link is read from the vault root and a protocol-free one is not:
/// `[[X]]` and `[[vault://X]]` in one document can name different documents,
/// so they are two keys and each rewrite reaches only its own. The prefix
/// stands; only the stem is written.
#[test]
fn the_two_spellings_of_one_target_are_rewritten_independently() {
    let source = "[[vault://Old|t]] [[Old]] [b](vault://x/old.md) [c](x/old.md)\n";

    let out = rewrite(source, LinkFamily::Wikilink, "Old", "Sub/New");
    assert_eq!(
        out.text,
        "[[vault://Old|t]] [[Sub/New]] [b](vault://x/old.md) [c](x/old.md)\n"
    );
    assert_eq!(out.rewritten, 1);

    let out = rewrite(source, LinkFamily::Wikilink, "vault://Old", "vault://New");
    assert_eq!(
        out.text,
        "[[vault://New|t]] [[Old]] [b](vault://x/old.md) [c](x/old.md)\n"
    );
    assert_eq!(out.rewritten, 1);

    let out = rewrite(
        source,
        LinkFamily::Markdown,
        "vault://x/old.md",
        "vault://y/new.md",
    );
    assert_eq!(
        out.text,
        "[[vault://Old|t]] [[Old]] [b](vault://y/new.md) [c](x/old.md)\n"
    );
    let out = rewrite(source, LinkFamily::Markdown, "x/old.md", "../y/new.md");
    assert_eq!(
        out.text,
        "[[vault://Old|t]] [[Old]] [b](vault://x/old.md) [c](../y/new.md)\n"
    );
}

/// A rewrite respells a target and never changes how it is addressed: a `to`
/// whose protocol is not the matched link's would have to rewrite the prefix,
/// so each such link is skipped as unrepresentable and left as written.
#[test]
fn a_target_changing_the_links_protocol_is_skipped() {
    for (family, source, from, to) in [
        (LinkFamily::Wikilink, "[[Old]]\n", "Old", "vault://New"),
        (
            LinkFamily::Wikilink,
            "[[vault://Old]]\n",
            "vault://Old",
            "New",
        ),
        (
            LinkFamily::Markdown,
            "[t](old.md)\n",
            "old.md",
            "vault://new.md",
        ),
        (
            LinkFamily::Markdown,
            "[t](vault://old.md)\n",
            "vault://old.md",
            "new.md",
        ),
        (
            LinkFamily::Markdown,
            "[t](old.md)\n",
            "old.md",
            "https://x/new.md",
        ),
    ] {
        let out = rewrite(source, family, from, to);
        assert_eq!(out.text, source, "{from:?} to {to:?}");
        assert_eq!(out.rewritten, 0, "{from:?} to {to:?}");
        assert_eq!(
            reasons(&out),
            [RewriteSkip::Unrepresentable],
            "{from:?} to {to:?}"
        );
    }
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

/// A `|` ends a table cell in the editors that render tables, and this crate
/// reads no tables, so it cannot see whether a destination stands in one: a
/// Markdown `to` holding one is skipped wherever the link is.
#[test]
fn a_markdown_target_holding_a_pipe_is_skipped() {
    for source in [
        "| h | i |\n|---|---|\n| [x](old.md) | y |\n",
        "[t](old.md)\n",
        "[t](<old.md>)\n",
    ] {
        let out = rewrite(source, LinkFamily::Markdown, "old.md", "a|b.md");
        assert_eq!(out.text, source, "{source:?}");
        assert_eq!(out.rewritten, 0, "{source:?}");
        assert_eq!(reasons(&out), [RewriteSkip::Unrepresentable], "{source:?}");
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
/// a Markdown link's bracket text, the two sharing the outer brackets.
/// Rewriting the wikilink changes the other link's display text from `[Old]`
/// to `[New]` — the token inside the shared brackets, respelled — and not
/// what it addresses.
#[test]
fn a_wikilink_inside_a_markdown_links_text_is_rewritten() {
    let source = "[[Old]](x.md)\n";
    let out = rewrite(source, LinkFamily::Wikilink, "Old", "New");
    assert_eq!(out.text, "[[New]](x.md)\n");
    assert_eq!(out.rewritten, 1);
    let titles: Vec<_> = Document::parse(&out.text)
        .links()
        .into_iter()
        .filter(|link| link.family == LinkFamily::Markdown)
        .map(|link| link.title)
        .collect();
    assert_eq!(titles, [Some("[New]".to_string())]);
}

// ── Everything else the document says stands ─────────────────────────────

/// A rewrite whose `to` changes anything the index reads besides the rewritten
/// targets is skipped, and the document is returned as it was.
fn assert_skipped_as_unrepresentable(source: &str, family: LinkFamily, to: &str) {
    let out = rewrite(source, family, "a", to);
    assert_eq!(out.text, source, "to {to:?}");
    assert_eq!(out.rewritten, 0, "to {to:?}");
    assert_eq!(reasons(&out), [RewriteSkip::Unrepresentable], "to {to:?}");
}

/// A target that opens an HTML comment turns the rest of the paragraph into
/// the comment, so a tag after the link would stop being a tag.
#[test]
fn a_target_that_hides_a_tag_is_skipped() {
    assert_skipped_as_unrepresentable(
        "see [[a]] and #tag here -->\n",
        LinkFamily::Wikilink,
        "<!--",
    );
}

/// A target that closes an HTML comment the link sits in turns the text after
/// it into prose, so a tag the comment hid would become one.
#[test]
fn a_target_that_reveals_a_tag_is_skipped() {
    assert_skipped_as_unrepresentable("x <!-- [[a]] #hidden -->\n", LinkFamily::Wikilink, "-->");
}

/// Code is a different document: a target that opens a comment swallowing a
/// later code span changes which bytes are code, even where no link or tag
/// stands in them yet.
#[test]
fn a_target_that_swallows_a_code_span_is_skipped() {
    assert_skipped_as_unrepresentable(
        "see [[a]] then `rm -rf` -->\n",
        LinkFamily::Wikilink,
        "<!--",
    );
}

/// A target that opens an inline HTML tag reads every byte up to the quote
/// closing its attribute as that tag, a tag and a code span among them.
#[test]
fn a_target_that_opens_an_html_tag_is_skipped() {
    assert_skipped_as_unrepresentable("[[a]] #tag `code` \">\n", LinkFamily::Wikilink, "<x y=\"");
}

/// `[[a]](dest)` is two links sharing bytes. A backslash written at the end
/// of the wikilink's stem escapes the bracket after it, so the Markdown link
/// would still read with its own target — one byte further on. A link that
/// moved is a link the index would place somewhere else.
#[test]
fn a_target_that_moves_another_link_is_skipped() {
    assert_skipped_as_unrepresentable("[[a]](dest)\n", LinkFamily::Wikilink, "x\\");
}

/// `[x]: [[a]]` is a link reference definition, so the `===` under it
/// underlines nothing. A space in the stem ends the definition's destination
/// and leaves a line that defines nothing — and the paragraph that line now
/// is would read as a heading.
#[test]
fn a_target_that_makes_a_heading_is_skipped() {
    assert_skipped_as_unrepresentable("[x]: [[a]]\n===\n", LinkFamily::Wikilink, "b c");
}

/// What a rewritten link's own bytes are part of changes with them and is not
/// a change in what the document says: a heading holding the link reads its
/// new spelling, and every heading, tag and block id after it reads where its
/// bytes moved to.
#[test]
fn a_heading_holding_a_rewritten_link_reads_its_new_spelling() {
    let source = "# See [[a]] #tag\n\nText [[a]] ^blk\n\n## After\n";
    let out = rewrite(source, LinkFamily::Wikilink, "a", "Longer/Name");
    assert_eq!(
        out.text,
        "# See [[Longer/Name]] #tag\n\nText [[Longer/Name]] ^blk\n\n## After\n"
    );
    assert_eq!(out.rewritten, 2);
    assert!(out.skipped.is_empty(), "{:?}", reasons(&out));
    let headings = Document::parse(&out.text).headings();
    assert_eq!(headings[0].text, "See [[Longer/Name]] #tag");
    assert_eq!(headings[0].slug, "see-longername-tag");
}

/// A heading's text and a Markdown link's bracket text read a link inside
/// them with exactly its new token where the old one stood, and nothing else
/// in them changes.
#[test]
fn a_heading_and_a_link_title_holding_a_rewritten_link_read_its_new_token() {
    let source = "# See [[a]] here\n\n[see [[a]] too](d)\n";
    let out = rewrite(source, LinkFamily::Wikilink, "a", "b");
    assert_eq!(out.text, "# See [[b]] here\n\n[see [[b]] too](d)\n");
    assert_eq!(out.rewritten, 2);
    assert!(out.skipped.is_empty(), "{:?}", reasons(&out));
    let document = Document::parse(&out.text);
    assert_eq!(document.headings()[0].text, "See [[b]] here");
    assert_eq!(document.headings()[0].slug, "see-b-here");
    let titles: Vec<_> = document
        .links()
        .into_iter()
        .filter(|link| link.family == LinkFamily::Markdown)
        .map(|link| link.title)
        .collect();
    assert_eq!(titles, [Some("see [[b]] too".to_string())]);
}

/// A `*` opening a target pairs with a literal one later in the heading, so
/// the heading would read as emphasis rather than its own characters.
#[test]
fn a_target_that_emphasises_a_heading_is_skipped() {
    assert_skipped_as_unrepresentable("# See [[a]] and b*\n", LinkFamily::Wikilink, "*x");
}

/// A `_` opening a target pairs with a literal one later in the heading,
/// changing the heading's text and so the slug a link addresses it by.
#[test]
fn a_target_that_emphasises_a_heading_by_underscore_is_skipped() {
    assert_skipped_as_unrepresentable("# See [[a]] and b_\n", LinkFamily::Wikilink, "_x");
}

/// A `**` opening a target pairs with a literal one later in the heading and
/// reads the text between as strong.
#[test]
fn a_target_that_strengthens_a_heading_is_skipped() {
    assert_skipped_as_unrepresentable("# See [[a]] and b**\n", LinkFamily::Wikilink, "**x");
}

/// A backslash ending a target escapes the bracket after it, so the heading
/// reads one character short of the link it holds.
#[test]
fn a_target_that_escapes_a_headings_bracket_is_skipped() {
    assert_skipped_as_unrepresentable("# See [[a]]\n", LinkFamily::Wikilink, "x\\");
}

/// A Markdown link's bracket text is read like a heading's: a `*` opening a
/// target pairs with a literal one later in it, and the title would read as
/// emphasis.
#[test]
fn a_target_that_emphasises_a_link_title_is_skipped() {
    assert_skipped_as_unrepresentable("[see [[a]] and b*](d)\n", LinkFamily::Wikilink, "*x");
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
