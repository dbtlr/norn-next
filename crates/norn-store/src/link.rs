//! How a stored link reaches the documents it may name: the addressing its
//! syntax selects, and the keys the link index holds for it.
//!
//! A link is stored as it was written, and what it names is decided at read
//! time against the vault as it stands. What this module decides is a function
//! of the link and the document holding it alone, so it is decided once, at
//! the write: which reading of the target applies, and — for a target that can
//! name a document — the keys the link index holds it under, which every read
//! of what the link names seeks.
//!
//! # The addressing is the wire's
//!
//! Which reading applies is [`norn_wire::LinkAddress`], the one addressing
//! selector: protocol first, family second. [`Addressing`] maps it onto what
//! the store reads. A link addressed elsewhere is held under no key. A suffix
//! address is read through the one resolver, whatever its leaf carries: a
//! target naming an attachment still resolves to a document carrying that
//! name, and is judged by what it resolves to.
//!
//! A path is read by URL rules: split into segments on the separator first,
//! each segment then percent-decoded — so an encoded separator is a character
//! inside its segment, and a segment holding one names no document — and
//! joined to the directory of the document holding the link, or to the vault
//! root, with `.` and `..` segments folded in and nothing reduced, so
//! `[t](foo)` names the path `foo` and never `foo.md`. Only a segment written
//! as `.` or `..` is folded in: one that decodes to either is data, which no
//! document path holds, so `[t](%2E%2E/foo.md)` names no document. A path that
//! climbs above the vault root, or that is no document path, names no
//! document. An empty target names the document holding the link.
//!
//! A `vault://` link is read from the vault root under its own family's
//! rules. A Markdown one is a path, as above. A wikilink's is a rooted name:
//! read by the wikilink grammar, with nothing decoded or cut off, it names
//! exactly the root path each reduction of its leaf spells — the stem as
//! written, and the stem with its extension stripped, each with the document
//! extension appended — mirroring a suffix wikilink's own two reductions, so
//! `[[vault://Notes]]` and `[[vault://Notes.md]]` name `Notes.md`,
//! `[[vault://v1.2]]` names `v1.2.md` or `v1.md`, and `[[vault://Deep]]`
//! never reaches `sub/Deep.md`.
//!
//! # The keys are what every read of a link seeks
//!
//! A suffix address's keys are its probe's prefixes — one per reduction of a
//! dotted leaf — in the segment-reversed form a document's suffix key takes,
//! beside the target's segment count its ambiguity-ignore test reads. A path's
//! key is the path itself, and a rooted name's keys are the paths its
//! reductions spell. Each is held raw and with ASCII case folded, so a read
//! seeks the one the store's path order selects. A suffix key always ends in
//! the separator and a path never does, so the two kinds share one key column
//! and no key of one kind equals a key of the other.

use norn_wire::{DOCUMENT_EXTENSION, LinkAddress};

use crate::facts::LinkFact;
use crate::path::{DocumentPath, SuffixKey, fold_ascii_case, leaf_stem, suffix_probe};

/// The separator between segments, in a target and in a path alike.
const SEPARATOR: char = '/';

/// How a link's target reaches documents, as the store reads it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Addressing<'a> {
    /// The link addresses no document, so it is held under no key.
    Elsewhere,
    /// A suffix address, resolved through the one resolver.
    Suffix(&'a str),
    /// The vault paths the target names, each resolved exactly: none where it
    /// names no vault path, one for a path, and one per reduction for a
    /// rooted name.
    Paths(Vec<String>),
}

impl<'a> Addressing<'a> {
    /// How `link`, held by the document at `holder`, reaches documents: the
    /// wire's address, with a path read to the vault path it names.
    pub(crate) fn of(link: &'a LinkFact, holder: &DocumentPath) -> Self {
        match LinkAddress::of(link.family.wire(), link.protocol.as_deref(), &link.target) {
            LinkAddress::Elsewhere => Addressing::Elsewhere,
            LinkAddress::HoldingDocument => Addressing::Paths(vec![holder.as_str().to_string()]),
            LinkAddress::Suffix(target) => Addressing::Suffix(target),
            LinkAddress::RootedName(name) => Addressing::Paths(rooted_name(name)),
            LinkAddress::Relative(path) => {
                Addressing::Paths(joined(directory_of(holder), path).into_iter().collect())
            }
            LinkAddress::Rooted(path) => {
                Addressing::Paths(joined(Vec::new(), path).into_iter().collect())
            }
        }
    }
}

/// One key the link index holds a link under.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct LinkKey {
    /// The key as the target spells it.
    pub(crate) key: String,
    /// The key with ASCII case folded.
    pub(crate) folded_key: String,
    /// How many segments a suffix address spells, which is what its
    /// ambiguity-ignore test reads, and `None` for a path's key.
    pub(crate) segments: Option<u64>,
}

/// The keys the link index holds `link`, held by the document at `holder`,
/// under: none for a link addressed elsewhere or naming no vault path, a
/// suffix address's prefixes, or each path it names.
pub(crate) fn link_keys(link: &LinkFact, holder: &DocumentPath) -> Vec<LinkKey> {
    match Addressing::of(link, holder) {
        Addressing::Elsewhere => Vec::new(),
        Addressing::Paths(paths) => paths
            .into_iter()
            .map(|path| LinkKey {
                folded_key: fold_ascii_case(&path),
                key: path,
                segments: None,
            })
            .collect(),
        Addressing::Suffix(target) => {
            let Ok(probe) = suffix_probe(target) else {
                return Vec::new();
            };
            let segments = target.split(SEPARATOR).count() as u64;
            probe
                .ranges()
                .map(|(lower, _)| LinkKey {
                    key: lower.to_string(),
                    folded_key: fold_ascii_case(lower),
                    segments: Some(segments),
                })
                .collect()
        }
    }
}

/// The keys, in the key space `key` selects, a link that could name
/// `document` is held under: each segment-aligned prefix of the document's
/// suffix key, which a suffix address naming it opens, and the document's
/// path, which a path naming it is.
///
/// Every link whose resolution holds the document is held under one of these,
/// so a seek of them reaches every such link; which of the links it reaches
/// resolve to the document alone is the seek's own question.
pub(crate) fn keys_naming(document: &DocumentPath, key: SuffixKey) -> Vec<String> {
    let (suffix, path) = match key {
        SuffixKey::Raw => (
            document.suffix_key().to_string(),
            document.as_str().to_string(),
        ),
        SuffixKey::Folded => (
            document.folded_suffix_key().to_string(),
            fold_ascii_case(document.as_str()),
        ),
    };
    let mut keys: Vec<String> = suffix
        .match_indices(SEPARATOR)
        .map(|(at, _)| suffix[..=at].to_string())
        .collect();
    keys.push(path);
    keys
}

/// The root paths a rooted wikilink's `name` spells, one per reduction of its
/// leaf — mirroring a suffix wikilink's own two reductions: the stem as
/// written, with the document extension appended, and, where the leaf carries
/// an extension, the stem with that extension stripped, with the document
/// extension appended in its place. `leaf_stem` is what decides whether the
/// leaf carries an extension and where it ends, the same rule a suffix
/// address's own reductions strip by, so `[[vault://v1.2]]` names `v1.2.md`
/// or `v1.md` — never the literal `v1.2`. A name the wikilink grammar
/// refuses — an empty segment, a `.` or `..` segment — names none, and so does
/// a spelling the document path grammar refuses. Nothing in the name is
/// decoded or cut off.
fn rooted_name(name: &str) -> Vec<String> {
    if suffix_probe(name).is_err() {
        return Vec::new();
    }
    let (ancestors, leaf) = name.rsplit_once(SEPARATOR).unwrap_or(("", name));
    let stem = leaf_stem(leaf);
    let as_a_stem = format!("{name}.{DOCUMENT_EXTENSION}");
    // The leaf carries an extension exactly where its stem is shorter than
    // it, which is the one case with a second reduction.
    let stripped = (stem != leaf).then(|| {
        if ancestors.is_empty() {
            format!("{stem}.{DOCUMENT_EXTENSION}")
        } else {
            format!("{ancestors}{SEPARATOR}{stem}.{DOCUMENT_EXTENSION}")
        }
    });
    std::iter::once(as_a_stem)
        .chain(stripped)
        .filter(|path| DocumentPath::new(path).is_ok())
        .collect()
}

/// The segments of the directory holding the document at `holder`.
fn directory_of(holder: &DocumentPath) -> Vec<String> {
    holder
        .as_str()
        .rsplit_once(SEPARATOR)
        .map(|(directory, _)| directory.split(SEPARATOR).map(str::to_string).collect())
        .unwrap_or_default()
}

/// The vault path `path` names read from the directory whose segments are
/// `from` — the empty list being the vault root — or `None` where it names
/// none.
///
/// The path is split into segments first, and each is then percent-decoded,
/// so an encoded separator is a character inside its segment: a decoding that
/// holds a separator, or that is not UTF-8, names no path. Empty and `.`
/// segments are dropped, and `..` climbs one directory; a climb above the
/// vault root names no path, and so does a result the document path grammar
/// refuses — a segment that decodes to `.` or `..` among them, since only a
/// segment written as one is dropped or climbs.
pub(crate) fn joined(mut from: Vec<String>, path: &str) -> Option<String> {
    for segment in path.split(SEPARATOR) {
        match segment {
            "" | "." => {}
            ".." => {
                from.pop()?;
            }
            written => {
                let decoded = percent_decoded(written)?;
                if decoded.contains(SEPARATOR) {
                    return None;
                }
                from.push(decoded);
            }
        }
    }
    let path = from.join("/");
    DocumentPath::new(&path).ok().map(|_| path)
}

/// `text` with every `%` followed by two hexadecimal digits read as the byte
/// they spell, or `None` where the bytes that makes are not UTF-8. A `%` not
/// followed by two hexadecimal digits is itself.
fn percent_decoded(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while at < bytes.len() {
        let spelled = (bytes[at] == b'%')
            .then(|| bytes.get(at + 1..at + 3))
            .flatten()
            .filter(|digits| digits.iter().all(u8::is_ascii_hexdigit))
            .and_then(|digits| std::str::from_utf8(digits).ok())
            .and_then(|digits| u8::from_str_radix(digits, 16).ok());
        match spelled {
            Some(byte) => {
                decoded.push(byte);
                at += 3;
            }
            None => {
                decoded.push(bytes[at]);
                at += 1;
            }
        }
    }
    String::from_utf8(decoded).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(path: &str) -> DocumentPath {
        DocumentPath::new(path).expect("a document path")
    }

    /// A link of `family`, written with `protocol` and `target`.
    fn written(family: crate::facts::LinkFamily, protocol: Option<&str>, target: &str) -> LinkFact {
        LinkFact {
            family,
            embed: false,
            protocol: protocol.map(str::to_string),
            target: target.to_string(),
            title: None,
            anchor: None,
            block_ref: None,
            span: crate::facts::Span {
                line: 1,
                column: 1,
                byte_offset: 0,
            },
        }
    }

    /// The vault path a Markdown link to `target` names from `holder`.
    fn named(holder: &DocumentPath, target: &str) -> Option<String> {
        let link = written(crate::facts::LinkFamily::Markdown, None, target);
        match Addressing::of(&link, holder) {
            Addressing::Paths(paths) => {
                assert!(paths.len() <= 1, "`{target}` names {paths:?}");
                paths.into_iter().next()
            }
            other => panic!("`{target}` is no path: {other:?}"),
        }
    }

    /// The keys a `vault://` link of `family` to `target` is held under.
    fn rooted_keys(family: crate::facts::LinkFamily, target: &str) -> Vec<String> {
        link_keys(
            &written(family, Some("vault"), target),
            &at("deep/in/doc.md"),
        )
        .into_iter()
        .inspect(|key| assert_eq!(key.segments, None, "`{target}` is keyed by a suffix"))
        .map(|key| key.key)
        .collect()
    }

    /// A `vault://` wikilink is keyed by the root paths its reductions spell —
    /// the target with the document extension appended, and, where its leaf
    /// carries an extension, the target with that extension stripped and the
    /// document extension appended in its place — with nothing decoded or cut
    /// off, and a target the wikilink grammar refuses names none. A
    /// `vault://` Markdown link is keyed by the one root path it decodes to,
    /// its query cut off and never reduced. Neither is read from the holding
    /// document's directory.
    #[test]
    fn a_vault_link_is_keyed_by_the_root_paths_its_family_spells() {
        use crate::facts::LinkFamily::{Markdown, Wikilink};
        for (family, target, keys) in [
            (Wikilink, "Notes", &["Notes.md"][..]),
            (Wikilink, "Notes.md", &["Notes.md.md", "Notes.md"]),
            (Wikilink, "v1.2", &["v1.2.md", "v1.md"]),
            (Wikilink, "a.b/Notes", &["a.b/Notes.md"]),
            (Wikilink, "my%20note", &["my%20note.md"]),
            (Wikilink, "Notes?x", &["Notes?x.md"]),
            (Wikilink, "notes/", &[]),
            (Wikilink, "/Notes", &[]),
            (Wikilink, "a/../Notes", &[]),
            (Wikilink, "./Notes", &[]),
            (Wikilink, "..", &[]),
            (Markdown, "Notes", &["Notes"]),
            (Markdown, "Notes.md", &["Notes.md"]),
            (Markdown, "my%20note.md?x=1", &["my note.md"]),
            (Markdown, "/a/./b.md", &["a/b.md"]),
            (Markdown, "../Notes.md", &[]),
        ] {
            assert_eq!(rooted_keys(family, target), keys, "{family:?} `{target}`");
        }
    }

    /// **A segment that decodes to `.` or `..` is data**: it is decoded after
    /// the path is split, so it names neither the directory it stands in nor
    /// its parent, and a document path holds no such segment, so the link
    /// names no document.
    #[test]
    fn a_segment_decoding_to_a_dot_name_names_no_document() {
        let holder = at("a/b/doc.md");
        for target in [
            "%2E%2E/c.md",
            "%2e%2e/c.md",
            "%2E/c.md",
            "sub/%2E%2E/c.md",
            "sub/%2E",
            "%2E%2E",
            "/%2E%2E/c.md",
        ] {
            assert_eq!(named(&holder, target), None, "`{target}`");
        }
        assert_eq!(named(&holder, "sub/../c.md").as_deref(), Some("a/b/c.md"));
    }

    /// A Markdown target is joined to its document's directory, or to the
    /// vault root where it is rooted, split before it is decoded, its query
    /// cut off, and never reduced.
    #[test]
    fn a_markdown_target_names_the_path_it_joins_to() {
        let holder = at("a/b/doc.md");
        for (target, named_path) in [
            ("./c.md", Some("a/b/c.md")),
            ("c.md", Some("a/b/c.md")),
            ("../x/y.md", Some("a/x/y.md")),
            ("../../top.md", Some("top.md")),
            ("/x/y.md", Some("x/y.md")),
            ("my%20note.md", Some("a/b/my note.md")),
            ("../x/my%20note.md", Some("a/x/my note.md")),
            ("foo", Some("a/b/foo")),
            ("./sub/./c.md", Some("a/b/sub/c.md")),
            ("q.md?x=1", Some("a/b/q.md")),
            ("sub%2Fc.md", None),
            ("../../../escape.md", None),
            ("..", Some("a")),
            ("../..", None),
            ("%ff.md", None),
            ("back\\slash.md", None),
            ("", Some("a/b/doc.md")),
        ] {
            assert_eq!(
                named(&holder, target).as_deref(),
                named_path,
                "`{target}` from `{}`",
                holder.as_str()
            );
        }
        assert_eq!(named(&at("top.md"), "../x.md"), None);
        assert_eq!(named(&at("top.md"), "x.md").as_deref(), Some("x.md"));
    }

    /// A document is named by each segment-aligned prefix of its suffix key
    /// and by its path, in the key space the root probes.
    #[test]
    fn a_document_is_named_by_its_suffix_prefixes_and_its_path() {
        let document = at("Docs/Norn/Glossary.md");
        assert_eq!(
            keys_naming(&document, SuffixKey::Raw),
            [
                "Glossary/",
                "Glossary/Norn/",
                "Glossary/Norn/Docs/",
                "Docs/Norn/Glossary.md"
            ]
        );
        assert_eq!(
            keys_naming(&document, SuffixKey::Folded),
            [
                "glossary/",
                "glossary/norn/",
                "glossary/norn/docs/",
                "docs/norn/glossary.md"
            ]
        );
    }

    /// A `%` that spells no byte is itself, and a spelled byte may be any
    /// byte, a separator among them.
    #[test]
    fn a_percent_spells_a_byte_only_before_two_hexadecimal_digits() {
        assert_eq!(percent_decoded("100%").as_deref(), Some("100%"));
        assert_eq!(percent_decoded("%zz%4").as_deref(), Some("%zz%4"));
        assert_eq!(percent_decoded("100%+1").as_deref(), Some("100%+1"));
        assert_eq!(percent_decoded("%-1%+f").as_deref(), Some("%-1%+f"));
        assert_eq!(percent_decoded("a%2Fb%20c").as_deref(), Some("a/b c"));
        assert_eq!(percent_decoded("caf%C3%A9").as_deref(), Some("café"));
        assert_eq!(percent_decoded("%C3"), None);
    }
}
