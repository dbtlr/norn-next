//! The line terminator a document is written with.

/// A document's observed line terminator.
///
/// Every line a synthesis path emits uses the terminator the document already
/// uses, so editing a CRLF document never leaves it half LF.
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

    /// The terminator `content` uses, decided by its first line break.
    pub fn of(content: &str) -> Self {
        match content.find('\n') {
            Some(index) if index > 0 && content.as_bytes()[index - 1] == b'\r' => LineEnding::Crlf,
            _ => LineEnding::Lf,
        }
    }
}
