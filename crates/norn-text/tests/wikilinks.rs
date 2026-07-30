//! Wikilink syntax: what a `[[…]]` token decomposes into, where code makes it
//! invisible, and what survives a target rewrite.
//!
//! The grammar is `[[ target [#anchor | #^block-ref] [| title] ]]`. Everything
//! beyond that — resolving a target to a document, reading a prefix as a
//! protocol — is somebody else's layer.

use norn_text::{
    BodyScan, Wikilink, parse_wikilinks_in_text, reconstruct_wikilink, splice_wikilinks_in_text,
    split_wikilink_target, wikilink_target_is_representable,
};

fn only(text: &str) -> Wikilink {
    let mut links = parse_wikilinks_in_text(text).into_iter();
    let link = links.next().expect("one link");
    assert!(
        links.next().is_none(),
        "expected exactly one link in {text:?}"
    );
    link
}

fn targets(links: &[Wikilink]) -> Vec<&str> {
    links.iter().map(|link| link.target.as_str()).collect()
}

// ── Decomposition ────────────────────────────────────────────────────────

#[test]
fn a_plain_link_is_a_target_and_nothing_else() {
    let link = only("see [[Target]] here\n");
    assert_eq!(link.raw, "[[Target]]");
    assert!(!link.embed);
    assert_eq!(link.target, "Target");
    assert_eq!(link.title, None);
    assert_eq!(link.anchor, None);
    assert_eq!(link.block_ref, None);
}

#[test]
fn the_title_after_a_pipe_is_split_and_trimmed() {
    let link = only("[[Target| Display Name ]]\n");
    assert_eq!(link.target, "Target");
    assert_eq!(link.title.as_deref(), Some("Display Name"));
}

#[test]
fn a_heading_anchor_and_a_block_reference_are_told_apart() {
    let anchored = only("[[Note#Heading]]\n");
    assert_eq!(anchored.target, "Note");
    assert_eq!(anchored.anchor.as_deref(), Some("Heading"));
    assert_eq!(anchored.block_ref, None);

    let referenced = only("[[Note#^block-id]]\n");
    assert_eq!(referenced.target, "Note");
    assert_eq!(referenced.anchor, None);
    assert_eq!(referenced.block_ref.as_deref(), Some("block-id"));
}

#[test]
fn an_embed_marker_is_part_of_the_token() {
    let link = only("![[Image.png]]\n");
    assert!(link.embed);
    assert_eq!(link.target, "Image.png");
    assert_eq!(link.raw, "![[Image.png]]");
}

#[test]
fn a_same_note_reference_has_an_empty_target() {
    let link = only("[[#Heading]]\n");
    assert_eq!(link.target, "");
    assert_eq!(link.anchor.as_deref(), Some("Heading"));
}

#[test]
fn a_token_reports_where_it_starts_and_how_far_it_runs() {
    let body = "abc [[Target]] def\n";
    let link = only(body);
    assert_eq!(&body[link.range()], "[[Target]]");
    assert_eq!(link.span.line, 1);
    assert_eq!(link.span.column, 5);
}

#[test]
fn several_links_on_one_line_are_all_recognized() {
    let links = parse_wikilinks_in_text("[[a]] and [[b]] and [[c]]\n");
    assert_eq!(targets(&links), ["a", "b", "c"]);
}

// ── One splitter, and the bare caret (NRN-433, NRN-440) ──────────────────

/// NRN-433: a second splitter treated any `^` as a block sigil, so a rewrite
/// of a caret-bearing name silently did nothing and reported success. There is
/// one splitter, and it says a caret is a block sigil only after a hash.
#[test]
fn a_bare_caret_is_an_ordinary_target_character() {
    assert_eq!(
        split_wikilink_target("a^b"),
        ("a^b".to_string(), None, None)
    );
    let link = only("[[a^b]]");
    assert_eq!(link.target, "a^b");
    assert_eq!(link.block_ref, None);
    assert!(wikilink_target_is_representable("a^b"));
    assert_eq!(
        reconstruct_wikilink(&link, "renamed").as_deref(),
        Some("[[renamed]]")
    );
}

#[test]
fn only_the_first_hash_splits_and_the_rest_stay_in_the_anchor() {
    assert_eq!(
        split_wikilink_target("Note#Heading#With#Hashes"),
        (
            "Note".to_string(),
            Some("Heading#With#Hashes".to_string()),
            None
        )
    );
    assert_eq!(
        split_wikilink_target("Note#^block-id"),
        ("Note".to_string(), None, Some("block-id".to_string()))
    );
    assert_eq!(
        split_wikilink_target("Note"),
        ("Note".to_string(), None, None)
    );
    assert_eq!(
        split_wikilink_target("#Heading"),
        ("".to_string(), Some("Heading".to_string()), None)
    );
}

// ── Code is opaque (NRN-432, NRN-350) ────────────────────────────────────

#[test]
fn a_link_inside_inline_code_is_literal_text() {
    let links = BodyScan::new("before `[[ignored]]` after [[real]]\n").wikilinks();
    assert_eq!(targets(&links), ["real"]);
}

#[test]
fn a_link_inside_a_fence_is_literal_text() {
    let body = "outside [[real]]\n\n```\n[[in code]]\n```\n\nafter [[real2]]\n";
    assert_eq!(targets(&BodyScan::new(body).wikilinks()), ["real", "real2"]);
}

#[test]
fn a_link_inside_an_indented_code_block_is_literal_text() {
    let body = "prose [[real]]\n\n    [[indented]]\n\nmore\n";
    assert_eq!(targets(&BodyScan::new(body).wikilinks()), ["real"]);
}

/// Raw text — a frontmatter value, say — has no Markdown code semantics, so
/// nothing is excluded there. The two entry points differ in exactly that.
#[test]
fn raw_text_has_no_code_to_be_opaque() {
    let links = parse_wikilinks_in_text("`[[still parsed]]`");
    assert_eq!(targets(&links), ["still parsed"]);
}

/// One fence, every parser: no heading, no link and no block id may be read
/// out of it.
#[test]
fn one_fence_is_opaque_to_every_body_parser() {
    let body = "outside [[real-link]]\n\n```\n# Fake Heading\n[[fake-wikilink]]\n^fake-block-id\n\
                ```\n";
    let scan = BodyScan::new(body);
    assert_eq!(targets(&scan.wikilinks()), ["real-link"]);
    assert!(scan.block_ids().is_empty());
    assert!(scan.headings().is_empty());
}

// ── Reconstruction (NRN-431) ─────────────────────────────────────────────

/// NRN-431: a hand-rolled rewriter that strips `[[` and re-emits loses the
/// embed marker and the title, so a transclusion silently becomes a plain
/// link. A tight link reconstructed with its own target is byte-identical.
#[test]
fn a_tight_link_reconstructed_with_its_own_target_is_unchanged() {
    for raw in [
        "[[Target]]",
        "![[Target]]",
        "[[Target|Title]]",
        "![[Target|Title]]",
        "[[Target#Heading]]",
        "![[Target#Heading]]",
        "[[Target#Heading|Title]]",
        "[[Target#^blk]]",
        "![[Target#^blk|Title]]",
    ] {
        let link = only(raw);
        assert_eq!(
            reconstruct_wikilink(&link, &link.target).as_deref(),
            Some(raw),
            "reconstructing {raw:?}"
        );
    }
}

#[test]
fn a_rewrite_keeps_the_embed_marker_the_anchor_and_the_title() {
    assert_eq!(
        reconstruct_wikilink(&only("![[old|Display]]"), "new").as_deref(),
        Some("![[new|Display]]")
    );
    assert_eq!(
        reconstruct_wikilink(&only("[[old#Heading]]"), "new").as_deref(),
        Some("[[new#Heading]]")
    );
    assert_eq!(
        reconstruct_wikilink(&only("[[old#^blk]]"), "new").as_deref(),
        Some("[[new#^blk]]")
    );
}

/// A padded link canonicalizes on rewrite: the title and anchor are re-emitted
/// from their parsed, trimmed forms. That is a rewrite's behaviour, not a
/// round-trip guarantee.
#[test]
fn a_padded_link_canonicalizes_when_it_is_rewritten() {
    let link = only("[[ Target | Shown ]]");
    assert_eq!(link.target, "Target");
    assert_eq!(link.title.as_deref(), Some("Shown"));
    assert_eq!(
        reconstruct_wikilink(&link, "Target").as_deref(),
        Some("[[Target|Shown]]")
    );
}

#[test]
fn a_target_carrying_a_delimiter_is_refused_rather_than_emitted() {
    let link = only("[[old]]");
    for bad in ["a|b", "a#b", "a]]b", "a[[b", "a]b", "a[b"] {
        assert!(!wikilink_target_is_representable(bad), "{bad:?}");
        assert!(
            reconstruct_wikilink(&link, bad).is_none(),
            "reconstructing with {bad:?}"
        );
    }
}

// ── Splicing (NRN-424, NRN-432, NRN-484) ─────────────────────────────────

#[test]
fn a_splice_rewrites_only_the_tokens_it_selects() {
    let body = "[[keep]] and [[old]] and [[old|Title]]\n";
    let out = BodyScan::new(body).splice_wikilinks(|link| {
        (link.target == "old")
            .then(|| reconstruct_wikilink(link, "new"))
            .flatten()
    });
    assert_eq!(out, "[[keep]] and [[new]] and [[new|Title]]\n");
}

/// NRN-432: a fenced sample was the first occurrence in the file, so a
/// textual rewriter changed it and left the prose link dangling.
#[test]
fn a_splice_over_a_body_never_touches_a_fenced_sample() {
    let body = "```\n[[old]]\n```\n\nprose [[old]]\n";
    let out = BodyScan::new(body).splice_wikilinks(|link| {
        (link.target == "old")
            .then(|| reconstruct_wikilink(link, "new"))
            .flatten()
    });
    assert_eq!(out, "```\n[[old]]\n```\n\nprose [[new]]\n");
}

/// NRN-484 needs occurrence selection, and the splice's callback is `FnMut`
/// precisely so it is expressible.
#[test]
fn a_splice_can_rewrite_the_first_match_only() {
    let body = "[[old]] then [[old]]\n";
    let mut done = false;
    let out = BodyScan::new(body).splice_wikilinks(|link| {
        if !done && link.target == "old" {
            done = true;
            reconstruct_wikilink(link, "new")
        } else {
            None
        }
    });
    assert_eq!(out, "[[new]] then [[old]]\n");
}

#[test]
fn a_splice_over_raw_text_has_no_code_exclusion() {
    let out = splice_wikilinks_in_text("`[[old]]`", |link| reconstruct_wikilink(link, "new"));
    assert_eq!(out, "`[[new]]`");
}

#[test]
fn a_splice_that_selects_nothing_returns_the_text_unchanged() {
    let body = "prose [[a]] and [[b]]\n";
    assert_eq!(BodyScan::new(body).splice_wikilinks(|_| None), body);
}

// ── Block ids (NRN-350) ──────────────────────────────────────────────────

#[test]
fn a_trailing_block_id_is_an_anchor() {
    assert_eq!(
        BodyScan::new("Some paragraph. ^block-1\n").block_ids(),
        ["block-1"]
    );
    assert_eq!(BodyScan::new("^block-2\n").block_ids(), ["block-2"]);
    assert_eq!(
        BodyScan::new("first ^a\nsecond ^b\nthird\n").block_ids(),
        ["a", "b"]
    );
    assert_eq!(BodyScan::new("hello ^ok  \n").block_ids(), ["ok"]);
}

#[test]
fn a_block_id_holding_an_unsupported_character_is_not_an_anchor() {
    assert!(BodyScan::new("hello ^bad.id\n").block_ids().is_empty());
}

/// NRN-350: block-id parsing had no code filter while wikilink parsing did,
/// so the two parsers disagreed about the same fence.
#[test]
fn a_block_id_inside_code_is_not_an_anchor() {
    assert_eq!(
        BodyScan::new("real ^outside\n\n```\n^incode\n```\n").block_ids(),
        ["outside"]
    );
    assert!(BodyScan::new("prose\n`^incode`\n").block_ids().is_empty());
}

/// The nuance that goes with it: a `^id` on the line *after* a closing fence
/// lies outside the fence and references the code block itself.
#[test]
fn a_block_id_on_the_line_after_a_fence_is_still_an_anchor() {
    assert_eq!(
        BodyScan::new("```\ncode line\n```\n^after-fence\n").block_ids(),
        ["after-fence"]
    );
}

// ── Where NORN-26 picks up ───────────────────────────────────────────────

/// A protocol-looking prefix is not recognized: `https://example.com` is one
/// ordinary target, colon and slashes included. Reading a prefix as a protocol
/// is the next task's work, and this states what the seed does until then.
#[test]
fn a_protocol_looking_prefix_is_part_of_the_target_today() {
    let link = only("[[https://example.com|Home]]");
    assert_eq!(link.target, "https://example.com");
    assert_eq!(link.title.as_deref(), Some("Home"));
    let prefixed = only("[[vault:Note#Heading]]");
    assert_eq!(prefixed.target, "vault:Note");
    assert_eq!(prefixed.anchor.as_deref(), Some("Heading"));
}

/// An inline Markdown link is not a wikilink and is not recognized as
/// anything: the token parser sees `[[…]]` and nothing else.
#[test]
fn an_inline_markdown_link_is_not_a_wikilink() {
    assert!(parse_wikilinks_in_text("[text](target.md)").is_empty());
}

/// A `#tag` in a body is ordinary text today.
#[test]
fn a_hash_tag_is_not_a_token_this_crate_recognizes() {
    let scan = BodyScan::new("prose with #tag and #another\n");
    assert!(scan.wikilinks().is_empty());
    assert!(scan.headings().is_empty());
}
