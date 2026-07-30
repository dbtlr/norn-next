//! `#tag` syntax: what a tag is made of, where a marker counts, and the two
//! homes that share the one grammar.
//!
//! A tag is its own fact kind. It is not a link, it carries no target, and
//! nothing here decides whether a tag is *allowed* — that is validation, and
//! validation has its own venue.

use norn_text::{BodyScan, Document, Tag};

fn names(tags: &[Tag]) -> Vec<&str> {
    tags.iter().map(|tag| tag.name.as_str()).collect()
}

fn body_tags(body: &str) -> Vec<Tag> {
    BodyScan::new(body).tags()
}

// ── The name ─────────────────────────────────────────────────────────────

#[test]
fn a_tag_is_the_marker_and_the_run_that_follows_it() {
    let tags = body_tags("prose #alpha and #beta here\n");
    assert_eq!(names(&tags), ["alpha", "beta"]);
}

/// The run ends at the first character outside the set, and the characters
/// outside it are not swallowed.
#[test]
fn the_run_ends_at_the_first_character_outside_the_set() {
    for (body, expected) in [
        ("#tag.\n", "tag"),
        ("#tag, next\n", "tag"),
        ("(#tag)\n", "tag"),
        ("#tag's\n", "tag"),
        ("#tag\n", "tag"),
        ("a #tag!\n", "tag"),
        ("#tag:value\n", "tag"),
    ] {
        assert_eq!(names(&body_tags(body)), [expected], "in {body:?}");
    }
}

/// Letters, digits, `_`, `-` and `/`. The slash nests, and a nested tag is one
/// name rather than a path this crate takes apart.
#[test]
fn the_set_is_letters_digits_underscore_hyphen_and_slash() {
    assert_eq!(
        names(&body_tags("#area/project/sub\n")),
        ["area/project/sub"]
    );
    assert_eq!(
        names(&body_tags("#snake_case-kebab\n")),
        ["snake_case-kebab"]
    );
    assert_eq!(names(&body_tags("#v2\n")), ["v2"]);
}

/// Unicode letters are letters. A tag alphabet is the author's business.
#[test]
fn a_tag_may_be_written_in_any_alphabet() {
    assert_eq!(names(&body_tags("#日本語 prose\n")), ["日本語"]);
    assert_eq!(names(&body_tags("#Ünicode\n")), ["Ünicode"]);
    assert_eq!(names(&body_tags("#проект/задача\n")), ["проект/задача"]);
}

/// A combining mark is part of the letter it sits on, so the run does not stop
/// at one. Without that, a Devanagari virama ends the name halfway through the
/// word the author wrote.
#[test]
fn a_combining_mark_is_part_of_the_run() {
    assert_eq!(names(&body_tags("#हिन्दी prose\n")), ["हिन्दी"]);
    assert_eq!(names(&body_tags("#עִבְרִית\n")), ["עִבְרִית"]);
}

/// The composed and the decomposed spelling of one visible tag are one tag
/// name each, and each is the whole word. The decomposed form is what a macOS
/// filesystem and several keyboard layouts produce, so a grammar that stopped
/// at the combining accent would report `cafe` for a document that reads
/// `#café`.
#[test]
fn the_composed_and_decomposed_spellings_both_carry_the_whole_name() {
    assert_eq!(names(&body_tags("#café\n")), ["café"]);
    assert_eq!(names(&body_tags("#cafe\u{301}\n")), ["cafe\u{301}"]);
}

/// A mark sits inside a word, so a marker written after one opens nothing —
/// the same rule that makes `foo#bar` carry no tag.
#[test]
fn a_marker_after_a_combining_mark_is_inside_a_word() {
    assert!(body_tags("cafe\u{301}#tag\n").is_empty());
}

/// Numerals are numerals in every alphabet, and a run of them is still a
/// number somebody wrote down.
#[test]
fn a_run_of_unicode_numerals_is_not_a_tag() {
    assert!(body_tags("#٠١٢\n").is_empty());
    assert!(body_tags("#Ⅻ\n").is_empty());
}

/// The look-behind asks whether the previous character is part of a word, and
/// non-ASCII punctuation is not. A non-ASCII letter is.
#[test]
fn the_look_behind_reads_non_ascii_characters_by_what_they_are() {
    for body in ["«#tag»\n", "—#tag\n", "。#tag\n"] {
        assert_eq!(names(&body_tags(body)), ["tag"], "in {body:?}");
    }
    assert!(body_tags("א#tag\n").is_empty());
}

/// A zero-width joiner is a format character rather than a letter or a mark,
/// so it ends the run.
#[test]
fn a_zero_width_joiner_ends_the_run() {
    assert_eq!(names(&body_tags("#a\u{200D}b\n")), ["a"]);
}

/// The grammar's letter, characterized: `-` and `_` are in the set and neither
/// is a digit, so a run made only of them is a tag name. Nothing here judges
/// whether it is a *useful* one.
#[test]
fn a_punctuation_only_run_is_a_tag_name() {
    assert_eq!(names(&body_tags("#---\n")), ["---"]);
    assert_eq!(names(&body_tags("#_\n")), ["_"]);
}

/// At least one character must not be a digit, so a number somebody wrote down
/// stays a number. A digit beside a letter is a tag.
#[test]
fn a_run_of_digits_is_not_a_tag() {
    assert!(body_tags("#123\n").is_empty());
    assert!(body_tags("issue #42 is open\n").is_empty());
    assert_eq!(names(&body_tags("#1a\n")), ["1a"]);
    assert_eq!(names(&body_tags("#2026-07\n")), ["2026-07"]);
}

/// A marker with nothing after it is not a tag.
#[test]
fn a_bare_marker_is_not_a_tag() {
    assert!(body_tags("a # b\n").is_empty());
    assert!(body_tags("##\n").is_empty());
}

/// Case is recorded as written. Deciding that `#Work` and `#work` are the same
/// tag is a matching question, and matching happens where queries happen.
#[test]
fn case_is_recorded_as_written() {
    let tags = body_tags("#Work and #work and #WORK\n");
    assert_eq!(names(&tags), ["Work", "work", "WORK"]);
}

// ── Where a marker counts ────────────────────────────────────────────────

/// A marker opens a tag at the start of the text, after whitespace, or after a
/// character that is not part of a word. Mid-word it opens nothing.
#[test]
fn a_marker_inside_a_word_is_not_a_marker() {
    assert!(body_tags("foo#bar\n").is_empty());
    assert!(body_tags("issue1#tag\n").is_empty());
    assert!(body_tags("snake_case#tag\n").is_empty());

    for body in ["#tag\n", "a #tag\n", "(#tag)\n", "—#tag\n", "a-#tag\n"] {
        assert_eq!(names(&body_tags(body)), ["tag"], "in {body:?}");
    }
}

/// A line-leading `#tag` is a tag and not a heading: CommonMark wants a space
/// after the hashes, so the two grammars partition instead of competing.
#[test]
fn a_line_leading_tag_is_not_a_heading() {
    let scan = BodyScan::new("#tag\n\n# Heading\n");
    assert_eq!(names(&scan.tags()), ["tag"]);
    assert_eq!(scan.headings().len(), 1);
    assert_eq!(scan.headings()[0].text, "Heading");
}

/// A tag written inside a heading is still a tag — the heading is text, and
/// the marker sits in it the way it sits in a paragraph.
#[test]
fn a_tag_inside_a_heading_is_a_tag() {
    let scan = BodyScan::new("## Planning #status/active\n");
    assert_eq!(names(&scan.tags()), ["status/active"]);
    assert_eq!(scan.headings()[0].text, "Planning #status/active");
}

/// The hash in a link token is that link's fragment syntax. Reading it as a
/// marker would mint a tag out of every same-note reference in the vault.
#[test]
fn a_hash_inside_a_link_token_is_not_a_marker() {
    for body in [
        "[[#Heading]]\n",
        "[[Note#Heading]]\n",
        "![[Note#^blk]]\n",
        "[text](#frag)\n",
        "[text](./note.md#Heading)\n",
        "[[Note#Heading|#Shown]]\n",
    ] {
        assert!(body_tags(body).is_empty(), "in {body:?}");
    }

    // A tag beside a link is unaffected.
    assert_eq!(names(&body_tags("[[Note#H]] #real\n")), ["real"]);
}

/// The same rule covers every construct the parse recognizes, not only the two
/// that produce link facts. A URL fragment written in an autolink, an image,
/// a reference-style link or a definition line is that construct's syntax, and
/// reading it as a marker would mint a schema-shaped fact out of every
/// documentation URL in the vault.
#[test]
fn a_hash_inside_any_recognized_construct_is_not_a_marker() {
    for body in [
        // Autolinks, of which a `mailto:` one is still a URI autolink.
        "<https://example.com/#install>\n",
        "<mailto:person@example.com/#note>\n",
        // An image, whose destination is a destination like any other.
        "![alt](./pics/#frag)\n",
        // A definition line, which produces no parser event at all.
        "[label]: ./target/#faq\n",
        // A reference-style link and its definition, in all three written
        // forms. Each token carries the hash in its label.
        "[text][sec-#faq]\n\n[sec-#faq]: ./target/#faq\n",
        "[sec-#faq][]\n\n[sec-#faq]: ./target/#faq\n",
        "[sec-#faq]\n\n[sec-#faq]: ./target/#faq\n",
    ] {
        assert!(body_tags(body).is_empty(), "in {body:?}");
    }

    // A tag beside one of them is unaffected.
    assert_eq!(
        names(&body_tags("<https://example.com/#install> #real\n")),
        ["real"]
    );
}

/// A definition is a leaf block, so it sits wherever a leaf block sits: behind
/// a block-quote marker, inside a bulleted or numbered list item, behind both
/// at once. Each of these lines defines the label `label`, and a shape test
/// anchored at column zero read the container marker as prose and minted `faq`
/// out of the definition's own fragment.
#[test]
fn a_definition_line_inside_a_container_is_still_a_definition() {
    for body in [
        "> [label]: ./target/#faq\n",
        ">> [label]: ./target/#faq\n",
        "- [label]: ./target/#faq\n",
        "* [label]: ./target/#faq\n",
        "1. [label]: ./target/#faq\n",
        "1) [label]: ./target/#faq\n",
        "> - [label]: ./target/#faq\n",
    ] {
        assert!(body_tags(body).is_empty(), "in {body:?}");
    }

    // The marker is a marker only with whitespace behind it, and prose that
    // merely opens with a bracket is prose.
    assert_eq!(names(&body_tags("-[label] #real\n")), ["real"]);
    assert_eq!(names(&body_tags("> a note about #real\n")), ["real"]);
}

/// Raw HTML is a construct the parse recognizes and hands over whole, so what
/// is written inside it is that markup's business: an `href` fragment is a URL
/// fragment, and **a `#tag` inside an HTML comment stays commented out**. A
/// commented-out tag reading as a live one is the case that bites a real vault,
/// because commenting a line out is how an author retires it.
#[test]
fn a_hash_inside_raw_html_is_not_a_marker() {
    for body in [
        "<a href=\"https://x/#frag\">link</a>\n",
        "<img src=\"./p/#frag\">\n",
        "<!-- a comment about #tag -->\n",
        "<div>\n  <a href=\"./p/#frag\">x</a>\n</div>\n",
    ] {
        assert!(body_tags(body).is_empty(), "in {body:?}");
    }

    // A tag in the prose beside an inline HTML span is unaffected.
    assert_eq!(names(&body_tags("<b>bold</b> and #real\n")), ["real"]);
}

/// A footnote definition has the definition line's shape — `[`, a label, `]:`
/// — so the definition rule masks it too. That is over-reach in the safe
/// direction and deliberate: the rule is a line shape rather than a parser
/// event, and a tag written in a footnote's prose going unread costs a fact,
/// while a fragment read as a tag mints one.
#[test]
fn a_footnote_definition_is_masked_by_the_definition_line_rule() {
    assert!(body_tags("[^fn]: prose with #tag\n").is_empty());
    // The footnote's reference site is ordinary prose, and a tag there reads.
    assert_eq!(names(&body_tags("text[^fn] and #real\n")), ["real"]);
}

/// The stated limitation. A bare URL in prose is recognized by nothing — no
/// autolink event, no link token — so its fragment reads as a marker like any
/// other hash after a non-word character. Closing this would mean a URL
/// detector, which is a second grammar for a construct CommonMark does not
/// have.
#[test]
fn a_bare_url_in_prose_still_mints_a_tag_from_its_fragment() {
    assert_eq!(names(&body_tags("see https://x/#setup\n")), ["setup"]);
}

// ── Code is opaque ───────────────────────────────────────────────────────

#[test]
fn a_tag_inside_code_is_literal_text() {
    assert_eq!(
        names(&body_tags("before `#ignored` after #real\n")),
        ["real"]
    );
    assert_eq!(
        names(&body_tags("#real\n\n```\n#in-code\n```\n\n#real2\n")),
        ["real", "real2"]
    );
    assert_eq!(
        names(&body_tags("prose\n\n    #indented\n\nafter\n")),
        Vec::<&str>::new()
    );
}

// ── Positions ────────────────────────────────────────────────────────────

#[test]
fn a_tag_reports_where_its_marker_begins() {
    let body = "line one\nprose #alpha here\n";
    let tags = body_tags(body);
    let span = tags[0].span.expect("a body tag carries a span");
    assert_eq!(span.line, 2);
    assert_eq!(span.column, 7);
    assert_eq!(&body[span.byte_offset..span.byte_offset + 6], "#alpha");
}

/// A document reports body tags in source coordinates, so a frontmatter block
/// above them shifts every offset and neither the caller nor the crate adds
/// anything by hand.
#[test]
fn a_document_reports_body_tags_in_source_coordinates() {
    let source = "---\ntitle: Note\n---\n\n#alpha\n";
    let document = Document::parse(source);
    let tags = document.tags();
    assert_eq!(names(&tags), ["alpha"]);
    let span = tags[0].span.expect("a body tag carries a span");
    assert_eq!(span.line, 5);
    assert_eq!(&source[span.byte_offset..span.byte_offset + 6], "#alpha");
}

// ── Frontmatter: the same grammar, the marker optional ───────────────────

/// One definition of what a tag is, in both homes. In the `tags` field the
/// marker is syntax rather than part of the name, so a hand-written `#foo`
/// reads as the tag `foo` and sorts, matches and counts with the ones written
/// bare.
#[test]
fn a_frontmatter_marker_is_stripped_and_the_name_is_the_same_grammar() {
    let document = Document::parse("---\ntags:\n  - alpha\n  - \"#beta\"\n---\nbody\n");
    assert_eq!(names(&document.frontmatter_tags()), ["alpha", "beta"]);
}

/// The two homes share one character set, and one entry is one whole tag.
#[test]
fn a_frontmatter_entry_is_a_whole_tag_in_the_body_character_set() {
    let document = Document::parse(
        "---\ntags:\n  - area/project\n  - snake_case-kebab\n  - 日本語\n  - 2026-07\n---\n",
    );
    assert_eq!(
        names(&document.frontmatter_tags()),
        ["area/project", "snake_case-kebab", "日本語", "2026-07"]
    );
}

/// Where the two homes part company, and why. A body `#` opens a run that
/// *ends* at the first character outside the set, because it was written in
/// free text with prose either side of it. A frontmatter entry is a discrete
/// value that was written to be one tag, so the whole string has to be one:
/// `a.b` in a property is not the tag `a` with a stray `.b` nobody meant.
///
/// So `#a.b` in the body is the tag `a`, and `a.b` in the field is nothing at
/// all.
#[test]
fn a_frontmatter_entry_is_judged_whole_where_a_body_run_terminates() {
    for entry in ["a.b", "tag,", "tag ", " tag", "a b"] {
        let source = format!("---\ntags:\n  - \"{entry}\"\n---\n");
        let document = Document::parse(&source);
        assert!(
            document.frontmatter_tags().is_empty(),
            "frontmatter entry {entry:?}"
        );
    }

    assert_eq!(names(&body_tags("#a.b\n")), ["a"]);
    assert_eq!(names(&body_tags("#tag, next\n")), ["tag"]);
}

/// The scalar forms an older hand-written vault carries, pinned at what this
/// grammar makes of them: one string is one tag, so a string holding several
/// is not a tag. Whether a comma- or space-separated scalar should be split
/// into several tags is a schema question with its own venue; until it is
/// answered, splitting here would invent facts the field does not state.
#[test]
fn a_scalar_holding_several_names_is_not_several_tags() {
    for scalar in ["a, b", "a b", "\"#a #b\""] {
        let source = format!("---\ntags: {scalar}\n---\n");
        let document = Document::parse(&source);
        assert!(
            document.frontmatter_tags().is_empty(),
            "scalar tags field {scalar:?}"
        );
        assert!(document.diagnostics().is_empty(), "for {scalar:?}");
    }
}

/// A string the grammar does not describe is not a tag, and saying so is not
/// this crate's job: no fact, no diagnostic. Whether such a string is an error
/// belongs to validation.
#[test]
fn a_non_conforming_entry_produces_no_tag_and_no_complaint() {
    let document =
        Document::parse("---\ntags:\n  - good\n  - not a tag\n  - 123\n  - \"\"\n  - \"#\"\n---\n");
    assert_eq!(names(&document.frontmatter_tags()), ["good"]);
    assert!(document.diagnostics().is_empty());
}

/// The field's own value is read when it is a scalar string, the same way a
/// sequence's items are.
#[test]
fn a_scalar_tags_field_holds_one_tag() {
    let document = Document::parse("---\ntags: alpha\n---\n");
    assert_eq!(names(&document.frontmatter_tags()), ["alpha"]);

    let marked = Document::parse("---\ntags: \"#alpha\"\n---\n");
    assert_eq!(names(&marked.frontmatter_tags()), ["alpha"]);
}

/// Only the `tags` field is read as tags, and no frontmatter value is scanned
/// for body tokens. A `#` inside some other string is text in a string.
#[test]
fn no_other_frontmatter_value_is_scanned_for_tags() {
    let document = Document::parse(
        "---\ntitle: Notes on #alpha\nsummary: \"see #beta\"\ncategories:\n  - gamma\n---\n\
         #real\n",
    );
    assert!(document.frontmatter_tags().is_empty());
    assert_eq!(names(&document.tags()), ["real"]);
}

/// A frontmatter tag points at the entry that produced it, so a caller can
/// take a reader to the line it was written on.
#[test]
fn a_frontmatter_tag_points_at_the_entry_that_produced_it() {
    let source = "---\ntags:\n  - alpha\n  - beta\n---\n";
    let document = Document::parse(source);
    let tags = document.frontmatter_tags();
    let first = tags[0].span.expect("a block sequence item has a span");
    assert_eq!(first.line, 3);
    assert_eq!(&source[first.byte_offset..first.byte_offset + 5], "alpha");
    assert_eq!(tags[1].span.expect("a span").line, 4);
}

/// A flow sequence's items have no separately nameable bytes — the same reason
/// a flow value has no value span — so the tags are read and the spans are
/// absent rather than guessed at.
#[test]
fn a_flow_sequence_yields_tags_without_spans() {
    let document = Document::parse("---\ntags: [alpha, \"#beta\"]\n---\n");
    let tags = document.frontmatter_tags();
    assert_eq!(names(&tags), ["alpha", "beta"]);
    assert!(tags.iter().all(|tag| tag.span.is_none()));
}

/// Body tags and frontmatter tags are separate answers about separate homes,
/// and neither reaches into the other's.
#[test]
fn the_two_homes_are_asked_separately() {
    let document = Document::parse("---\ntags:\n  - declared\n---\n\nprose #written\n");
    assert_eq!(names(&document.frontmatter_tags()), ["declared"]);
    assert_eq!(names(&document.tags()), ["written"]);
}
