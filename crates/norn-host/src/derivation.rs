//! The per-document derivation act: bytes in, one document's derived facts
//! and verdicts out.
//!
//! This module is pure — no IO, no store access.

use std::path::Path;

use norn_store::{
    BlockFact, DocumentFacts, DocumentPath, FrontmatterValue, HeadingFact, LinkFact, LinkFamily,
    Span, TagFact, TagSource,
};
use norn_text::{BlockRefusal, Document, SourceSpan, Value};

/// Why a path the vault holds produces no document facts.
///
/// One variant per finding kind, which is how a reader tells a name the store
/// cannot hold from bytes the parser cannot read. Every one of them leaves the
/// deriving act with nothing to store: no identity to hold a row under, or no
/// text to read facts out of.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Undecodable {
    /// The path bytes are not UTF-8.
    PathBytes,
    /// The path is UTF-8 and is not a document path.
    PathSpelling,
    /// The document's bytes are not UTF-8.
    BodyBytes,
}

/// Why a document that derives carries no frontmatter value.
///
/// The block was read by nothing, so the document's fields are unknown: it is
/// the vault's own defect and not a shape of a document. The row still holds
/// every fact the act could derive — identity, body, headings, links, body
/// tags — and this cause is what a finding beside that row states, because a
/// row alone would answer *this document has no tags, no title, no aliases*
/// about fields nothing ever read.
///
/// One variant per way [`norn_text::BlockRefusal`] leaves a block unread, each
/// fixed by a different edit to the document.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum UnreadBlock {
    /// The block opens and never closes.
    Unclosed,
    /// Nothing read the block: it is not well-formed, or it is well-formed and
    /// says something no value can be made of — a key written twice, a merge
    /// directive naming no mapping.
    Unreadable,
    /// The block is past [`norn_text::FRONTMATTER_MAX_BYTES`], so the text
    /// layer refuses it unparsed rather than paying a read that grows with the
    /// block's own length.
    TooLarge,
}

impl UnreadBlock {
    /// The cause behind the state the text layer reports.
    ///
    /// The match carries no wildcard, so a new way to leave a block unread
    /// arrives here as a cause rather than as silence on a derived row.
    const fn of(refusal: &BlockRefusal) -> Self {
        match refusal {
            BlockRefusal::Unclosed => UnreadBlock::Unclosed,
            BlockRefusal::Unreadable { .. } => UnreadBlock::Unreadable,
            BlockRefusal::TooLarge { .. } => UnreadBlock::TooLarge,
        }
    }
}

/// One document held out of derived state, and why.
#[derive(Clone, Debug)]
pub(crate) struct Quarantine {
    pub(crate) cause: Undecodable,
    /// The decoder's own account of the refusal, which the finding carries in
    /// its detail beside the spelling it was read from.
    pub(crate) problem: String,
}

/// One document derived without its frontmatter, and why.
///
/// The document keeps its row: this is what stands beside it, so that the
/// fields nothing read are absent from derived state *and* stated rather than
/// silently absent.
#[derive(Clone, Debug)]
pub(crate) struct UnreadFrontmatter {
    pub(crate) cause: UnreadBlock,
    /// The reader's own account of the refusal, where the cause is not the
    /// whole of it. A block that never closes has nothing to add.
    pub(crate) problem: Option<String>,
}

/// The document path a vault-relative spelling names, or why it names none.
///
/// This is the one place a walked or watched path becomes a document identity,
/// so the two ways a spelling fails to be one are told apart here rather than
/// at each caller.
pub(crate) fn document_path(path: &Path) -> Result<DocumentPath, Quarantine> {
    let Some(spelling) = path.to_str() else {
        return Err(Quarantine {
            cause: Undecodable::PathBytes,
            problem: "the path bytes are not valid UTF-8".to_string(),
        });
    };
    DocumentPath::new(spelling).map_err(|problem| Quarantine {
        cause: Undecodable::PathSpelling,
        problem: problem.to_string(),
    })
}

/// One document as derived state holds it: the facts, and the defect standing
/// beside them where the document derives with something unread.
pub(crate) struct Derived {
    pub(crate) facts: DocumentFacts,
    pub(crate) unread_frontmatter: Option<UnreadFrontmatter>,
}

pub(crate) fn map_document(path: &str, bytes: &[u8], hash: String) -> Result<Derived, Quarantine> {
    // Identity before content: a path that names no document has nothing to
    // say about its own bytes.
    let document_path = document_path(Path::new(path))?;
    let source = std::str::from_utf8(bytes).map_err(|problem| Quarantine {
        cause: Undecodable::BodyBytes,
        problem: problem.to_string(),
    })?;
    let document = Document::parse(source);
    // The text layer reads a block only up to its own bound and says so rather
    // than parsing past it, so this costs the bound at worst however the block
    // is shaped. A block nothing read — unclosed, not well-formed, or past the
    // bound — leaves the fields unknown, which the document reports as state:
    // the facts below are derived without them, and the finding beside them is
    // where the absence is stated.
    let unread_frontmatter = document
        .frontmatter_refusal()
        .map(|refusal| UnreadFrontmatter {
            cause: UnreadBlock::of(refusal),
            problem: refusal.problem(),
        });
    let scan = document.scan_body();
    let mut facts = DocumentFacts::new(document_path, hash, document.body(), bytes.len() as u64);
    facts.body_offset = document.body_start() as u64;
    facts.frontmatter = document.frontmatter().map(map_value);
    facts.frontmatter_diagnostic_count = document
        .diagnostics()
        .iter()
        .filter(|d| d.code.frontmatter_scoped())
        .count() as u32;
    facts.links = document
        .frontmatter_wikilinks()
        .into_iter()
        .chain(scan.links())
        .map(map_link)
        .collect();
    facts.headings = scan
        .headings()
        .iter()
        .map(|h| HeadingFact {
            level: h.level,
            text: h.text.clone(),
            slug: h.slug.clone(),
            span: span(h.span),
            body_offset: h.body_offset as u64,
            inside_container: h.inside_container,
        })
        .collect();
    facts.blocks = scan
        .block_ids()
        .into_iter()
        .map(|b| BlockFact {
            block_id: b.id,
            span: Some(span(b.span)),
        })
        .collect();
    facts.tags = scan
        .tags()
        .into_iter()
        .map(|t| TagFact {
            name: t.name,
            source: TagSource::Body,
            span: t.span.map(span),
        })
        .chain(document.frontmatter_tags().into_iter().map(|t| TagFact {
            name: t.name,
            source: TagSource::Frontmatter,
            span: t.span.map(span),
        }))
        .collect();
    Ok(Derived {
        facts,
        unread_frontmatter,
    })
}

fn map_link(link: norn_text::Link) -> LinkFact {
    LinkFact {
        family: match link.family {
            norn_text::LinkFamily::Wikilink => LinkFamily::Wikilink,
            norn_text::LinkFamily::Markdown => LinkFamily::Markdown,
        },
        embed: link.embed,
        protocol: link.protocol,
        target: link.target,
        title: link.title,
        anchor: link.anchor,
        block_ref: link.block_ref,
        span: span(link.span),
    }
}
fn span(value: SourceSpan) -> Span {
    Span {
        line: value.line as u64,
        column: value.column as u64,
        byte_offset: value.byte_offset as u64,
    }
}
fn map_value(value: &Value) -> FrontmatterValue {
    match value {
        Value::Null => FrontmatterValue::Null,
        Value::Bool(v) => FrontmatterValue::Bool(*v),
        Value::Int(v) => FrontmatterValue::Int(*v),
        Value::Float(v) => FrontmatterValue::Float(*v),
        Value::String(v) => FrontmatterValue::String(v.clone()),
        Value::Sequence(v) => FrontmatterValue::Sequence(v.iter().map(map_value).collect()),
        Value::Map(v) => FrontmatterValue::Map(
            v.iter()
                .map(|(k, v)| (k.to_owned(), map_value(v)))
                .collect(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn document_identity_boundary_quarantines_non_utf8_path_bytes() {
        use std::os::unix::ffi::OsStrExt;

        let path = Path::new(std::ffi::OsStr::from_bytes(b"bad-\xff.md"));
        let quarantine = document_path(path).expect_err("non-UTF-8 path bytes name no document");
        assert_eq!(quarantine.cause, Undecodable::PathBytes);
    }

    #[test]
    fn document_identity_boundary_quarantines_a_spelling_the_grammar_refuses() {
        let path = Path::new("notes/bad\\name.md");
        let quarantine = document_path(path).expect_err("a backslash names no document");
        assert_eq!(quarantine.cause, Undecodable::PathSpelling);
    }

    /// **A key written twice reaches the same degradation whichever spelling
    /// wrote it.** The text layer refuses the block either way, so the row, the
    /// body facts, the note count and the cause a finding is filed under agree
    /// between a document whose duplicate the parser sees and one whose keys
    /// only collapse into a single name. Both accounts name the repeated key;
    /// how precisely each places it is the text layer's contract, not this
    /// layer's.
    #[test]
    fn a_repeated_key_degrades_alike_whether_or_not_a_tag_spelled_it() {
        let derive = |source: &str| {
            let bytes = source.as_bytes();
            map_document(
                "note.md",
                bytes,
                norn_fs::ContentHash::of(bytes).to_string(),
            )
            .expect("a document whose block went unread still derives")
        };
        let plain = derive("---\nk: 1\nk: 2\n---\n# heading\n");
        let tagged = derive("---\n!x k: 1\nk: 2\n---\n# heading\n");
        for (spelling, derived) in [("plain", &plain), ("tagged", &tagged)] {
            assert!(
                derived.facts.frontmatter.is_none(),
                "the {spelling} duplicate produced a projection"
            );
            assert_eq!(
                derived.facts.frontmatter_diagnostic_count, 1,
                "the {spelling} duplicate counted another number of block-scoped notes"
            );
            assert_eq!(
                derived.facts.headings.len(),
                1,
                "the {spelling} duplicate lost the body facts the act could derive"
            );
            let unread = derived
                .unread_frontmatter
                .as_ref()
                .expect("the block was read by nothing");
            assert_eq!(unread.cause, UnreadBlock::Unreadable);
            assert_eq!(
                unread.cause.kind().as_str(),
                "document/frontmatter-unreadable"
            );
        }
        for (spelling, derived) in [("plain", plain), ("tagged", tagged)] {
            let problem = derived
                .unread_frontmatter
                .expect("refused")
                .problem
                .expect("a refusal the reader accounted for");
            assert!(
                problem.contains("duplicate entry with key \"k\""),
                "the {spelling} duplicate's finding does not name the repeated key: {problem:?}"
            );
        }
    }

    /// **The store's projection bound is not a fourth outcome a document can
    /// reach.** The store refuses a frontmatter projection nesting past
    /// `MAX_FRONTMATTER_DEPTH`, and that refusal would withdraw the whole
    /// increment rather than one document — so the bound has to stand above what
    /// any readable block can carry. The text layer refuses the deeper block
    /// first, and a block it refuses is the degradation above: a row, and a
    /// finding naming the cause. The ceiling is searched rather than assumed, so
    /// either bound moving toward the other fails here.
    #[test]
    fn no_readable_block_nests_deeper_than_the_store_projects() {
        let block_nesting = |depth: usize| {
            let mut source = String::from("---\nk: ");
            source.push_str(&"[".repeat(depth));
            source.push_str(&"]".repeat(depth));
            source.push_str("\n---\n# body\n");
            source
        };
        let derive = |source: &str| {
            let bytes = source.as_bytes();
            map_document(
                "note.md",
                bytes,
                norn_fs::ContentHash::of(bytes).to_string(),
            )
            .expect("a document whose block went unread still derives")
        };

        let refused = (1..=norn_store::MAX_FRONTMATTER_DEPTH)
            .find(|depth| derive(&block_nesting(*depth)).unread_frontmatter.is_some())
            .expect("the text layer reads every block the store's bound admits");
        let deepest = derive(&block_nesting(refused - 1));
        let projection = deepest
            .facts
            .frontmatter
            .as_ref()
            .expect("the deepest block the text layer reads produced no projection");
        norn_store::canonical_json(projection)
            .expect("the deepest block the text layer reads is past the store's bound");
        assert_eq!(
            derive(&block_nesting(refused))
                .unread_frontmatter
                .expect("the block past the ceiling was read")
                .cause,
            UnreadBlock::Unreadable,
            "a block the text layer will not nest through took another outcome"
        );
    }

    /// **Where the body starts differs by cause.** A closed block bounds its own
    /// bytes, so an unreadable one is skipped whole and nothing inside it is
    /// read. A block that never closes bounds nothing, so the document is body
    /// from its first byte and the links and tags written in the lines that
    /// opened like a block are the document's own body facts. The finding says
    /// the block was read by nothing; it does not say the text is unread.
    #[test]
    fn an_unclosed_block_bounds_nothing_so_its_text_reads_as_body() {
        let derive = |source: &str| {
            let bytes = source.as_bytes();
            map_document(
                "note.md",
                bytes,
                norn_fs::ContentHash::of(bytes).to_string(),
            )
            .expect("a document whose block went unread still derives")
            .facts
        };

        let unclosed = derive("---\ntags: [alpha]\nlink: [[Some Target]]\nnote: #hashtag\n");
        assert_eq!(unclosed.body_offset, 0, "an unclosed block bounded a body");
        assert_eq!(
            unclosed
                .tags
                .iter()
                .map(|t| t.name.as_str())
                .collect::<Vec<_>>(),
            ["hashtag"]
        );
        assert!(
            unclosed.tags.iter().all(|t| t.source == TagSource::Body),
            "a tag was attributed to a block nothing read"
        );
        assert_eq!(
            unclosed
                .links
                .iter()
                .map(|l| l.target.as_str())
                .collect::<Vec<_>>(),
            ["Some Target"]
        );

        // The same text inside a block that closes: the block is skipped whole,
        // so none of it is read either as frontmatter or as body.
        let closed = derive("---\ntags: [alpha]\nlink: [[Some Target]]\nnote: : :\n---\n# body\n");
        assert!(closed.body_offset > 0, "a closed block bounded no body");
        assert!(closed.tags.is_empty(), "a skipped block yielded a tag");
        assert!(closed.links.is_empty(), "a skipped block yielded a link");
    }

    #[test]
    fn mapper_is_the_complete_text_to_store_boundary() {
        let source = b"---\ntags: [front]\nkind: note\n---\n# Heading\n[[target#Part|Title]] #body\nblock ^id\n";
        let derived = map_document(
            "note.md",
            source,
            norn_fs::ContentHash::of(source).to_string(),
        )
        .unwrap();
        let facts = derived.facts;
        assert!(derived.unread_frontmatter.is_none());
        assert!(facts.frontmatter.is_some());
        assert_eq!(facts.headings.len(), 1);
        assert_eq!(facts.links.len(), 1);
        assert_eq!(facts.blocks.len(), 1);
        assert_eq!(facts.tags.len(), 2);
    }

    /// The count beside an absent frontmatter projection is what tells a
    /// document with no block apart from one whose block did not read, so what
    /// the mapper counts is the notes the text layer scoped to the block.
    ///
    /// Every code that layer raises is scoped to the block today, so no
    /// document here is one the filter and a count of every note disagree
    /// over. What the filter holds is the seam: a note the text layer raises
    /// about something other than the block leaves this count through it, and
    /// the scope of a code is that layer's own answer rather than a spelling
    /// read here.
    #[test]
    fn the_frontmatter_note_count_separates_no_block_from_a_block_that_did_not_read() {
        for (source, projection, notes) in [
            (b"# Heading\nbody\n".to_vec(), false, 0),
            (b"---\ntitle: note\n---\nbody\n".to_vec(), true, 0),
            (b"---\ntitle: note\nbody\n".to_vec(), false, 1),
        ] {
            let facts = map_document(
                "note.md",
                &source,
                norn_fs::ContentHash::of(&source).to_string(),
            )
            .unwrap()
            .facts;
            let read = String::from_utf8(source).unwrap();
            assert_eq!(
                facts.frontmatter.is_some(),
                projection,
                "the projection of `{read}` is not what the block is"
            );
            assert_eq!(
                facts.frontmatter_diagnostic_count, notes,
                "`{read}` raised another count of block-scoped notes"
            );
        }
    }
}
