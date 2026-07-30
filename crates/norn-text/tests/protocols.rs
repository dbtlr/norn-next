//! Protocol recognition: which target prefixes are read as a protocol, which
//! are ordinary bytes, and what recognition is forbidden from changing.
//!
//! The sentinel is the whole `://`, the identifier is a lowercase RFC 3986
//! scheme, and recognition is the entire feature — nothing here resolves,
//! routes or validates anything, and no protocol is ever supplied or removed.

use norn_text::{
    BodyScan, Link, LinkFamily, Resolution, parse_wikilinks_in_text, reconstruct_wikilink,
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

fn only_markdown(body: &str) -> Link {
    let mut links = BodyScan::new(body)
        .links()
        .into_iter()
        .filter(|link| link.family == LinkFamily::Markdown);
    let link = links.next().expect("one markdown link");
    assert!(
        links.next().is_none(),
        "expected exactly one markdown link in {body:?}"
    );
    link
}

// ── What is a protocol ───────────────────────────────────────────────────

#[test]
fn a_scheme_followed_by_the_sentinel_is_a_protocol() {
    let link = only("[[vault://Note]]");
    assert_eq!(link.protocol.as_deref(), Some("vault"));
    assert_eq!(link.target, "Note");
    assert_eq!(link.family, LinkFamily::Wikilink);
}

/// `vault` is one identifier in the grammar and gets no special handling here.
/// Every scheme shape RFC 3986 allows — a letter, then letters, digits, `+`,
/// `.` and `-` — is recognized the same way.
#[test]
fn every_lowercase_scheme_identifier_is_recognized_alike() {
    for (raw, protocol, target) in [
        ("[[vault://Note]]", "vault", "Note"),
        ("[[https://example.com]]", "https", "example.com"),
        ("[[a://b]]", "a", "b"),
        ("[[svn+ssh://host/repo]]", "svn+ssh", "host/repo"),
        ("[[view-source://x]]", "view-source", "x"),
        ("[[z39.50r://x]]", "z39.50r", "x"),
    ] {
        let link = only(raw);
        assert_eq!(
            link.protocol.as_deref(),
            Some(protocol),
            "protocol of {raw}"
        );
        assert_eq!(link.target, target, "target of {raw}");
    }
}

/// The sentinel is the whole `://`. A single colon is how people write titles,
/// dates and notes-about-notes, so reading one as a protocol would hide a real
/// target behind a prefix nobody wrote. Recognizing too little is recoverable
/// — the target still reads as itself — which is why the grammar leans that
/// way.
#[test]
fn a_single_colon_form_is_an_ordinary_target() {
    for (raw, target) in [
        ("[[note:draft]]", "note:draft"),
        ("[[todo: buy milk]]", "todo: buy milk"),
        ("[[mailto:person@example.com]]", "mailto:person@example.com"),
        ("[[doi:10.1000/182]]", "doi:10.1000/182"),
        ("[[vault:/Note]]", "vault:/Note"),
        ("[[10:30 standup]]", "10:30 standup"),
    ] {
        let link = only(raw);
        assert_eq!(link.protocol, None, "protocol of {raw}");
        assert_eq!(link.target, target, "target of {raw}");
    }
}

/// Only lowercase identifiers are recognized, and only well-formed ones.
#[test]
fn an_uppercase_or_malformed_scheme_is_an_ordinary_target() {
    for raw in [
        "[[HTTPS://example.com]]",
        "[[Vault://Note]]",
        "[[vaulT://Note]]",
        "[[://Note]]",
        "[[1vault://Note]]",
        "[[_vault://Note]]",
        "[[va ult://Note]]",
        "[[vau_lt://Note]]",
    ] {
        let link = only(raw);
        assert_eq!(link.protocol, None, "protocol of {raw}");
        assert_eq!(link.target, &raw[2..raw.len() - 2], "target of {raw}");
    }
}

/// A sentinel with nothing after it addresses nothing, so there is no protocol
/// to recognize and the whole string stays the target.
#[test]
fn a_protocol_with_an_empty_stem_is_an_ordinary_target() {
    let link = only("[[vault://]]");
    assert_eq!(link.protocol, None);
    assert_eq!(link.target, "vault://");
}

/// The sentinel is a prefix, not a separator, so whitespace behind it is
/// padding on the stem's left the way `[[note #Head]]`'s space is padding on
/// its right. Trimming one side only left the target `" Note"`, which no
/// resolver matches and which `wikilink_target_is_representable` refuses — a
/// token that could not round-trip to itself.
#[test]
fn whitespace_behind_the_sentinel_is_padding_and_not_the_stem() {
    let link = only("[[vault:// Note]]");
    assert_eq!(link.protocol.as_deref(), Some("vault"));
    assert_eq!(link.target, "Note");
    assert!(wikilink_target_is_representable(&link.target));
    assert_eq!(
        reconstruct_wikilink(&link, &link.target).as_deref(),
        Some("[[vault:// Note]]"),
        "the padding survives an identity rewrite"
    );
    assert_eq!(
        reconstruct_wikilink(&link, "Renamed").as_deref(),
        Some("[[vault:// Renamed]]")
    );

    // Padding on both sides of the whole target composes with it.
    let padded = only("[[ vault://  Note #Head | Shown ]]");
    assert_eq!(padded.target, "Note");
    assert_eq!(padded.anchor.as_deref(), Some("Head"));
    assert_eq!(
        reconstruct_wikilink(&padded, "New").as_deref(),
        Some("[[ vault://  New #Head | Shown ]]")
    );
}

/// Only the first sentinel splits; the rest belongs to the stem.
#[test]
fn only_the_first_sentinel_splits() {
    let link = only("[[vault://https://example.com]]");
    assert_eq!(link.protocol.as_deref(), Some("vault"));
    assert_eq!(link.target, "https://example.com");
}

// ── Recognition composes with the rest of the grammar ────────────────────

#[test]
fn a_protocol_composes_with_the_fragment_the_title_and_the_embed_marker() {
    let anchored = only("[[vault://Note#Heading|Shown]]");
    assert_eq!(anchored.protocol.as_deref(), Some("vault"));
    assert_eq!(anchored.target, "Note");
    assert_eq!(anchored.anchor.as_deref(), Some("Heading"));
    assert_eq!(anchored.title.as_deref(), Some("Shown"));

    let referenced = only("[[atlas://Note#^blk]]");
    assert_eq!(referenced.protocol.as_deref(), Some("atlas"));
    assert_eq!(referenced.block_ref.as_deref(), Some("blk"));

    let embedded = only("![[vault://Image.png]]");
    assert!(embedded.embed);
    assert_eq!(embedded.protocol.as_deref(), Some("vault"));
    assert_eq!(embedded.target, "Image.png");
}

/// One grammar, both families: the protocol column means the same thing
/// whichever form the author wrote.
#[test]
fn an_inline_markdown_destination_is_read_by_the_same_grammar() {
    let external = only_markdown("[Home](https://example.com/page#top)\n");
    assert_eq!(external.protocol.as_deref(), Some("https"));
    assert_eq!(external.target, "example.com/page");
    assert_eq!(external.anchor.as_deref(), Some("top"));

    let relative = only_markdown("[Note](./note.md)\n");
    assert_eq!(relative.protocol, None);
    assert_eq!(relative.target, "./note.md");

    let colon = only_markdown("[Mail](mailto:person@example.com)\n");
    assert_eq!(colon.protocol, None);
    assert_eq!(colon.target, "mailto:person@example.com");
}

// ── Losslessness ─────────────────────────────────────────────────────────

/// `[[x]]` and `[[vault://x]]` are two facts, and stay two facts. Nothing
/// supplies an absent protocol and nothing drops a written one, because a
/// layer that normalized either direction would make the two indistinguishable
/// to every layer above it.
#[test]
fn an_absent_protocol_and_a_written_one_are_different_facts() {
    let bare = only("[[Note]]");
    let written = only("[[vault://Note]]");

    assert_eq!(bare.protocol, None);
    assert_eq!(written.protocol.as_deref(), Some("vault"));
    assert_eq!(bare.target, written.target);
    assert_ne!(bare, written);

    assert_eq!(bare.raw, "[[Note]]");
    assert_eq!(written.raw, "[[vault://Note]]");
}

/// The bytes survive recognition: reconstructing a vault-addressed link with
/// its own stem returns exactly what was written, padded or not.
#[test]
fn recognition_does_not_move_a_byte() {
    for raw in [
        "[[vault://Note]]",
        "![[vault://Note]]",
        "[[ vault://Note | Shown ]]",
        "[[vault://Note#Heading|Shown]]",
    ] {
        let link = only(raw);
        assert_eq!(
            reconstruct_wikilink(&link, &link.target).as_deref(),
            Some(raw),
            "reconstructing {raw:?}"
        );
    }

    // An external address is recognized just as losslessly and rewritten by
    // nobody, so its bytes survive by never being touched.
    let external = only("[[https://example.com/a/b#frag]]");
    assert_eq!(external.raw, "[[https://example.com/a/b#frag]]");
    assert_eq!(reconstruct_wikilink(&external, &external.target), None);
}

/// A rewrite changes the stem and leaves the protocol where the author put it.
/// Changing a protocol is not a target rewrite.
#[test]
fn a_rewrite_keeps_the_protocol_the_author_wrote() {
    assert_eq!(
        reconstruct_wikilink(&only("[[vault://Old|Shown]]"), "New").as_deref(),
        Some("[[vault://New|Shown]]")
    );
    assert_eq!(
        reconstruct_wikilink(&only("[[ vault://Old #Heading]]"), "New").as_deref(),
        Some("[[ vault://New #Heading]]")
    );
}

/// A rewrite of a non-`vault` protocol's stem is refused. `[[https://Old
/// Note|Docs]]` has a stem span, and splicing a rename over it edits somebody
/// else's URL — the same corruption the target side already refuses, applied
/// to the link being rewritten rather than to the name it is rewritten to.
///
/// The reserved identifier is the exception: a `vault://` stem is a vault
/// path, and renaming one is exactly what a rename is.
#[test]
fn a_rewrite_of_a_non_vault_protocol_is_refused() {
    for raw in [
        "[[https://Old Note|Docs]]",
        "[[https://example.com/page]]",
        "![[atlas://Image.png]]",
        "[[svn+ssh://host/repo]]",
    ] {
        let link = only(raw);
        assert!(link.stem_range.is_some(), "stem span of {raw:?}");
        assert_eq!(
            reconstruct_wikilink(&link, "Renamed"),
            None,
            "rewriting {raw:?}"
        );
    }

    assert_eq!(
        reconstruct_wikilink(&only("[[vault://Old]]"), "New").as_deref(),
        Some("[[vault://New]]")
    );
    // A single-colon form is no protocol at all, so it rewrites like the
    // ordinary target it is.
    assert_eq!(
        reconstruct_wikilink(&only("[[note:draft]]"), "New").as_deref(),
        Some("[[New]]")
    );
}

/// A stem that would re-parse as a protocol is not a representable stem: the
/// bytes would read back as a different fact, and this crate refuses rather
/// than emitting something it cannot prove.
#[test]
fn a_protocol_bearing_stem_is_refused_as_a_target() {
    let link = only("[[Old]]");
    for bad in ["vault://x", "https://example.com", "a://b"] {
        assert!(!wikilink_target_is_representable(bad), "{bad:?}");
        assert!(
            reconstruct_wikilink(&link, bad).is_none(),
            "reconstructing with {bad:?}"
        );
    }

    // The forms that are not protocols are ordinary targets and rewrite fine.
    for fine in ["note:draft", "HTTPS://x", "vault:/x"] {
        assert!(wikilink_target_is_representable(fine), "{fine:?}");
    }
}

// ── Recognition is all there is ──────────────────────────────────────────

/// No protocol is *resolved* here. A `vault://` target and an `https://`
/// target are read by the same grammar into the same fact shape, each
/// reporting the scheme it was written with, and the crate offers no way to
/// ask what either one points at.
#[test]
fn a_protocol_is_reported_and_never_resolved() {
    let internal = only("[[vault://Note]]");
    let external = only("[[https://example.com]]");
    assert_eq!(internal.family, external.family);
    assert_eq!(internal.resolution(), Resolution::Protocol("vault"));
    assert_eq!(external.resolution(), Resolution::Protocol("https"));
}

/// `vault` is reserved, and a reservation is a promise about naming rather
/// than a behaviour: written out it is recognized like any other identifier,
/// and left out it stays absent. Supplying it — or reading `[[vault://Note]]`
/// back as `[[Note]]` — is the normalization the reservation exists to keep
/// available to the layer that will mean something by it.
#[test]
fn the_reserved_identifier_is_recognized_and_never_supplied() {
    assert_eq!(only("[[vault://Note]]").protocol.as_deref(), Some("vault"));
    assert_eq!(only("[[Note]]").protocol, None);
    assert_eq!(only("[[vault:Note]]").protocol, None);
    assert_eq!(
        reconstruct_wikilink(&only("[[Note]]"), "Other").as_deref(),
        Some("[[Other]]")
    );
}
