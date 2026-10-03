//! Which comments a control file holds, read by its format's own rule for
//! what begins one — without parsing the file, since a rewrite is judged
//! against the text its author wrote.
//!
//! **A comment is its text**: from the `#` that begins it to the end of its
//! line, its trailing whitespace aside. Each lexer reads the file once, a
//! byte at a time: every byte that begins a comment, a quote or a block in
//! either format is ASCII, and no byte of a multi-byte character is, so a
//! comment's `#` is always a character boundary.

use std::collections::BTreeMap;

use super::Format;

/// The first comment of `before`, in its order, that `after` does not hold
/// as often as `before` does.
pub(super) fn first_lost(format: Format, before: &str, after: &str) -> Option<String> {
    let mut kept: BTreeMap<&str, usize> = BTreeMap::new();
    for comment in comments(format, after) {
        *kept.entry(comment).or_default() += 1;
    }
    comments(format, before)
        .into_iter()
        .find(|comment| match kept.get_mut(comment) {
            Some(count) if *count > 0 => {
                *count -= 1;
                false
            }
            _ => true,
        })
        .map(str::to_string)
}

/// Every comment `text` holds, in order.
fn comments(format: Format, text: &str) -> Vec<&str> {
    match format {
        Format::Yaml => yaml(text),
        Format::Toml => toml(text),
    }
}

/// Each line of `text`, without its line break.
fn lines(text: &str) -> impl Iterator<Item = &str> {
    text.split('\n')
        .map(|line| line.strip_suffix('\r').unwrap_or(line))
}

/// A block scalar being read: the indentation of the line holding its
/// header, and of its content once the first content line fixes it.
struct Block {
    parent: usize,
    content: Option<usize>,
}

impl Block {
    /// Whether `line` is the block's content rather than what follows it: a
    /// blank line, or one indented as deep as its content — more deeply than
    /// its header's line, where no content line has fixed it yet.
    fn holds(&mut self, line: &str) -> bool {
        let indent = line.len() - line.trim_start_matches(' ').len();
        if indent == line.len() {
            return true;
        }
        match self.content {
            Some(content) => indent >= content,
            None if indent > self.parent => {
                self.content = Some(indent);
                true
            }
            None => false,
        }
    }
}

/// YAML's comments: a `#` at the start of a line or after a space or a tab,
/// outside a single- or double-quoted scalar — which may span lines — and
/// outside a block scalar's content.
fn yaml(text: &str) -> Vec<&str> {
    let mut found = Vec::new();
    let mut quote: Option<u8> = None;
    let mut block: Option<Block> = None;
    for line in lines(text) {
        if quote.is_none()
            && let Some(open) = block.as_mut()
        {
            if open.holds(line) {
                continue;
            }
            block = None;
        }
        let bytes = line.as_bytes();
        let mut comment = None;
        let mut previous: Option<u8> = None;
        let mut at = 0;
        while at < bytes.len() {
            let byte = bytes[at];
            match quote {
                Some(b'\'') if byte == b'\'' => {
                    // A quote doubled is one quote inside the scalar.
                    if bytes.get(at + 1) == Some(&b'\'') {
                        at += 1;
                    } else {
                        quote = None;
                    }
                }
                Some(b'"') if byte == b'\\' => at += 1,
                Some(b'"') if byte == b'"' => quote = None,
                Some(_) => {}
                None if byte == b'#'
                    && previous.is_none_or(|before| matches!(before, b' ' | b'\t')) =>
                {
                    comment = Some(at);
                    break;
                }
                None if matches!(byte, b'\'' | b'"')
                    && previous.is_none_or(|before| {
                        matches!(before, b' ' | b'\t' | b'[' | b'{' | b',')
                    }) =>
                {
                    quote = Some(byte);
                }
                None => {}
            }
            previous = Some(byte);
            at += 1;
        }
        if let Some(at) = comment {
            found.push(line[at..].trim_end());
        }
        if quote.is_none() && opens_a_block(&line[..comment.unwrap_or(line.len())]) {
            let parent = line.len() - line.trim_start_matches(' ').len();
            block = Some(Block {
                parent,
                content: None,
            });
        }
    }
    found
}

/// Whether `content`, a line with its comment taken off, ends in a block
/// scalar's header: `|` or `>` with its chomping and indentation indicators,
/// standing where a value does — the line's first token, or after a key, a
/// sequence entry's `-`, a `?`, a document start, a tag or an anchor.
fn opens_a_block(content: &str) -> bool {
    let mut tokens = content.split_whitespace().rev();
    let Some(last) = tokens.next() else {
        return false;
    };
    let header = last.strip_prefix(['|', '>']).is_some_and(|indicators| {
        indicators.len() <= 2
            && indicators
                .bytes()
                .all(|byte| matches!(byte, b'+' | b'-' | b'1'..=b'9'))
    });
    header
        && tokens.next().is_none_or(|before| {
            before.ends_with(':')
                || matches!(before, "-" | "?" | "---")
                || before.starts_with(['!', '&'])
        })
}

/// TOML's comments: a `#` outside a basic, literal or multi-line string,
/// with or without whitespace before it.
fn toml(text: &str) -> Vec<&str> {
    let mut found = Vec::new();
    let mut multi: Option<u8> = None;
    for line in lines(text) {
        let bytes = line.as_bytes();
        let mut single: Option<u8> = None;
        let mut at = 0;
        while at < bytes.len() {
            let byte = bytes[at];
            if let Some(quote) = multi {
                if byte == quote && bytes[at..].starts_with(&[quote; 3]) {
                    // Up to two quotes before the closing three are the
                    // string's own.
                    let run = bytes[at..].iter().take_while(|&&b| b == quote).count();
                    at += run.min(5);
                    multi = None;
                    continue;
                }
                if quote == b'"' && byte == b'\\' {
                    at += 1;
                }
                at += 1;
                continue;
            }
            if let Some(quote) = single {
                if quote == b'"' && byte == b'\\' {
                    at += 1;
                } else if byte == quote {
                    single = None;
                }
                at += 1;
                continue;
            }
            match byte {
                b'#' => {
                    found.push(line[at..].trim_end());
                    break;
                }
                b'"' | b'\'' if bytes[at..].starts_with(&[byte; 3]) => {
                    multi = Some(byte);
                    at += 3;
                    continue;
                }
                b'"' | b'\'' => single = Some(byte),
                _ => {}
            }
            at += 1;
        }
    }
    found
}
