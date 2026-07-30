//! Headings and the sections they own: where a section starts, where it
//! stops, how it is addressed, and what a replace leaves alone.

use norn_text::{
    BodyScan, Document, EditError, SectionAddress, SectionError, Value, resolve_section, slugify,
};

const DOC: &str = "intro\n\n## Alpha\na1\na2\n\n## Beta\nb1\n";

// ── Headings ─────────────────────────────────────────────────────────────

#[test]
fn headings_carry_level_text_and_where_they_start() {
    let body = "# Title\n\n## Section\ntext\n### Sub\n";
    let scan = BodyScan::new(body);
    let seen: Vec<(u8, &str)> = scan
        .headings()
        .iter()
        .map(|heading| (heading.level, heading.text.as_str()))
        .collect();
    assert_eq!(seen, [(1, "Title"), (2, "Section"), (3, "Sub")]);
    let span = scan.headings()[1].span;
    assert!(body[span.byte_offset..].starts_with("## Section"));
    assert_eq!(span.line, 3);
    assert_eq!(span.column, 1);
}

#[test]
fn inline_markup_in_a_heading_is_flattened_into_its_text() {
    let scan = BodyScan::new("## Use `norn` *now*\n");
    assert_eq!(scan.headings()[0].text, "Use norn now");
}

#[test]
fn a_hash_inside_a_fence_is_not_a_heading() {
    let scan = BodyScan::new("## Real\n```\n## Fake\n```\n");
    assert_eq!(scan.headings().len(), 1);
    assert_eq!(scan.headings()[0].text, "Real");
}

/// The slug is ASCII-only by construction: a heading with no ASCII
/// alphanumerics slugs to nothing, and two headings differing only outside
/// ASCII slug identically. Anchors are addressed by text, not by slug, so this
/// is a property of the slug rather than of section addressing.
#[test]
fn the_slug_keeps_ascii_alphanumerics_and_collapses_everything_else() {
    assert_eq!(slugify("Hello World"), "hello-world");
    assert_eq!(slugify("HELLO   WORLD"), "hello-world");
    assert_eq!(slugify("hello!!! world!!!"), "hello-world");
    assert_eq!(slugify("Heading 1.2.3"), "heading-1-2-3");
    assert_eq!(slugify("---"), "");
    assert_eq!(slugify("Café"), "caf");
    assert_eq!(slugify("日本語"), "");
    assert_eq!(slugify("日本語"), slugify("한국어"));
}

// ── Section boundaries ───────────────────────────────────────────────────

#[test]
fn a_section_runs_from_its_heading_to_the_next_of_the_same_or_higher_level() {
    let span = resolve_section(DOC, "Alpha").expect("Alpha");
    assert_eq!(&DOC[span.heading_start..span.body_start], "## Alpha\n");
    assert_eq!(&DOC[span.body_start..span.end], "a1\na2\n\n");
}

#[test]
fn the_last_section_runs_to_the_end_of_the_body() {
    let span = resolve_section(DOC, "Beta").expect("Beta");
    assert_eq!(&DOC[span.body_start..span.end], "b1\n");
    assert_eq!(span.end, DOC.len());
}

#[test]
fn a_deeper_subsection_belongs_to_its_parent() {
    let body = "## Parent\np\n### Child\nc\n## Sibling\ns\n";
    let span = resolve_section(body, "Parent").expect("Parent");
    assert_eq!(&body[span.body_start..span.end], "p\n### Child\nc\n");
}

#[test]
fn a_missing_heading_refuses_by_name() {
    assert_eq!(
        resolve_section(DOC, "Gamma"),
        Err(SectionError::HeadingNotFound {
            heading: "Gamma".into()
        })
    );
}

/// NRN-445: a duplicate heading refuses, which is right — an ambiguous address
/// is a question, not an edit.
#[test]
fn a_duplicate_heading_refuses_the_bare_address() {
    let body = "## Dup\nx\n## Dup\ny\n";
    assert_eq!(
        resolve_section(body, "Dup"),
        Err(SectionError::HeadingAmbiguous {
            heading: "Dup".into(),
            count: 2
        })
    );
}

/// NRN-445's actual defect: with no way to say *which* one, a document that
/// grew a duplicate heading could not be repaired by the tool that reads it.
/// Occurrence addressing is the escape hatch; the bare refusal stays the
/// default.
#[test]
fn an_occurrence_addresses_one_of_several_headings_with_the_same_text() {
    let body = "## Dup\nfirst\n## Dup\nsecond\n";
    let first = resolve_section(body, SectionAddress::occurrence("Dup", 1)).expect("the first");
    let second = resolve_section(body, SectionAddress::occurrence("Dup", 2)).expect("the second");
    assert_eq!(&body[first.content_start..first.content_end], "first\n");
    assert_eq!(&body[second.content_start..second.content_end], "second\n");

    for occurrence in [0, 3] {
        assert_eq!(
            resolve_section(body, SectionAddress::occurrence("Dup", occurrence)),
            Err(SectionError::OccurrenceOutOfRange {
                heading: "Dup".into(),
                occurrence,
                count: 2
            })
        );
    }
    // A heading that is not there is not there, whatever occurrence is asked
    // for.
    assert_eq!(
        resolve_section(body, SectionAddress::occurrence("Gamma", 1)),
        Err(SectionError::HeadingNotFound {
            heading: "Gamma".into()
        })
    );
    // An occurrence of a unique heading is the heading.
    assert_eq!(
        resolve_section(DOC, SectionAddress::occurrence("Alpha", 1)),
        resolve_section(DOC, "Alpha")
    );
}

#[test]
fn a_heading_inside_a_fence_addresses_nothing_and_belongs_to_its_owner() {
    let body = "## Real\n```\n## Fake\n```\nbody\n";
    assert_eq!(
        resolve_section(body, "Fake"),
        Err(SectionError::HeadingNotFound {
            heading: "Fake".into()
        })
    );
    assert_eq!(resolve_section(body, "Real").expect("Real").end, body.len());
}

// ── Setext, and a heading at end of file (NRN-437) ───────────────────────

/// NRN-437: `body_start` was "the byte after the first newline", which landed
/// on a setext underline. A section read surfaced the underline, and a section
/// write ate it and demoted the heading to a paragraph.
#[test]
fn a_setext_headings_underline_belongs_to_the_heading() {
    let body = "Alpha\n-----\nbody under alpha.\n\n## Beta\nb\n";
    let span = resolve_section(body, "Alpha").expect("Alpha");
    assert_eq!(&body[span.heading_start..span.body_start], "Alpha\n-----\n");
    assert_eq!(&body[span.body_start..span.end], "body under alpha.\n\n");
    // The whole-section read is lossless.
    assert_eq!(
        &body[span.heading_start..span.end],
        "Alpha\n-----\nbody under alpha.\n\n"
    );
}

/// A setext title may run across several lines. The break between them
/// separates two words, so the text — and the slug, and the address — reads as
/// the heading a human sees rather than as the two words run together.
#[test]
fn a_setext_title_spanning_lines_reads_as_one_spaced_heading() {
    let body = "foo\nbar\n===\n\nunder it\n";
    let scan = BodyScan::new(body);
    assert_eq!(scan.headings()[0].text, "foo bar");
    assert_eq!(scan.headings()[0].slug, "foo-bar");
    let span = resolve_section(body, "foo bar").expect("the heading resolves by its text");
    assert_eq!(&body[span.content_start..span.content_end], "under it\n");
    // A hard break — two trailing spaces — separates words the same way.
    assert_eq!(
        BodyScan::new("foo  \nbar\n===\n").headings()[0].text,
        "foo bar"
    );
}

#[test]
fn two_setext_headings_in_a_row_leave_the_first_with_no_body() {
    let body = "Alpha\n=====\nBeta\n=====\nb\n";
    let span = resolve_section(body, "Alpha").expect("Alpha");
    assert_eq!(&body[span.heading_start..span.body_start], "Alpha\n=====\n");
    assert_eq!(&body[span.body_start..span.end], "");
}

/// NRN-437: a heading at end of file with no trailing newline had content
/// welded onto it. It reads as an empty section instead, and a replace opens
/// the line it needs.
#[test]
fn a_heading_at_end_of_file_without_a_newline_reads_as_an_empty_section() {
    let body = "intro\n\n## Tail";
    let span = resolve_section(body, "Tail").expect("Tail");
    assert_eq!(span.body_start, body.len());
    assert_eq!(span.end, body.len());
    assert_eq!(&body[span.heading_start..span.end], "## Tail");

    let source = "---\ntitle: t\n---\nintro\n\n## Tail";
    assert_eq!(
        Document::parse(source).replace_section("Tail", "written"),
        Ok("---\ntitle: t\n---\nintro\n\n## Tail\nwritten\n".to_string())
    );
}

// ── ATX-prefixed anchors (NRN-164) ───────────────────────────────────────

/// Agents write `## State` where the anchor is `State`, constantly. The
/// resolver tries the anchor verbatim first and only falls back to stripping
/// an ATX prefix on a total miss, so no working anchor is ever reinterpreted.
#[test]
fn an_atx_prefixed_anchor_resolves_by_its_text() {
    assert_eq!(
        resolve_section(DOC, "## Alpha"),
        resolve_section(DOC, "Alpha")
    );
    // A trailing closer is stripped the same way a real heading's is.
    assert_eq!(
        resolve_section(DOC, "## Alpha ##"),
        resolve_section(DOC, "Alpha")
    );
}

#[test]
fn the_level_of_an_atx_anchor_is_syntax_noise() {
    let body = "### Deep\nbody\n";
    assert_eq!(
        resolve_section(body, "## Deep"),
        resolve_section(body, "Deep")
    );
    assert_eq!(
        resolve_section(DOC, "#### Alpha"),
        resolve_section(DOC, "Alpha")
    );
    // Crossing styles too: a `## X` anchor resolves a setext `X` heading.
    let setext = "Overview\n========\nbody\n";
    assert_eq!(
        resolve_section(setext, "## Overview"),
        resolve_section(setext, "Overview")
    );
}

#[test]
fn seven_hashes_is_not_an_atx_opening_and_is_not_stripped() {
    let body = "## Edge\nbody\n";
    assert_eq!(
        resolve_section(body, "###### Edge"),
        resolve_section(body, "Edge")
    );
    assert_eq!(
        resolve_section(body, "####### Edge"),
        Err(SectionError::HeadingNotFound {
            heading: "####### Edge".into()
        })
    );
}

#[test]
fn a_hash_run_with_no_space_after_it_is_not_an_atx_opening() {
    assert_eq!(
        resolve_section(DOC, "#Alpha"),
        Err(SectionError::HeadingNotFound {
            heading: "#Alpha".into()
        })
    );
}

#[test]
fn a_degenerate_atx_anchor_refuses_rather_than_matching_anything() {
    let body = "## Alpha\nbody\n";
    assert!(resolve_section(body, "## ").is_err());
    assert!(resolve_section(body, "##").is_err());
}

/// An anchor carrying a line break is not one heading. Reading the first
/// heading out of it answers a question about `## Alpha` when the caller asked
/// about two lines, and lands on a section nothing in the result names.
#[test]
fn an_anchor_carrying_a_line_break_matches_nothing() {
    let body = "## Alpha\na\n\n## Beta\nb\n";
    for anchor in ["## Alpha\n## Beta", "## Alpha\ntrailing", "## Alpha\r\n"] {
        assert_eq!(
            resolve_section(body, anchor),
            Err(SectionError::HeadingNotFound {
                heading: anchor.to_string()
            }),
            "for {anchor:?}"
        );
    }
}

/// The ambiguity guard: a document holding both a heading whose text is
/// literally `## Verbatim` and a different heading `Verbatim`. Exact-first is
/// what stops the anchor landing on the wrong section.
#[test]
fn an_exact_match_wins_over_the_forgiving_strip() {
    let body = "#### ## Verbatim\nright body\n\n## Verbatim\nwrong body\n";
    let exact = resolve_section(body, "## Verbatim").expect("the verbatim heading");
    assert_eq!(
        &body[exact.content_start..exact.content_end],
        "right body\n"
    );
    let other = resolve_section(body, "Verbatim").expect("the other heading");
    assert_eq!(
        &body[other.content_start..other.content_end],
        "wrong body\n"
    );
}

#[test]
fn a_miss_reports_the_anchor_as_it_was_given() {
    assert_eq!(
        resolve_section(DOC, "## Gamma"),
        Err(SectionError::HeadingNotFound {
            heading: "## Gamma".into()
        })
    );
}

// ── Separator-aware content, and replacing it (NRN-166) ──────────────────

/// NRN-166: a section replace collapsed `## Alpha\n\nbody\n\n## Beta` into
/// `## Alpha\nnew\n## Beta`, so every edit rewrote the document's blank
/// structure and no edit was idempotent. The blank lines around a heading are
/// separators, and the content range excludes them.
#[test]
fn the_content_range_excludes_the_blank_lines_around_a_heading() {
    let body = "## Alpha\n\nbody\n\n## Beta\nb\n";
    let span = resolve_section(body, "Alpha").expect("Alpha");
    assert_eq!(&body[span.body_start..span.end], "\nbody\n\n");
    assert_eq!(&body[span.content_start..span.content_end], "body\n");
}

#[test]
fn replacing_a_section_leaves_the_blank_lines_around_its_heading() {
    let source = "---\ntitle: t\n---\n## Alpha\n\nbody\n\n## Beta\nb\n";
    assert_eq!(
        Document::parse(source).replace_section("Alpha", "new body"),
        Ok("---\ntitle: t\n---\n## Alpha\n\nnew body\n\n## Beta\nb\n".to_string())
    );
}

#[test]
fn replacing_a_section_twice_produces_the_same_document() {
    let source = "---\ntitle: t\n---\n## Alpha\n\nbody\n\n## Beta\nb\n";
    let once = Document::parse(source)
        .replace_section("Alpha", "new body")
        .expect("once");
    let twice = Document::parse(&once)
        .replace_section("Alpha", "new body")
        .expect("twice");
    assert_eq!(once, twice);
}

#[test]
fn a_section_with_no_separators_gains_none() {
    let source = "---\ntitle: t\n---\n## Alpha\nbody\n## Beta\nb\n";
    assert_eq!(
        Document::parse(source).replace_section("Alpha", "new"),
        Ok("---\ntitle: t\n---\n## Alpha\nnew\n## Beta\nb\n".to_string())
    );
}

#[test]
fn a_multi_line_replacement_keeps_its_own_line_structure() {
    let source = "## Alpha\n\nbody\n\n## Beta\n";
    assert_eq!(
        Document::parse(source).replace_section("Alpha", "one\n\ntwo\n"),
        Ok("## Alpha\n\none\n\ntwo\n\n## Beta\n".to_string())
    );
}

#[test]
fn an_empty_replacement_empties_the_section_and_keeps_its_heading() {
    let source = "## Alpha\n\nbody\n\n## Beta\nb\n";
    assert_eq!(
        Document::parse(source).replace_section("Alpha", ""),
        Ok("## Alpha\n\n\n## Beta\nb\n".to_string())
    );
}

/// An empty section's content range collapses onto the next heading, so the
/// separator the section had sits entirely above the splice. Writing into it
/// restores one below, or the written content jams against the heading beneath
/// it — and where that heading is setext, is absorbed into it.
#[test]
fn writing_into_an_empty_section_lands_between_its_separators() {
    let source = "## Alpha\n\n\n## Beta\nb\n";
    assert_eq!(
        Document::parse(source).replace_section("Alpha", "new"),
        Ok("## Alpha\n\n\nnew\n\n## Beta\nb\n".to_string())
    );
    // A section with no separators to mirror gains none, and an empty section
    // at the end of the body has no heading below to separate from.
    assert_eq!(
        Document::parse("## Alpha\n## Beta\nb\n").replace_section("Alpha", "new"),
        Ok("## Alpha\nnew\n## Beta\nb\n".to_string())
    );
    assert_eq!(
        Document::parse("## Alpha\n\n").replace_section("Alpha", "new"),
        Ok("## Alpha\n\nnew\n".to_string())
    );
}

/// The heading below an empty section is setext here, so content written
/// without a separator would become the first line of its title and delete the
/// heading. The separator is what keeps the write honest; the post-image check
/// is what would refuse it otherwise.
#[test]
fn writing_into_an_empty_section_above_a_setext_heading_keeps_that_heading() {
    let source = "## Alpha\n\n\nBeta\n====\nb\n";
    let edited = Document::parse(source)
        .replace_section("Alpha", "new")
        .expect("a replacement");
    assert_eq!(edited, "## Alpha\n\n\nnew\n\nBeta\n====\nb\n");
    let headings = BodyScan::new(&edited);
    assert_eq!(
        headings
            .headings()
            .iter()
            .map(|heading| heading.text.as_str())
            .collect::<Vec<_>>(),
        ["Alpha", "Beta"]
    );
}

#[test]
fn replacing_a_section_by_occurrence_touches_only_that_one() {
    let source = "## Dup\nfirst\n## Dup\nsecond\n";
    assert_eq!(
        Document::parse(source).replace_section(SectionAddress::occurrence("Dup", 2), "changed"),
        Ok("## Dup\nfirst\n## Dup\nchanged\n".to_string())
    );
}

#[test]
fn replacing_an_ambiguous_section_refuses() {
    let source = "## Dup\nfirst\n## Dup\nsecond\n";
    assert!(
        Document::parse(source)
            .replace_section("Dup", "changed")
            .is_err()
    );
}

// ── A replace is proven or refused (NORN-25) ─────────────────────────────

/// Section content is arbitrary Markdown, and Markdown is not inert. An
/// unclosed fence swallows every heading below it, an indented block makes
/// them literal text, and a line of `=` turns the heading beneath it into its
/// own title. None of these move a byte the splice did not address, so only
/// re-reading the result catches them.
#[test]
fn content_that_swallows_the_document_below_it_refuses() {
    let source = "---\ntitle: t\n---\n## Alpha\n\nbody\n\n## Beta\nb\n\n## Gamma\ng\n";
    for content in ["```", "```rust\nfn main() {}", "~~~"] {
        assert_eq!(
            Document::parse(source).replace_section("Alpha", content),
            Err(EditError::SectionPostImageMismatch {
                heading: "Alpha".to_string()
            }),
            "for content {content:?}"
        );
    }
    // A closed fence is ordinary content and is written.
    assert_eq!(
        Document::parse(source).replace_section("Alpha", "```\nsample\n```"),
        Ok(
            "---\ntitle: t\n---\n## Alpha\n\n```\nsample\n```\n\n## Beta\nb\n\n## Gamma\ng\n"
                .to_string()
        )
    );
}

/// Content that restates the addressed heading makes the address ambiguous, so
/// there is no longer one section to have written — and the replace refuses
/// rather than reporting a success nothing can re-address. Occurrence
/// addressing is the way through.
#[test]
fn content_restating_the_heading_refuses_and_an_occurrence_gets_through() {
    let source = "## Alpha\n\nbody\n\n## Beta\nb\n";
    for content in ["## Alpha\n\nnested", "### Alpha\n\nnested"] {
        assert_eq!(
            Document::parse(source).replace_section("Alpha", content),
            Err(EditError::SectionPostImageMismatch {
                heading: "Alpha".to_string()
            }),
            "for content {content:?}"
        );
    }
    // A deeper restatement nests inside the section rather than splitting it,
    // so naming the occurrence resolves the ambiguity and the write goes
    // through.
    assert_eq!(
        Document::parse(source).replace_section(
            SectionAddress::occurrence("Alpha", 1),
            "### Alpha\n\nnested"
        ),
        Ok("## Alpha\n\n### Alpha\n\nnested\n\n## Beta\nb\n".to_string())
    );
    // A restatement at the same level splits the section in two, so the
    // content the address was given is not what any one section came to hold,
    // and even the occurrence refuses.
    assert_eq!(
        Document::parse(source)
            .replace_section(SectionAddress::occurrence("Alpha", 1), "## Alpha\n\nnested"),
        Err(EditError::SectionPostImageMismatch {
            heading: "Alpha".to_string()
        })
    );
}

/// A heading inside a blockquote or a list item owns no byte range of its own:
/// the bytes below it are the container's, and splicing over them lifts them
/// out of it. The refusal is early and typed — container-aware splicing is not
/// something this crate does.
#[test]
fn a_heading_inside_a_container_refuses_a_replace() {
    for (source, heading) in [
        ("> ## Quoted\n> body\n\nafter\n", "Quoted"),
        ("- ## Listed\n  body\n\nafter\n", "Listed"),
    ] {
        assert_eq!(
            Document::parse(source).replace_section(heading, "new"),
            Err(EditError::SectionInContainer {
                heading: heading.to_string()
            }),
            "for {source:?}"
        );
        // Reading one is fine; only replacing it is refused.
        assert!(
            BodyScan::new(source)
                .resolve_section(heading.into())
                .is_ok()
        );
    }
}

/// Every line a replace writes uses the document's terminator, `content`'s own
/// lines included. Splicing content verbatim is how a CRLF document ends up
/// with LF lines through the middle of it.
#[test]
fn a_multi_line_replacement_into_a_crlf_document_is_all_crlf() {
    let source = "---\r\ntitle: t\r\n---\r\n## Alpha\r\n\r\nold\r\n\r\n## Beta\r\nb\r\n";
    let edited = Document::parse(source)
        .replace_section("Alpha", "one\ntwo\n\nthree")
        .expect("a replacement");
    assert_eq!(
        edited,
        "---\r\ntitle: t\r\n---\r\n## Alpha\r\n\r\none\r\ntwo\r\n\r\nthree\r\n\r\n## Beta\r\nb\r\n"
    );
    assert_eq!(edited.matches('\n').count(), edited.matches("\r\n").count());
    // And an LF document given CRLF content comes back all LF.
    let lf = "## Alpha\n\nold\n\n## Beta\n";
    assert_eq!(
        Document::parse(lf).replace_section("Alpha", "one\r\ntwo"),
        Ok("## Alpha\n\none\ntwo\n\n## Beta\n".to_string())
    );
}

// ── Section spans against a whole document ───────────────────────────────

#[test]
fn a_documents_section_span_is_in_source_coordinates() {
    let source = "---\ntitle: t\n---\n## Alpha\nbody\n";
    let document = Document::parse(source);
    let span = document.resolve_section("Alpha").expect("Alpha");
    assert_eq!(&source[span.heading_start..span.body_start], "## Alpha\n");
    assert_eq!(&source[span.content_start..span.content_end], "body\n");
    // The body scan's own coordinates are relative to the body.
    let relative = document
        .scan_body()
        .resolve_section("Alpha".into())
        .expect("Alpha");
    assert_eq!(
        relative.heading_start + document.body_start(),
        span.heading_start
    );
}

#[test]
fn replacing_a_section_leaves_the_frontmatter_untouched() {
    let source = "---\ntitle: t\ntags:\n  - a\n---\n## Alpha\nbody\n";
    let edited = Document::parse(source)
        .replace_section("Alpha", "new")
        .expect("a replacement");
    assert_eq!(
        Document::parse(&edited).frontmatter(),
        Document::parse(source).frontmatter()
    );
    let body_start = Document::parse(source).body_start();
    assert_eq!(&edited[..body_start], &source[..body_start]);
    assert_eq!(
        Document::parse(&edited)
            .frontmatter()
            .and_then(Value::as_map)
            .map(|map| map.keys().collect::<Vec<_>>()),
        Some(vec!["title", "tags"])
    );
}
