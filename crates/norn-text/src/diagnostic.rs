//! Text-layer diagnostics.
//!
//! Reading a document is forgiving: a malformed frontmatter block, a key the
//! vault value model cannot hold, a block whose spans cannot be trusted — each
//! yields a [`Diagnostic`] and a usable rest-of-document rather than an error
//! return. Writing is not forgiving; a write that cannot be proven refuses.
//! That split is why a diagnostic carries no severity: everything on the
//! reading side is something the reader worked around, and everything on the
//! writing side is a refusal rather than a note.
//!
//! `code` is the stable identifier a caller branches on. The codes this crate
//! emits:
//!
//! | Code | Meaning |
//! |---|---|
//! | `frontmatter-unclosed` | An opening `---` with no closing delimiter. No block, no value. |
//! | `frontmatter-parse-failed` | The block is not well-formed YAML — duplicate keys included. No value. |
//! | `frontmatter-non-string-key` | An entry keyed by something other than a string. The entry is dropped. |
//! | `frontmatter-tag-stripped` | An explicit YAML tag. The tag is dropped and its value kept. |
//! | `frontmatter-integer-out-of-range` | An integer outside `i64`. It is carried as a float. |
//! | `frontmatter-not-editable` | The block parses, and its field spans cannot be trusted. Reads work; field edits refuse. |

/// A coded, human-readable note produced while reading text. `code` is a
/// stable kebab identifier a caller can branch on; `message` is prose;
/// `detail` carries an optional underlying cause.
///
/// Every diagnostic this crate emits is a warning — reading is forgiving by
/// contract, and the one thing that is not forgiving returns an error rather
/// than a note — so there is no severity to carry. A second severity would be
/// a change to that contract, and it would arrive with the field.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Diagnostic {
    pub code: String,
    pub message: String,
    pub detail: Option<String>,
}

impl Diagnostic {
    pub fn warning(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            detail: None,
        }
    }

    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }
}

impl std::fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)?;
        if let Some(detail) = &self.detail {
            write!(f, " ({detail})")?;
        }
        Ok(())
    }
}
