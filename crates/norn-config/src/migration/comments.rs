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

/// A block scalar being read: the column of the node holding its header,
/// and the indentation of its content once its header's indentation
/// indicator or its first content line fixes it.
struct Block {
    parent: usize,
    content: Option<usize>,
}

impl Block {
    /// Whether `line` is the block's content rather than what follows it: a
    /// blank line, or one indented as deep as its content — more deeply than
    /// the node holding its header, where nothing has fixed it yet.
    fn holds(&mut self, line: &str) -> bool {
        let indent = indentation(line);
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

/// The spaces `line` is indented by.
fn indentation(line: &str) -> usize {
    line.len() - line.trim_start_matches(' ').len()
}

/// Whether the byte at `at` in `bytes` is followed by whitespace or ends
/// the line.
fn ends_a_token(bytes: &[u8], at: usize) -> bool {
    bytes
        .get(at + 1)
        .is_none_or(|next| matches!(next, b' ' | b'\t'))
}

/// YAML's comments: a `#` at the start of a line or after a space or a tab,
/// outside a single- or double-quoted scalar — which may span lines — and
/// outside a block scalar's content.
///
/// **A quote opens a quoted scalar only where a scalar begins**: a line's
/// first token, after a key's `:`, a sequence entry's `-`, a `?`, a
/// document start, a tag or an anchor, or a flow collection's `[`, `{` or
/// `,`. A quote anywhere else is a character of the plain scalar it stands
/// in, and so is one on a line continuing a plain scalar — a line indented
/// past the node holding the scalar.
///
/// **A value is bounded by the node holding it**: the key whose value it
/// is, or the sequence entry's `-`, whose column may stand past its line's
/// indentation (`- a: b`) and whose line may precede the value's (`a:` with
/// the value on the next line). A plain scalar continues onto, and a block
/// scalar's content is, a line indented past that column.
fn yaml(text: &str) -> Vec<&str> {
    let mut found = Vec::new();
    let mut quote: Option<u8> = None;
    let mut block: Option<Block> = None;
    // The column of the node holding a plain scalar value, while the scalar
    // may still continue onto the next line.
    let mut plain: Option<usize> = None;
    // The column of the key or `-` whose value is still to come at the end
    // of a line, carried to the next line the value may begin on.
    let mut pending: Option<usize> = None;
    // How deep in flow collections the text stands.
    let mut flow = 0_usize;
    // Whether a scalar may begin at the next token: carried across lines in
    // a flow collection.
    let mut node_start = true;
    for line in lines(text) {
        if quote.is_none()
            && let Some(open) = block.as_mut()
        {
            if open.holds(line) {
                continue;
            }
            block = None;
        }
        let indent = indentation(line);
        if line.trim().is_empty() {
            continue;
        }
        let continues_plain = quote.is_none() && plain.is_some_and(|holder| indent > holder);
        if !continues_plain {
            plain = None;
        }
        if flow == 0 {
            node_start = !continues_plain;
        }
        let bytes = line.as_bytes();
        let mut comment = None;
        let mut previous: Option<u8> = None;
        let mut value_plain = continues_plain;
        let mut in_property = false;
        // The column of the node holding the next value on this line, and
        // of the node token begun since the last key or `-`.
        let mut holder = pending.take();
        let mut token: Option<usize> = None;
        // The holder of the plain scalar value this line began.
        let mut plain_holder = None;
        let mut at = 0;
        if quote.is_none()
            && (line.starts_with("---") || line.starts_with("..."))
            && ends_a_token(bytes, 2)
        {
            at = 3;
            previous = Some(bytes[2]);
            holder = None;
        }
        while at < bytes.len() {
            let byte = bytes[at];
            match quote {
                Some(b'\'') if byte == b'\'' => {
                    // A quote doubled is one quote inside the scalar.
                    if bytes.get(at + 1) == Some(&b'\'') {
                        at += 1;
                    } else {
                        quote = None;
                        node_start = false;
                    }
                }
                Some(b'"') if byte == b'\\' => at += 1,
                Some(b'"') if byte == b'"' => {
                    quote = None;
                    node_start = false;
                }
                Some(_) => {}
                None if byte == b'#'
                    && previous.is_none_or(|before| matches!(before, b' ' | b'\t')) =>
                {
                    comment = Some(at);
                    break;
                }
                None if matches!(byte, b' ' | b'\t') => in_property = false,
                None if in_property => {}
                None if node_start && matches!(byte, b'\'' | b'"') => {
                    token.get_or_insert(at);
                    quote = Some(byte);
                    value_plain = false;
                }
                None if node_start && matches!(byte, b'[' | b'{') => {
                    token.get_or_insert(at);
                    flow += 1;
                    value_plain = false;
                }
                None if flow > 0 && matches!(byte, b']' | b'}') => {
                    flow -= 1;
                    node_start = false;
                    value_plain = false;
                }
                None if flow > 0 && byte == b',' => {
                    node_start = true;
                    value_plain = false;
                }
                None if byte == b':' && ends_a_token(bytes, at) => {
                    // What stood before it was a key, which holds the value
                    // beginning next.
                    holder = Some(token.take().unwrap_or(at));
                    node_start = true;
                    value_plain = false;
                }
                None if node_start && matches!(byte, b'-' | b'?') && ends_a_token(bytes, at) => {
                    holder = Some(at);
                    token = None;
                }
                None if node_start && matches!(byte, b'!' | b'&') => {
                    token.get_or_insert(at);
                    in_property = true;
                }
                None if node_start && matches!(byte, b'*' | b'|' | b'>') => {
                    token.get_or_insert(at);
                    node_start = false;
                }
                None => {
                    if node_start {
                        token.get_or_insert(at);
                        value_plain = true;
                        plain_holder = holder;
                    }
                    node_start = false;
                }
            }
            previous = Some(byte);
            at += 1;
        }
        if let Some(at) = comment {
            found.push(line[at..].trim_end());
        }
        let opens_block = quote
            .is_none()
            .then(|| block_header(&line[..comment.unwrap_or(line.len())]))
            .flatten();
        if let Some(indicator) = opens_block {
            let parent = holder.unwrap_or(indent);
            block = Some(Block {
                parent,
                content: indicator.map(|digit| parent + digit),
            });
        }
        // A comment ends a plain scalar; one left open may continue onto a
        // line indented past the node holding it.
        plain = (quote.is_none() && flow == 0 && comment.is_none() && value_plain)
            .then(|| plain.or(plain_holder).unwrap_or(indent));
        // A key or `-` whose value has not begun holds the value the next
        // line may begin.
        pending = (quote.is_none() && flow == 0 && node_start)
            .then_some(holder)
            .flatten();
    }
    found
}

/// Whether `content`, a line with its comment taken off, ends in a block
/// scalar's header — `|` or `>` with its chomping and indentation
/// indicators, standing where a value does: the line's first token, or
/// after a key, a sequence entry's `-`, a `?`, a document start, a tag or
/// an anchor — and the content indentation its indicator states past its
/// line's, where it states one.
fn block_header(content: &str) -> Option<Option<usize>> {
    let mut tokens = content.split_whitespace().rev();
    let last = tokens.next()?;
    let indicators = last.strip_prefix(['|', '>'])?;
    let header = indicators.len() <= 2
        && indicators
            .bytes()
            .all(|byte| matches!(byte, b'+' | b'-' | b'1'..=b'9'));
    let placed = tokens.next().is_none_or(|before| {
        before.ends_with(':')
            || matches!(before, "-" | "?" | "---")
            || before.starts_with(['!', '&'])
    });
    (header && placed).then(|| {
        indicators
            .bytes()
            .find(u8::is_ascii_digit)
            .map(|digit| usize::from(digit - b'0'))
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
