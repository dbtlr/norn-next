//! Base64, RFC 4648 §5 URL-safe alphabet, unpadded — strict in both
//! directions.
//!
//! The cursor is the one thing in this vocabulary that crosses as an opaque
//! string, and this is what makes it one. The alphabet is the URL-safe one so
//! a cursor rides in a query string, a header and a JSON body without being
//! escaped differently in each; padding is omitted for the same reason, since
//! `=` is what a URL escapes.
//!
//! **The decoder is strict, and that is what makes the encoding canonical.**
//! It refuses a length no encoding produces, a character outside the alphabet,
//! padding, and a final group whose unused bits are not zero. One byte
//! sequence therefore has exactly one spelling, so two clients cannot hold two
//! cursors that mean the same position and compare unequal.

use std::fmt;

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

/// Why a string does not decode.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Base64Error {
    /// The input's length is one past a whole group, which no encoding
    /// produces.
    Length(usize),
    /// A byte outside the alphabet appeared. Padding is one of these: the
    /// encoding is unpadded, so `=` is simply not in the alphabet.
    Character(u8),
    /// The final group's unused bits are not zero, so the input is one of
    /// several spellings of the same bytes.
    NonCanonical,
}

impl fmt::Display for Base64Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Base64Error::Length(len) => {
                write!(f, "length {len} is one character past a whole group")
            }
            Base64Error::Character(byte) => {
                write!(
                    f,
                    "byte {byte:#04x} is outside the URL-safe base64 alphabet"
                )
            }
            Base64Error::NonCanonical => {
                write!(f, "the final group's unused bits are not zero")
            }
        }
    }
}

/// Encode `bytes`, without padding.
pub(crate) fn encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = u32::from(chunk[0]);
        let b1 = chunk.get(1).copied().map_or(0, u32::from);
        let b2 = chunk.get(2).copied().map_or(0, u32::from);
        let group = (b0 << 16) | (b1 << 8) | b2;
        for position in 0..=chunk.len() {
            let index = (group >> (18 - 6 * position)) & 0x3f;
            out.push(char::from(ALPHABET[index as usize]));
        }
    }
    out
}

fn value_of(byte: u8) -> Result<u32, Base64Error> {
    ALPHABET
        .iter()
        .position(|candidate| *candidate == byte)
        .map(|index| index as u32)
        .ok_or(Base64Error::Character(byte))
}

/// Decode `text`, refusing anything that is not a canonical encoding.
pub(crate) fn decode(text: &str) -> Result<Vec<u8>, Base64Error> {
    let bytes = text.as_bytes();
    if bytes.len() % 4 == 1 {
        return Err(Base64Error::Length(bytes.len()));
    }
    let mut out = Vec::with_capacity(bytes.len() / 4 * 3);
    for group in bytes.chunks(4) {
        let mut values = [0u32; 4];
        for (position, byte) in group.iter().enumerate() {
            values[position] = value_of(*byte)?;
        }
        let combined = (values[0] << 18) | (values[1] << 12) | (values[2] << 6) | values[3];
        match group.len() {
            4 => {
                out.push((combined >> 16) as u8);
                out.push((combined >> 8) as u8);
                out.push(combined as u8);
            }
            3 => {
                if values[2] & 0x03 != 0 {
                    return Err(Base64Error::NonCanonical);
                }
                out.push((combined >> 16) as u8);
                out.push((combined >> 8) as u8);
            }
            _ => {
                if values[1] & 0x0f != 0 {
                    return Err(Base64Error::NonCanonical);
                }
                out.push((combined >> 16) as u8);
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RFC 4648 section 10, with the padding dropped: the alphabet differs
    /// from the standard one only in its last two characters, which these
    /// vectors do not reach, so the spellings are the published ones.
    const VECTORS: &[(&str, &str)] = &[
        ("", ""),
        ("f", "Zg"),
        ("fo", "Zm8"),
        ("foo", "Zm9v"),
        ("foob", "Zm9vYg"),
        ("fooba", "Zm9vYmE"),
        ("foobar", "Zm9vYmFy"),
    ];

    #[test]
    fn matches_the_published_vectors_without_their_padding() {
        for (plain, encoded) in VECTORS {
            assert_eq!(&encode(plain.as_bytes()), encoded, "encoding {plain:?}");
            assert_eq!(
                decode(encoded).expect("a published vector decodes"),
                plain.as_bytes(),
                "decoding {encoded:?}"
            );
        }
    }

    /// The two characters that make the alphabet URL-safe stand where the
    /// standard alphabet puts `+` and `/`.
    #[test]
    fn the_last_two_characters_are_the_url_safe_pair() {
        let spans = encode(&[0xfb, 0xff, 0xbf]);
        assert_eq!(spans, "-_-_");
        assert_eq!(
            decode("-_-_").expect("the pair decodes"),
            [0xfb, 0xff, 0xbf]
        );
        assert_eq!(decode("+/+/"), Err(Base64Error::Character(b'+')));
    }

    #[test]
    fn round_trips_every_byte_value() {
        let all: Vec<u8> = (0..=255).collect();
        for length in 0..=all.len() {
            let slice = &all[..length];
            assert_eq!(
                decode(&encode(slice)).expect("a round trip decodes"),
                slice,
                "round trip at length {length}"
            );
        }
    }

    #[test]
    fn a_length_no_encoding_produces_is_refused() {
        assert_eq!(decode("Z"), Err(Base64Error::Length(1)));
        assert_eq!(decode("Zm9vY"), Err(Base64Error::Length(5)));
    }

    #[test]
    fn a_character_outside_the_alphabet_is_refused() {
        assert_eq!(decode("Zm9="), Err(Base64Error::Character(b'=')));
        assert_eq!(decode("Zg=="), Err(Base64Error::Character(b'=')));
        assert_eq!(decode("Zm9 "), Err(Base64Error::Character(b' ')));
        assert_eq!(decode("Zm9\n"), Err(Base64Error::Character(b'\n')));
    }

    /// `Zh` and `Zg` would both decode to `f` under a lax decoder, which would
    /// make one position two cursors.
    #[test]
    fn a_non_canonical_final_group_is_refused() {
        assert_eq!(decode("Zh"), Err(Base64Error::NonCanonical));
        assert_eq!(decode("Zm9"), Err(Base64Error::NonCanonical));
        assert_eq!(decode("Zm8"), Ok(b"fo".to_vec()));
    }
}
