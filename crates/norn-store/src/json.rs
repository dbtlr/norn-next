//! The frontmatter projection — one value tree in, one canonical JSON text out.
//!
//! `documents.frontmatter` holds a JSON1 projection of the frontmatter block:
//! one column, queryable through SQLite's JSON functions, schema-independent
//! and carrying no invalidation key. **It is what a field's value is read off.**
//! The field pillar beside it ([`crate::FieldRows`]) is derived from this same
//! value and answers the other two questions — which documents carry a key and
//! a value, and in what order they stand — so a predicate or an order is
//! answered off the pillar's rows, while a projected value is read whole,
//! structure included, from here.
//!
//! # Projected, never authoritative
//!
//! The file is the authority for what a document's frontmatter says. This text
//! is a derived reading of it, kept because a reading in a column can be
//! queried and a file cannot. Two consequences follow and both are contract:
//!
//! - **The projection is one-way.** Key order is not preserved, so the value
//!   tree cannot be reconstructed from the column. Anything that has to render
//!   or edit frontmatter reads the file through the one parser; anything that
//!   has to *filter* on it reads this column.
//! - **A shape JSON cannot hold is projected to `null`**, which happens for a
//!   non-finite float. JSON has no NaN and no infinity, and inventing a
//!   spelling for them would put a value in the column that no JSON reader
//!   agrees about.
//!
//! # Canonical means the same value always writes the same bytes
//!
//! - **Map keys are sorted** by byte order, so a document whose fields were
//!   written in a different order projects identically. Document order is
//!   information, and it is information the *file* carries; a projection that
//!   kept it would make the column's bytes depend on something the column
//!   cannot be queried by.
//! - **A repeated key keeps its last value.** No document reaches this with
//!   one: a block writing one key twice, however the two were spelled, is
//!   refused where it is read, so a projected document's map answers each of
//!   its keys once. This is the canonicalization being total over the values a
//!   caller can compose rather than a case anything a document produces relies
//!   on, and the value it keeps is the one a second write through a map that
//!   replaces in place would leave.
//! - **Numbers are written as plain decimal digits, never in exponent
//!   notation**, in the shortest such spelling that reads back to the same
//!   value, with integral floats keeping a `.0` so that a float never projects
//!   as something `json_type` calls an integer. Choosing between plain and
//!   exponent form by magnitude would give one number two spellings — which is
//!   what canonicalization exists to prevent — so an extreme value is long
//!   rather than compact.
//! - **Strings escape exactly what JSON requires** and nothing else: the quote,
//!   the backslash, and the C0 controls. Every other scalar value is emitted as
//!   the UTF-8 it already is.
//!
//! # Nesting is bounded
//!
//! A value nested deeper than [`MAX_FRONTMATTER_DEPTH`] is **refused**, because
//! the alternative is a column no reader can read: SQLite's JSON1 functions
//! parse to a bounded depth, so past it a write that succeeded would produce
//! bytes `json_valid` rejects — which the store's own verification reports as
//! damage, permanently, through the rung that discards the database. Refusing
//! also keeps the writer's own recursion shallow.
//!
//! The bound sits above what the text layer can produce (libyaml stops at 128)
//! and below what SQLite accepts, so nothing a parsed document carries reaches
//! it. The API is public, though, and a caller composing a value tree is not
//! bounded by a parser at all.
//!
//! Which is why **every walk over a value is iterative**, the measurement and the
//! value's own [`Drop`] alike. A value past the bound is refused rather than
//! written, and it then still has to be *freed*: a recursive drop would recurse
//! once per level and abort the process on a value this API accepted, which would
//! make the refusal a crash one statement later rather than an error a caller
//! reads. Reading a projection back ([`projected_fields`]) is a walk over text
//! the writer already bounded, so it recurses and refuses past the same bound.

/// A frontmatter value, as the store takes it.
///
/// The seven shapes of the frontmatter value model and no eighth. It is the
/// store's own type rather than the text layer's: the store depends on no
/// parser, and the host maps one onto the other.
///
/// A map is a list of pairs rather than a sorted structure because the caller
/// hands over what it read, in the order it read it; sorting is
/// [`canonical_json`]'s job and happens once, where the bytes are written.
///
/// **Its `Drop` is iterative**, because a value this type can hold is not bounded
/// by what this crate will project: nesting deeper than
/// [`MAX_FRONTMATTER_DEPTH`] is refused, and the refused value is still the
/// caller's to drop. A derived recursive drop would recurse once per level and
/// abort the process on a deep one — turning a refusal into a crash, at the far
/// end of the call that refused. Dropping is the one traversal that happens
/// whether or not anything asked for it; the derived `Clone`, `Debug` and
/// `PartialEq` recurse, and a caller that asks one of them for a value past the
/// bound is asking for that depth.
#[derive(Clone, Debug, PartialEq)]
pub enum FrontmatterValue {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    String(String),
    Sequence(Vec<FrontmatterValue>),
    Map(Vec<(String, FrontmatterValue)>),
}

impl Drop for FrontmatterValue {
    fn drop(&mut self) {
        let mut pending = Vec::new();
        take_children(self, &mut pending);
        // Each value taken out of the worklist has had its own children moved
        // into the worklist, so the drop that runs when it leaves this scope has
        // an empty container to free and recurses no further.
        while let Some(mut value) = pending.pop() {
            take_children(&mut value, &mut pending);
        }
    }
}

/// Move a value's children onto `pending`, leaving an empty container behind.
///
/// A map's keys are dropped here rather than carried: a `String` holds no
/// [`FrontmatterValue`], so it is not part of the walk.
fn take_children(value: &mut FrontmatterValue, pending: &mut Vec<FrontmatterValue>) {
    match value {
        FrontmatterValue::Sequence(items) => pending.append(items),
        FrontmatterValue::Map(entries) => {
            pending.extend(std::mem::take(entries).into_iter().map(|(_, value)| value));
        }
        _ => {}
    }
}

/// How deeply a frontmatter projection may nest.
///
/// Above what any parsed document can carry, below what SQLite's JSON1 reader
/// accepts. See the module documentation for why the projection is bounded at
/// all.
pub const MAX_FRONTMATTER_DEPTH: usize = 256;

/// The canonical JSON text for one value, or a refusal where it nests deeper
/// than [`MAX_FRONTMATTER_DEPTH`].
pub fn canonical_json(value: &FrontmatterValue) -> Result<String, crate::StoreError> {
    let depth = depth(value);
    if depth > MAX_FRONTMATTER_DEPTH {
        return Err(crate::StoreError::Bound {
            what: "a frontmatter projection's nesting",
            limit: MAX_FRONTMATTER_DEPTH,
            given: depth,
        });
    }
    let mut out = String::new();
    write_value(value, &mut out);
    Ok(out)
}

/// How deeply a value nests: a scalar is 1, a container is one more than its
/// deepest member, and an empty container is 1.
///
/// Walked with an explicit stack and stopped one past the bound. Measuring
/// recursively would be the same overflow the bound exists to refuse, and
/// measuring a pathological tree exactly would cost the walk the refusal is
/// meant to avoid — so a value deeper than the bound reports the bound plus one
/// rather than its true depth.
fn depth(value: &FrontmatterValue) -> usize {
    let mut deepest = 0;
    let mut pending = vec![(value, 1_usize)];
    while let Some((value, depth)) = pending.pop() {
        deepest = deepest.max(depth);
        if depth > MAX_FRONTMATTER_DEPTH {
            return depth;
        }
        match value {
            FrontmatterValue::Sequence(items) => {
                pending.extend(items.iter().map(|item| (item, depth + 1)));
            }
            FrontmatterValue::Map(entries) => {
                pending.extend(entries.iter().map(|(_, value)| (value, depth + 1)));
            }
            _ => {}
        }
    }
    deepest
}

fn write_value(value: &FrontmatterValue, out: &mut String) {
    match value {
        FrontmatterValue::Null => out.push_str("null"),
        FrontmatterValue::Bool(true) => out.push_str("true"),
        FrontmatterValue::Bool(false) => out.push_str("false"),
        FrontmatterValue::Int(number) => out.push_str(&number.to_string()),
        FrontmatterValue::Float(number) => write_float(*number, out),
        FrontmatterValue::String(text) => write_string(text, out),
        FrontmatterValue::Sequence(items) => {
            out.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                write_value(item, out);
            }
            out.push(']');
        }
        FrontmatterValue::Map(entries) => {
            let mut sorted: Vec<&(String, FrontmatterValue)> = entries.iter().collect();
            // A stable sort, so that a repeated key keeps the entries in the
            // order they arrived and the last of the run is the one written.
            sorted.sort_by(|left, right| left.0.cmp(&right.0));
            out.push('{');
            let mut written = 0usize;
            for (index, (key, value)) in sorted.iter().enumerate() {
                if sorted.get(index + 1).is_some_and(|next| next.0 == *key) {
                    continue;
                }
                if written > 0 {
                    out.push(',');
                }
                write_string(key, out);
                out.push(':');
                write_value(value, out);
                written += 1;
            }
            out.push('}');
        }
    }
}

/// A float as plain decimal digits, or `null` where JSON has no spelling for it.
///
/// The form carries no exponent, whatever the magnitude: an extreme value writes
/// its digits out. It is the shortest *plain decimal* spelling that reads back to
/// the same double, which is the property the projection needs — a compact
/// exponent notation would be a second spelling of the same number, and a column
/// two equal values disagree about is the thing canonicalization exists to
/// prevent.
fn write_float(number: f64, out: &mut String) {
    out.push_str(float_text(number).as_deref().unwrap_or("null"));
}

/// A finite float's canonical spelling, or nothing where JSON has none.
///
/// The one spelling of a float in the store: the projection writes it, and the
/// field pillar's raw text is it, so a value read off either says the same
/// digits.
pub(crate) fn float_text(number: f64) -> Option<String> {
    if !number.is_finite() {
        return None;
    }
    let mut written = number.to_string();
    // `1.0` writes as `1`, which every JSON reader calls an integer. The
    // fractional marker is what keeps a float's shape in the projection.
    if !written.contains('.') {
        written.push_str(".0");
    }
    Some(written)
}

fn write_string(text: &str, out: &mut String) {
    out.push('"');
    for ch in text.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            control if control < ' ' => {
                out.push_str(&format!("\\u{:04x}", control as u32));
            }
            other => out.push(other),
        }
    }
    out.push('"');
}

/// The fields a canonical projection's top-level map holds, each as the tree
/// of values the wire carries it as.
///
/// **The reader of the text [`canonical_json`] writes**, and of nothing
/// looser: a string crosses as its content, a number and a boolean as the
/// digits and the word the projection spelled, a null as
/// [`FieldValue::Null`], and a sequence and a map as the containers they
/// are. A scalar's text is therefore the field pillar's raw text for the same
/// value, so a projected value and the value a predicate matched are one
/// spelling. A projection whose top level is not a map carries no fields, as
/// it derives no field rows.
///
/// The walk recurses once per level of nesting, and refuses a text nested past
/// [`MAX_FRONTMATTER_DEPTH`] as the writer does, so the recursion is bounded
/// by what a projection can hold. Text that is not JSON is not a projection
/// this crate wrote, and is reported as damage.
pub(crate) fn projected_fields(
    text: &str,
) -> Result<std::collections::BTreeMap<String, norn_wire::FieldValue>, crate::StoreError> {
    let mut reader = ProjectionReader {
        text,
        at: 0,
        depth: 0,
    };
    reader.skip_space();
    let fields = if reader.peek() == Some(b'{') {
        let norn_wire::FieldValue::Map { entries, .. } = reader.value()? else {
            unreachable!("an object reads as a map")
        };
        entries
    } else {
        reader.value()?;
        std::collections::BTreeMap::new()
    };
    reader.skip_space();
    if reader.at != text.len() {
        return Err(reader.damaged("text after the value"));
    }
    Ok(fields)
}

/// A cursor over one projection's text.
struct ProjectionReader<'a> {
    text: &'a str,
    at: usize,
    depth: usize,
}

impl ProjectionReader<'_> {
    fn peek(&self) -> Option<u8> {
        self.text.as_bytes().get(self.at).copied()
    }

    fn skip_space(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\n' | b'\r' | b'\t')) {
            self.at += 1;
        }
    }

    fn damaged(&self, problem: &str) -> crate::StoreError {
        crate::StoreError::Damaged {
            what: format!(
                "`documents.frontmatter` is not a projection this store writes: {problem} at \
                 byte {}",
                self.at
            ),
        }
    }

    fn expect(&mut self, byte: u8) -> Result<(), crate::StoreError> {
        if self.peek() == Some(byte) {
            self.at += 1;
            Ok(())
        } else {
            Err(self.damaged(&format!("expected `{}`", byte as char)))
        }
    }

    fn value(&mut self) -> Result<norn_wire::FieldValue, crate::StoreError> {
        use norn_wire::FieldValue;

        self.skip_space();
        match self.peek() {
            Some(b'{') => self.nested(|reader| {
                let mut entries = std::collections::BTreeMap::new();
                reader.at += 1;
                reader.skip_space();
                if reader.peek() == Some(b'}') {
                    reader.at += 1;
                    return Ok(FieldValue::map(entries));
                }
                loop {
                    reader.skip_space();
                    let key = reader.string()?;
                    reader.skip_space();
                    reader.expect(b':')?;
                    let value = reader.value()?;
                    entries.insert(key, value);
                    reader.skip_space();
                    match reader.peek() {
                        Some(b',') => reader.at += 1,
                        Some(b'}') => {
                            reader.at += 1;
                            return Ok(FieldValue::map(entries));
                        }
                        _ => return Err(reader.damaged("expected `,` or `}`")),
                    }
                }
            }),
            Some(b'[') => self.nested(|reader| {
                let mut items = Vec::new();
                reader.at += 1;
                reader.skip_space();
                if reader.peek() == Some(b']') {
                    reader.at += 1;
                    return Ok(FieldValue::sequence(items));
                }
                loop {
                    items.push(reader.value()?);
                    reader.skip_space();
                    match reader.peek() {
                        Some(b',') => reader.at += 1,
                        Some(b']') => {
                            reader.at += 1;
                            return Ok(FieldValue::sequence(items));
                        }
                        _ => return Err(reader.damaged("expected `,` or `]`")),
                    }
                }
            }),
            Some(b'"') => Ok(FieldValue::scalar(self.string()?)),
            Some(b'n') => self.word("null").map(|()| FieldValue::null()),
            Some(b't') => self.word("true").map(|()| FieldValue::scalar("true")),
            Some(b'f') => self.word("false").map(|()| FieldValue::scalar("false")),
            Some(b'-' | b'0'..=b'9') => {
                let start = self.at;
                while matches!(
                    self.peek(),
                    Some(b'-' | b'+' | b'.' | b'e' | b'E' | b'0'..=b'9')
                ) {
                    self.at += 1;
                }
                Ok(FieldValue::scalar(&self.text[start..self.at]))
            }
            _ => Err(self.damaged("expected a value")),
        }
    }

    /// Read one container one level deeper, refusing past the projection's
    /// bound.
    fn nested(
        &mut self,
        read: impl FnOnce(&mut Self) -> Result<norn_wire::FieldValue, crate::StoreError>,
    ) -> Result<norn_wire::FieldValue, crate::StoreError> {
        self.depth += 1;
        if self.depth > MAX_FRONTMATTER_DEPTH {
            return Err(self.damaged("nesting past the projection's bound"));
        }
        let value = read(self);
        self.depth -= 1;
        value
    }

    fn word(&mut self, word: &str) -> Result<(), crate::StoreError> {
        if self.text[self.at..].starts_with(word) {
            self.at += word.len();
            Ok(())
        } else {
            Err(self.damaged("expected a value"))
        }
    }

    /// A JSON string's content, its escapes read back.
    fn string(&mut self) -> Result<String, crate::StoreError> {
        self.expect(b'"')?;
        let mut content = String::new();
        loop {
            let rest = &self.text[self.at..];
            let run = rest
                .find(['"', '\\'])
                .ok_or_else(|| self.damaged("an unclosed string"))?;
            content.push_str(&rest[..run]);
            self.at += run;
            if self.peek() == Some(b'"') {
                self.at += 1;
                return Ok(content);
            }
            self.at += 1;
            let escaped = self
                .peek()
                .ok_or_else(|| self.damaged("an unclosed escape"))?;
            self.at += 1;
            match escaped {
                b'"' => content.push('"'),
                b'\\' => content.push('\\'),
                b'/' => content.push('/'),
                b'b' => content.push('\u{8}'),
                b'f' => content.push('\u{c}'),
                b'n' => content.push('\n'),
                b'r' => content.push('\r'),
                b't' => content.push('\t'),
                b'u' => {
                    let high = self.code_unit()?;
                    let character = if (0xD800..0xDC00).contains(&high) {
                        self.expect(b'\\')?;
                        self.expect(b'u')?;
                        let low = self.code_unit()?;
                        char::decode_utf16([high, low]).next().and_then(Result::ok)
                    } else {
                        char::from_u32(u32::from(high))
                    };
                    content.push(character.ok_or_else(|| self.damaged("an unpaired surrogate"))?);
                }
                _ => return Err(self.damaged("an unknown escape")),
            }
        }
    }

    /// The four hex digits of a `\u` escape.
    fn code_unit(&mut self) -> Result<u16, crate::StoreError> {
        let digits = self
            .text
            .get(self.at..self.at + 4)
            .and_then(|digits| u16::from_str_radix(digits, 16).ok())
            .ok_or_else(|| self.damaged("a `\\u` escape without four hex digits"))?;
        self.at += 4;
        Ok(digits)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use norn_wire::FieldValue;

    use super::*;

    fn string(text: &str) -> FrontmatterValue {
        FrontmatterValue::String(text.to_string())
    }

    /// **What the projection writes, the reader reads back as the wire's
    /// tree**: every scalar as the text the field pillar's raw column holds
    /// for it, a null as a null, and the containers as containers, escapes and
    /// characters outside the Basic Multilingual Plane included.
    #[test]
    fn a_projection_reads_back_as_the_fields_it_was_written_from() {
        let value = FrontmatterValue::Map(vec![
            ("title".to_string(), string("a \"quoted\"\\ line\n\u{1}é𝄞")),
            ("count".to_string(), FrontmatterValue::Int(-3)),
            ("ratio".to_string(), FrontmatterValue::Float(1.0)),
            ("done".to_string(), FrontmatterValue::Bool(false)),
            ("gone".to_string(), FrontmatterValue::Null),
            (
                "list".to_string(),
                FrontmatterValue::Sequence(vec![
                    FrontmatterValue::Int(1),
                    FrontmatterValue::Sequence(Vec::new()),
                ]),
            ),
            (
                "nested".to_string(),
                FrontmatterValue::Map(vec![("inner".to_string(), FrontmatterValue::Bool(true))]),
            ),
        ]);
        let text = canonical_json(&value).expect("a projection");
        let expected: BTreeMap<String, FieldValue> = [
            (
                "title".to_string(),
                FieldValue::scalar("a \"quoted\"\\ line\n\u{1}é𝄞"),
            ),
            ("count".to_string(), FieldValue::scalar("-3")),
            ("ratio".to_string(), FieldValue::scalar("1.0")),
            ("done".to_string(), FieldValue::scalar("false")),
            ("gone".to_string(), FieldValue::null()),
            (
                "list".to_string(),
                FieldValue::sequence([FieldValue::scalar("1"), FieldValue::sequence([])]),
            ),
            (
                "nested".to_string(),
                FieldValue::map([("inner".to_string(), FieldValue::scalar("true"))]),
            ),
        ]
        .into_iter()
        .collect();
        assert_eq!(projected_fields(&text).expect("a read"), expected);
        assert_eq!(
            projected_fields(r#"{"pair":"\ud834\udd1e"}"#).expect("a read")["pair"],
            FieldValue::scalar("𝄞")
        );
    }

    /// A projection whose top level is no map carries no fields, and text that
    /// is not one is damage.
    #[test]
    fn only_a_map_carries_fields_and_what_is_not_json_is_damage() {
        assert!(projected_fields("[1,2]").expect("a read").is_empty());
        assert!(projected_fields("\"text\"").expect("a read").is_empty());
        for broken in [
            "{",
            "{\"a\":}",
            "{\"a\":1} x",
            "\"\\q\"",
            "[1,",
            "{\"a\" 1}",
        ] {
            assert!(
                matches!(
                    projected_fields(broken),
                    Err(crate::StoreError::Damaged { .. })
                ),
                "`{broken}` read as a projection"
            );
        }
        let deep = format!(
            "{}{}",
            "[".repeat(MAX_FRONTMATTER_DEPTH + 1),
            "]".repeat(MAX_FRONTMATTER_DEPTH + 1)
        );
        assert!(projected_fields(&deep).is_err());
        let bounded = format!(
            "{}{}",
            "[".repeat(MAX_FRONTMATTER_DEPTH),
            "]".repeat(MAX_FRONTMATTER_DEPTH)
        );
        assert!(projected_fields(&bounded).is_ok());
    }
}
