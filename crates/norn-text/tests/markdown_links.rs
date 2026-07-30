//! Inline Markdown links: the second link family, the fact shape it shares
//! with wikilinks, the addressing mode that tells them apart, and the fence
//! around the forms that stay characterized rather than implemented.

use norn_text::{AddressingMode, BodyScan, Document, Link, LinkFamily, reconstruct_wikilink};

fn markdown_links(body: &str) -> Vec<Link> {
    BodyScan::new(body).markdown_links()
}

fn only(body: &str) -> Link {
    let mut links = markdown_links(body).into_iter();
    let link = links.next().expect("one markdown link");
    assert!(
        links.next().is_none(),
        "expected exactly one markdown link in {body:?}"
    );
    link
}

fn targets(links: &[Link]) -> Vec<&str> {
    links.iter().map(|link| link.target.as_str()).collect()
}

// ── One fact shape, two addressing modes ─────────────────────────────────

#[test]
fn an_inline_link_decomposes_into_the_link_fact() {
    let link = only("see [Title](./note.md) here\n");
    assert_eq!(link.family, LinkFamily::Markdown);
    assert_eq!(link.raw, "[Title](./note.md)");
    assert_eq!(link.target, "./note.md");
    assert_eq!(link.title.as_deref(), Some("Title"));
    assert_eq!(link.anchor, None);
    assert_eq!(link.block_ref, None);
    assert_eq!(link.protocol, None);
    assert!(!link.embed);
}

/// The two families reach the same document by two standards, and the fact
/// says which one a resolver should use. A wikilink stem is a suffix address;
/// a Markdown destination is a path relative to the document it was written
/// in.
#[test]
fn the_family_decides_the_addressing_mode() {
    let wikilink = BodyScan::new("[[norn-links]]\n").wikilinks().remove(0);
    let markdown = only("[Links](./norn-links.md)\n");

    assert_eq!(wikilink.addressing(), AddressingMode::Suffix);
    assert_eq!(markdown.addressing(), AddressingMode::RelativePath);
    assert_eq!(LinkFamily::Wikilink.addressing(), AddressingMode::Suffix);
    assert_eq!(
        LinkFamily::Markdown.addressing(),
        AddressingMode::RelativePath
    );
}

/// A destination is recorded as written. Joining it against a directory,
/// deciding what a leading `/` means, and refusing one that climbs out of the
/// vault are all resolution, and resolution is not in this crate.
#[test]
fn a_destination_is_recorded_raw() {
    for (body, target) in [
        ("[t](note.md)\n", "note.md"),
        ("[t](./note.md)\n", "./note.md"),
        ("[t](../up/note.md)\n", "../up/note.md"),
        ("[t](/rooted/note.md)\n", "/rooted/note.md"),
        ("[t](folder/note.md)\n", "folder/note.md"),
        ("[t](../../way/up.md)\n", "../../way/up.md"),
        ("[t](note%20with%20spaces.md)\n", "note%20with%20spaces.md"),
    ] {
        assert_eq!(only(body).target, target, "target of {body:?}");
    }
}

#[test]
fn the_bracket_text_is_the_title() {
    assert_eq!(only("[Shown](x.md)\n").title.as_deref(), Some("Shown"));
    assert_eq!(only("[ Padded ](x.md)\n").title.as_deref(), Some("Padded"));
    // The brackets are always written, so the title is always present — an
    // empty one is empty rather than absent.
    assert_eq!(only("[](x.md)\n").title.as_deref(), Some(""));
    // Inline markup inside the brackets is flattened, the way heading text is.
    assert_eq!(
        only("[Use `norn` **now**](x.md)\n").title.as_deref(),
        Some("Use norn now")
    );
}

// ── Fragments, split the wikilink way ────────────────────────────────────

#[test]
fn a_fragment_splits_into_an_anchor_or_a_block_reference() {
    let anchored = only("[t](./note.md#Heading)\n");
    assert_eq!(anchored.target, "./note.md");
    assert_eq!(anchored.anchor.as_deref(), Some("Heading"));
    assert_eq!(anchored.block_ref, None);

    let referenced = only("[t](./note.md#^blk)\n");
    assert_eq!(referenced.target, "./note.md");
    assert_eq!(referenced.anchor, None);
    assert_eq!(referenced.block_ref.as_deref(), Some("blk"));

    let same_document = only("[t](#Heading)\n");
    assert_eq!(same_document.target, "");
    assert_eq!(same_document.anchor.as_deref(), Some("Heading"));

    let extra_hashes = only("[t](x.md#a#b)\n");
    assert_eq!(extra_hashes.anchor.as_deref(), Some("a#b"));
}

/// The split runs over the destination as written, and this crate
/// percent-decodes nothing. A file whose name contains a hash is addressed by
/// encoding it, and the encoded form carries no fragment.
#[test]
fn a_percent_encoded_hash_is_not_a_fragment() {
    let link = only("[t](note%23draft.md)\n");
    assert_eq!(link.target, "note%23draft.md");
    assert_eq!(link.anchor, None);
    assert_eq!(link.block_ref, None);
}

// ── Code is opaque, structurally ─────────────────────────────────────────

#[test]
fn a_link_inside_code_is_literal_text() {
    assert_eq!(
        targets(&markdown_links("`[t](hidden.md)` and [t](real.md)\n")),
        ["real.md"]
    );
    assert_eq!(
        targets(&markdown_links(
            "[t](real.md)\n\n```\n[t](fenced.md)\n```\n\n    [t](indented.md)\n"
        )),
        ["real.md"]
    );
}

// ── Both families in one reading ─────────────────────────────────────────

#[test]
fn links_reports_both_families_in_document_order() {
    let body = "[Md](./a.md) then [[Wiki]] then [Md2](./b.md)\n";
    let scan = BodyScan::new(body);
    let links = scan.links();
    assert_eq!(targets(&links), ["./a.md", "Wiki", "./b.md"]);
    assert_eq!(
        links.iter().map(|link| link.family).collect::<Vec<_>>(),
        [
            LinkFamily::Markdown,
            LinkFamily::Wikilink,
            LinkFamily::Markdown
        ]
    );
    assert_eq!(targets(&scan.wikilinks()), ["Wiki"]);
    assert_eq!(targets(&scan.markdown_links()), ["./a.md", "./b.md"]);
}

#[test]
fn a_document_reports_links_in_source_coordinates() {
    let source = "---\ntitle: Note\n---\n\n[Md](./a.md) and [[Wiki]]\n";
    let document = Document::parse(source);
    let links = document.links();
    assert_eq!(targets(&links), ["./a.md", "Wiki"]);
    for link in &links {
        assert_eq!(&source[link.range()], link.raw);
        assert_eq!(link.span.line, 5);
    }
}

#[test]
fn a_token_reports_where_it_starts_and_how_far_it_runs() {
    let body = "abc [t](x.md) def\n";
    let link = only(body);
    assert_eq!(&body[link.range()], "[t](x.md)");
    assert_eq!(link.span.line, 1);
    assert_eq!(link.span.column, 5);
}

// ── Rewriting is not a token substitution here ───────────────────────────

/// A wikilink rewrite refuses a Markdown link. Its target is relative to the
/// document it sits in, so one move produces different bytes per referencing
/// file — a per-document computation rather than a splice, and it belongs to
/// the layer that knows where documents are.
#[test]
fn a_markdown_link_is_not_rewritten_by_the_wikilink_splicer() {
    let link = only("[t](./old.md)\n");
    assert_eq!(link.stem_range, None);
    assert_eq!(reconstruct_wikilink(&link, "./new.md"), None);

    let body = "[t](./old.md) and [[old]]\n";
    let out = BodyScan::new(body).splice_wikilinks(|link| reconstruct_wikilink(link, "new"));
    assert_eq!(out, "[t](./old.md) and [[new]]\n");
}

// ── Frontmatter parity: wikilinks only, by design ────────────────────────

/// A `[title](target)` string in a property is inert text. Markdown links are
/// body syntax and an editor's properties recognize the wikilink form only;
/// extracting facts here would make norn and the editor disagree about whether
/// a property holds a link. Writing the wikilink form is what opts a property
/// into the link graph.
#[test]
fn a_markdown_link_in_a_frontmatter_value_is_not_a_link() {
    let document = Document::parse(
        "---\nsource: \"[Title](./note.md)\"\nrelated: \"[[Note]]\"\n---\n\n[Body](./b.md)\n",
    );

    let from_values: Vec<Link> = document
        .field_texts()
        .iter()
        .flat_map(|text| norn_text::parse_wikilinks_in_text(text.text))
        .collect();
    assert_eq!(targets(&from_values), ["Note"]);
    assert!(
        from_values
            .iter()
            .all(|link| link.family == LinkFamily::Wikilink)
    );

    // The body is where the Markdown family lives, and it is unaffected.
    assert_eq!(targets(&document.links()), ["./b.md"]);
}

// ── The scope fence, characterized ───────────────────────────────────────

/// An image is a different element to the parser, and a transclusion in this
/// vocabulary is `![[…]]`. Recognized, and no link fact.
#[test]
fn an_image_is_recognized_and_produces_no_link() {
    assert!(markdown_links("![alt](picture.png)\n").is_empty());
    assert!(BodyScan::new("![alt](picture.png)\n").links().is_empty());
}

/// An image nested inside a link is still not a link of its own, and its alt
/// text flattens into the enclosing link's title — the same flattening every
/// other inline element in bracket text gets.
#[test]
fn an_image_inside_a_link_flattens_into_that_links_title() {
    let link = only("[![alt](i.png)](./target.md)\n");
    assert_eq!(link.target, "./target.md");
    assert_eq!(link.title.as_deref(), Some("alt"));
}

/// An autolink is a URL written as its own text. Recognized by the parse, and
/// no link fact: it addresses nothing in the vault and there is no rewrite
/// story for one.
#[test]
fn an_autolink_is_recognized_and_produces_no_link() {
    assert!(markdown_links("<https://example.com>\n").is_empty());
    assert!(markdown_links("<person@example.com>\n").is_empty());
    assert!(
        BodyScan::new("see <https://example.com>\n")
            .links()
            .is_empty()
    );
}

/// Reference-style links keep their destination in a definition line
/// elsewhere, so a fact emitted here would name a target the token does not
/// carry and could not be rewritten in place. All three written forms are
/// recognized by the parse and produce nothing.
#[test]
fn every_reference_style_link_is_recognized_and_produces_no_link() {
    for body in [
        "[text][label]\n\n[label]: ./target.md\n",
        "[label][]\n\n[label]: ./target.md\n",
        "[label]\n\n[label]: ./target.md\n",
    ] {
        assert!(markdown_links(body).is_empty(), "in {body:?}");
        assert!(BodyScan::new(body).links().is_empty(), "in {body:?}");
    }
}

/// The definition line itself produces no parser event at all — it is
/// invisible rather than ignored — and an *undefined* reference is not a link
/// to the parser either, arriving as ordinary text.
#[test]
fn a_definition_line_and_an_undefined_reference_are_invisible() {
    let definition = "[label]: ./target.md\n";
    assert!(markdown_links(definition).is_empty());
    assert!(BodyScan::new(definition).headings().is_empty());

    assert!(markdown_links("[text][missing]\n").is_empty());
    assert!(markdown_links("[missing]\n").is_empty());
}

// ── What the CommonMark parse resolves before this crate sees it ─────────

/// The document is read through one parser, so a link's destination is the
/// destination that parser reports: an angle-bracketed form is unwrapped and a
/// backslash escape is resolved. The stem therefore has no sub-span — the
/// parsed target need not appear literally in the source — and a span that is
/// not certainly right is absent rather than guessed.
#[test]
fn a_destination_is_the_parsers_and_carries_no_stem_span() {
    let angled = only("[t](<my file.md>)\n");
    assert_eq!(angled.target, "my file.md");
    assert_eq!(angled.raw, "[t](<my file.md>)");
    assert_eq!(angled.stem_range, None);

    // A backslash-escaped hash is resolved by the parse and then reads as a
    // fragment. Percent-encoding is the spelling that survives the split.
    let escaped = only("[t](note\\#draft.md)\n");
    assert_eq!(escaped.target, "note");
    assert_eq!(escaped.anchor.as_deref(), Some("draft.md"));
}

/// CommonMark's optional link title — the quoted string after the destination
/// — is not a fact this crate reports. The title a [`Link`] carries is the
/// bracket text, in both families, so one field means one thing.
#[test]
fn the_commonmark_title_attribute_is_not_the_links_title() {
    let link = only("[Shown](x.md \"Tooltip\")\n");
    assert_eq!(link.title.as_deref(), Some("Shown"));
    assert_eq!(link.target, "x.md");
    assert_eq!(link.raw, "[Shown](x.md \"Tooltip\")");
}

/// `[[a]](b)` satisfies both grammars at once — a wikilink and a Markdown link
/// sharing bytes — and both facts are reported. Neither grammar is given
/// precedence over the other, because deciding what an author meant by an
/// ambiguous token is not a syntax question.
#[test]
fn a_token_that_satisfies_both_grammars_reports_both_facts() {
    let body = "[[a]](b)\n";
    let links = BodyScan::new(body).links();
    assert_eq!(targets(&links), ["a", "b"]);
    assert_eq!(
        links.iter().map(|link| link.family).collect::<Vec<_>>(),
        [LinkFamily::Wikilink, LinkFamily::Markdown]
    );
}
