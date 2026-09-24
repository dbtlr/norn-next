//! How a stored link reaches the documents it may name: the addressing its
//! syntax selects, and the keys the link index holds for it.
//!
//! A link is stored as it was written, and what it names is decided at read
//! time against the vault as it stands. What this module decides is a function
//! of the link and the document holding it alone, so it is the same at a write
//! and at every read: which reading of the target applies, and — for a target
//! that can name a document — the keys a links-to seek finds the link by.
//!
//! # The addressing is the link's own
//!
//! **Protocol first, family second, and only a document link is judged.** A
//! link that names no document — written with a protocol, or with a target
//! naming an attachment ([`norn_wire::link_names_a_document`]) — is not
//! judged, resolves to nothing and is held under no key. A wikilink's target
//! is a suffix address, read through the one resolver. A Markdown link's
//! target is a path: percent-decoded, joined to the directory of the document
//! holding it — or to the vault root where it opens with a separator — with
//! `.` and `..` segments folded in, and never reduced, so `[t](foo)` names the
//! path `foo` and never `foo.md`. A path that climbs above the vault root, or
//! that is no document path, names no document. A link with an empty target
//! is a same-document anchor, and names the document holding it.
//!
//! # The keys are the link's side of a links-to seek
//!
//! A suffix address's keys are its probe's prefixes — one per reduction of a
//! dotted leaf — in the segment-reversed form a document's suffix key takes,
//! beside the target's segment count its ambiguity-ignore test reads. A path's
//! key is the path itself. Each is held raw and with ASCII case folded, so a
//! links-to seek reads the one the store's path order selects. A suffix key
//! always ends in the separator and a path never does, so the two kinds share
//! one key column and no key of one kind equals a key of the other.

use norn_wire::link_names_a_document;

use crate::facts::{LinkFact, LinkFamily};
use crate::path::{DocumentPath, SuffixKey, fold_ascii_case, suffix_probe};

/// The separator between segments, in a target and in a path alike.
const SEPARATOR: char = '/';

/// How a link's target reaches documents.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Addressing<'a> {
    /// The link names no document, so no resolution judges it.
    NotJudged,
    /// A suffix address, resolved through the one resolver.
    Suffix(&'a str),
    /// A vault path, resolved exactly, or `None` where the target names no
    /// vault path.
    Path(Option<String>),
}

impl<'a> Addressing<'a> {
    /// How `link`, held by the document at `holder`, reaches documents.
    pub(crate) fn of(link: &'a LinkFact, holder: &DocumentPath) -> Self {
        if !link_names_a_document(link.protocol.as_deref(), &link.target) {
            return Addressing::NotJudged;
        }
        if link.target.is_empty() {
            return Addressing::Path(Some(holder.as_str().to_string()));
        }
        match link.family {
            LinkFamily::Wikilink => Addressing::Suffix(&link.target),
            LinkFamily::Markdown => Addressing::Path(joined(holder, &link.target)),
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
/// under: none for a link that names no document or names no vault path, a
/// suffix address's prefixes, or a path.
pub(crate) fn link_keys(link: &LinkFact, holder: &DocumentPath) -> Vec<LinkKey> {
    match Addressing::of(link, holder) {
        Addressing::NotJudged | Addressing::Path(None) => Vec::new(),
        Addressing::Path(Some(path)) => vec![LinkKey {
            folded_key: fold_ascii_case(&path),
            key: path,
            segments: None,
        }],
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

/// The vault path a Markdown target names from the document at `holder`, or
/// `None` where it names none.
///
/// The target is percent-decoded first, and a decoding that is not UTF-8 names
/// no path. A rooted target is read from the vault root and any other from the
/// holder's directory. Empty and `.` segments are dropped, and `..` climbs one
/// directory; a climb above the vault root names no path, and so does a
/// result the document path grammar refuses.
pub(crate) fn joined(holder: &DocumentPath, target: &str) -> Option<String> {
    let decoded = percent_decoded(target)?;
    let (mut segments, relative): (Vec<&str>, &str) = match decoded.strip_prefix(SEPARATOR) {
        Some(rooted) => (Vec::new(), rooted),
        None => (
            holder
                .as_str()
                .rsplit_once(SEPARATOR)
                .map(|(directory, _)| directory.split(SEPARATOR).collect())
                .unwrap_or_default(),
            decoded.as_str(),
        ),
    };
    for segment in relative.split(SEPARATOR) {
        match segment {
            "" | "." => {}
            ".." => {
                segments.pop()?;
            }
            name => segments.push(name),
        }
    }
    let path = segments.join("/");
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

    /// A Markdown target is joined to its document's directory, or to the
    /// vault root where it is rooted, and never reduced.
    #[test]
    fn a_markdown_target_names_the_path_it_joins_to() {
        let holder = at("a/b/doc.md");
        for (target, named) in [
            ("./c.md", Some("a/b/c.md")),
            ("c.md", Some("a/b/c.md")),
            ("../x/y.md", Some("a/x/y.md")),
            ("../../top.md", Some("top.md")),
            ("/x/y.md", Some("x/y.md")),
            ("my%20note.md", Some("a/b/my note.md")),
            ("../x/my%20note.md", Some("a/x/my note.md")),
            ("foo", Some("a/b/foo")),
            ("./sub/./c.md", Some("a/b/sub/c.md")),
            ("../../../escape.md", None),
            ("..", Some("a")),
            ("../..", None),
            ("%ff.md", None),
            ("back\\slash.md", None),
        ] {
            assert_eq!(
                joined(&holder, target).as_deref(),
                named,
                "`{target}` from `{}`",
                holder.as_str()
            );
        }
        assert_eq!(joined(&at("top.md"), "../x.md"), None);
        assert_eq!(joined(&at("top.md"), "x.md").as_deref(), Some("x.md"));
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
        assert_eq!(percent_decoded("a%2Fb%20c").as_deref(), Some("a/b c"));
        assert_eq!(percent_decoded("caf%C3%A9").as_deref(), Some("café"));
        assert_eq!(percent_decoded("%C3"), None);
    }
}
