//! Anchor addressing: what a heading's slug is, how two headings with the
//! same text are told apart, and how the two link families address a heading
//! differently while this crate records both fragments raw.

use norn_text::{BodyScan, Link, LinkFamily, slugify};

fn slugs(body: &str) -> Vec<String> {
    BodyScan::new(body)
        .headings()
        .iter()
        .map(|heading| heading.slug.clone())
        .collect()
}

fn only_wikilink(body: &str) -> Link {
    BodyScan::new(body).wikilinks().remove(0)
}

fn only_markdown(body: &str) -> Link {
    BodyScan::new(body)
        .links()
        .into_iter()
        .find(|link| link.family == LinkFamily::Markdown)
        .expect("a markdown link")
}

// ── The slug is GFM's ────────────────────────────────────────────────────

/// Lowercase, letters and digits kept, punctuation dropped, a space written as
/// a hyphen. Matching what a Markdown renderer emits is the whole point of a
/// slug: an anchor nobody else computes the same way addresses nothing.
#[test]
fn a_slug_lowercases_keeps_alphanumerics_and_drops_punctuation() {
    assert_eq!(slugify("What? Really!"), "what-really");
    assert_eq!(slugify("Hello, World"), "hello-world");
    assert_eq!(slugify("Heading 1.2.3"), "heading-123");
    assert_eq!(
        slugify("snake_case and kebab-case"),
        "snake_case-and-kebab-case"
    );
    assert_eq!(slugify("  padded  "), "padded");
}

/// Letters are letters in every alphabet. The ASCII-only form this replaces
/// collapsed a non-English heading to nothing, which made every such heading
/// in a document collide with every other one.
#[test]
fn a_slug_keeps_letters_whatever_alphabet_they_are_written_in() {
    assert_eq!(slugify("日本語"), "日本語");
    assert_eq!(slugify("한국어"), "한국어");
    assert_eq!(slugify("Café Münster"), "café-münster");
    assert_eq!(slugify("Проект"), "проект");
    assert_ne!(slugify("日本語"), slugify("한국어"));
}

/// A combining mark is kept, because dropping it writes a different word and
/// the anchor stops matching the one a renderer emits. Lowercasing is where
/// this bites hardest: `İ` lowercases to an `i` and a combining dot above, and
/// the dot has to survive.
#[test]
fn a_slug_keeps_the_combining_marks_the_text_carries() {
    assert_eq!(slugify("हिन्दी"), "हिन्दी");
    assert_eq!(slugify("İstanbul"), "i\u{307}stanbul");
    assert_eq!(slugify("cafe\u{301}"), "cafe\u{301}");
}

/// Characterized: a space is the one whitespace character a slug turns into a
/// hyphen. A tab or a non-breaking space is neither a space nor a letter, so
/// it is dropped like any other unsluggable character and the words either
/// side of it run together.
#[test]
fn only_a_space_becomes_a_hyphen() {
    assert_eq!(slugify("A B"), "a-b");
    assert_eq!(slugify("A\tB"), "ab");
    assert_eq!(slugify("A\u{A0}B"), "ab");
    assert_eq!(slugs("## A\u{A0}B\n"), ["ab"]);
}

#[test]
fn a_heading_carries_the_slug_of_its_flattened_text() {
    assert_eq!(slugs("## What? Really!\n"), ["what-really"]);
    assert_eq!(slugs("## Use `norn` **now**\n"), ["use-norn-now"]);
    assert_eq!(slugs("Setext Title\n===\n"), ["setext-title"]);
}

// ── Document-order dedupe ────────────────────────────────────────────────

/// Two headings with the same text get two anchors, because one anchor that
/// addresses two headings addresses neither. The first keeps the bare slug and
/// each later one takes the next suffix, in document order.
#[test]
fn repeated_headings_take_document_order_suffixes() {
    assert_eq!(
        slugs("# Notes\n\n# Notes\n\n# Notes\n"),
        ["notes", "notes-1", "notes-2"]
    );
}

/// A suffixed slug is itself taken, so a heading whose own slug collides with
/// one already issued keeps going rather than duplicating it.
#[test]
fn a_suffix_that_is_already_taken_is_stepped_past() {
    assert_eq!(slugs("# a\n\n# a\n\n# a-1\n"), ["a", "a-1", "a-1-1"]);
}

/// Dedupe is a property of the document, so the free function answers about
/// one text and the scan applies the suffixes.
#[test]
fn the_free_function_is_the_base_form_without_suffixes() {
    assert_eq!(slugify("Notes"), "notes");
    assert_eq!(slugify("Notes"), slugify("Notes"));
    assert_eq!(slugs("# Notes\n\n# Notes\n"), ["notes", "notes-1"]);
}

/// Headings that slug to nothing are still told apart from each other.
#[test]
fn headings_with_no_sluggable_characters_are_still_told_apart() {
    assert_eq!(slugs("# ???\n\n# !!!\n"), ["", "-1"]);
}

// ── Each family addresses by its own standard ────────────────────────────

/// One heading, two correct fragments. A wikilink anchor is the heading's
/// text, the way the editor that popularized the syntax reads it; a Markdown
/// fragment is the heading's slug, the way every Markdown renderer reads it.
/// Both are recorded exactly as written — dispatching on the family, and
/// matching either one, is the resolution layer's work.
#[test]
fn a_wikilink_addresses_heading_text_and_a_markdown_link_addresses_the_slug() {
    let body = "## What? Really!\n\n[[Note#What? Really!]] and [t](./note.md#what-really)\n";
    let scan = BodyScan::new(body);

    let heading = &scan.headings()[0];
    assert_eq!(heading.text, "What? Really!");
    assert_eq!(heading.slug, "what-really");

    let wikilink = scan.wikilinks().remove(0);
    assert_eq!(wikilink.anchor.as_deref(), Some("What? Really!"));

    let markdown = only_markdown(body);
    assert_eq!(markdown.anchor.as_deref(), Some("what-really"));
    assert_eq!(markdown.anchor.as_deref(), Some(heading.slug.as_str()));
}

/// A wikilink anchor stays raw and lossless: case, spaces and punctuation are
/// recorded as written, and no slug is computed for one. Whether `#Heading`
/// and `#heading` are the same anchor is a matching question with an owner
/// elsewhere.
#[test]
fn a_wikilink_anchor_is_never_slugged() {
    for (body, anchor) in [
        ("[[Note#What? Really!]]\n", "What? Really!"),
        ("[[Note#Mixed Case Heading]]\n", "Mixed Case Heading"),
        ("[[Note#日本語]]\n", "日本語"),
        // The padding a token carries is trimmed off the token; the padding
        // written inside the fragment is the fragment's.
        ("[[Note#  padded  ]]\n", "  padded"),
    ] {
        assert_eq!(
            only_wikilink(body).anchor.as_deref(),
            Some(anchor),
            "anchor of {body:?}"
        );
    }
}

/// A Markdown fragment is recorded raw too. Recording the slug form is what
/// the author wrote, not a normalization this crate applied, and an
/// unsluggable fragment is reported as written rather than corrected.
#[test]
fn a_markdown_fragment_is_recorded_as_written() {
    for (body, anchor) in [
        ("[t](./note.md#what-really)\n", "what-really"),
        ("[t](./note.md#What%20Really)\n", "What%20Really"),
        ("[t](./note.md#日本語)\n", "日本語"),
    ] {
        assert_eq!(
            only_markdown(body).anchor.as_deref(),
            Some(anchor),
            "anchor of {body:?}"
        );
    }
}

/// Block references split the same way in both families, so a `#^id` is never
/// mistaken for a heading anchor whichever form carries it.
#[test]
fn a_block_reference_splits_the_same_way_in_both_families() {
    let wikilink = only_wikilink("[[Note#^blk]]\n");
    let markdown = only_markdown("[t](./note.md#^blk)\n");
    assert_eq!(wikilink.block_ref.as_deref(), Some("blk"));
    assert_eq!(markdown.block_ref.as_deref(), Some("blk"));
    assert_eq!(wikilink.anchor, None);
    assert_eq!(markdown.anchor, None);
}
