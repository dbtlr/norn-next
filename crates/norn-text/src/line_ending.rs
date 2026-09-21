//! The line terminator a document is written with.

/// A document's observed line terminator: the two spellings a synthesis path
/// can write.
///
/// Every line a synthesis path emits uses the terminator the document already
/// uses, so editing a CRLF document never leaves it half LF.
///
/// **This is a classification of a document into two, not the crate's line
/// break rule.** The rule — `\n`, `\r\n`, or a lone `\r` — is
/// `crate::span`'s, and it is what a *reader* counts lines by. A document
/// broken by lone `\r` is read as the several lines it is, and classifies
/// here as [`LineEnding::Lf`], which is what an edit writes into the bytes it
/// replaces. Nothing an edit leaves alone is respelled, so the two answers do
/// not collide.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LineEnding {
    /// `\n`. The default for a document that contains no line break at all.
    #[default]
    Lf,
    /// `\r\n`.
    Crlf,
}

impl LineEnding {
    /// The terminator's bytes.
    pub fn as_str(self) -> &'static str {
        match self {
            LineEnding::Lf => "\n",
            LineEnding::Crlf => "\r\n",
        }
    }

    /// The terminator `content` uses, decided by its first `\n`: `Crlf` when
    /// a `\r` stands immediately before it, `Lf` otherwise.
    ///
    /// Content holding no `\n` classifies as `Lf`, which covers both a
    /// document with no line break at all and one broken by lone `\r` —
    /// neither carries evidence of the `\r\n` spelling, and `Lf` is the
    /// spelling this crate writes without it.
    pub fn of(content: &str) -> Self {
        match content.find('\n') {
            Some(index) if index > 0 && content.as_bytes()[index - 1] == b'\r' => LineEnding::Crlf,
            _ => LineEnding::Lf,
        }
    }
}
