//! Wikilink syntax: what a `[[…]]` token decomposes into, where code makes it
//! invisible, and what survives a target rewrite.
//!
//! The grammar is `[[ [protocol://] stem [#anchor | #^block-ref] [| title] ]]`.
//! Protocol recognition has its own suite (`protocols.rs`) and so does the
//! inline Markdown family (`markdown_links.rs`). Everything beyond
//! recognition — resolving a target to a document, giving a protocol a
//! meaning — is somebody else's layer.

use norn_text::{
    BodyScan, Link, parse_wikilinks_in_text, reconstruct_wikilink, splice_wikilinks_in_text,
    wikilink_target_is_representable,
};

fn only(text: &str) -> Link {
    let mut links = parse_wikilinks_in_text(text).into_iter();
    let link = links.next().expect("one link");
    assert!(
        links.next().is_none(),
        "expected exactly one link in {text:?}"
    );
    link
}

fn targets(links: &[Link]) -> Vec<&str> {
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

/// The degenerate tokens, pinned as what they are. `[[]]` is not a token —
/// the grammar wants at least one character inside the fences — and the rest
/// are tokens with empty parts, which is different from being unrecognized.
#[test]
fn the_degenerate_tokens_are_told_apart_from_no_token_at_all() {
    assert!(parse_wikilinks_in_text("[[]]").is_empty());
    assert!(parse_wikilinks_in_text("![[]]").is_empty());

    // `[[   ]]` *is* a token: there are characters between the fences, and
    // padding is padding whether or not anything is padded. Its target is
    // empty, which is a broken link somebody wrote rather than a token nobody
    // wrote.
    let blank = only("[[   ]]");
    assert_eq!(blank.target, "");
    assert_eq!(blank.title, None);
    assert_eq!(blank.anchor, None);
    assert_eq!(
        reconstruct_wikilink(&blank, "new").as_deref(),
        Some("[[   new]]")
    );

    // `[[|]]` is a token with an empty target and an empty title.
    let piped = only("[[|]]");
    assert_eq!(piped.target, "");
    assert_eq!(piped.title.as_deref(), Some(""));
    assert_eq!(piped.anchor, None);

    // `[[a#]]` is a token with an empty anchor, which is not the same as no
    // anchor: the `#` was written.
    let anchored = only("[[a#]]");
    assert_eq!(anchored.target, "a");
    assert_eq!(anchored.anchor.as_deref(), Some(""));

    // `[[a|]]` is a token with an empty title.
    let titled = only("[[a|]]");
    assert_eq!(titled.target, "a");
    assert_eq!(titled.title.as_deref(), Some(""));

    // None of the three has a representable target to be rewritten to, and
    // reconstructing the two that do keeps the empty part they carry.
    assert!(!wikilink_target_is_representable(&piped.target));
    assert_eq!(
        reconstruct_wikilink(&anchored, "new").as_deref(),
        Some("[[new#]]")
    );
    assert_eq!(
        reconstruct_wikilink(&titled, "new").as_deref(),
        Some("[[new|]]")
    );
}

/// A soft break does not interrupt recognition. Two tokens on two lines of one
/// paragraph are two tokens, and a token whose fences straddle the break is
/// still a token — its target simply carries the newline.
///
/// It reads and it does not rewrite. A replacement is one line, and splicing
/// one over bytes that span several deletes the line breaks between them: the
/// paragraph the token swallowed comes back reflowed. Recognizing the token
/// and refusing to rewrite it is the pair that keeps a rename out of prose it
/// was never pointed at.
#[test]
fn a_token_is_recognized_across_a_soft_break_and_refuses_to_be_rewritten() {
    let body = "see [[Target]]\nand [[Other]] too\n";
    assert_eq!(
        targets(&BodyScan::new(body).wikilinks()),
        ["Target", "Other"]
    );

    let straddling = "before [[Target\nOther]] after\n";
    let link = only(straddling);
    assert_eq!(link.target, "Target\nOther");
    assert!(!wikilink_target_is_representable(&link.target));
    assert_eq!(reconstruct_wikilink(&link, &link.target), None);
    assert_eq!(reconstruct_wikilink(&link, "new"), None);
    assert_eq!(
        BodyScan::new(straddling).splice_wikilinks(|link| reconstruct_wikilink(link, "new")),
        straddling
    );
}

/// The reason the refusal is not pedantry: an unclosed `[[` swallows every
/// byte up to the next `]]`, paragraph breaks included. A rename that spliced
/// a one-line stem over that token would delete two paragraphs and the blank
/// line between them, and report success.
#[test]
fn a_token_that_swallowed_two_paragraphs_is_a_no_op_splice() {
    let body = "an unclosed [[link here\n\nand a later ]] closer\n\nplus [[Real]]\n";
    let out = BodyScan::new(body).splice_wikilinks(|link| reconstruct_wikilink(link, "new"));
    assert_eq!(
        out,
        "an unclosed [[link here\n\nand a later ]] closer\n\nplus [[new]]\n"
    );
}

// ── One splitter, and the bare caret (NRN-433, NRN-440) ──────────────────

/// NRN-433: a second splitter treated any `^` as a block sigil, so a rewrite
/// of a caret-bearing name silently did nothing and reported success. There is
/// one splitter, and it says a caret is a block sigil only after a hash.
#[test]
fn a_bare_caret_is_an_ordinary_target_character() {
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
    let hashes = only("[[Note#Heading#With#Hashes]]");
    assert_eq!(hashes.target, "Note");
    assert_eq!(hashes.anchor.as_deref(), Some("Heading#With#Hashes"));
    assert_eq!(hashes.block_ref, None);

    let block = only("[[Note#^block-id]]");
    assert_eq!(block.target, "Note");
    assert_eq!(block.anchor, None);
    assert_eq!(block.block_ref.as_deref(), Some("block-id"));

    let plain = only("[[Note]]");
    assert_eq!(plain.target, "Note");
    assert_eq!(plain.anchor, None);

    let same_note = only("[[#Heading]]");
    assert_eq!(same_note.target, "");
    assert_eq!(same_note.anchor.as_deref(), Some("Heading"));
}

/// The block-reference alphabets are not symmetric, and the asymmetry is
/// characterized rather than fixed. A `#^id` fragment is recorded raw, so it
/// carries whatever the author wrote; a trailing `^id` *definition* is matched
/// against an ASCII alphabet, so the non-ASCII one it would point at is
/// invisible. Widening the definition's alphabet is a behaviour change with
/// its own venue.
#[test]
fn a_block_reference_carries_what_a_block_id_definition_will_not_match() {
    assert_eq!(only("[[N#^ünicode]]").block_ref.as_deref(), Some("ünicode"));
    assert!(
        BodyScan::new("a paragraph ^ünicode\n")
            .block_ids()
            .is_empty()
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

// ── Splicing (NRN-424, NRN-432, NRN-484) ─────────────────────────────────
//
// What a rewrite preserves and what it refuses is `rewrite_fidelity.rs`. What
// a splice selects is here.

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

// ── What the token parser is not ─────────────────────────────────────────

/// An inline Markdown link is not a wikilink, and the token parser sees
/// `[[…]]` and nothing else. This is also the frontmatter contract: a
/// frontmatter value is scanned through this entry point, so a
/// `[title](target)` string in a property yields no link fact.
#[test]
fn an_inline_markdown_link_is_not_a_wikilink() {
    assert!(parse_wikilinks_in_text("[text](target.md)").is_empty());
}

/// A target is reported exactly as it was written. This crate applies no
/// normalization to it — no extension stripping, no path-stem semantics — so
/// nothing here can turn `v1.2` into `v1`. Normalizing a target is resolution's
/// job, and resolution is not in this crate.
#[test]
fn a_target_is_reported_verbatim_and_never_normalized() {
    for raw in [
        "[[Note.md]]",
        "[[folder/Note.md]]",
        "[[v1.2]]",
        "[[Note.markdown]]",
    ] {
        let link = only(raw);
        let inner = &raw[2..raw.len() - 2];
        assert_eq!(link.target, inner, "target of {raw:?}");
    }
}

/// A `#tag` is its own fact kind. It is never a wikilink and never a heading,
/// and the tag family's own contract lives beside it.
#[test]
fn a_hash_tag_is_not_a_wikilink_and_not_a_heading() {
    let scan = BodyScan::new("prose with #tag and #another\n");
    assert!(scan.wikilinks().is_empty());
    assert!(scan.headings().is_empty());
    assert_eq!(scan.tags().len(), 2);
}
