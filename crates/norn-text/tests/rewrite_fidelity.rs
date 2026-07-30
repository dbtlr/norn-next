//! Rewrite fidelity: a wikilink rewrite writes over the stem's bytes and
//! nothing else.
//!
//! Minimal diffs are this crate's identity — a one-field edit is a one-field
//! diff — and a rename cascade is where that matters most: the reviewer of a
//! hundred-file rename should see a hundred hunks that read as exactly the
//! rename. Preserving the title, the padding and the protocol is structural
//! here rather than tested-for: those bytes are never inside the edited range.

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

/// The ruling case: padding and title survive a rename, byte for byte.
#[test]
fn a_rewrite_replaces_the_stem_and_leaves_the_padding_and_title_alone() {
    assert_eq!(
        reconstruct_wikilink(&only("[[ Old | Title ]]"), "New").as_deref(),
        Some("[[ New | Title ]]")
    );
}

#[test]
fn every_byte_outside_the_stem_survives_a_rewrite() {
    for (raw, rewritten) in [
        ("[[Old]]", "[[New]]"),
        ("![[Old]]", "![[New]]"),
        ("[[Old|Shown]]", "[[New|Shown]]"),
        ("[[  Old  ]]", "[[  New  ]]"),
        ("[[ Old|  Shown  ]]", "[[ New|  Shown  ]]"),
        ("[[Old#Heading]]", "[[New#Heading]]"),
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
        assert_eq!(
            reconstruct_wikilink(&only(raw), "New").as_deref(),
            Some(rewritten),
            "rewriting {raw:?}"
        );
    }
}

/// Rewriting a link to the target it already has returns its own bytes. A
/// rename that renames nothing writes nothing.
#[test]
fn a_rewrite_to_the_same_target_is_the_identity() {
    for raw in [
        "[[Target]]",
        "![[ Target | Shown ]]",
        "[[ Target #Heading]]",
        "[[vault://Target#^blk|Shown]]",
        "[[Target|  odd   spacing  ]]",
    ] {
        let link = only(raw);
        assert_eq!(
            reconstruct_wikilink(&link, &link.target).as_deref(),
            Some(raw),
            "rewriting {raw:?} to itself"
        );
    }
}

/// The stem is exactly the bytes the fact reports as the target, whitespace
/// included: a space written between the stem and the fragment belongs to the
/// stem, so a rewrite replaces it with the rest.
#[test]
fn the_stem_is_the_bytes_the_fact_reports() {
    let link = only("[[ Old #Heading]]");
    assert_eq!(link.target, "Old ");
    assert_eq!(
        reconstruct_wikilink(&link, "New").as_deref(),
        Some("[[ New#Heading]]")
    );
}

// ── Over a whole document ────────────────────────────────────────────────

#[test]
fn a_splice_over_a_body_preserves_every_untouched_byte() {
    let body = "prose [[ Old | Shown ]] and ![[Old#^blk]] and [[keep]]\n\n\
                ```\n[[Old]]\n```\n";
    let out = BodyScan::new(body).splice_wikilinks(|link| {
        (link.target == "Old")
            .then(|| reconstruct_wikilink(link, "New"))
            .flatten()
    });
    assert_eq!(
        out,
        "prose [[ New | Shown ]] and ![[New#^blk]] and [[keep]]\n\n\
         ```\n[[Old]]\n```\n"
    );
}

#[test]
fn a_splice_over_raw_text_preserves_the_same_way() {
    assert_eq!(
        splice_wikilinks_in_text("[[ Old | Shown ]]", |link| reconstruct_wikilink(
            link, "New"
        )),
        "[[ New | Shown ]]"
    );
}

// ── What is still refused ────────────────────────────────────────────────

/// The representability refusals are unchanged: a stem that would re-parse as
/// a different link shape, or as no link at all, is refused rather than
/// emitted.
#[test]
fn an_unrepresentable_stem_is_still_refused() {
    let link = only("[[ Old | Shown ]]");
    for bad in [
        "a|b",
        "a#b",
        "a[b",
        "a]b",
        "",
        "   ",
        "\t",
        "a\nb",
        "a\r\nb",
        "vault://x",
    ] {
        assert!(!wikilink_target_is_representable(bad), "{bad:?}");
        assert!(
            reconstruct_wikilink(&link, bad).is_none(),
            "rewriting with {bad:?}"
        );
    }
}

/// A hand-built fact whose stem span does not index its own bytes is refused
/// rather than sliced. The fields are public, so the relationship is checked
/// where it is used.
#[test]
fn a_stem_span_that_does_not_index_its_own_bytes_is_refused() {
    let link = only("[[Old]]");
    let past_the_end = 0..link.raw.len() + 92;
    let backwards = link.raw.len()..2;
    for stem_range in [None, Some(past_the_end), Some(backwards)] {
        let broken = Link {
            stem_range,
            ..link.clone()
        };
        assert_eq!(reconstruct_wikilink(&broken, "New"), None);
    }

    // A span that lands inside a multi-byte character is refused too.
    let unicode = only("[[日本語]]");
    let broken = Link {
        stem_range: Some(2..4),
        ..unicode
    };
    assert_eq!(reconstruct_wikilink(&broken, "New"), None);
}

/// A multi-byte stem is replaced whole, and the bytes around it are untouched.
#[test]
fn a_multibyte_stem_is_replaced_whole() {
    assert_eq!(
        reconstruct_wikilink(&only("[[ 日本語 | 表示 ]]"), "New").as_deref(),
        Some("[[ New | 表示 ]]")
    );
    assert_eq!(
        reconstruct_wikilink(&only("[[Old]]"), "日本語").as_deref(),
        Some("[[日本語]]")
    );
}
