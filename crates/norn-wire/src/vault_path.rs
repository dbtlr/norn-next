//! The grammar of a vault-relative path, written once.
//!
//! **A document path is one the store can hold a document at.** It is
//! relative to the vault root and not empty; its segments are separated by
//! `/`, and none is empty, `.` or `..`; it holds no backslash and no control
//! character; and its file name is not `.` or `..` once its extension is
//! dropped. Every reader of a document path — the wire's [`DocumentPath`],
//! the store's index key, a schema's creation-rule target — judges it here, so
//! a path one of them admits is a path every one of them admits.
//!
//! **Three altitudes of one grammar.** [`PathProblem::of_place`] is the floor
//! every vault-relative path keeps, the folder and file paths that name places
//! on disk included: it names something and does not start at a filesystem
//! root. [`PathProblem::of_segments`] adds the segment rules, which a
//! directory prefix the store ranges over keeps. [`PathProblem::of_document`]
//! adds the leaf's rule, which only a document has.
//!
//! [`DocumentPath`]: crate::DocumentPath

use std::fmt;

/// The separator between a path's segments.
const SEPARATOR: char = '/';

/// Why a text is no vault-relative path of the kind asked for.
///
/// Closed: these are every refusal the grammar makes, and [`message`] is the
/// one sentence each is read as.
///
/// [`message`]: PathProblem::message
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[non_exhaustive]
pub enum PathProblem {
    /// The path is empty.
    Empty,
    /// The path starts with `/`.
    Absolute,
    /// The path holds a `\`.
    Backslash,
    /// The path holds a control character, NUL included.
    ControlCharacter,
    /// The path holds an empty segment: a doubled `/`, or a `/` at its end.
    EmptySegment,
    /// The path holds a `.` or `..` segment.
    DotSegment,
    /// The path's file name is `.` or `..` once its extension is dropped.
    DotStem,
}

impl PathProblem {
    /// Why `text` names no place in a vault, or `None` where it names one:
    /// it is not empty and does not start at a filesystem root.
    pub fn of_place(text: &str) -> Option<Self> {
        if text.is_empty() {
            return Some(PathProblem::Empty);
        }
        if text.starts_with(SEPARATOR) {
            return Some(PathProblem::Absolute);
        }
        None
    }

    /// Why `text` is no path of segments the store can hold, or `None` where
    /// it is one: [`of_place`](Self::of_place), then no refused character and
    /// no refused segment.
    pub fn of_segments(text: &str) -> Option<Self> {
        if let Some(problem) = PathProblem::of_place(text) {
            return Some(problem);
        }
        if text.contains('\\') {
            return Some(PathProblem::Backslash);
        }
        if text.contains(char::is_control) {
            return Some(PathProblem::ControlCharacter);
        }
        text.split(SEPARATOR).find_map(|segment| match segment {
            "" => Some(PathProblem::EmptySegment),
            "." | ".." => Some(PathProblem::DotSegment),
            _ => None,
        })
    }

    /// Why `text` is no document path, or `None` where it is one:
    /// [`of_segments`](Self::of_segments), then a file name whose stem is not
    /// `.` or `..`, which would name no ambiguity class.
    pub fn of_document(text: &str) -> Option<Self> {
        if let Some(problem) = PathProblem::of_segments(text) {
            return Some(problem);
        }
        let leaf = text.rsplit(SEPARATOR).next().unwrap_or(text);
        matches!(leaf_stem(leaf), "." | "..").then_some(PathProblem::DotStem)
    }

    /// The refusal as the clause a sentence about the path ends with.
    pub const fn message(self) -> &'static str {
        match self {
            PathProblem::Empty => "it is empty",
            PathProblem::Absolute => {
                "it is absolute, and a vault path is relative to the vault root"
            }
            PathProblem::Backslash => "it carries a backslash; segments are separated by `/`",
            PathProblem::ControlCharacter => "it carries a control character",
            PathProblem::EmptySegment => "it carries an empty segment",
            PathProblem::DotSegment => "it carries a `.` or `..` segment",
            PathProblem::DotStem => {
                "its file name reduces to a `.` or `..` stem once its extension is dropped"
            }
        }
    }
}

impl fmt::Display for PathProblem {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message())
    }
}

/// Whether a path holding `character` is refused for holding it: a backslash
/// or a control character.
///
/// The character half of [`PathProblem::of_segments`]; a renderer that
/// replaces what the grammar refuses replaces exactly these.
pub fn is_refused_character(character: char) -> bool {
    character == '\\' || character.is_control()
}

/// Whether a path holding `segment` is refused for holding it: an empty, `.`
/// or `..` segment.
///
/// The segment half of [`PathProblem::of_segments`].
pub fn is_refused_segment(segment: &str) -> bool {
    matches!(segment, "" | "." | "..")
}

/// A leaf segment with its final extension removed.
///
/// `.gitignore` is a name, not an empty stem with an extension, and reducing
/// it to nothing would put every dotfile in the same ambiguity class.
pub fn leaf_stem(leaf: &str) -> &str {
    match extension_dot(leaf) {
        Some(dot) => &leaf[..dot],
        None => leaf,
    }
}

/// The extension the last segment of `path` carries, or `None` where it
/// carries none. `.md` carries none. The one reading of a leaf's extension
/// that a link's address and a `move` request's ends are judged by.
pub(crate) fn leaf_extension(path: &str) -> Option<&str> {
    let leaf = path.rsplit(SEPARATOR).next().unwrap_or(path);
    extension_dot(leaf).map(|dot| &leaf[dot + 1..])
}

/// Where `leaf`'s extension starts: its last dot, where that dot is inside
/// the name. A dot leading the leaf opens a name rather than an extension.
/// The one rule for what a leaf's extension is, which [`leaf_stem`] and
/// [`leaf_extension`] both read.
fn extension_dot(leaf: &str) -> Option<usize> {
    leaf.rfind('.').filter(|&dot| dot > 0)
}
