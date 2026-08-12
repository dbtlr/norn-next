//! Text-layer diagnostics.
//!
//! Reading a document is forgiving: a malformed frontmatter block, a key the
//! value model cannot hold, a block whose spans cannot be trusted — each
//! yields a [`Diagnostic`] and a usable rest-of-document rather than an error
//! return. Writing is not forgiving; a write that cannot be proven refuses.
//! That split is why a diagnostic carries no severity: everything on the
//! reading side is something the reader worked around, and everything on the
//! writing side is a refusal rather than a note.
//!
//! `code` is the stable identifier a caller branches on, and
//! [`DiagnosticCode`] is the closed list of the ones this crate raises. It is
//! a value rather than a string because every question a consumer asks of a
//! note is answered by the code itself, through a match that carries no
//! wildcard: a code minted without an answer does not compile, and no consumer
//! learns what a note is about by reading how its code is spelled.
//!
//! **A diagnostic code is not a code in the wire vocabulary.** The codes a
//! client enumerates and dispatches on are flat `namespace/what-happened`
//! strings, and they live in the wire registries — what the host refused, what
//! a finding is filed under. A diagnostic code carries no namespace and
//! crosses no seam: what a consumer sends on is what it derives from a note —
//! the count of frontmatter-scoped ones, which is what separates a document
//! with no frontmatter block from one whose block did not read — never the
//! code itself. A note that has to reach a client is a refusal or a finding,
//! and both of those are spelled in a registry rather than here.

use std::fmt;

/// The code a text-layer diagnostic is filed under: a stable kebab-case
/// identifier a caller branches on.
///
/// Plain rather than `#[non_exhaustive]`: matching this exhaustively is the
/// point, and a consumer that has not decided what a new note means should
/// fail to compile rather than fall into a default arm.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DiagnosticCode {
    /// An opening `---` with no closing delimiter. No block, no value.
    FrontmatterUnclosed,
    /// The block is not well-formed YAML — duplicate keys included. No value.
    FrontmatterParseFailed,
    /// The block is longer than
    /// [`FRONTMATTER_MAX_BYTES`](crate::FRONTMATTER_MAX_BYTES), so no parser
    /// reads it. No value, and nothing is truncated.
    FrontmatterTooLarge,
    /// An entry keyed by something other than a string. The entry is dropped.
    FrontmatterNonStringKey,
    /// An explicit YAML tag. The tag is dropped and its value kept.
    FrontmatterTagStripped,
    /// An integer outside `i64`. It is carried as a float.
    FrontmatterIntegerOutOfRange,
    /// The block parses, and its field spans cannot be trusted. Reads work;
    /// field edits refuse.
    FrontmatterNotEditable,
}

impl DiagnosticCode {
    /// How this code is written.
    pub const fn as_str(self) -> &'static str {
        match self {
            DiagnosticCode::FrontmatterUnclosed => "frontmatter-unclosed",
            DiagnosticCode::FrontmatterParseFailed => "frontmatter-parse-failed",
            DiagnosticCode::FrontmatterTooLarge => "frontmatter-too-large",
            DiagnosticCode::FrontmatterNonStringKey => "frontmatter-non-string-key",
            DiagnosticCode::FrontmatterTagStripped => "frontmatter-tag-stripped",
            DiagnosticCode::FrontmatterIntegerOutOfRange => "frontmatter-integer-out-of-range",
            DiagnosticCode::FrontmatterNotEditable => "frontmatter-not-editable",
        }
    }

    /// Whether the note this code files is about the frontmatter block.
    ///
    /// A consumer counting the frontmatter-scoped notes is telling a document
    /// with no block apart from a document whose block did not read, so the
    /// answer belongs to the code rather than to a caller matching on how the
    /// code is spelled.
    pub const fn frontmatter_scoped(self) -> bool {
        match self {
            DiagnosticCode::FrontmatterUnclosed
            | DiagnosticCode::FrontmatterParseFailed
            | DiagnosticCode::FrontmatterTooLarge
            | DiagnosticCode::FrontmatterNonStringKey
            | DiagnosticCode::FrontmatterTagStripped
            | DiagnosticCode::FrontmatterIntegerOutOfRange
            | DiagnosticCode::FrontmatterNotEditable => true,
        }
    }
}

impl fmt::Display for DiagnosticCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A coded, human-readable note produced while reading text. `code` is the
/// identifier a caller branches on; `message` is prose; `detail` carries an
/// optional underlying cause.
///
/// Every diagnostic this crate emits is a warning — reading is forgiving by
/// contract, and the one thing that is not forgiving returns an error rather
/// than a note — so there is no severity to carry. A second severity would be
/// a change to that contract, and it would arrive with the field.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Diagnostic {
    pub code: DiagnosticCode,
    pub message: String,
    pub detail: Option<String>,
}

impl Diagnostic {
    pub fn warning(code: DiagnosticCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            detail: None,
        }
    }

    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }
}

impl fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)?;
        if let Some(detail) = &self.detail {
            write!(f, " ({detail})")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The code after `code`, and nothing at the end of the walk. The match
    /// carries no wildcard, so a code minted without a place in the walk does
    /// not compile, and the place it is given is what puts it in the census
    /// below.
    const fn after(code: DiagnosticCode) -> Option<DiagnosticCode> {
        match code {
            DiagnosticCode::FrontmatterUnclosed => Some(DiagnosticCode::FrontmatterParseFailed),
            DiagnosticCode::FrontmatterParseFailed => Some(DiagnosticCode::FrontmatterTooLarge),
            DiagnosticCode::FrontmatterTooLarge => Some(DiagnosticCode::FrontmatterNonStringKey),
            DiagnosticCode::FrontmatterNonStringKey => Some(DiagnosticCode::FrontmatterTagStripped),
            DiagnosticCode::FrontmatterTagStripped => {
                Some(DiagnosticCode::FrontmatterIntegerOutOfRange)
            }
            DiagnosticCode::FrontmatterIntegerOutOfRange => {
                Some(DiagnosticCode::FrontmatterNotEditable)
            }
            DiagnosticCode::FrontmatterNotEditable => None,
        }
    }

    /// Every code this crate raises, walked from the first. The census is the
    /// codes' own account of themselves rather than a list kept beside them,
    /// and each test below reads every code it holds back through the
    /// wildcard-free matches on [`DiagnosticCode`].
    fn every_code() -> Vec<DiagnosticCode> {
        let mut codes = vec![DiagnosticCode::FrontmatterUnclosed];
        while let Some(next) = after(*codes.last().expect("the walk starts at a code")) {
            assert!(
                !codes.contains(&next),
                "`{next}` is reached twice, so the walk is a cycle rather than a census"
            );
            codes.push(next);
        }
        codes
    }

    /// A code is spelled the way a text-layer code is spelled and never the
    /// way a wire code is: lowercase kebab-case, and no namespace separator.
    /// A code that grew one would be a code in the wire grammar, which is a
    /// registry entry rather than a string minted here.
    #[test]
    fn no_code_is_spelled_as_a_wire_code() {
        for code in every_code() {
            let spelling = code.as_str();
            assert!(!spelling.is_empty(), "{code:?} is spelled with nothing");
            assert!(
                !spelling.contains('/'),
                "`{spelling}` carries a namespace separator"
            );
            assert!(
                spelling
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'),
                "`{spelling}` is not lowercase kebab-case"
            );
            assert!(
                !spelling.starts_with('-') && !spelling.ends_with('-'),
                "`{spelling}` is bounded by a hyphen"
            );
        }
    }

    /// One spelling per code, and a diagnostic reads as its own code.
    #[test]
    fn each_code_is_spelled_once_and_reads_back_in_a_diagnostic() {
        let mut seen = Vec::new();
        for code in every_code() {
            assert!(
                !seen.contains(&code.as_str()),
                "`{}` is spelled by two codes",
                code.as_str()
            );
            seen.push(code.as_str());
            let note = Diagnostic::warning(code, "a note");
            assert_eq!(note.code, code);
            assert_eq!(note.to_string(), format!("{}: a note", code.as_str()));
        }
    }

    /// Every code this crate raises today names the frontmatter block, which
    /// is what makes a count of them the count of the block's own notes.
    #[test]
    fn every_code_names_the_frontmatter_block() {
        for code in every_code() {
            assert!(
                code.frontmatter_scoped(),
                "`{}` is not scoped to the frontmatter block",
                code.as_str()
            );
        }
    }
}
