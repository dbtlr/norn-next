//! The document reader a get is handed: `norn-text`'s one section resolver
//! and its one block reading, over the rows the store holds of one document.
//!
//! **A get reads no vault bytes.** The body and the headings this reader is
//! handed are the snapshot's own rows of the document — the body the store
//! derived, and the headings read out of that body at derivation, with their
//! offsets into it — so a section or a block is cut from the text the
//! snapshot holds, at the instant the snapshot was established, whatever the
//! file on disk holds by then. The reader converts the stored headings back to
//! the `norn-text` headings they were derived from, and answers what
//! `norn_text::resolve_section` and `BodyScan::block_extent` answer over that
//! body.

use std::ops::Range;

use norn_store::{DocumentText, HeadingFact, SectionAt};
use norn_text::{BodyScan, Heading, SectionAddress, SourceSpan};

/// `norn-text`'s reading of a document's text, as a get reads it.
pub(crate) struct TextLayer;

impl DocumentText for TextLayer {
    fn section(&self, headings: &[HeadingFact], body: &str, anchor: &str) -> Option<SectionAt> {
        let headings: Vec<Heading> = headings.iter().map(text_heading).collect();
        let span =
            norn_text::resolve_section(&headings, body, SectionAddress::first(anchor)).ok()?;
        Some(SectionAt {
            heading: span.heading,
            body: span.body_start..span.end,
        })
    }

    fn block(&self, body: &str, marker: usize) -> Range<usize> {
        BodyScan::new(body).block_extent(marker)
    }
}

/// The `norn-text` heading a stored heading was derived from.
fn text_heading(heading: &HeadingFact) -> Heading {
    Heading {
        level: heading.level,
        text: heading.text.clone(),
        slug: heading.slug.clone(),
        span: SourceSpan {
            line: offset(heading.span.line),
            column: offset(heading.span.column),
            byte_offset: offset(heading.span.byte_offset),
        },
        body_offset: offset(heading.body_offset),
        inside_container: heading.inside_container,
    }
}

/// A stored position as a `usize`. The store hands this reader only byte
/// offsets that are positions in the body it hands with them, so an offset
/// always converts; a line or a column past the address space saturates, and
/// the resolver reads neither to cut a body.
fn offset(value: u64) -> usize {
    usize::try_from(value).unwrap_or(usize::MAX)
}
