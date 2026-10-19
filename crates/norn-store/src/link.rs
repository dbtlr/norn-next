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
//! selector (ADR 0040): the family decides how the target text is taken — a
//! wikilink's as written, a Markdown destination's with its query cut off and
//! each segment percent-decoded — and the form decides where it is read from,
//! alike in both families. [`Addressing`] maps it onto what the store reads.
//! A link addressed elsewhere is held under no key. An unprefixed target is a
//! suffix address, read through the one resolver whatever its leaf carries: a
//! target naming an attachment still resolves to a document carrying that
//! name, and is judged by what it resolves to.
//!
//! A path form is joined to where it is read from: a target whose first
//! segment is `.` or `..` to the folder of the document holding the link, a
//! `/` target and a `vault://` one to the vault root. Empty and `.` segments
//! are dropped and `..` climbs one folder; one that climbs above the vault
//! root names nothing. The joined path names a document by the naming rule:
//! with `.md` appended, and, where its leaf already ends in `.md`, as it
//! stands — so `./Notes` names `Notes.md`, `./Notes.md` names `Notes.md.md`
//! and `Notes.md`, and `./v1.2` names only `v1.2.md`. An empty target names
//! the document holding the link.
//!
//! # The keys are what every read of a link seeks
//!
//! A suffix address opens one segment-reversed prefix, or two where the
//! target ends in `.md`, beside the segment count its ambiguity-ignore test
//! reads; its exact vault paths are the documents whose whole suffix key
//! equals one of those prefixes, so the same keys carry the exact-path-first
//! reading ([`crate::resolve`]). A path's key is its complete-path comparison
//! key, one per path the naming rule names. Every root uses the same
//! comparison key. A suffix key always ends in the separator and a path never
//! does, so the two kinds share one key column and no key of one kind equals
//! a key of the other.

use std::borrow::Cow;

use norn_wire::{DOCUMENT_EXTENSION, LinkAddress, LinkAddressKind, LinkFamily, VAULT_PROTOCOL};

use crate::facts::LinkFact;
use crate::path::{DocumentPath, SuffixKey, markdown_stem, spell_path_key, suffix_probe};

/// The separator between segments, in a target and in a path alike.
const SEPARATOR: char = '/';

/// How a link's target reaches documents, as the store reads it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Addressing<'a> {
    /// The link addresses no document, so it is held under no key.
    Elsewhere,
    /// A suffix address, resolved through the one resolver: its exact vault
    /// path first, otherwise every path it is a segment-aligned suffix of.
    Suffix(Cow<'a, str>),
    /// The vault paths the target names, each resolved exactly: none where it
    /// names no vault path, one for the holding document, and one or two for
    /// a path form, two where its leaf ends in `.md`.
    Paths(Vec<String>),
}

impl<'a> Addressing<'a> {
    /// How `link`, held by the document at `holder`, reaches documents: the
    /// wire's address, with a path form read to the vault paths it names.
    pub(crate) fn of(link: &'a LinkFact, holder: &DocumentPath) -> Self {
        match LinkAddress::of(link.family.wire(), link.protocol.as_deref(), &link.target) {
            LinkAddress::Elsewhere => Addressing::Elsewhere,
            LinkAddress::Unnameable(_) => Addressing::Paths(Vec::new()),
            LinkAddress::HoldingDocument => Addressing::Paths(vec![holder.as_str().to_string()]),
            LinkAddress::Suffix(target) => Addressing::Suffix(target),
            LinkAddress::RootedName(path) | LinkAddress::Rooted(path) => {
                Addressing::Paths(path_names(Vec::new(), &path))
            }
            LinkAddress::Relative(path) => {
                Addressing::Paths(path_names(directory_of(holder), &path))
            }
        }
    }
}

/// How `link`'s address stands to judging it, which `links.address` stores:
/// the wire's one classification ([`LinkAddress::kind`]).
pub(crate) fn address_kind(link: &LinkFact) -> LinkAddressKind {
    LinkAddress::of(link.family.wire(), link.protocol.as_deref(), &link.target).kind()
}

/// One key the link index holds a link under.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct LinkKey {
    /// The normalized comparison key of the target.
    pub(crate) key: String,
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
                key: spell_path_key(&path, SuffixKey::Comparison),
                segments: None,
            })
            .collect(),
        Addressing::Suffix(target) => suffix_keys(&target),
    }
}

/// The keys a suffix address `target` is read through: one, or two where
/// it ends in `.md`, each a prefix of the documents' suffix comparison keys,
/// beside how many segments the address spells. None where `target` is no
/// suffix address. A link written with the address is held under exactly
/// these, and a request's target naming it is read through the same.
pub(crate) fn suffix_keys(target: &str) -> Vec<LinkKey> {
    let Ok(probe) = suffix_probe(target) else {
        return Vec::new();
    };
    let segments = target.split(SEPARATOR).count() as u64;
    probe
        .ranges()
        .map(|(lower, _)| LinkKey {
            key: lower.to_string(),
            segments: Some(segments),
        })
        .collect()
}

/// The keys a target given with no document holding it is read through,
/// as a get's target and a delete's `rewrite_to` are: taken as written, as a
/// wikilink's is, a `/` target is a path from the vault root keyed by the
/// paths it names, a target whose first segment is `.` or `..` has no folder
/// to be read from and names nothing, and any other target is a suffix
/// address ([`suffix_keys`]).
pub(crate) fn target_keys(address: &str) -> Vec<LinkKey> {
    match LinkAddress::of(LinkFamily::Wikilink, None, address) {
        LinkAddress::Suffix(target) => suffix_keys(&target),
        LinkAddress::Rooted(path) => path_names(Vec::new(), &path)
            .into_iter()
            .map(|path| LinkKey {
                key: spell_path_key(&path, SuffixKey::Comparison),
                segments: None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// The keys, in the key space `key` selects, a link that could name
/// `document` is held under: each segment-aligned prefix of the document's
/// suffix key, which a suffix address naming it opens — the whole key among
/// them, which a suffix address naming its exact path opens — and the
/// document's path, which a path naming it is.
///
/// Every link whose resolution holds the document, or whose exact-path
/// reading the document's presence decides, is held under one of these, so a
/// seek of them reaches every such link; which of the links it reaches
/// resolve to the document alone is the seek's own question.
pub(crate) fn keys_naming(document: &DocumentPath, key: SuffixKey) -> Vec<String> {
    let suffix = document.suffix_key();
    let mut keys: Vec<String> = suffix
        .match_indices(SEPARATOR)
        .map(|(at, _)| suffix[..=at].to_string())
        .collect();
    // The document's own path key, spelled by the one function that spells
    // one, so a path naming this document lands on the same key a change to
    // it discards by.
    keys.push(document.path_key_in(key).as_str().to_string());
    keys
}

/// The vault paths a path form names once read from the folder whose
/// segments are `from` — the empty list being the vault root — by the naming
/// rule: the joined path with `.md` appended and, where its leaf already
/// ends in `.md` in any ASCII case, the joined path itself. None where the
/// path climbs above the root or names no document path, and none where its
/// last segment is empty, `.` or `..`, which names a folder rather than a
/// file.
fn path_names(from: Vec<String>, path: &str) -> Vec<String> {
    let last = path.rsplit(SEPARATOR).next().unwrap_or(path);
    if matches!(last, "" | "." | "..") {
        return Vec::new();
    }
    let Some(path) = joined(from, path) else {
        return Vec::new();
    };
    let leaf = path.rsplit(SEPARATOR).next().unwrap_or(&path);
    let explicit = (markdown_stem(leaf) != leaf).then(|| path.clone());
    std::iter::once(format!("{path}.{DOCUMENT_EXTENSION}"))
        .chain(explicit)
        .filter(|path| DocumentPath::new(path).is_ok())
        .collect()
}

/// The place the suffix address `address` spells, as a request's target
/// naming no document is read: the document path a document must stand at
/// for a wikilink written with the address to name it by every segment it
/// spells — `address` itself where its leaf carries the document extension,
/// in any ASCII case, and `address` with the extension appended otherwise,
/// with no other extension removed. `None` where the wikilink grammar
/// refuses the address, or the
/// document path grammar the place.
///
/// **What a repair of broken links names.** A wikilink rewrite whose `old`
/// names no document repairs the broken wikilinks that would name a
/// document standing here, so `Old Note` repairs `[[Old Note]]` and
/// `[[Old Note.md]]` alike, and `v1.2` spells `v1.2.md`, never `v1.md`.
pub fn spelled_place(address: &str) -> Option<DocumentPath> {
    suffix_probe(address).ok()?;
    let leaf = address.rsplit(SEPARATOR).next().unwrap_or(address);
    let place = if without_document_extension(leaf) != leaf {
        address.to_string()
    } else {
        format!("{address}.{DOCUMENT_EXTENSION}")
    };
    DocumentPath::new(&place).ok()
}

/// The protocol-stripped leaf a similarity query reads: the leaf of the
/// target as its family takes it ([`LinkAddress::of`]) — a Markdown
/// destination's decoded and without its query — and without the document
/// extension in any ASCII case. A target taken as no path, one addressed
/// elsewhere or one whose decoding names nothing, stays written, as the query
/// must still describe the authored segment.
pub fn query_leaf(link: &norn_wire::LinkKey) -> String {
    let (protocol, written) = link.protocol_and_stem();
    let read = match LinkAddress::of(link.syntax, protocol, written) {
        LinkAddress::Suffix(read)
        | LinkAddress::RootedName(read)
        | LinkAddress::Relative(read)
        | LinkAddress::Rooted(read) => read,
        LinkAddress::Elsewhere | LinkAddress::Unnameable(_) | LinkAddress::HoldingDocument => {
            Cow::Borrowed(written)
        }
    };
    let leaf = read.rsplit(SEPARATOR).next().expect("a leaf segment");
    without_document_extension(leaf).to_string()
}

fn without_document_extension(leaf: &str) -> &str {
    let stem = markdown_stem(leaf);
    if stem != leaf && leaf[stem.len() + 1..].eq_ignore_ascii_case(DOCUMENT_EXTENSION) {
        stem
    } else {
        leaf
    }
}

/// The vault paths `link`, held by the document at `holder`, names by path:
/// each path a path form names by the naming rule, read from the holder's
/// folder or the vault root, and the holder itself for an empty target —
/// read by this module's one reading of a link's address. None for a suffix
/// address, which names documents through the resolver rather than by path,
/// and none for a link addressed elsewhere or naming no vault path.
///
/// **What a moved document's own path links named.** A move that carries a
/// document away from its folder respells the relative links it holds so
/// they name what they named from where it stood; this is how the planner
/// reads what that was, by the rule a read of the link uses.
pub fn named_paths(link: &LinkFact, holder: &DocumentPath) -> Vec<String> {
    match Addressing::of(link, holder) {
        Addressing::Paths(paths) => paths,
        Addressing::Elsewhere | Addressing::Suffix(_) => Vec::new(),
    }
}

/// The vault path a path-form link — a `./`, `../`, `/` or `vault://`
/// target — held by the document at `holder` spells, before the naming rule
/// reads a document at it: `../img.png` from `a/b/doc.md` spells `a/img.png`,
/// the file it names while no document stands at `a/img.png.md`. `None` for
/// any other link, and for a path that climbs above the vault root, names a
/// folder, or is no vault path.
///
/// **What a moved document's own path link spelled.** A link that resolved
/// to no document — an attachment's, or a broken one — is followed by the
/// path it spells, so the planner respells it toward that same file.
pub fn spelled_path(link: &LinkFact, holder: &DocumentPath) -> Option<String> {
    let (from, path) =
        match LinkAddress::of(link.family.wire(), link.protocol.as_deref(), &link.target) {
            LinkAddress::Relative(path) => (directory_of(holder), path),
            LinkAddress::Rooted(path) | LinkAddress::RootedName(path) => (Vec::new(), path),
            _ => return None,
        };
    let last = path.rsplit(SEPARATOR).next().unwrap_or(&path);
    if matches!(last, "" | "." | "..") {
        return None;
    }
    joined(from, &path).filter(|spelled| DocumentPath::new(spelled).is_ok())
}

/// Whether `read`, a path form taken by its family, reads back from the
/// folder whose segments are `from` as naming `path`: a document the naming
/// rule reads there, or the file the path spells.
fn reads_back(from: Vec<String>, read: &str, path: &str) -> bool {
    joined(from.clone(), read)
        .filter(|spelled| DocumentPath::new(spelled).is_ok())
        .as_deref()
        == Some(path)
        || path_names(from, read).iter().any(|named| named == path)
}

/// `path` spelled as a Markdown destination read from the directory of the
/// document at `holder`, in the style the destination `written` was written
/// in; or `None` where no spelling reads back as `path` from there.
///
/// **The inverse of the reading, held to it.** The spelling climbs out of
/// the holder's directory to the deepest folder it shares with `path` and
/// down from there, one `..` per folder climbed, and opens with `./` where
/// it climbs none, because only a destination whose first segment is `.` or
/// `..` is read from the holder's folder: an unprefixed one is a suffix
/// address (ADR 0040). It is accepted only where the one reading every
/// Markdown link is resolved by reads it back from the holder as a path form
/// naming `path`.
///
/// **The style is the written link's.** A destination written with a percent
/// escape is spelled with every byte outside the unreserved set escaped, so a
/// link written `my%20note.md` becomes `new%20name.md`; one written without
/// escapes keeps every byte as itself but the three a destination cannot hold
/// as themselves — `%`, which would read as an escape, and `?` and `#`, which
/// would end its path. A query the destination carried, from its first `?`,
/// is kept as written. Whether the destination's syntax can then carry the
/// spelling where the link stands — a space outside angle brackets — is the
/// text layer's to judge.
pub fn relative_spelling(holder: &DocumentPath, path: &str, written: &str) -> Option<String> {
    let (written_path, query) = split_query(written);
    let escaped = holds_an_escape(written_path);
    let from = directory_of(holder);
    let to: Vec<&str> = path.split(SEPARATOR).collect();
    let (folders, _) = to.split_at(to.len() - 1);
    let shared = from
        .iter()
        .zip(folders)
        .take_while(|(from, to)| from.as_str() == **to)
        .count();
    let climbed = std::iter::repeat_n("..".to_string(), from.len() - shared);
    let here = (from.len() == shared).then(|| ".".to_string());
    let spelling = here
        .into_iter()
        .chain(climbed)
        .chain(to[shared..].iter().map(|segment| spelled(segment, escaped)))
        .collect::<Vec<_>>()
        .join("/");
    let read_back = match LinkAddress::of(LinkFamily::Markdown, None, &spelling) {
        LinkAddress::Relative(read) => reads_back(from, &read, path),
        _ => false,
    };
    read_back.then(|| format!("{spelling}{query}"))
}

/// `path` spelled as a Markdown destination read from the vault root, in the
/// style the destination `written` was written in — led by a separator where
/// `written` is, as a root-relative destination is and a `vault://` one is
/// not — or `None` where no spelling reads back as `path`.
///
/// The style is [`relative_spelling`]'s, and so is the proof: the spelling is
/// accepted only where it reads back from the root as a path naming `path`.
pub fn rooted_spelling(path: &str, written: &str) -> Option<String> {
    let (written_path, query) = split_query(written);
    let escaped = holds_an_escape(written_path);
    let spelling = path
        .split(SEPARATOR)
        .map(|segment| spelled(segment, escaped))
        .collect::<Vec<_>>()
        .join("/");
    let lead = if written_path.starts_with(SEPARATOR) {
        "/"
    } else {
        ""
    };
    let read_back = match LinkAddress::of(LinkFamily::Markdown, Some(VAULT_PROTOCOL), &spelling) {
        LinkAddress::RootedName(read) => reads_back(Vec::new(), &read, path),
        _ => false,
    };
    read_back.then(|| format!("{lead}{spelling}{query}"))
}

/// The suffix `spelling`, a document path's suffix, spelled as a Markdown
/// destination in the style the destination `written` was written in: each
/// segment escaped as [`relative_spelling`] escapes one, and a query the
/// destination carried kept as written. An unprefixed destination is a
/// suffix address (ADR 0040), so it reads back as the suffix spelled.
pub fn suffix_spelling(spelling: &str, written: &str) -> String {
    let (written_path, query) = split_query(written);
    let escaped = holds_an_escape(written_path);
    let spelling = spelling
        .split(SEPARATOR)
        .map(|segment| spelled(segment, escaped))
        .collect::<Vec<_>>()
        .join("/");
    format!("{spelling}{query}")
}

/// `written` cut at its first `?`: the path, and the query from that `?` on,
/// empty where there is none.
fn split_query(written: &str) -> (&str, &str) {
    written
        .find('?')
        .map_or((written, ""), |at| written.split_at(at))
}

/// Whether `text` holds a percent escape: a `%` followed by two hexadecimal
/// digits.
fn holds_an_escape(text: &str) -> bool {
    text.as_bytes()
        .windows(3)
        .any(|window| window[0] == b'%' && window[1..].iter().all(u8::is_ascii_hexdigit))
}

/// One path segment as a destination spells it: every character outside the
/// unreserved set escaped, byte by byte of its UTF-8, where the style is
/// `escaped`, and otherwise only the characters a destination cannot hold as
/// themselves — `%`, `?` and `#`.
fn spelled(segment: &str, escaped: bool) -> String {
    let mut spelling = String::with_capacity(segment.len());
    for character in segment.chars() {
        let keep = if escaped {
            character.is_ascii_alphanumeric() || matches!(character, '-' | '.' | '_' | '~')
        } else {
            !matches!(character, '%' | '?' | '#')
        };
        if keep {
            spelling.push(character);
            continue;
        }
        let mut buffer = [0; 4];
        for byte in character.encode_utf8(&mut buffer).bytes() {
            spelling.push_str(&format!("%{byte:02X}"));
        }
    }
    spelling
}

/// The segments of the directory holding the document at `holder`.
fn directory_of(holder: &DocumentPath) -> Vec<String> {
    holder
        .as_str()
        .rsplit_once(SEPARATOR)
        .map(|(directory, _)| directory.split(SEPARATOR).map(str::to_string).collect())
        .unwrap_or_default()
}

/// The path `path`, a target already taken by its family, names read from
/// the directory whose segments are `from` — the empty list being the vault
/// root — or `None` where it names none.
///
/// Empty and `.` segments are dropped, and `..` climbs one directory; a
/// climb above the vault root, and a path that folds to nothing, name no
/// path. Nothing is decoded here: a Markdown destination's segments were
/// decoded when its address was read, and a segment that decoded to a step
/// never reaches here ([`LinkAddress::Unnameable`]).
pub(crate) fn joined(mut from: Vec<String>, path: &str) -> Option<String> {
    for segment in path.split(SEPARATOR) {
        match segment {
            "" | "." => {}
            ".." => {
                from.pop()?;
            }
            name => from.push(name.to_string()),
        }
    }
    (!from.is_empty()).then(|| from.join("/"))
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
            field: None,
            family,
            embed: false,
            protocol: protocol.map(str::to_string),
            target: target.to_string(),
            title: None,
            anchor: None,
            span: Some(crate::facts::Span {
                line: 1,
                column: 1,
                byte_offset: 0,
            }),
        }
    }

    /// The vault paths a Markdown link to the path form `target` names from
    /// `holder`.
    fn named(holder: &DocumentPath, target: &str) -> Vec<String> {
        let link = written(crate::facts::LinkFamily::Markdown, None, target);
        match Addressing::of(&link, holder) {
            Addressing::Paths(paths) => paths,
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

    /// **A `vault://` link is a path from the root in both families**, named
    /// by the one naming rule: `.md` appended and, where the leaf already
    /// ends in `.md`, the path itself, with no other extension removed. A
    /// wikilink's target is read as written and a Markdown destination's
    /// decoded; `.` and `..` fold, and a climb above the root or a folder
    /// leaf names nothing.
    #[test]
    fn a_vault_link_is_keyed_by_the_root_paths_its_target_names() {
        use crate::facts::LinkFamily::{Markdown, Wikilink};
        for (family, target, keys) in [
            (Wikilink, "Notes", &["notes.md"][..]),
            (Wikilink, "Notes.md", &["notes.md.md", "notes.md"]),
            (Wikilink, "v1.2", &["v1.2.md"]),
            (Wikilink, "a.b/Notes", &["a.b/notes.md"]),
            (Wikilink, "my%20note", &["my%20note.md"]),
            (Wikilink, "Notes?x", &["notes?x.md"]),
            (Wikilink, "a/../Notes", &["notes.md"]),
            (Wikilink, "notes/", &[]),
            (Wikilink, "..", &[]),
            (Markdown, "Notes", &["notes.md"]),
            (Markdown, "Notes.md", &["notes.md.md", "notes.md"]),
            (
                Markdown,
                "my%20note.md?x=1",
                &["my note.md.md", "my note.md"],
            ),
            (Markdown, "/a/./b.md", &["a/b.md.md", "a/b.md"]),
            (Markdown, "../Notes.md", &[]),
        ] {
            assert_eq!(rooted_keys(family, target), keys, "{family:?} `{target}`");
        }
    }

    /// **The place a suffix address spells is itself where its leaf carries
    /// the document extension, and itself with the extension appended
    /// otherwise**: a dotted leaf is never stripped to a shorter stem, and an
    /// address the wikilink grammar refuses spells none.
    #[test]
    fn a_suffix_address_spells_the_place_its_whole_name_names() {
        for (address, place) in [
            ("Old Note", Some("Old Note.md")),
            ("Old Note.md", Some("Old Note.md")),
            ("Old Note.MD", Some("Old Note.MD")),
            ("notes/Old Note", Some("notes/Old Note.md")),
            ("v1.2", Some("v1.2.md")),
            ("a.b/c", Some("a.b/c.md")),
            ("notes/", None),
            ("a/../b", None),
        ] {
            assert_eq!(
                spelled_place(address).as_ref().map(DocumentPath::as_str),
                place,
                "`{address}`"
            );
        }
    }

    /// **A segment that decodes to `.` or `..` is data**: it is decoded after
    /// the path is split, so it names neither the directory it stands in nor
    /// its parent, and the link names no document.
    #[test]
    fn a_segment_decoding_to_a_dot_name_names_no_document() {
        let holder = at("a/b/doc.md");
        for target in [
            "%2E%2E/c.md",
            "%2e%2e/c.md",
            "%2E/c.md",
            "./sub/%2E%2E/c.md",
            "./sub/%2E",
            "%2E%2E",
            "/%2E%2E/c.md",
        ] {
            assert_eq!(named(&holder, target), Vec::<String>::new(), "`{target}`");
        }
        assert_eq!(named(&holder, "./sub/../c"), ["a/b/c.md"]);
    }

    /// **A path form is joined to where it is read from and named by the
    /// naming rule.** A destination whose first segment is `.` or `..` is
    /// read from its document's folder and one opening with `/` from the
    /// vault root, split before it is decoded and its query cut off; it
    /// names the joined path with `.md` appended, and the joined path itself
    /// where its leaf ends in `.md`. A climb above the root, a folder leaf
    /// and a decoded separator name nothing.
    #[test]
    fn a_markdown_path_form_names_the_paths_it_joins_to() {
        let holder = at("a/b/doc.md");
        for (target, paths) in [
            ("./c.md", &["a/b/c.md.md", "a/b/c.md"][..]),
            ("./c", &["a/b/c.md"]),
            ("../x/y.md", &["a/x/y.md.md", "a/x/y.md"]),
            ("../../top", &["top.md"]),
            ("/x/y", &["x/y.md"]),
            ("./my%20note", &["a/b/my note.md"]),
            ("../x/my note", &["a/x/my note.md"]),
            ("../x/my%20note", &["a/x/my note.md"]),
            ("./foo.png", &["a/b/foo.png.md"]),
            ("./sub/./c", &["a/b/sub/c.md"]),
            ("./q?x=1", &["a/b/q.md"]),
            ("./sub%2Fc.md", &[]),
            ("../../../escape.md", &[]),
            ("..", &[]),
            ("./sub/", &[]),
            ("../..", &[]),
            ("./%ff.md", &[]),
            ("", &["a/b/doc.md"]),
        ] {
            assert_eq!(
                named(&holder, target),
                paths,
                "`{target}` from `{}`",
                holder.as_str()
            );
        }
        assert_eq!(named(&at("top.md"), "../x"), Vec::<String>::new());
        assert_eq!(named(&at("top.md"), "./x"), ["x.md"]);
    }

    /// **An unprefixed Markdown destination is a suffix address**, keyed by
    /// the classes it opens like a wikilink's, never joined to its
    /// document's folder.
    #[test]
    fn an_unprefixed_markdown_destination_is_keyed_as_a_suffix() {
        use crate::facts::LinkFamily::{Markdown, Wikilink};
        let holder = at("a/b/doc.md");
        for target in ["dir/t.md", "t", "my%20note.md"] {
            let keys = link_keys(&written(Markdown, None, target), &holder);
            assert!(
                !keys.is_empty() && keys.iter().all(|key| key.segments.is_some()),
                "`{target}` is keyed {keys:?}"
            );
        }
        assert_eq!(
            link_keys(&written(Markdown, None, "dir/t.md"), &holder),
            link_keys(&written(Wikilink, None, "dir/t.md"), &holder)
        );
        assert_eq!(
            link_keys(&written(Markdown, None, "my%20note"), &holder),
            link_keys(&written(Wikilink, None, "my note"), &holder)
        );
    }

    /// **A relative spelling reads back as the path it names.** From the
    /// holder's directory it climbs to the deepest folder shared with the path
    /// and down again, opens with `./` where it does not climb — an
    /// unprefixed destination would be read as a suffix — keeps a query as
    /// written, and escapes the three characters a destination cannot hold
    /// as themselves; each spelling is read back through the reader a
    /// Markdown link is resolved by. A path no spelling reaches — one the
    /// store's grammar refuses — has none.
    #[test]
    fn a_relative_spelling_reads_back_as_the_path_it_names() {
        let holder = at("a/b/doc.md");
        for (path, written, spelling) in [
            ("a/b/c.md", "old.md", "./c.md"),
            ("a/b/c.md", "./old.md", "./c.md"),
            ("a/b/sub/c.md", "old.md", "./sub/c.md"),
            ("a/x/y.md", "old.md", "../x/y.md"),
            ("a/x/y.md", "./old.md", "../x/y.md"),
            ("top.md", "../old.md", "../../top.md"),
            ("a/b/doc.md", "old.md", "./doc.md"),
            ("a/b/my note.md", "old.md", "./my note.md"),
            ("a/b/q.md", "old.md?x=1", "./q.md?x=1"),
            ("a/b/100%.md", "old.md", "./100%25.md"),
            ("a/b/what?.md", "old.md", "./what%3F.md"),
            ("a/b/c#1.md", "old.md", "./c%231.md"),
            ("a/b/diagram.png", "old.png", "./diagram.png"),
        ] {
            assert_eq!(
                relative_spelling(&holder, path, written).as_deref(),
                Some(spelling),
                "`{path}` written as `{written}`"
            );
            let link = super::tests::written(crate::facts::LinkFamily::Markdown, None, spelling);
            assert!(
                named(&holder, spelling).iter().any(|named| named == path)
                    || spelled_path(&link, &holder).as_deref() == Some(path),
                "`{spelling}` reads back"
            );
        }
        assert_eq!(
            relative_spelling(&holder, "a/b/back\\slash.md", "x.md"),
            None
        );
        assert_eq!(
            relative_spelling(&at("top.md"), "x/y.md", "old.md").as_deref(),
            Some("./x/y.md")
        );
    }

    /// **The written link's escaping style is kept.** A destination written
    /// with a percent escape is spelled with every byte outside the
    /// unreserved set escaped, a non-ASCII character by its UTF-8 bytes; one
    /// written without escapes keeps its characters as themselves. A path
    /// from the root keeps the leading separator a root-relative destination
    /// carries and a `vault://` one does not.
    #[test]
    fn percent_encoding_style_is_kept() {
        let holder = at("a/b/doc.md");
        for (path, written, spelling) in [
            ("a/b/new name.md", "my%20old.md", "./new%20name.md"),
            ("a/b/new name.md", "old.md", "./new name.md"),
            ("a/b/café.md", "my%20old.md", "./caf%C3%A9.md"),
            ("a/b/café.md", "old.md", "./café.md"),
            ("a/b/(x).md", "a%20b.md", "./%28x%29.md"),
        ] {
            assert_eq!(
                relative_spelling(&holder, path, written).as_deref(),
                Some(spelling),
                "`{path}` written as `{written}`"
            );
            assert!(named(&holder, spelling).iter().any(|named| named == path));
        }
        for (path, written, spelling) in [
            ("x/new name.md", "/my%20old.md", "/x/new%20name.md"),
            ("x/new name.md", "/old.md?q", "/x/new name.md?q"),
            ("x/new.md", "notes/old.md", "x/new.md"),
        ] {
            assert_eq!(
                rooted_spelling(path, written).as_deref(),
                Some(spelling),
                "`{path}` written as `{written}`"
            );
        }
    }

    /// Path-addressed links name complete paths by the naming rule, in both
    /// families. Suffix links name documents through class probes.
    #[test]
    fn named_paths_are_the_paths_a_link_reads_to() {
        use crate::facts::LinkFamily::{Markdown, Wikilink};
        let holder = at("a/b/doc.md");
        assert_eq!(
            named_paths(&written(Markdown, None, "../x.md"), &holder),
            ["a/x.md.md", "a/x.md"]
        );
        assert_eq!(
            named_paths(&written(Wikilink, None, "../x"), &holder),
            ["a/x.md"]
        );
        assert_eq!(
            named_paths(&written(Wikilink, None, "/x"), &holder),
            ["x.md"]
        );
        assert_eq!(
            named_paths(&written(Wikilink, Some("vault"), "v1.2"), &holder),
            ["v1.2.md"]
        );
        assert_eq!(
            spelled_path(&written(Markdown, None, "../img.png"), &holder).as_deref(),
            Some("a/img.png")
        );
        assert_eq!(
            spelled_path(&written(Markdown, None, "img.png"), &holder),
            None
        );
        assert!(named_paths(&written(Wikilink, None, "x"), &holder).is_empty());
        assert!(named_paths(&written(Markdown, None, "x.md"), &holder).is_empty());
        assert!(named_paths(&written(Markdown, Some("https"), "x.md"), &holder).is_empty());
    }

    /// A similarity query reads the leaf as its family takes it: a Markdown
    /// destination decoded and without its query, a wikilink's as written.
    #[test]
    fn a_query_leaf_is_the_leaf_its_family_reads() {
        use norn_wire::LinkFamily::{Markdown, Wikilink};
        for (family, target, leaf) in [
            (Markdown, "../t/space%20name.md?x=1", "space name"),
            (Wikilink, "t/space%20name", "space%20name"),
            (Markdown, "a%2Fb.md", "a%2Fb"),
        ] {
            let holder = norn_wire::DocumentPath::new("h.md").expect("a document path");
            let link = norn_wire::LinkKey::new(holder, family, target);
            assert_eq!(query_leaf(&link), leaf, "{family:?} `{target}`");
        }
    }

    /// A document is named by normalized suffix prefixes and its complete
    /// path comparison key. Original path case does not select another key.
    #[test]
    fn a_document_is_named_by_normalized_suffix_prefixes_and_its_complete_path_key() {
        let document = at("Docs/Norn/Glossary.md");
        assert_eq!(
            keys_naming(&document, SuffixKey::Comparison),
            [
                "glossary/",
                "glossary/norn/",
                "glossary/norn/docs/",
                "docs/norn/glossary.md"
            ]
        );
    }

    /// A rooted Markdown path and a rooted wikilink naming the same document
    /// record its normalized path key. Changeset maintenance uses that key too.
    #[test]
    fn path_addressed_links_and_documents_share_the_same_comparison_key() {
        let holder = at("holder.md");
        for (path, stem) in [("Dir/T.md", "Dir/T"), ("Été/Ü.md", "Été/Ü")] {
            let document = at(path);
            let document_key = document.path_key_in(SuffixKey::Comparison);

            let markdown = link_keys(
                &written(
                    crate::facts::LinkFamily::Markdown,
                    None,
                    &format!("/{stem}"),
                ),
                &holder,
            );
            let wikilink = link_keys(
                &written(crate::facts::LinkFamily::Wikilink, Some("vault"), stem),
                &holder,
            );

            for (family, keys) in [("markdown", &markdown), ("wikilink", &wikilink)] {
                assert_eq!(keys.len(), 1, "{family} link to {path} named {keys:?}");
                assert_eq!(
                    keys[0].key,
                    document_key.as_str(),
                    "{family} link to {path}'s document comparison key"
                );
            }
        }
    }
}
