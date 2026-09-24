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

// ── The block a definition names ─────────────────────────────────────────

/// The text of the block the definition `id` names in `body`.
fn block_of<'a>(body: &'a str, id: &str) -> &'a str {
    let scan = BodyScan::new(body);
    let definition = scan
        .block_ids()
        .into_iter()
        .find(|block| block.id == id)
        .unwrap_or_else(|| panic!("no definition of `{id}` in {body:?}"));
    &body[scan.block_extent(definition.span.byte_offset)]
}

/// A marker trailing a paragraph names the whole paragraph, every line of it,
/// its trailing break left out.
#[test]
fn a_block_is_the_paragraph_its_marker_trails() {
    let body = "intro\n\nfirst line\nsecond line ^para\n\nafter\n";
    assert_eq!(block_of(body, "para"), "first line\nsecond line ^para");
    assert_eq!(block_of("only ^solo", "solo"), "only ^solo");
    let quoted = "> quoted line\n> more ^q\n\nafter\n";
    assert_eq!(block_of(quoted, "q"), "quoted line\n> more ^q");
}

/// A marker in a list item names the item's own text, tight or loose, and
/// never a sibling or a nested list.
#[test]
fn a_block_in_a_list_is_the_item_text_its_marker_trails() {
    assert_eq!(
        block_of("- one\n- two ^item\n- three\n", "item"),
        "two ^item"
    );
    assert_eq!(
        block_of("- one\n\n- two ^item\n\n- three\n", "item"),
        "two ^item"
    );
    let nested = "- parent ^top\n  - child ^deep\n- sibling\n";
    assert_eq!(block_of(nested, "top"), "parent ^top");
    assert_eq!(block_of(nested, "deep"), "child ^deep");
}

/// A tight list item's own text starts at the item's first byte of text,
/// whatever inline markup opens it: emphasis, a link, a code span, in a
/// bulleted or an ordered list.
#[test]
fn a_block_in_a_list_item_opening_with_inline_markup_keeps_the_markup() {
    for (body, id, held) in [
        ("- **bold** ^b1\n", "b1", "**bold** ^b1"),
        ("- *em* ^e1\n- other\n", "e1", "*em* ^e1"),
        ("- [link](x.md) tail ^l1\n", "l1", "[link](x.md) tail ^l1"),
        ("- `code` ^c1\n", "c1", "`code` ^c1"),
        ("1. **bold** ^o1\n2. two\n", "o1", "**bold** ^o1"),
        ("- one\n  - *deep* ^d1\n", "d1", "*deep* ^d1"),
    ] {
        assert_eq!(block_of(body, id), held, "for {body:?}");
    }
}

/// The body is read as CommonMark with no extension, so a table is the
/// paragraph its lines make, and a marker ending one of its rows names that
/// paragraph: the whole table.
#[test]
fn a_marker_in_a_table_row_names_the_paragraph_the_table_is() {
    let body = "intro\n\n| a | b |\n|---|---|\n| c | d | ^t\n\nafter\n";
    assert_eq!(block_of(body, "t"), "| a | b |\n|---|---|\n| c | d | ^t");
}

/// A marker no leaf block holds — inside a raw HTML block — names the line it
/// stands on.
#[test]
fn a_marker_in_an_html_block_names_its_line() {
    assert_eq!(block_of("<div>\ntext ^hb\n</div>\n", "hb"), "text ^hb");
}

/// A marker names the leaf block it stands in, never a block around it or
/// beside it: a heading inside a tight list item is its own block, and the
/// item's text on either side of it is the item's.
#[test]
fn a_block_in_a_list_item_holding_a_heading_is_the_heading_or_the_text_beside_it() {
    let body = "- intro\n  # H ^mx\n  tail ^tl\n";
    assert_eq!(block_of(body, "mx"), "# H ^mx");
    assert_eq!(block_of(body, "tl"), "tail ^tl");
}

/// A marker opening the line after a closing fence names the fenced block
/// inside a container as outside one, the container's own prefix on its line
/// between them: a block quote's `>`, a list item's indent. A marker in
/// another container — a sibling item, a nested quote — or apart from the
/// fence by a blank line names its own text.
#[test]
fn a_marker_after_a_closing_fence_in_a_container_names_the_fenced_block() {
    for (body, id, held) in [
        ("> ```\n> x\n> ```\n> ^qf\n", "qf", "```\n> x\n> ```"),
        (
            "- item\n  ```\n  x\n  ```\n  ^lf\n",
            "lf",
            "```\n  x\n  ```",
        ),
        ("- ```\n  x\n  ```\n- ^s\n", "s", "^s"),
        ("> ```\n> x\n> ```\n> > ^nq\n", "nq", "^nq"),
        ("> ```\n> x\n> ```\n>\n> ^gap\n", "gap", "^gap"),
    ] {
        assert_eq!(block_of(body, id), held, "for {body:?}");
    }
}

/// A list item holding a fenced block answers the item's own text on either
/// side of the fence, never the fence, and never a sibling.
#[test]
fn a_list_item_holding_a_fence_answers_its_own_text() {
    let body = "- intro ^i\n  ```\n  x\n  ```\n  tail ^t\n- sib ^sib\n";
    assert_eq!(block_of(body, "i"), "intro ^i");
    assert_eq!(block_of(body, "t"), "tail ^t");
    assert_eq!(block_of(body, "sib"), "sib ^sib");
}

/// A marker on the line after a closing fence names the fenced block, fences
/// included; one anywhere else names the paragraph it is.
#[test]
fn a_marker_after_a_closing_fence_names_the_fenced_block() {
    let body = "intro\n\n```rust\nfn main() {}\n```\n^code\n\nafter\n";
    assert_eq!(block_of(body, "code"), "```rust\nfn main() {}\n```");
    let apart = "```\nx\n```\n\n^loose\n";
    assert_eq!(block_of(apart, "loose"), "^loose");
    let trailing = "```\nx\n```\ntext ^pa\n";
    assert_eq!(block_of(trailing, "pa"), "text ^pa");
    // A no-break space is text, not the whitespace a marker-only line holds.
    let nbsp = "```\nx\n```\n\u{a0}^nb\n";
    assert_eq!(block_of(nbsp, "nb"), "\u{a0}^nb");
}

/// A heading carrying a marker is its own block.
#[test]
fn a_marker_on_a_heading_names_the_heading() {
    assert_eq!(block_of("## Title ^h\nbody\n", "h"), "## Title ^h");
}

/// A body broken by lone CR names the same block its LF spelling does, at the
/// same bytes: the parse normalizes the breaks without moving an offset.
#[test]
fn a_lone_cr_body_names_the_block_its_lf_spelling_names() {
    let lf = "intro\n\nfirst\nsecond ^para\n\n- a\n- b ^item\n";
    let cr = lf.replace('\n', "\r");
    assert_eq!(block_of(&cr, "para"), "first\rsecond ^para");
    assert_eq!(block_of(&cr, "item"), "b ^item");
}
