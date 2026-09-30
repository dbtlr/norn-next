//! Headings and the sections they own: where a section starts, where it
//! stops, how it is addressed, and what a replace leaves alone.

use norn_text::{
    AnchorReadings, BodyScan, Document, EditError, Heading, SectionAddress, SectionError,
    SectionSpan, Value, anchor_readings, heading_reading, resolve_section as resolve_section_over,
    slugify,
};

/// A section resolved against a bare body, through the one public entry point
/// a body-level question goes through.
fn resolve_section<'a>(
    body: &str,
    address: impl Into<SectionAddress<'a>>,
) -> Result<SectionSpan, SectionError> {
    BodyScan::new(body).resolve_section(address.into())
}

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

/// **A heading on the first line of a document that opens with a byte-order
/// mark is a heading**, and a write over its section leaves the mark the
/// document's first bytes.
#[test]
fn a_heading_right_after_a_byte_order_mark_is_a_heading() {
    let source = "\u{feff}# A\ntext\n\n# B\nb\n";
    let document = Document::parse(source);
    assert_eq!(document.headings()[0].text, "A");
    assert_eq!(
        document.replace_section("A", "new"),
        Ok("\u{feff}# A\nnew\n\n# B\nb\n".to_string())
    );
    assert_eq!(
        document.delete_section("A"),
        Ok("\u{feff}# B\nb\n".to_string())
    );
    assert_eq!(
        document.insert_before_heading("A", "lead"),
        Ok("\u{feff}lead\n# A\ntext\n\n# B\nb\n".to_string())
    );
}

/// **An ATX closing sequence followed by a tab is a closing sequence**, as one
/// followed by a space is: CommonMark lets either trail it, so `## A ##\t`
/// is the heading `A`.
#[test]
fn a_closing_sequence_followed_by_a_tab_is_not_heading_text() {
    for source in ["## A ##\t\nb\n", "## A ## \t \nb\n", "## A\t#\t\nb\n"] {
        let scan = BodyScan::new(source);
        assert_eq!(scan.headings()[0].text, "A", "for {source:?}");
        assert_eq!(
            Document::parse(source).replace_section("A", "new"),
            Ok(format!(
                "{}new\n",
                &source[..source.find('\n').unwrap() + 1]
            )),
            "for {source:?}"
        );
    }
    // A hash that is not preceded by space or tab is the heading's text.
    assert_eq!(BodyScan::new("## C#\t\n").headings()[0].text, "C#");
    assert_eq!(BodyScan::new("## A \\##\t\n").headings()[0].text, "A ##");
}

#[test]
fn a_hash_inside_a_fence_is_not_a_heading() {
    let scan = BodyScan::new("## Real\n```\n## Fake\n```\n");
    assert_eq!(scan.headings().len(), 1);
    assert_eq!(scan.headings()[0].text, "Real");
}

/// The slug is GFM's: letters and digits survive whatever alphabet they are
/// written in, punctuation is dropped rather than collapsed, and a space is a
/// hyphen. A section anchor matches a slug only where no heading's text
/// matches it, so this is a property of the slug alone.
#[test]
fn the_slug_is_the_gfm_anchor_form() {
    assert_eq!(slugify("Hello World"), "hello-world");
    assert_eq!(slugify("hello!!! world!!!"), "hello-world");
    assert_eq!(slugify("Heading 1.2.3"), "heading-123");
    assert_eq!(slugify("---"), "---");
    assert_eq!(slugify("Café"), "café");
    assert_eq!(slugify("日本語"), "日本語");
    assert_ne!(slugify("日本語"), slugify("한국어"));

    // Runs are not collapsed, because GFM does not collapse them.
    assert_eq!(slugify("HELLO   WORLD"), "hello---world");
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

/// **A section ends at the start of the line holding the heading that ends
/// it**, so where that heading sits inside a container the container's prefix
/// on its line — a `>`, a list marker — belongs to the next section, and a
/// read answers no byte of it.
#[test]
fn a_section_ended_by_a_heading_inside_a_container_ends_at_its_line() {
    for (body, owned) in [
        ("## A\ntext\n> ## Q\nquoted\n\nafter\n## B\nb\n", "text\n"),
        ("## A\ntext\n- # L\n  item\n## B\nb\n", "text\n"),
        ("## A\ntext\n1. > ## N\n## B\nb\n", "text\n"),
        ("## A\r\rtext\r> ## Q\rquoted\r", "\rtext\r"),
    ] {
        let span = resolve_section(body, "A").expect("A");
        assert_eq!(&body[span.body_start..span.end], owned, "for {body:?}");
    }
}

/// **A write over a section ended by a heading inside a container leaves the
/// container standing**: its prefix on the heading's line is the next
/// section's, so the splice writes over none of it.
#[test]
fn a_write_over_a_section_ended_inside_a_container_keeps_the_container() {
    for (source, edited) in [
        (
            "## A\ntext\n> ## Q\nquoted\n",
            "## A\nnew\n> ## Q\nquoted\n",
        ),
        ("## A\ntext\n- # L\n  item\n", "## A\nnew\n- # L\n  item\n"),
    ] {
        let written = Document::parse(source)
            .replace_section("A", "new\n")
            .expect("a replacement");
        assert_eq!(written, edited, "for {source:?}");
        let reread = BodyScan::new(&written);
        assert!(
            reread
                .headings()
                .iter()
                .all(|heading| heading.text == "A" || heading.inside_container),
            "the container around the next heading survives: {written:?}"
        );
    }
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

/// An occurrence counts matches by text, and text is all it counts by: two
/// headings with the same words at different levels are occurrence 1 and 2 in
/// document order. Level is not part of the address, so it does not narrow the
/// count — the same reason a `## X` anchor resolves a `### X` heading.
#[test]
fn an_occurrence_counts_across_levels_in_document_order() {
    let body = "## Notes\nshallow\n\n### Notes\ndeep\n\n# Notes\ntop\n";
    assert_eq!(
        resolve_section(body, "Notes"),
        Err(SectionError::HeadingAmbiguous {
            heading: "Notes".into(),
            count: 3
        })
    );
    // What each one owns still follows from its own level, so the first — a
    // `## ` heading — owns the `### ` one counted after it.
    for (occurrence, content) in [
        (1, "shallow\n\n### Notes\ndeep\n"),
        (2, "deep\n"),
        (3, "top\n"),
    ] {
        let span = resolve_section(body, SectionAddress::occurrence("Notes", occurrence))
            .unwrap_or_else(|error| panic!("occurrence {occurrence}: {error}"));
        assert_eq!(
            &body[span.content_start..span.content_end],
            content,
            "occurrence {occurrence}"
        );
    }
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

// ── How an anchor matches a heading ──────────────────────────────────────

/// A heading anchor matches the heading's text with surrounding whitespace
/// trimmed, each whitespace run collapsed to one space, and ASCII case folded,
/// on both sides, so an anchor typed from memory reaches the heading it names.
#[test]
fn an_anchor_matches_heading_text_with_case_and_whitespace_folded() {
    let body = "## Design  Notes\nd\n## Other\no\n";
    let named = resolve_section(body, "Design  Notes").expect("the heading as written");
    assert_eq!(&body[named.content_start..named.content_end], "d\n");
    for anchor in ["design notes", "  DESIGN   notes ", "Design\tNotes"] {
        assert_eq!(resolve_section(body, anchor), Ok(named), "for {anchor:?}");
    }
}

/// The case fold is ASCII's alone: a letter outside ASCII keeps its case.
/// The heading's slug is lowercase whatever alphabet it is written in, so the
/// all-lowercase spelling still reaches it, as a fragment.
#[test]
fn the_case_fold_is_ascii_alone() {
    let body = "## Émile\ne\n";
    for anchor in ["Émile", "ÉMILE", "  émile  ".trim()] {
        assert!(resolve_section(body, anchor).is_ok(), "for {anchor:?}");
    }
    assert_eq!(
        resolve_section(body, "éMILE"),
        Err(SectionError::HeadingNotFound {
            heading: "éMILE".into()
        })
    );
}

/// The whitespace fold is ASCII's alone, as the case fold is: a no-break space
/// or an ideographic space is text, so a heading carrying one and a heading
/// spelled without it are two headings, and a write reaches the exact one.
#[test]
fn the_whitespace_fold_is_ascii_alone() {
    let body = "## Intro\u{a0}\nspaced\n## Intro\nplain\n";
    let plain = resolve_section(body, "Intro").expect("the plain heading alone");
    assert_eq!(&body[plain.content_start..plain.content_end], "plain\n");
    let spaced = resolve_section(body, "Intro\u{a0}").expect("the spaced heading alone");
    assert_eq!(&body[spaced.content_start..spaced.content_end], "spaced\n");
    assert_eq!(
        resolve_section(body, " \tINTRO\u{b}\u{c}\r"),
        Ok(plain),
        "every ASCII space folds"
    );
    assert_eq!(
        Document::parse(body).replace_section("Intro", "new\n"),
        Ok("## Intro\u{a0}\nspaced\n## Intro\nnew\n".to_string())
    );

    let body = "## a\u{3000}b\nwide\n## a b\nnarrow\n";
    let narrow = resolve_section(body, "a  b").expect("the narrow heading alone");
    assert_eq!(&body[narrow.content_start..narrow.content_end], "narrow\n");
    let wide = resolve_section(body, "a\u{3000}b").expect("the wide heading alone");
    assert_eq!(&body[wide.content_start..wide.content_end], "wide\n");
}

/// A Markdown `#fragment` names a heading's slug, dedupe suffix included, and
/// it is matched exactly, only where no heading's text matches the anchor.
#[test]
fn a_slug_matches_only_where_no_heading_text_does() {
    let body = "## What? Really!\nw\n## Dup\nfirst\n## Dup\nsecond\n";
    let by_text = resolve_section(body, "What? Really!").expect("the heading by its text");
    assert_eq!(resolve_section(body, "what-really"), Ok(by_text));
    let second = resolve_section(body, "dup-1").expect("the second heading by its slug");
    assert_eq!(&body[second.content_start..second.content_end], "second\n");
    assert_eq!(
        resolve_section(body, "What-Really"),
        Err(SectionError::HeadingNotFound {
            heading: "What-Really".into()
        }),
        "a slug is matched exactly"
    );

    // One heading's text is another's slug: the text answers.
    let body = "## Foo Bar\nslugged\n## foo-bar\ntexted\n";
    let span = resolve_section(body, "foo-bar").expect("the heading whose text it is");
    assert_eq!(&body[span.content_start..span.content_end], "texted\n");
}

/// **A resolved section names the heading it matched** by its index among the
/// headings resolved over, whichever reading matched it, so a caller holding
/// those headings reads the matched one without finding it again.
#[test]
fn a_resolved_section_names_the_heading_it_matched_by_index() {
    let body = "intro\n## Dup\nfirst\n### Deep\nd\n## dup\nsecond\n";
    let scan = BodyScan::new(body);
    for (address, index) in [
        (SectionAddress::first("dup"), 0),
        (SectionAddress::occurrence("DUP", 2), 2),
        (SectionAddress::from("## Deep"), 1),
        (SectionAddress::from("dup-1"), 2),
    ] {
        let span = scan.resolve_section(address).expect("a section");
        assert_eq!(span.heading, index, "for {address:?}");
        assert_eq!(
            scan.headings()[span.heading].span.byte_offset,
            span.heading_start,
            "for {address:?}"
        );
    }
}

/// **A read takes the first matching heading in document order; a write
/// refuses several.** The duplicate policy is the caller's, and it is the only
/// thing a read and a write resolve differently.
#[test]
fn a_read_takes_the_first_matching_heading_and_a_write_refuses_several() {
    let body = "## Dup\nfirst\n## dup\nsecond\n";
    assert_eq!(
        resolve_section(body, "DUP"),
        Err(SectionError::HeadingAmbiguous {
            heading: "DUP".into(),
            count: 2
        })
    );
    let first = resolve_section(body, SectionAddress::first("DUP")).expect("the first");
    assert_eq!(&body[first.content_start..first.content_end], "first\n");
    assert_eq!(
        resolve_section(DOC, SectionAddress::first("Alpha")),
        resolve_section(DOC, "Alpha")
    );
    assert_eq!(
        resolve_section(DOC, SectionAddress::first("Gamma")),
        Err(SectionError::HeadingNotFound {
            heading: "Gamma".into()
        })
    );
    assert!(
        Document::parse(body).replace_section("dup", "x").is_err(),
        "a write refuses headings its anchor matches twice"
    );
}

/// **A section resolves over heading facts exactly as it resolves over a
/// scan**, so a caller holding a document's headings as rows resolves through
/// the one resolver rather than growing a second.
#[test]
fn a_section_resolves_over_heading_facts_as_over_a_scan() {
    let body = "intro\n## Alpha\na\n### Deep\nd\n## Beta\nb\n## beta\nc\n";
    let scan = BodyScan::new(body);
    let facts: Vec<Heading> = scan.headings().to_vec();
    for anchor in ["Alpha", "deep", "BETA", "## Alpha", "beta-1", "Gamma"] {
        for address in [SectionAddress::from(anchor), SectionAddress::first(anchor)] {
            assert_eq!(
                resolve_section_over(&facts, body, address),
                scan.resolve_section(address),
                "for {address:?}"
            );
        }
    }
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
    // A tab after the opening is an ATX opening, as it is for a heading.
    assert_eq!(
        resolve_section(DOC, "#\tAlpha"),
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

/// A section owns its subsections, so replacing its content removes them. That
/// is what the address means, not structure lost to the content — so the
/// survival check asks only about headings outside the range the splice
/// overwrote.
#[test]
fn replacing_a_section_that_owns_subsections_removes_them() {
    assert_eq!(
        Document::parse("# Top\n\nold\n\n## Sub\n\nx\n").replace_section("Top", "new"),
        Ok("# Top\n\nnew\n".to_string())
    );

    // A sibling below the section is outside that range, so it still has to
    // survive — and the same replace behind an unclosed fence, which swallows
    // it, refuses.
    let with_sibling = "# Top\n\nold\n\n## Sub\n\nx\n\n# Next\n\ny\n";
    assert_eq!(
        Document::parse(with_sibling).replace_section("Top", "new"),
        Ok("# Top\n\nnew\n\n# Next\n\ny\n".to_string())
    );
    assert_eq!(
        Document::parse(with_sibling).replace_section("Top", "```"),
        Err(EditError::SectionPostImageMismatch {
            heading: "Top".to_string()
        })
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

// ── The readings an anchor and a heading are compared by ─────────────────

/// **A heading's reading is its text as an anchor compares it**: trimmed, each
/// run of ASCII space one space, ASCII case folded, and nothing outside ASCII
/// touched.
#[test]
fn a_headings_reading_folds_ascii_case_and_space_alone() {
    assert_eq!(heading_reading("  Design \t Notes "), "design notes");
    assert_eq!(heading_reading("ÉMILE"), "Émile".to_ascii_lowercase());
    assert_eq!(heading_reading("Intro\u{a0}"), "intro\u{a0}");
    assert_eq!(heading_reading(""), "");
}

/// **An anchor's readings are its text as a heading reading and the heading
/// text past its `#` markers**; its slug reading is the anchor itself.
#[test]
fn an_anchor_is_read_as_its_text_and_past_its_markers() {
    let readings = |anchor: &str| anchor_readings(anchor).expect("an anchor");
    assert_eq!(
        readings("Design  NOTES"),
        AnchorReadings {
            text: "design notes".into(),
            marked: None,
        }
    );
    assert_eq!(
        readings("## State ##"),
        AnchorReadings {
            text: "## state ##".into(),
            marked: Some("state".into()),
        },
        "an ATX anchor is marked by its opening"
    );
    assert_eq!(
        readings("Top#Sub Part"),
        AnchorReadings {
            text: "top#sub part".into(),
            marked: Some("sub part".into()),
        },
        "a heading chain is marked by its last heading"
    );
}

/// **An empty anchor reads as no anchor.** `[[note#]]` names the note, and so
/// does a target written `note#`.
#[test]
fn an_empty_anchor_reads_as_no_anchor() {
    assert_eq!(anchor_readings(""), None);
    let body = "#\nempty heading\n## Alpha\na\n";
    assert_eq!(
        resolve_section(body, ""),
        Err(SectionError::HeadingNotFound { heading: "".into() }),
        "an empty anchor names no heading, the empty one included"
    );
}

/// The heading anchor the one link `link` records, as the text layer parses
/// it.
fn anchor_of(link: &str) -> &'static str {
    let mut links = BodyScan::new(link).links().into_iter();
    let parsed = links.next().expect("one link");
    assert!(links.next().is_none(), "one link in {link:?}");
    parsed.anchor.expect("a heading anchor").leak()
}

/// **A wikilink anchor is literal, and a Markdown fragment is decoded once.**
/// Only a Markdown link percent-encodes its fragment, so the text layer
/// decodes it where the link is parsed and the resolver reads every anchor as
/// written: `[[n#100%25]]` and `[x](n.md#100%2525)` both reach `## 100%25`,
/// and `[x](n.md#My%20Heading)` reaches `## My Heading` where the wikilink
/// `[[n#My%20Heading]]` does not.
#[test]
fn a_wikilink_anchor_is_literal_and_a_markdown_fragment_is_decoded_once() {
    let body = "## 100%25\nencoded\n## My Heading\nmine\n## Über uns\nabout\n";
    let encoded = resolve_section(body, "100%25").expect("the heading by its text");
    assert_eq!(
        resolve_section(body, anchor_of("[[n#100%25]]")),
        Ok(encoded)
    );
    assert_eq!(
        resolve_section(body, anchor_of("[x](n.md#100%2525)")),
        Ok(encoded)
    );
    let mine = resolve_section(body, "My Heading").expect("the heading by its text");
    assert_eq!(
        resolve_section(body, anchor_of("[x](n.md#My%20Heading)")),
        Ok(mine)
    );
    assert_eq!(
        resolve_section(body, anchor_of("[[n#My%20Heading]]")),
        Err(SectionError::HeadingNotFound {
            heading: "My%20Heading".into()
        }),
        "a wikilink anchor is not decoded"
    );
    let about = resolve_section(body, "Über uns").expect("the heading by its text");
    for fragment in ["%C3%BCber-uns", "%c3%bcber-uns"] {
        assert_eq!(
            resolve_section(body, anchor_of(&format!("[x](n.md#{fragment})"))),
            Ok(about),
            "{fragment} reaches the heading by its slug, decoded in either case"
        );
    }
}

/// **A decoded `%23` in a Markdown fragment is a `#` as a written one is**:
/// the resolver reads the anchor by its whole text first, so `C%23` reaches
/// `## C#`, and as a heading chain only where no heading's text is the whole
/// anchor, so `Top%23Sub` reaches `## Sub` as `Top#Sub` does. A wikilink's
/// `%23` is three literal characters and never a chain's `#`.
#[test]
fn a_decoded_hash_in_a_markdown_fragment_is_read_as_a_written_one() {
    let body = "## C#\nsharp\n# Top\nt\n## Sub\nsub\n";
    let sharp = resolve_section(body, "C#").expect("the heading whose text holds a hash");
    assert_eq!(
        resolve_section(body, anchor_of("[x](n.md#C%23)")),
        Ok(sharp)
    );
    let sub = resolve_section(body, "Sub").expect("the last heading");
    assert_eq!(
        resolve_section(body, anchor_of("[x](n.md#Top%23Sub)")),
        Ok(sub)
    );
    assert_eq!(
        resolve_section(body, anchor_of("[[n#Top%23Sub]]")),
        Err(SectionError::HeadingNotFound {
            heading: "Top%23Sub".into()
        }),
        "a wikilink's `%23` is no chain separator"
    );
}

/// **A get's anchor is literal**, as a wikilink's is: `100%25` names the
/// heading `100%25` and never `100%`.
#[test]
fn a_targets_anchor_is_read_as_written() {
    let body = "## 100%\npercent\n## 100%25\nencoded\n";
    let encoded = resolve_section(body, "100%25").expect("a section");
    assert_eq!(
        &body[encoded.content_start..encoded.content_end],
        "encoded\n"
    );
    assert_eq!(
        anchor_readings("a%FFb").map(|readings| readings.text),
        Some("a%ffb".into()),
        "an anchor is read as written"
    );
}

/// **A heading chain matches on its last heading**, as `[[note#Top#Sub]]` is
/// written, where no heading's own text is the whole anchor: a heading whose
/// text holds a `#` is still reached by that text first.
#[test]
fn a_heading_chain_matches_on_its_last_heading() {
    let body = "# Top\nt\n## Sub\nsub\n## Other\no\n";
    let sub = resolve_section(body, "Sub").expect("the last heading");
    assert_eq!(resolve_section(body, "Top#Sub"), Ok(sub));
    assert_eq!(resolve_section(body, "Anything#sub"), Ok(sub));
    let three = "# A\na\n## B\nb\n### C\nc\n## D\nd\n";
    let c = resolve_section(three, "C").expect("the last heading");
    assert_eq!(
        resolve_section(three, "A#B#C"),
        Ok(c),
        "a chain of three is read as its last heading"
    );
    let encoded = "# Top\nt\n## My Sub\nmine\n";
    assert_eq!(
        resolve_section(encoded, anchor_of("[x](n.md#Top#My%20Sub)")),
        resolve_section(encoded, "My Sub"),
        "a Markdown fragment's chain is read after it is decoded"
    );

    let body = "## C# tips\ntips\n## tips\nplain\n";
    let tips = resolve_section(body, "C# tips").expect("the heading whose text holds a hash");
    assert_eq!(&body[tips.content_start..tips.content_end], "tips\n");

    // A chain names a heading on both sides of a `#`: a hash run opening the
    // anchor is not a chain, and a chain ending in a `#` names no last heading.
    let body = "## Alpha\na\n";
    for anchor in ["#Alpha", "####### Alpha", "Alpha#", "Alpha# "] {
        assert_eq!(
            resolve_section(body, anchor),
            Err(SectionError::HeadingNotFound {
                heading: anchor.into()
            }),
            "for {anchor:?}"
        );
    }
}

// ── Section write verbs: append, delete, insert around a heading ──────────

/// **An append lands at the end of the section's content, above the blank
/// lines that separate it from the next heading.** The section's subsections
/// are its content, so the append follows them.
#[test]
fn an_append_lands_below_the_sections_content_and_above_its_separator() {
    let source = "---\ntitle: t\n---\n## Alpha\n\na1\n\n### Sub\ns1\n\n## Beta\nb1\n";
    assert_eq!(
        Document::parse(source).append_to_section("Alpha", "added"),
        Ok("---\ntitle: t\n---\n## Alpha\n\na1\n\n### Sub\ns1\nadded\n\n## Beta\nb1\n".to_string())
    );
}

/// **An append into a CRLF document writes CRLF lines**, whatever breaks the
/// content arrived with.
#[test]
fn an_append_into_a_crlf_document_is_all_crlf() {
    let source = "## Alpha\r\n\r\na1\r\n\r\n## Beta\r\n";
    assert_eq!(
        Document::parse(source).append_to_section("Alpha", "one\ntwo"),
        Ok("## Alpha\r\n\r\na1\r\none\r\ntwo\r\n\r\n## Beta\r\n".to_string())
    );
}

/// **An append to a last line with no terminator starts a new line** rather
/// than welding onto it, and an append to an empty section lands between its
/// separators as a replace does.
#[test]
fn an_append_starts_its_own_line_and_fills_an_empty_section_between_separators() {
    assert_eq!(
        Document::parse("## Alpha\na1").append_to_section("Alpha", "added"),
        Ok("## Alpha\na1\nadded\n".to_string())
    );
    assert_eq!(
        Document::parse("## Alpha\n\n\n## Beta\n").append_to_section("Alpha", "added"),
        Ok("## Alpha\n\n\nadded\n\n## Beta\n".to_string())
    );
}

/// **A delete removes the heading line and everything the section owns** —
/// its subsections and the separator below it — and leaves the blank lines
/// above the heading, which are the section before's.
#[test]
fn a_delete_removes_the_heading_and_its_body_and_keeps_the_blank_lines_above() {
    let source = "---\ntitle: t\n---\nintro\n\n## Alpha\n\na1\n### Sub\ns1\n\n## Beta\nb1\n";
    assert_eq!(
        Document::parse(source).delete_section("Alpha"),
        Ok("---\ntitle: t\n---\nintro\n\n## Beta\nb1\n".to_string())
    );
    assert_eq!(
        Document::parse(source).delete_section("Beta"),
        Ok("---\ntitle: t\n---\nintro\n\n## Alpha\n\na1\n### Sub\ns1\n\n".to_string())
    );
}

/// **A delete that would fuse the text above into the heading below
/// refuses**: without the deleted section between them, a paragraph line
/// becomes the first line of a setext heading's title.
#[test]
fn a_delete_that_rewrites_a_surviving_heading_refuses() {
    let source = "para\n## Alpha\na1\nBeta\n====\n";
    assert_eq!(
        Document::parse(source).delete_section("Alpha"),
        Err(EditError::SectionPostImageMismatch {
            heading: "Alpha".into()
        })
    );
}

/// **An insert before a heading lands directly above its line, below the
/// blank lines that separate it from the section before.**
#[test]
fn an_insert_before_a_heading_lands_directly_above_its_line() {
    let source = "---\ntitle: t\n---\nintro\n\n## Alpha\n\na1\n";
    assert_eq!(
        Document::parse(source).insert_before_heading("Alpha", "## Before\nb"),
        Ok("---\ntitle: t\n---\nintro\n\n## Before\nb\n## Alpha\n\na1\n".to_string())
    );
}

/// **An insert after a heading lands directly below its line, above the blank
/// lines that separate it from the section's content**, and a heading that
/// ends the file without a terminator gains one first.
#[test]
fn an_insert_after_a_heading_lands_directly_below_its_line() {
    let source = "## Alpha\n\na1\n\n## Beta\n";
    assert_eq!(
        Document::parse(source).insert_after_heading("Alpha", "lead"),
        Ok("## Alpha\nlead\n\na1\n\n## Beta\n".to_string())
    );
    assert_eq!(
        Document::parse("## Alpha").insert_after_heading("Alpha", "lead"),
        Ok("## Alpha\nlead\n".to_string())
    );
    // A setext heading's line is its title and its underline together.
    assert_eq!(
        Document::parse("Alpha\n=====\n\na1\n").insert_after_heading("Alpha", "lead"),
        Ok("Alpha\n=====\nlead\n\na1\n".to_string())
    );
}

/// **An insert that rewrites the heading it is placed against refuses**: a
/// line written directly above a setext heading becomes part of its title.
#[test]
fn an_insert_that_rewrites_a_heading_refuses() {
    assert_eq!(
        Document::parse("intro\n\nAlpha\n=====\n").insert_before_heading("Alpha", "lead"),
        Err(EditError::SectionPostImageMismatch {
            heading: "Alpha".into()
        })
    );
}

/// Each section write verb, applied to `source` at `heading` with `content`
/// where the verb takes content.
fn every_section_verb(
    source: &str,
    heading: &'static str,
    content: &str,
) -> Vec<(&'static str, Result<String, EditError>)> {
    let document = Document::parse(source);
    vec![
        ("append", document.append_to_section(heading, content)),
        ("delete", document.delete_section(heading)),
        ("before", document.insert_before_heading(heading, content)),
        ("after", document.insert_after_heading(heading, content)),
    ]
}

/// **The same heading text twice refuses every section write, whatever levels
/// the two headings are at**: an ambiguous address is a question, not an edit.
#[test]
fn a_heading_duplicated_across_levels_refuses_every_section_write() {
    let source = "# Notes\na\n### Notes\nb\n";
    for (verb, result) in every_section_verb(source, "Notes", "x") {
        assert_eq!(
            result,
            Err(EditError::Section(SectionError::HeadingAmbiguous {
                heading: "Notes".into(),
                count: 2
            })),
            "for {verb}"
        );
    }
}

/// **A heading inside fenced code is never addressed by a section write**: the
/// only real heading with that text is the one outside the fence, and where
/// there is none the address is not found.
#[test]
fn a_heading_inside_a_fence_never_matches_a_section_write() {
    let fenced = "## Real\n```\n## Fake\n```\n";
    for (verb, result) in every_section_verb(fenced, "Fake", "x") {
        assert_eq!(
            result,
            Err(EditError::Section(SectionError::HeadingNotFound {
                heading: "Fake".into()
            })),
            "for {verb}"
        );
    }
    let beside = "```\n## Alpha\n```\n\n## Alpha\na1\n";
    assert_eq!(
        Document::parse(beside).delete_section("Alpha"),
        Ok("```\n## Alpha\n```\n\n".to_string())
    );
}

/// **A heading inside a blockquote or list item refuses every section write**,
/// as it refuses a replace: its bytes are the container's first.
#[test]
fn a_heading_inside_a_container_refuses_every_section_write() {
    let source = "> ## Quoted\n> body\n\nafter\n";
    for (verb, result) in every_section_verb(source, "Quoted", "x") {
        assert_eq!(
            result,
            Err(EditError::SectionInContainer {
                heading: "Quoted".into()
            }),
            "for {verb}"
        );
    }
}

/// **Every section write keeps a CRLF document CRLF**, the lines it writes
/// included, and leaves every blank line it does not own standing.
#[test]
fn every_section_write_keeps_crlf_and_the_blank_lines_around_headings() {
    let source = "intro\r\n\r\n## Alpha\r\n\r\na1\r\n\r\n## Beta\r\nb1\r\n";
    let expected = [
        (
            "append",
            "intro\r\n\r\n## Alpha\r\n\r\na1\r\nx\r\ny\r\n\r\n## Beta\r\nb1\r\n",
        ),
        ("delete", "intro\r\n\r\n## Beta\r\nb1\r\n"),
        (
            "before",
            "intro\r\n\r\nx\r\ny\r\n## Alpha\r\n\r\na1\r\n\r\n## Beta\r\nb1\r\n",
        ),
        (
            "after",
            "intro\r\n\r\n## Alpha\r\nx\r\ny\r\n\r\na1\r\n\r\n## Beta\r\nb1\r\n",
        ),
    ];
    for ((verb, result), (_, want)) in every_section_verb(source, "Alpha", "x\ny")
        .into_iter()
        .zip(expected)
    {
        assert_eq!(result, Ok(want.to_string()), "for {verb}");
    }
}

/// **An append whose content swallows the document below it refuses**: an
/// unclosed fence turns the next heading into code, and a setext underline
/// turns the section's last line into a heading.
#[test]
fn an_append_that_swallows_structure_refuses() {
    let source = "## Alpha\na1\n\n## Beta\nb1\n";
    for content in ["```", "==="] {
        assert_eq!(
            Document::parse(source).append_to_section("Alpha", content),
            Err(EditError::SectionPostImageMismatch {
                heading: "Alpha".into()
            }),
            "for {content:?}"
        );
    }
}
