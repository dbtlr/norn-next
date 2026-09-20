//! One line break, everywhere: `\n`, `\r\n`, or a lone `\r`.
//!
//! A lone `\r` is a line ending in CommonMark and in YAML, so it is one here,
//! and the crate answers the same questions about a document however its
//! author terminated the lines. Two things make that true and are checked
//! here: the CommonMark pass is handed input whose lone `\r` breaks read as
//! `\n` without any byte moving, and every line-shaped scan in the crate cuts
//! on the one break rule rather than on `\n` alone.
//!
//! The reading half is the offset property — what a scan reports still
//! addresses the bytes the caller handed in. The editing half is one case per
//! site that used to cut on `\n` alone, each stating what the site now does.

use norn_text::{BodyScan, Document, LineEnding, Mapping, SectionAddress, Value, render_document};

// ── The offset property ──────────────────────────────────────────────────

/// A document with something for every family to find: a heading, both link
/// grammars, a tag, a block id, and a fenced sample holding one of each that
/// none of them may reach.
const TEMPLATE: &str = "# Alpha\n\nprose [[Note|shown]] and [text](./t.md) and #atag ^aid\n\n```\nfenced [[Hidden]] #hidden ^hid\n```\n\n## Beta\ntail #btag\n";

/// `TEMPLATE` rewritten so its breaks cycle through the three styles, so the
/// mixed document is mixed at every line rather than in one place.
///
/// One adjustment keeps the rewrite line-count-preserving: a blank line
/// written as `\n` directly after a `\r` would weld the two into a single
/// `\r\n` break and lose a line, so a blank line in that position is written
/// `\r` instead.
fn mixed(template: &str) -> String {
    let styles = ["\n", "\r\n", "\r"];
    let mut out = String::new();
    for (index, line) in template.split_inclusive('\n').enumerate() {
        let content = line.trim_end_matches('\n');
        let mut style = styles[index % styles.len()];
        if content.is_empty() && style == "\n" && out.ends_with('\r') {
            style = "\r";
        }
        out.push_str(content);
        out.push_str(style);
    }
    out
}

/// Every construct `document` carries, named by what the document's **own
/// bytes** say at the span the scan reported.
///
/// Nothing here quotes a fact's parsed text alone: each line is built by
/// slicing `document` at the reported offset, so a span that drifted off its
/// construct produces a different fact rather than the same one. Line and
/// column ride along, since a position that addresses the right byte under the
/// wrong line number is still wrong.
fn facts_by_slicing(document: &str) -> Vec<String> {
    let scan = BodyScan::new(document);
    let mut facts = Vec::new();
    let mut record = |kind: &str, span: norn_text::SourceSpan, expected: &str| {
        let at = span.byte_offset;
        assert!(
            document[at..].starts_with(expected),
            "a {kind} span points at {:?}, not at {expected:?}",
            &document[at..(at + expected.len()).min(document.len())]
        );
        facts.push(format!("{kind} {expected} @{}:{}", span.line, span.column));
    };
    for heading in scan.headings() {
        record(
            "heading",
            heading.span,
            &"#".repeat(usize::from(heading.level)),
        );
    }
    for link in scan.links() {
        let raw = link.raw.clone();
        record("link", link.span, &raw);
    }
    for tag in scan.tags() {
        let name = format!("#{}", tag.name);
        record("tag", tag.span.expect("a body tag names its bytes"), &name);
    }
    for block_id in scan.block_ids() {
        let id = format!("^{}", block_id.id);
        record("block-id", block_id.span, &id);
    }
    facts
}

/// The offset property: for a document written with lone `\r` breaks, every
/// span the scan reports slices the **original** bytes to the same text, on
/// the same line and column, as the `\n` twin of the same document.
///
/// This is what makes the normalization the CommonMark pass applies
/// admissible. The pass reads a copy whose lone `\r` bytes are `\n`; because
/// that rewrite is one byte for one byte, the ranges it reports are offsets
/// into the caller's bytes and nothing downstream has to know a copy existed.
#[test]
fn every_span_addresses_the_original_bytes_on_every_break_style() {
    let lf = TEMPLATE.to_string();
    let crlf = TEMPLATE.replace('\n', "\r\n");
    let cr = TEMPLATE.replace('\n', "\r");
    let mixed = mixed(TEMPLATE);

    let expected = facts_by_slicing(&lf);
    // The facts are the document's, not an accident of an empty scan.
    assert_eq!(
        expected,
        [
            "heading # @1:1",
            "heading ## @9:1",
            "link [[Note|shown]] @3:7",
            "link [text](./t.md) @3:26",
            "tag #atag @3:45",
            "tag #btag @10:6",
            "block-id ^aid @3:51",
        ]
    );
    assert_eq!(facts_by_slicing(&cr), expected, "a lone-`\\r` document");
    assert_eq!(facts_by_slicing(&crlf), expected, "a `\\r\\n` document");
    assert_eq!(facts_by_slicing(&mixed), expected, "a mixed document");
}

/// The fence is opaque on a lone `\r` too, which is the leak the normalization
/// closes: the fenced `[[Hidden]]`, `#hidden` and `^hid` of `TEMPLATE` reach
/// no family on any break style.
///
/// `a_lone_cr_backtick_fence_is_opaque` in `wikilinks.rs` states the same
/// contract against a bare fence; this one states it inside a document that
/// also carries constructs the scan **must** find, so an over-wide mask fails
/// it as loudly as a missing one.
#[test]
fn a_fenced_sample_reaches_no_family_on_any_break_style() {
    for document in [
        TEMPLATE.to_string(),
        TEMPLATE.replace('\n', "\r\n"),
        TEMPLATE.replace('\n', "\r"),
        mixed(TEMPLATE),
    ] {
        let facts = facts_by_slicing(&document);
        for hidden in ["[[Hidden]]", "#hidden", "^hid"] {
            assert!(
                !facts.iter().any(|fact| fact.contains(hidden)),
                "{hidden} leaked out of the fence: {facts:?}"
            );
        }
    }
}

// ── The sites that cut lines ─────────────────────────────────────────────

/// A standalone comment is the document's, not the field's above it, so
/// removing that field leaves it standing — and the break above the comment
/// may be a lone `\r`.
///
/// This is a data-loss case: a scan that cut on `\n` alone read `\r# keep me`
/// as one chunk, found it neither blank nor column-0, and stopped the trailing
/// separator run short — putting the comment inside the removed field's own
/// bytes.
#[test]
fn removing_a_field_keeps_a_comment_standing_after_a_lone_cr_break() {
    let source = "---\ntitle: x\ntags: y\r# keep me\nkeep: z\n---\nbody\n";
    let edited = Document::parse(source)
        .remove_field("tags")
        .expect("the field splits and removes");
    assert!(
        edited.contains("# keep me"),
        "the comment was deleted: {edited:?}"
    );
    assert!(edited.contains("keep: z"));
    assert!(!edited.contains("tags:"));
}

/// The comment survives whatever mix of blank lines and `\r` breaks stands
/// between the field and it.
///
/// Three shapes, because the trailing-separator run is walked backwards and
/// each shape stops it at a different place: a blank `\r` line above the
/// comment, the same above a block sequence, and the comment directly after a
/// sequence item. Each deletes the comment when the run cuts on `\n` alone.
#[test]
fn a_comment_survives_every_lone_cr_separator_shape() {
    for source in [
        "---\ntags: y\r\r# keep me\rkeep: z\n---\nbody\n",
        "---\ntags:\r  - a\r\r# keep me\rkeep: z\n---\nbody\n",
        "---\ntags:\r  - a\r# keep me\rkeep: z\n---\nbody\n",
    ] {
        let edited = Document::parse(source)
            .remove_field("tags")
            .unwrap_or_else(|error| panic!("{source:?} refused: {error:?}"));
        assert!(
            edited.contains("# keep me"),
            "the comment was deleted from {source:?}: {edited:?}"
        );
        assert!(edited.contains("keep: z"), "{edited:?}");
        assert!(!edited.contains("tags:"), "{edited:?}");
    }
}

/// The document's own terminator wins a splice. `content` arrives written
/// however its author wrote it — `\n`, `\r\n` or a lone `\r` — and every line
/// the splice writes carries the terminator the document already uses.
#[test]
fn a_section_replace_writes_the_documents_terminator_over_every_caller_break() {
    let lf = Document::parse("# A\nold\n")
        .replace_section("A", "one\rtwo\r\nthree")
        .expect("the replace holds");
    assert_eq!(lf, "# A\none\ntwo\nthree\n");

    let crlf = Document::parse("# A\r\nold\r\n")
        .replace_section("A", "one\rtwo\nthree")
        .expect("the replace holds");
    assert_eq!(crlf, "# A\r\none\r\ntwo\r\nthree\r\n");
}

/// The post-image check the replace makes compares lines on the same rule the
/// splice writes by, so it is comparing what the splice produced.
///
/// A comparison cutting on `\n` alone held `one\rtwo` as a single line on both
/// sides: it approved the splice without having looked at either break.
#[test]
fn the_replace_post_image_check_reads_a_lone_cr_as_a_break() {
    let edited = Document::parse("# A\nold\n")
        .replace_section("A", "one\rtwo")
        .expect("the replace holds");
    // Two lines went in and two lines came out, on the document's terminator.
    let scan = Document::parse(&edited);
    let span = scan
        .resolve_section(SectionAddress::from("A"))
        .expect("the section resolves");
    assert_eq!(&edited[span.content_start..span.content_end], "one\ntwo\n");
}

/// Bytes outside the addressed range keep their spelling. A `\r`-broken
/// document's heading line is `\r`-terminated before the replace and
/// `\r`-terminated after it.
///
/// The separator the splice adds where the byte above it does not end a line
/// is decided by the crate's break rule. Under a `\n`-only test that byte does
/// not end a line, so the splice writes a terminator the heading did not ask
/// for and the heading's own bytes move — the one-construct-diff promise
/// broken by the edit that was supposed to keep it.
#[test]
fn a_replace_leaves_a_lone_cr_heading_line_byte_identical() {
    let source = "# A\rold\r";
    let edited = Document::parse(source)
        .replace_section("A", "one\rtwo")
        .expect("the replace holds");
    assert!(
        edited.starts_with("# A\r"),
        "the heading line was respelled: {edited:?}"
    );
    // The document holds no `\n` before the edit, so it classifies as `Lf` and
    // the replaced content is written with `\n`. Only the content changed.
    assert_eq!(edited, "# A\rone\ntwo\n");
}

/// Blank lines separated by a lone `\r` are blank, so a section's content
/// starts and stops where the prose does.
#[test]
fn section_content_bounds_trim_blank_lines_broken_by_a_lone_cr() {
    let content_of = |body: &str, expected: &str| {
        let span = BodyScan::new(body)
            .resolve_section(SectionAddress::from("A"))
            .expect("the section resolves");
        assert_eq!(&body[span.content_start..span.content_end], expected);
    };
    content_of("## A\n\nc1\n\n\n## B\n", "c1\n");
    content_of("## A\r\rc1\r\r\r## B\r", "c1\r");
}

/// A block sequence broken by lone `\r` reports one byte range per item, the
/// same ranges its `\n` twin reports.
///
/// The site refuses when its scanned item count disagrees with the parsed one.
/// Cutting on `\n` alone handed it one chunk holding every item, so the counts
/// disagreed for a sequence that was never ambiguous and every splice point in
/// it was lost.
#[test]
fn a_lone_cr_block_sequence_reports_a_range_for_every_item() {
    let ranges_of = |source: &str| {
        let document = Document::parse(source);
        assert!(document.split_refusal().is_none(), "{source:?} refused");
        document
            .field_texts()
            .into_iter()
            .map(|text| {
                let range = text.range.clone().expect("the item names its bytes");
                source[range].to_string()
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(
        ranges_of("---\ntags:\n  - one\n  - two\n---\nbody\n"),
        ["one", "two"]
    );
    assert_eq!(
        ranges_of("---\ntags:\r  - one\r  - two\n---\nbody\n"),
        ["one", "two"]
    );
}

/// A closing `---` fence after a lone `\r` closes the block.
///
/// The opening fence is terminated by `\n` or `\r\n`, so the document that
/// reaches this site is a mixed one — which is exactly the document an editor
/// that rewrites one line produces.
#[test]
fn a_closing_fence_after_a_lone_cr_closes_the_block() {
    let document = Document::parse("---\ntitle: x\r---\nbody\n");
    assert!(
        document.frontmatter_refusal().is_none(),
        "{:?}",
        document.frontmatter_refusal()
    );
    assert_eq!(
        document.frontmatter().and_then(|value| match value {
            Value::Map(map) => map.get("title").cloned(),
            _ => None,
        }),
        Some(Value::String("x".to_string()))
    );
    assert_eq!(document.body(), "body\n");
}

/// Every key of a `\r`-broken block is located, so the block splits and its
/// fields are editable.
///
/// Cutting on `\n` alone located only the first key of such a block; the rest
/// went unlocated, the split refused, and every field edit over the document
/// was disabled — safely, and without saying so anywhere a caller reads.
#[test]
fn a_lone_cr_frontmatter_block_splits_into_its_fields() {
    let source = "---\na: 1\rb: 2\nc: 3\n---\nbody\n";
    let document = Document::parse(source);
    assert!(
        document.split_refusal().is_none(),
        "{:?}",
        document.split_refusal()
    );
    assert_eq!(
        document
            .fields()
            .iter()
            .map(|field| field.name.as_str())
            .collect::<Vec<_>>(),
        ["a", "b", "c"]
    );
    let edited = document
        .set_field("b", &Value::Int(9))
        .expect("the field is editable");
    assert!(edited.contains("b: 9"), "{edited:?}");
    assert!(edited.contains("a: 1"), "{edited:?}");
    assert!(edited.contains("c: 3"), "{edited:?}");
}

/// A body whose last line is terminated gets no second terminator, whichever
/// break terminated it.
///
/// Under a `\n`-only test a `\r`-terminated body reads as unterminated and the
/// render welds a terminator onto a line that already ended, adding a blank
/// line the caller's bytes never had.
#[test]
fn rendering_adds_no_terminator_to_a_body_that_already_ends_a_line() {
    let fields = Mapping::default();
    let rendered = |body: &str| {
        render_document(&fields, body, LineEnding::Lf).expect("an empty mapping renders")
    };
    assert_eq!(rendered("prose\n"), "---\n---\nprose\n");
    assert_eq!(rendered("prose\r"), "---\n---\nprose\r");
    // A body that really does not end a line still gets one.
    assert_eq!(rendered("prose"), "---\n---\nprose\n");
}

// ── One definition, and no seventh site ──────────────────────────────────

/// Every `.rs` file of the crate, pinned at compile time.
///
/// The list is pinned rather than walked so the scan below cannot quietly read
/// zero files; `the_pinned_sources_are_every_source_file` is what keeps the
/// pin honest when a module is added.
const SOURCES: &[(&str, &str)] = &[
    ("src/body.rs", include_str!("../src/body.rs")),
    ("src/diagnostic.rs", include_str!("../src/diagnostic.rs")),
    ("src/document.rs", include_str!("../src/document.rs")),
    (
        "src/frontmatter/extract.rs",
        include_str!("../src/frontmatter/extract.rs"),
    ),
    (
        "src/frontmatter/fields.rs",
        include_str!("../src/frontmatter/fields.rs"),
    ),
    (
        "src/frontmatter/mod.rs",
        include_str!("../src/frontmatter/mod.rs"),
    ),
    (
        "src/frontmatter/render.rs",
        include_str!("../src/frontmatter/render.rs"),
    ),
    ("src/heading.rs", include_str!("../src/heading.rs")),
    ("src/lib.rs", include_str!("../src/lib.rs")),
    ("src/line_ending.rs", include_str!("../src/line_ending.rs")),
    ("src/link.rs", include_str!("../src/link.rs")),
    ("src/section.rs", include_str!("../src/section.rs")),
    ("src/span.rs", include_str!("../src/span.rs")),
    ("src/tag.rs", include_str!("../src/tag.rs")),
    ("src/value.rs", include_str!("../src/value.rs")),
];

/// The shapes that cut text into lines on `\n` alone, or on whatever the
/// standard library's `lines` calls a line.
///
/// Each one is a line rule, and a second line rule is a second answer to
/// *where does this line end* that nothing reconciles with the first. The
/// crate's answer is `span::split_lines_inclusive` and `span::LineCursor`.
const FORBIDDEN: &[&str] = &[
    "split_inclusive('\\n')",
    "split_inclusive(\"\\n\")",
    "split('\\n')",
    "split(\"\\n\")",
    "split_terminator('\\n')",
    "split_terminator(\"\\n\")",
    ".lines()",
    ".lines_any()",
];

/// The one file allowed to spell them: the module that defines the break rule,
/// whose own prose names the shape it exists in place of.
const DEFINITION: &str = "src/span.rs";

/// The crate has one definition of a line break, so no file outside
/// `span.rs` cuts text into lines by any other rule.
///
/// A seventh site returning silently is the failure this stands against: every
/// site fixed here was correct-looking, locally reasonable, and wrong only
/// about a break style its own author never wrote.
#[test]
fn no_source_outside_the_definition_cuts_lines_by_its_own_rule() {
    let mut found = Vec::new();
    for (file, source) in SOURCES {
        if *file == DEFINITION {
            continue;
        }
        for shape in FORBIDDEN {
            if source.contains(shape) {
                found.push(format!("`{shape}` in {file}"));
            }
        }
    }
    assert_eq!(
        found,
        Vec::<String>::new(),
        "use `span::split_lines_inclusive` or `span::LineCursor` instead"
    );
}

/// The negative control for the scan above: the shapes it bans are shapes it
/// can actually see, so a clean result means *absent* rather than *unread*.
#[test]
fn the_forbidden_shapes_are_findable_in_source_text() {
    let planted = "for line in text.split_inclusive('\\n') { drop(line); }";
    assert!(FORBIDDEN.iter().any(|shape| planted.contains(shape)));
    // And the definition file, the one exemption, really does spell one —
    // so the exemption is load-bearing and not a leftover.
    let definition = SOURCES
        .iter()
        .find(|(file, _)| *file == DEFINITION)
        .expect("the definition file is pinned");
    assert!(FORBIDDEN.iter().any(|shape| definition.1.contains(shape)));
}

/// The pin covers the crate: `SOURCES` is exactly the set of files the crate's
/// own `mod` declarations reach, and nothing else.
///
/// Without this, adding a module is how a new site escapes the scan — the scan
/// would keep passing, over the files it was told about in the diff before.
/// The declarations are read out of the pinned text rather than off the disk,
/// so the check needs no filesystem and cannot drift from what was compiled.
#[test]
fn the_pinned_sources_are_every_module_the_crate_declares() {
    /// The module names a file declares. Rust source is `\n`-terminated here,
    /// and this file is not one the scan above reads.
    fn declared(source: &str) -> Vec<String> {
        source
            .lines()
            .map(str::trim)
            .filter_map(|line| {
                let rest = line
                    .strip_prefix("mod ")
                    .or_else(|| line.strip_prefix("pub mod "))
                    .or_else(|| line.strip_prefix("pub(crate) mod "))?;
                rest.strip_suffix(';').map(str::to_string)
            })
            .collect()
    }
    let text = |file: &str| {
        SOURCES
            .iter()
            .find(|(pinned, _)| *pinned == file)
            .unwrap_or_else(|| panic!("{file} is declared but not pinned in SOURCES"))
            .1
    };

    let mut reached = vec!["src/lib.rs".to_string()];
    for module in declared(text("src/lib.rs")) {
        let leaf = format!("src/{module}.rs");
        let directory = format!("src/{module}/mod.rs");
        if SOURCES.iter().any(|(file, _)| *file == directory) {
            reached.push(directory.clone());
            for child in declared(text(&directory)) {
                reached.push(format!("src/{module}/{child}.rs"));
                let _ = text(&reached[reached.len() - 1]);
            }
        } else {
            reached.push(leaf.clone());
            let _ = text(&leaf);
        }
    }
    reached.sort();
    let mut pinned: Vec<String> = SOURCES
        .iter()
        .map(|(file, _)| (*file).to_string())
        .collect();
    pinned.sort();
    assert_eq!(
        pinned, reached,
        "SOURCES and the crate's module declarations name different files"
    );
}
