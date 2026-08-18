//! The name a vault is keyed by, everywhere it is keyed by one.
//!
//! One string does three jobs at once: it is the key of a table in the
//! registry file, it is a directory component under the data directory, and it
//! is the authority-ish identifier a `norn://` address would carry. The
//! grammar is the intersection of what all three accept, not the union, which
//! is why it lives here rather than beside any one of them: a request naming a
//! vault crosses this seam, and the name it carries is checked once, here,
//! rather than re-checked at every surface that reads one.
//!
//! **Parse, don't validate, and no bypass.** There is no unchecked
//! constructor and no force flag, and the read path is the constructor: a
//! string outside the grammar has no representation, so no later code —
//! including a reader taking one off the wire — has to ask whether the name it
//! holds was checked. The grammar also makes the dangerous directory
//! components unspellable for free: a name cannot be empty, cannot be `.` or
//! `..`, and cannot contain a separator, because it must open with a lowercase
//! letter and holds no `/`.
//!
//! **The length bound is a filesystem fact, and it is deliberate here.**
//! [`VaultName::MAXIMUM_BYTES`] is `NAME_MAX`, the bound one directory
//! component is held to. It is part of the intersection rather than an
//! intruder in it: a name past it is a registry entry whose derived-state
//! directory cannot be created at all, so a name this crate accepts and a
//! machine refuses would be a name that crosses and then serves nothing.
//!
//! **The character grammar is RFC 3986's scheme production**, `[a-z][a-z0-9+.-]*`
//! — the production `norn-text` recognizes a protocol prefix by, spelled again
//! here. What this crate adds to it is the length bound above; the characters
//! are the same set, and neither definition is derived from the other.
//!
//! Their agreeing is not a coupling. A protocol identifier is read out of a
//! document, and a vault name is minted by a person registering a vault and
//! becomes a directory component and an address authority, so each is held to
//! its own consumers: a reader widened to accept more of what documents
//! contain widens nothing here, because an underscore that a scheme cannot
//! carry is one this grammar refuses whatever a document spells.
//!
//! ```
//! use norn_wire::VaultName;
//!
//! assert_eq!(VaultName::PATTERN, "^[a-z][a-z0-9+.-]*$");
//! assert!(VaultName::new("notes").is_ok());
//! assert!(VaultName::new("norn+notes.2-b").is_ok());
//! assert!(VaultName::new("norn_notes").is_err());
//! ```

use std::borrow::Cow;
use std::fmt;

use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Deserializer, Serialize, de::Error as _};

/// A vault's name: a lowercase letter, then lowercase letters, digits, `+`,
/// `.` and `-`, at most 255 bytes of them.
///
/// On the wire a name is the string itself: `"notes"`. A string outside the
/// grammar is refused rather than read as a name.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct VaultName(String);

impl VaultName {
    /// The longest a name may be: `NAME_MAX`, the bound every filesystem norn
    /// runs on puts on one directory component.
    pub const MAXIMUM_BYTES: usize = 255;

    /// The grammar as a regular expression, which is how a schema advertises
    /// it: RFC 3986's scheme production.
    pub const PATTERN: &'static str = "^[a-z][a-z0-9+.-]*$";

    /// The name `text` spells, or the reason it spells none.
    pub fn new(text: impl AsRef<str>) -> Result<Self, IllegalVaultName> {
        let text = text.as_ref();
        let refuse = |problem: &'static str| {
            Err(IllegalVaultName {
                name: elided(text),
                problem,
            })
        };

        if text.len() > VaultName::MAXIMUM_BYTES {
            return refuse("a name is at most 255 bytes, which is what a directory component is");
        }
        let mut characters = text.chars();
        let Some(first) = characters.next() else {
            return refuse("a name is at least one character");
        };
        if !first.is_ascii_lowercase() {
            return refuse("a name opens with a lowercase ASCII letter");
        }
        for character in characters {
            let legal = character.is_ascii_lowercase()
                || character.is_ascii_digit()
                || matches!(character, '+' | '.' | '-');
            if !legal {
                return refuse(
                    "a name holds only lowercase ASCII letters, digits, `+`, `.` and `-`",
                );
            }
        }
        Ok(VaultName(text.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for VaultName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl AsRef<str> for VaultName {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for VaultName {
    /// A name arrives as the string it is written as and is read through the
    /// grammar, so a name that crossed is a name that parsed. There is no
    /// second door: the derived read path a `#[serde(transparent)]` newtype
    /// would give takes any string at all.
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let text = String::deserialize(deserializer)?;
        VaultName::new(text).map_err(D::Error::custom)
    }
}

impl JsonSchema for VaultName {
    fn schema_name() -> Cow<'static, str> {
        Cow::Borrowed("VaultName")
    }

    fn schema_id() -> Cow<'static, str> {
        Cow::Borrowed("norn_wire::VaultName")
    }

    /// The advertised schema is the grammar the constructor keeps, so a
    /// surface that validates against the schema and a reader that parses the
    /// string refuse the same names.
    ///
    /// This is the crate's one hand-written schema. Every other type derives
    /// one, and a derive lifts the description out of the doc comment over the
    /// type; here the description is maintained beside that doc rather than
    /// lifted from it, so the two are two spellings of one sentence and the
    /// schema suite pins them equal.
    ///
    /// **The pattern is ECMA-262**, the dialect JSON Schema names, and a
    /// validator reading it as another flavor of regular expression admits
    /// names this reader refuses. Python's `re` — what the common `jsonschema`
    /// library matches through, via `re.search` — lets `$` stand before a
    /// trailing newline, so `"notes\n"` passes there and is refused here.
    fn json_schema(_generator: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "string",
            "description": "A vault's name: a lowercase letter, then lowercase letters, digits, `+`, `.` and `-`, at most 255 bytes of them.",
            "pattern": VaultName::PATTERN,
            "maxLength": VaultName::MAXIMUM_BYTES,
        })
    }
}

/// The marker an echoed name that was cut carries in place of its tail.
const ELISION: &str = "…";

/// The offered string as a refusal echoes it: at most
/// [`VaultName::MAXIMUM_BYTES`] bytes, then [`ELISION`] where a tail was cut.
///
/// The echo identifies the offender; it does not reproduce it. The bound is
/// already stated by the problem the refusal carries, and a string offered as a
/// name is any string at all — a megabyte of one arriving off the wire would
/// otherwise ride whole into the refusal, into its `Display`, and into the
/// serde error built from that. The cut is taken at a character boundary,
/// because what was offered is arbitrary UTF-8 even though every name the
/// grammar accepts is ASCII.
fn elided(text: &str) -> String {
    if text.len() <= VaultName::MAXIMUM_BYTES {
        return text.to_string();
    }
    let mut end = VaultName::MAXIMUM_BYTES;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}{ELISION}", &text[..end])
}

/// A string that spells no vault name.
///
/// A name outside the grammar is read as a refusal rather than as a name, the
/// same way the tagged vocabulary refuses a variant it does not know. It
/// carries the string that was offered — bounded, so an enormous one names its
/// offender without riding along whole — and what the grammar wanted, because a
/// person who typed one is the reader of both.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IllegalVaultName {
    name: String,
    problem: &'static str,
}

impl IllegalVaultName {
    /// The string that was offered as a name, bounded: at most
    /// [`VaultName::MAXIMUM_BYTES`] bytes of it, with an elision marker where
    /// a tail was cut. A name inside the bound is echoed whole.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// What the grammar wanted instead.
    pub const fn problem(&self) -> &'static str {
        self.problem
    }
}

impl fmt::Display for IllegalVaultName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "`{}` is not a vault name: {}", self.name, self.problem)
    }
}

impl std::error::Error for IllegalVaultName {}

#[cfg(test)]
mod tests {
    use super::*;

    /// The grammar is RFC 3986's scheme production: a lowercase letter, then
    /// lowercase letters, digits, `+`, `.` and `-`.
    #[test]
    fn the_names_the_grammar_accepts() {
        for text in [
            "a",
            "notes",
            "notes2",
            "norn-vault",
            "a.b",
            "a+b",
            "a-b.c+d",
            "x1234567890",
        ] {
            let name = VaultName::new(text).unwrap_or_else(|e| panic!("`{text}` was refused: {e}"));
            assert_eq!(name.as_str(), text);
            assert_eq!(name.to_string(), text);
        }
    }

    /// Each refusal with the reason it carries. The underscore case is the one
    /// that matters most: it is the character a person coming from a tag or a
    /// heading reaches for first, and a vault name becomes a directory
    /// component and an address authority, so RFC 3986's scheme production is
    /// what it is held to and an underscore is outside it.
    #[test]
    fn the_names_the_grammar_refuses() {
        for (text, needle) in [
            ("", "at least one character"),
            ("Notes", "lowercase ASCII letter"),
            ("1notes", "lowercase ASCII letter"),
            ("-notes", "lowercase ASCII letter"),
            (".notes", "lowercase ASCII letter"),
            ("+notes", "lowercase ASCII letter"),
            ("_notes", "lowercase ASCII letter"),
            ("notes_1", "lowercase ASCII letters, digits"),
            ("notesX", "lowercase ASCII letters, digits"),
            ("note s", "lowercase ASCII letters, digits"),
            ("notes/deep", "lowercase ASCII letters, digits"),
            ("notés", "lowercase ASCII letters, digits"),
            ("notes:1", "lowercase ASCII letters, digits"),
        ] {
            let refusal = VaultName::new(text).expect_err(&format!("`{text}` is not a vault name"));
            assert_eq!(refusal.name(), text);
            assert!(
                refusal.problem().contains(needle),
                "`{text}` was refused with `{}`, which does not mention `{needle}`",
                refusal.problem()
            );
        }
    }

    /// The grammar's free consequence: the dangerous directory components are
    /// unspellable, because a name opens with a lowercase letter and holds no
    /// separator.
    #[test]
    fn the_grammar_makes_a_traversal_component_unspellable() {
        for text in [".", "..", "../..", "/", "a/../b"] {
            assert!(
                VaultName::new(text).is_err(),
                "`{text}` was accepted as a vault name"
            );
        }
    }

    /// A name is a directory component, and a directory component has a length
    /// every filesystem norn runs on agrees about.
    #[test]
    fn a_name_is_bounded_at_the_length_a_directory_component_is() {
        let longest = format!("a{}", "x".repeat(VaultName::MAXIMUM_BYTES - 1));
        assert_eq!(longest.len(), VaultName::MAXIMUM_BYTES);
        assert_eq!(
            VaultName::new(&longest).expect("the longest name").as_str(),
            longest
        );

        let refusal = VaultName::new(format!("{longest}x")).expect_err("a name past the bound");
        assert!(refusal.problem().contains("255 bytes"), "{refusal}");
        assert_eq!(
            refusal.name(),
            format!("{longest}{ELISION}"),
            "a name one byte past the bound lost more than its tail"
        );
    }

    /// A refusal identifies the offender rather than reproducing it. What is
    /// offered as a name is any string at all — a megabyte of one arrives off
    /// the wire — so the echo is cut at the bound the problem already states,
    /// and the refusal a person reads stays a line rather than the megabyte
    /// that earned it.
    #[test]
    fn a_refusal_echoes_a_bounded_prefix_of_an_enormous_name() {
        let enormous = "a".repeat(4 * 1024 * 1024);
        let refusal = VaultName::new(&enormous).expect_err("a name past the bound");
        assert!(
            refusal.name().ends_with(ELISION),
            "the echo carries no elision marker: {}",
            refusal.name()
        );
        assert_eq!(
            refusal.name().len(),
            VaultName::MAXIMUM_BYTES + ELISION.len()
        );
        let rendered = refusal.to_string();
        assert!(
            rendered.len() < 512,
            "the refusal renders {} bytes",
            rendered.len()
        );
    }

    /// The cut lands where a character ends. What is offered is arbitrary
    /// UTF-8 even though every name the grammar accepts is ASCII, and a slice
    /// taken mid-character panics.
    #[test]
    fn the_echo_is_cut_where_a_character_ends() {
        let offered = "é".repeat(4096);
        let refusal = VaultName::new(&offered).expect_err("a name past the bound");
        let echoed = refusal
            .name()
            .strip_suffix(ELISION)
            .expect("an echo that was cut");
        assert!(echoed.len() <= VaultName::MAXIMUM_BYTES);
        assert!(echoed.chars().all(|character| character == 'é'));
    }
}
