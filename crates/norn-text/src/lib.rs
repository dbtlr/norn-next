#![forbid(unsafe_code)]
//! The syntax of a vault document, and never its semantics.
//!
//! This crate answers *what does this document say* — where its frontmatter
//! block is, what its fields are worth, where its headings and sections start,
//! which `[[…]]` tokens it carries — and it never answers *what does that
//! mean* or *is it right*. It knows nothing about vaults, schemas, caches or
//! configuration, links nothing else in the workspace, and reaches no
//! filesystem: pure functions over strings, in and out.
//!
//! It is also the workspace's one document grammar. A second reading of
//! document text anywhere else is a defect, because two readings of the same
//! bytes drift and the drift is invisible until it corrupts something.
//!
//! # The fidelity boundary
//!
//! **Bytes outside the edited construct do not move.** That is the invariant
//! this crate exists to hold, and it is exact rather than aspirational:
//!
//! - Comments survive an edit — a standalone comment is the document's, not
//!   the field's it happens to sit above, so removing that field leaves it
//!   standing.
//! - Blank structure survives. The blank lines separating fields, and the ones
//!   separating a heading from its body, are separators rather than content,
//!   and an edit addressed at what they separate leaves them alone.
//! - The line terminator survives. A CRLF document stays CRLF, including in
//!   the lines an edit synthesizes.
//! - Key order survives. Fields are never reordered, and a new one is appended
//!   rather than sorted in.
//! - Untouched values keep their quoting and their block style, byte for byte.
//!
//! **The edited construct itself renders minimally.** It is emitted at the
//! least-quoted style its origin permits, and escalates only as far as the
//! context-aware round-trip requires — see [`RenderError`]. An explicit quote
//! style is a floor and is never weakened.
//!
//! **Where the construct ends is where the boundary is.** A scalar set
//! replaces the value's bytes, so a comment sharing that line survives. A
//! sequence set replaces the whole entry — a list's items are not separately
//! addressable, and rewriting the items around interleaved comments would be
//! guesswork — so a comment written *inside* that entry, **including one
//! trailing it on the same line**, is replaced with it: the comment sits
//! inside the entry's bytes, and the entry's bytes are what a whole-entry
//! replacement writes over. A comment on its own line between two entries
//! belongs to neither and always survives.
//!
//! **What cannot be proven is refused.** Every emitted scalar is re-parsed in
//! the lexical context it will live in and compared against the value it came
//! from, and every edit re-reads the document it produced before returning it.
//! An edit that cannot be shown to have done what was asked returns an error
//! and no bytes. Refusing costs an edit; a wrong span costs a document.
//!
//! # The value model
//!
//! A frontmatter value is one of seven shapes — null, bool, int, float,
//! string, sequence, string-keyed map — and the parse boundary strips
//! everything outside them once, loudly, as diagnostics. See [`Value`] for
//! what is stripped and why.
//!
//! The *model* is dialect-free: no shape in it is a YAML shape, and no
//! serialization framework appears in any signature here, so the parser behind
//! the seam is replaceable without touching a caller. The *style* vocabulary
//! is not, and does not pretend to be — [`ValueStyle`] and [`ScalarContext`]
//! are YAML's own, because preserving how a value was written is a question
//! about the dialect the document is in, and the round-trip contract this
//! crate holds is YAML's by charter. What that dialect resolves is stated
//! rather than inherited: scalar reads follow the YAML 1.2 core schema, so
//! `publish: no` is the string `no`, and [`Value`] names the rest.
//!
//! # Code is opaque
//!
//! Fenced code blocks, indented code blocks and inline code spans are a
//! different document. No heading, wikilink or block id may be recognized
//! inside one. See [`BodyScan`], which is also the single CommonMark pass
//! every body-level answer comes from.
//!
//! # Where to start
//!
//! - [`Document`] — read a document, then edit one field or one section.
//! - [`BodyScan`] — headings, sections, wikilinks and block ids, in one pass.
//! - [`render_document`] — write a whole document from scratch.

mod body;
mod diagnostic;
mod document;
mod frontmatter;
mod heading;
mod line_ending;
mod section;
mod span;
mod value;
mod wikilink;

pub use body::BodyScan;
pub use diagnostic::Diagnostic;
pub use document::{Document, EditError, FieldText, frontmatter_reads_back};
pub use frontmatter::{Field, RenderError, ScalarContext, ValueStyle, render_document};
pub use heading::{Heading, slugify};
pub use line_ending::LineEnding;
pub use section::{SectionAddress, SectionError, SectionSpan};
pub use span::SourceSpan;
pub use value::{Mapping, Value};
pub use wikilink::{
    Wikilink, parse_wikilinks_in_text, reconstruct_wikilink, splice_wikilinks_in_text,
    split_wikilink_target, wikilink_target_is_representable,
};
