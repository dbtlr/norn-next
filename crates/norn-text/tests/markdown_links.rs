//! Inline Markdown links: the second link family, the fact shape it shares
//! with wikilinks, the resolution that tells them apart, and the fence around
//! the forms that stay characterized rather than implemented.

use norn_text::{BodyScan, Document, Link, LinkFamily, Resolution, reconstruct_wikilink};

fn markdown_links(body: &str) -> Vec<Link> {
    BodyScan::new(body)
        .links()
        .into_iter()
        .filter(|link| link.family == LinkFamily::Markdown)
        .collect()
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

// ── One fact shape, protocol-first resolution ────────────────────────────

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

/// A protocol-free target reaches its document by its family's standard, and
/// the fact says which one a resolver should use. A wikilink stem is a suffix
/// address; a Markdown destination is a path relative to the document it was
/// written in.
#[test]
fn a_protocol_free_target_resolves_by_its_family() {
    let wikilink = BodyScan::new("[[norn-links]]\n").wikilinks().remove(0);
    let markdown = only("[Links](./norn-links.md)\n");

    assert_eq!(wikilink.resolution(), Resolution::Suffix);
    assert_eq!(markdown.resolution(), Resolution::RelativePath);
}

/// A written protocol shadows the family, because the family says which
/// grammar the author wrote and the protocol says what the address is.
/// Deriving the reading from the family alone claimed `https://example.com`
/// was a relative path and `[[https://x|Docs]]` a suffix address — two false
/// facts, and the second one licensed rewriting somebody else's URL.
#[test]
fn a_written_protocol_shadows_the_family() {
    assert_eq!(
        only("[Home](https://example.com)\n").resolution(),
        Resolution::Protocol("https")
    );
    assert_eq!(
        BodyScan::new("[[https://x|Docs]]\n").wikilinks()[0].resolution(),
        Resolution::Protocol("https")
    );
    // The reserved identifier routes through the same arm; it is a written
    // protocol like any other until a layer means something by it.
    assert_eq!(
        BodyScan::new("[[vault://Note]]\n").wikilinks()[0].resolution(),
        Resolution::Protocol("vault")
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
}

/// One parser is the authority, so the link universe is identical whichever
/// accessor computes it. `BodyScan::links` is exactly its wikilinks plus its
/// Markdown links; `Document::links` and `Document::wikilinks` are the same
/// facts with every span rebased by one `body_start` and nothing else changed.
///
/// Two readings of the same bytes drift, and the drift is invisible until it
/// corrupts something — so this is asserted over a corpus built to make them
/// drift: overlapping and nested tokens, code of all three kinds, escapes,
/// protocols, the characterize-only forms, and multi-byte text.
#[test]
fn the_link_universe_is_the_same_whichever_accessor_computes_it() {
    let body = "\
[Md](./a.md) then [[Wiki]] then [[a]](b) and [see [[Inner]] here](./out.md)\n\
\n\
`[[incode]]` and `[t](incode.md)` and a real [[Tight|Shown]]\n\
\n\
```\n[[fenced]]\n[t](fenced.md)\n```\n\
\n\
    [[indented]]\n\
\n\
![alt](picture.png) and <https://example.com> and [text][label]\n\
\n\
[label]: ./definition.md#faq\n\
\n\
[t](note\\#draft.md) and [t](<a#b.md>) and [[vault://Proto#H|T]]\n\
\n\
[[日本語#見出し]] and [多字節](./路径.md#锚) and ![[ Padded | Title ]]\n\
\n\
[[Straddling\nAcross]] and [[a^b]] and [t](x.md \"Tooltip\")\n";
    let source = format!("---\ntitle: Note\ntags:\n  - alpha\n---\n\n{body}");
    let offset = source.len() - body.len();

    let scan = BodyScan::new(body);
    let document = Document::parse(&source);

    let both: Vec<Link> = {
        let mut links = scan.wikilinks();
        links.extend(
            scan.links()
                .into_iter()
                .filter(|link| link.family == LinkFamily::Markdown),
        );
        links.sort_by_key(|link| link.span.byte_offset);
        links
    };
    assert_eq!(scan.links(), both, "links() is its two families");
    assert!(!scan.links().is_empty(), "the corpus produces links");

    let rebase = |mut link: Link| {
        link.span.byte_offset += offset;
        link.span.line += source[..offset].lines().count();
        link
    };
    let expected: Vec<Link> = scan.links().into_iter().map(rebase).collect();
    let from_document = document.links();
    assert_eq!(
        from_document
            .iter()
            .map(|link| (&link.raw, link.family, link.span.byte_offset))
            .collect::<Vec<_>>(),
        expected
            .iter()
            .map(|link| (&link.raw, link.family, link.span.byte_offset))
            .collect::<Vec<_>>(),
        "Document::links is BodyScan::links rebased"
    );
    for link in &from_document {
        assert_eq!(&source[link.range()], link.raw);
    }

    assert_eq!(
        document
            .wikilinks()
            .iter()
            .map(|link| link.span.byte_offset)
            .collect::<Vec<_>>(),
        scan.wikilinks()
            .iter()
            .map(|link| link.span.byte_offset + offset)
            .collect::<Vec<_>>(),
        "Document::wikilinks is BodyScan::wikilinks rebased"
    );
    assert_eq!(
        document
            .links()
            .into_iter()
            .filter(|link| link.family == LinkFamily::Wikilink)
            .collect::<Vec<_>>(),
        document.wikilinks(),
        "the wikilinks in links() are the wikilinks"
    );
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

/// A wikilink rewrite refuses a Markdown link even though its stem is
/// nameable. Its target is relative to the document it sits in, so one move
/// produces different bytes per referencing file — a per-document computation
/// rather than a splice, and it belongs to the layer that knows where
/// documents are. The span is what that layer writes over; it is not a licence
/// for this one to.
#[test]
fn a_markdown_link_is_not_rewritten_by_the_wikilink_splicer() {
    let link = only("[t](./old.md)\n");
    assert_eq!(link.stem_range, Some(4..12));
    assert_eq!(reconstruct_wikilink(&link, "./new.md"), None);
    assert_eq!(reconstruct_wikilink(&link, "new"), None);

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

/// The frontmatter side of that contract has its own accessor, mirroring
/// `frontmatter_tags`: every string the block holds is read, not just one
/// field, because writing the wikilink form is what opts a property into the
/// link graph. Spans are the source's, so a caller takes a reader to the
/// property the link was written in.
#[test]
fn a_document_reports_the_wikilinks_its_frontmatter_values_carry() {
    let source = "---\ntitle: Note\nrelated: \"[[Other]]\"\nsee:\n  - \"[[A]]\"\n  \
                  - \"[Md](./x.md) and [[B]]\"\n---\n\n[[Body]]\n";
    let document = Document::parse(source);

    let links = document.frontmatter_wikilinks();
    assert_eq!(targets(&links), ["Other", "A", "B"]);
    for link in &links {
        assert_eq!(&source[link.range()], link.raw);
        assert_eq!(link.family, LinkFamily::Wikilink);
    }
    assert_eq!(links[0].span.line, 3);
    assert_eq!(links[1].span.line, 5);
    assert_eq!(links[2].span.line, 6);

    // The body's own links are a separate answer about a separate home.
    assert_eq!(targets(&document.links()), ["Body"]);
}

/// The stated limitation. A flow sequence's items have no separately nameable
/// bytes — the same reason a flow value has no value span — so a link written
/// in one is reported by nothing here rather than by a span that is not
/// certainly right. `field_texts` plus `parse_wikilinks_in_text` reads it
/// without one.
#[test]
fn a_frontmatter_link_with_no_nameable_bytes_is_absent_rather_than_guessed() {
    let document = Document::parse("---\nsee: [\"[[A]]\", \"[[B]]\"]\n---\n");
    assert!(document.frontmatter_wikilinks().is_empty());

    let unnamed: Vec<Link> = document
        .field_texts()
        .iter()
        .flat_map(|text| norn_text::parse_wikilinks_in_text(text.text))
        .collect();
    assert_eq!(targets(&unnamed), ["A", "B"]);
}

// ── The scope fence, characterized ───────────────────────────────────────

/// An image is a different element to the parser, and a transclusion in this
/// vocabulary is `![[…]]`. Recognized, and no link fact.
#[test]
fn an_image_is_recognized_and_produces_no_link() {
    assert!(markdown_links("![alt](picture.png)\n").is_empty());
    assert!(BodyScan::new("![alt](picture.png)\n").links().is_empty());
}

/// A link written inside an image's alt text is a link, and it is reported. It
/// is a construct of an implemented family with its own target and its own
/// bytes, and the image around it is still not a link — the fence is about
/// which written form produces a fact, not about where in the document it was
/// written.
#[test]
fn a_link_inside_an_images_alt_text_is_still_a_link() {
    let links = BodyScan::new("![see [here](a.md)](i.png)\n").links();
    assert_eq!(targets(&links), ["a.md"]);
    assert_eq!(links[0].raw, "[here](a.md)");
    assert_eq!(links[0].title.as_deref(), Some("here"));
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
/// backslash escape is resolved. What the parse does *not* answer is which
/// hash the author wrote, so a destination whose bytes it changed carries no
/// stem sub-span — a span that is not certainly right is absent rather than
/// guessed.
#[test]
fn a_destination_is_the_parsers_and_a_rewritten_one_carries_no_stem_span() {
    let angled = only("[t](<my file.md>)\n");
    assert_eq!(angled.target, "my file.md");
    assert_eq!(angled.raw, "[t](<my file.md>)");
    assert_eq!(angled.stem_range, None);

    let escaped = only("[t](note\\#draft.md)\n");
    assert_eq!(escaped.stem_range, None);
}

/// The fragment split runs over the destination's *source* bytes. An escaped
/// `\#` is a literal hash the parse resolves, so splitting its answer turned a
/// real filename into a note with an anchor; an unescaped `#` splits wherever
/// it is written, angle brackets included, which is the convention every URL
/// reader follows.
#[test]
fn the_fragment_split_reads_the_hash_the_author_wrote() {
    let escaped = only("[t](note\\#draft.md)\n");
    assert_eq!(escaped.target, "note#draft.md");
    assert_eq!(escaped.anchor, None);
    assert_eq!(escaped.block_ref, None);

    let angled = only("[t](<a#b.md>)\n");
    assert_eq!(angled.target, "a");
    assert_eq!(angled.anchor.as_deref(), Some("b.md"));

    // An escape after the fragment opens belongs to the fragment, and the
    // first unescaped hash is still the one that splits.
    let both = only("[t](a\\#b#c)\n");
    assert_eq!(both.target, "a#b");
    assert_eq!(both.anchor.as_deref(), Some("c"));
}

/// A destination standing in the token exactly as the parse read it has
/// nameable bytes, and the stem's own sub-span is what a rewrite at the layer
/// that owns relative paths writes over. Re-emitting a destination from its
/// parts is lossy, which is why the span rather than the parts is the answer.
#[test]
fn a_literal_destination_carries_the_stems_sub_span() {
    for (body, stem) in [
        ("[t](./note.md#Heading)\n", "./note.md"),
        ("[t](note.md)\n", "note.md"),
        ("[t](note%23draft.md)\n", "note%23draft.md"),
        ("[Home](https://example.com/page#top)\n", "example.com/page"),
        ("[t](x.md \"Tooltip\")\n", "x.md"),
        ("[t](#frag)\n", ""),
    ] {
        let link = only(body);
        let range = link
            .stem_range
            .clone()
            .unwrap_or_else(|| panic!("a literal destination has a stem span in {body:?}"));
        assert_eq!(&link.raw[range], stem, "stem span of {body:?}");
        assert_eq!(link.target, stem, "target of {body:?}");
    }
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
///
/// So link ranges overlap, and across the families they nest. The order is
/// what keeps that usable: ascending by start, wikilink first on a tie, which
/// is what makes the same link the same row twice.
#[test]
fn a_token_that_satisfies_both_grammars_reports_both_overlapping_facts() {
    let body = "[[a]](b)\n";
    let links = BodyScan::new(body).links();
    assert_eq!(targets(&links), ["a", "b"]);
    assert_eq!(
        links.iter().map(|link| link.family).collect::<Vec<_>>(),
        [LinkFamily::Wikilink, LinkFamily::Markdown]
    );
    assert_eq!(links[0].range(), 0..5);
    assert_eq!(links[1].range(), 0..8);

    // Nesting: a wikilink written inside a Markdown link's bracket text.
    let nested = BodyScan::new("[see [[Inner]] here](./out.md)\n").links();
    assert_eq!(targets(&nested), ["./out.md", "Inner"]);
    assert_eq!(nested[0].range(), 0..30);
    assert_eq!(nested[1].range(), 5..14);
}
