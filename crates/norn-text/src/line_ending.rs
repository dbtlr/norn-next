//! The line terminator a document is written with.

use crate::span::{split_lines_inclusive, trailing_break};

/// A document's observed line terminator: the three spellings a synthesis path
/// can write.
///
/// Every line a synthesis path emits uses the terminator the document already
/// uses, so editing a CRLF document never leaves it half LF and editing a
/// CR-only document never leaves it half CR.
///
/// The three variants are the three spellings of this crate's line break —
/// `\n`, `\r\n`, and a lone `\r` — so a document is classified by the same
/// rule a reader counts its lines by.
///
/// Plain rather than `#[non_exhaustive]`: a fourth spelling arriving should
/// break every `match` that writes a terminator, because a synthesis path that
/// falls into a default arm writes the wrong bytes into somebody's document.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LineEnding {
    /// `\n`. The default for a document that contains no line break at all.
    #[default]
    Lf,
    /// `\r\n`.
    Crlf,
    /// A lone `\r`.
    Cr,
}

impl LineEnding {
    /// The terminator's bytes.
    pub fn as_str(self) -> &'static str {
        match self {
            LineEnding::Lf => "\n",
            LineEnding::Crlf => "\r\n",
            LineEnding::Cr => "\r",
        }
    }

    /// The terminator `content` uses: the spelling of the **first** line break
    /// it carries, whichever of the three that is.
    ///
    /// A mixed document is written with whatever its first break is: a
    /// document whose first line is terminated by a lone `\r` is `Cr` however
    /// many `\n` breaks stand below it, and one whose first line is terminated
    /// by `\n` is `Lf` however many lone `\r` breaks stand below it. Content
    /// carrying no break at all is `Lf`, the spelling this crate writes
    /// without evidence of another.
    ///
    /// The fence is a document's first line, so a mixed document is classified
    /// by how its block is written rather than by its body: a `\r\n` fence
    /// over a `\r`-broken body is `Crlf`, and an edit writes CRLF into the
    /// block it edits. The body keeps its own spelling, because an edit
    /// respells nothing it leaves alone.
    pub fn of(content: &str) -> Self {
        let first_line = split_lines_inclusive(content).next().unwrap_or_default();
        match trailing_break(first_line) {
            Some("\r\n") => LineEnding::Crlf,
            Some("\r") => LineEnding::Cr,
            _ => LineEnding::Lf,
        }
    }
}
