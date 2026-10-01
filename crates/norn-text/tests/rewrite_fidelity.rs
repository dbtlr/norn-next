//! Rewrite fidelity: a wikilink rewrite writes over the stem's bytes and
//! nothing else.
//!
//! Minimal diffs are this crate's identity — a one-field edit is a one-field
//! diff — and a rename cascade is where that matters most: the reviewer of a
//! hundred-file rename should see a hundred hunks that read as exactly the
//! rename. Preserving the title, the padding and the protocol is structural
//! here rather than tested-for: those bytes are never inside the edited range.

use norn_text::{Document, Link, LinkFamily, RewriteSkip, RewrittenLinks, parse_wikilinks_in_text};

fn only(text: &str) -> Link {
    let mut links = parse_wikilinks_in_text(text).into_iter();
    let link = links.next().expect("one link");
    assert!(
        links.next().is_none(),
        "expected exactly one link in {text:?}"
    );
    link
}

/// `raw`, a document holding one wikilink, with that link's stem rewritten to
/// `stem` through the one link rewrite — under the protocol it was written
/// with, because a rewrite keys a link by its address as written.
fn respelled(raw: &str, stem: &str) -> RewrittenLinks {
    let link = only(raw);
    let address = |stem: &str| match &link.protocol {
        Some(protocol) => format!("{protocol}://{stem}"),
        None => stem.to_string(),
    };
    Document::parse(raw).rewrite_links(LinkFamily::Wikilink, &address(&link.target), &address(stem))
}

/// [`respelled`]'s text, asserting the one link was rewritten.
fn rewritten(raw: &str, stem: &str) -> String {
    let out = respelled(raw, stem);
    assert_eq!(out.rewritten, 1, "rewriting {raw:?} to {stem:?}");
    out.text
}

// ── The stem sub-span ────────────────────────────────────────────────────

/// The stem's span indexes the token's own bytes, so a caller can see exactly
/// what a rewrite will replace before it replaces it.
#[test]
fn the_stem_span_names_the_targets_bytes_inside_the_token() {
    for raw in [
        "[[Target]]",
        "![[Target]]",
        "[[ Target | Shown ]]",
        "[[vault://Target#Heading|Shown]]",
        "[[Target#^blk]]",
        "[[#Heading]]",
    ] {
        let link = only(raw);
        let stem = link.stem_range.clone().expect("a wikilink has a stem span");
        assert_eq!(&link.raw[stem], link.target, "stem span of {raw:?}");
    }
}

// ── Preserve ─────────────────────────────────────────────────────────────

/// Padding and title survive a rename, byte for byte: neither is inside the
/// edited range.
#[test]
fn a_rewrite_replaces_the_stem_and_leaves_the_padding_and_title_alone() {
    assert_eq!(rewritten("[[ Old | Title ]]", "New"), "[[ New | Title ]]");
}

/// NRN-431: a hand-rolled rewriter that strips `[[` and re-emits loses the
/// embed marker and the title, so a transclusion silently becomes a plain
/// link. Every byte outside the stem is outside the edit.
#[test]
fn every_byte_outside_the_stem_survives_a_rewrite() {
    for (raw, expected) in [
        ("[[Old]]", "[[New]]"),
        ("![[Old]]", "![[New]]"),
        ("[[Old|Shown]]", "[[New|Shown]]"),
        ("![[Old|Shown]]", "![[New|Shown]]"),
        ("[[  Old  ]]", "[[  New  ]]"),
        ("[[ Old|  Shown  ]]", "[[ New|  Shown  ]]"),
        ("[[Old#Heading]]", "[[New#Heading]]"),
        ("![[Old#Heading]]", "![[New#Heading]]"),
        ("[[ Old#Heading | Shown ]]", "[[ New#Heading | Shown ]]"),
        ("[[Old#^blk]]", "[[New#^blk]]"),
        ("![[ Old#^blk | Shown ]]", "![[ New#^blk | Shown ]]"),
        ("[[vault://Old]]", "[[vault://New]]"),
        (
            "[[ vault://Old#Heading | Shown ]]",
            "[[ vault://New#Heading | Shown ]]",
        ),
        (
            "[[Old|Shown | With | Pipes]]",
            "[[New|Shown | With | Pipes]]",
        ),
    ] {
        assert_eq!(rewritten(raw, "New"), expected, "rewriting {raw:?}");
    }
}

/// Rewriting a link to the target it already has returns its own bytes. A
/// rename that renames nothing writes nothing.
#[test]
fn a_rewrite_to_the_same_target_is_the_identity() {
    for raw in [
        "[[Target]]",
        "![[Target]]",
        "[[Target|Title]]",
        "![[ Target | Shown ]]",
        "[[ Target #Heading]]",
        "[[Target#Heading|Title]]",
        "[[vault://Target#^blk|Shown]]",
        "[[Target|  odd   spacing  ]]",
    ] {
        assert_eq!(
            rewritten(raw, &only(raw).target),
            raw,
            "rewriting {raw:?} to itself"
        );
    }
}

/// Whitespace between the stem and the fragment is padding. The address is
/// `Old`, not `Old `, because no resolver will ever match a target with a
/// space welded to its end — and the space stays outside the edited range, so
/// a rename leaves it exactly where the author put it.
#[test]
fn whitespace_between_the_stem_and_the_fragment_is_padding() {
    assert_eq!(only("[[ Old #Heading]]").target, "Old");
    assert_eq!(rewritten("[[ Old #Heading]]", "New"), "[[ New #Heading]]");

    assert_eq!(only("![[ Old #Head | Title ]]").target, "Old");
    assert_eq!(
        rewritten("![[ Old #Head | Title ]]", "NEW"),
        "![[ NEW #Head | Title ]]"
    );
}

// ── What is still refused ────────────────────────────────────────────────

/// A stem that would re-parse as a different link shape, as no link at all, or
/// as a different target, is refused rather than emitted. This is the one
/// matrix: a delimiter re-parses as another part of the grammar, an empty
/// target writes `[[]]` and vanishes, a line break rewrites the block the link
/// sat in, a protocol prefix re-parses as a protocol, and a padded target
/// re-parses trimmed — so the fact it produced would not be the fact it was
/// written from.
#[test]
fn an_unrepresentable_stem_is_refused() {
    let raw = "[[ Old | Shown ]]";
    for bad in [
        "a|b",
        "a#b",
        "a[b",
        "a]b",
        "a[[b",
        "a]]b",
        "",
        "   ",
        "\t",
        "\n",
        "a\nb",
        "a\r\nb",
        "vault://x",
        "https://example.com",
        "a://b",
        " New ",
        "New ",
        "\tNew",
        "New\n",
    ] {
        let out = respelled(raw, bad);
        assert_eq!(out.text, raw, "rewriting with {bad:?}");
        assert_eq!(
            out.skipped
                .iter()
                .map(|skip| skip.reason)
                .collect::<Vec<_>>(),
            [RewriteSkip::Unrepresentable],
            "rewriting with {bad:?}"
        );
    }

    // The forms that only look like the refused ones round-trip. A caret is
    // an ordinary target character, and a single colon is no protocol.
    for fine in ["a^b", "note:draft", "HTTPS://x", "vault:/x", "a b"] {
        assert_eq!(
            rewritten(raw, fine),
            format!("[[ {fine} | Shown ]]"),
            "{fine:?}"
        );
    }
}

/// The reason the refusal is not pedantry: a line break inside a table cell
/// ends the row, and one inside a blockquote ends the quote. A rewrite that
/// emitted these would rewrite the block the link sat in, so it emits nothing
/// and leaves the token alone.
#[test]
fn a_rewrite_to_an_unrepresentable_target_leaves_the_document_alone() {
    for body in [
        "| a | b |\n| --- | --- |\n| [[old]] | x |\n",
        "> quoted [[old]] here\n> still quoted\n",
    ] {
        for bad in ["a\nb", "", " padded "] {
            let out = Document::parse(body).rewrite_links(LinkFamily::Wikilink, "old", bad);
            assert_eq!(out.text, body, "rewriting {body:?} to {bad:?}");
            assert_eq!(out.rewritten, 0, "rewriting {body:?} to {bad:?}");
        }
    }
}

/// A token carrying a line break in its own bytes is refused whatever it is
/// rewritten to. The token reads; the rewrite does not happen. Without this
/// the recognize-and-refuse pair was only half real — the identity path
/// refused because the target was unrepresentable, and every other rename went
/// straight through and reflowed the paragraph.
#[test]
fn a_token_carrying_a_line_break_refuses_every_rewrite() {
    for raw in [
        "[[Target\nOther]]",
        "[[Target\r\nOther]]",
        "[[ Target\nOther | Shown ]]",
    ] {
        for stem in ["New", only(raw).target.as_str()] {
            let out = respelled(raw, stem);
            assert_eq!(out.text, raw, "rewriting {raw:?} to {stem:?}");
            assert_eq!(
                out.skipped
                    .iter()
                    .map(|skip| skip.reason)
                    .collect::<Vec<_>>(),
                [RewriteSkip::LinkNotRewritable],
                "rewriting {raw:?} to {stem:?}"
            );
        }
    }
}

/// A multi-byte stem is replaced whole, and the bytes around it are untouched.
#[test]
fn a_multibyte_stem_is_replaced_whole() {
    assert_eq!(rewritten("[[ 日本語 | 表示 ]]", "New"), "[[ New | 表示 ]]");
    assert_eq!(rewritten("[[Old]]", "日本語"), "[[日本語]]");
}
