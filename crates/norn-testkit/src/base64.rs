//! Base64, RFC 4648 standard alphabet, padded — strict in both directions.
//!
//! A recorded tree entry whose bytes are not valid UTF-8 carries them through
//! here, because JSON has no other way to hold arbitrary bytes exactly. That
//! is the whole reason this exists: a recording that stored a *length* instead
//! of bytes would not be a recording of the file.
//!
//! The decoder is strict on purpose. It rejects a wrong length, a character
//! outside the alphabet, misplaced padding, and a final group whose unused
//! bits are not zero. Strictness makes the encoding **canonical**: one byte
//! sequence has exactly one spelling, so a corpus cannot acquire two
//! recordings of the same file that differ only in how they were written down.
//!
//! [`encode`] is here so a recording can be produced and round-tripped
//! against [`decode`] without a second implementation of the alphabet.

use std::fmt;

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
const PAD: u8 = b'=';

/// Why a base64 string does not decode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Base64Error {
    /// The input is not a whole number of four-character groups.
    Length(usize),
    /// A byte outside the alphabet appeared.
    Character(u8),
    /// Padding appeared anywhere but the end of the final group.
    Padding,
    /// The final group's unused bits are not zero, so the input is one of
    /// several spellings of the same bytes.
    NonCanonical,
}

impl fmt::Display for Base64Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Base64Error::Length(len) => {
                write!(f, "length {len} is not a multiple of four")
            }
            Base64Error::Character(byte) => {
                write!(f, "byte {byte:#04x} is outside the base64 alphabet")
            }
            Base64Error::Padding => write!(f, "padding appears before the end of the input"),
            Base64Error::NonCanonical => {
                write!(f, "the final group's unused bits are not zero")
            }
        }
    }
}

/// Encode `bytes`.
pub fn encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = u32::from(chunk[0]);
        let b1 = chunk.get(1).copied().map_or(0, u32::from);
        let b2 = chunk.get(2).copied().map_or(0, u32::from);
        let group = (b0 << 16) | (b1 << 8) | b2;
        let indices = [
            (group >> 18) & 0x3f,
            (group >> 12) & 0x3f,
            (group >> 6) & 0x3f,
            group & 0x3f,
        ];
        for (position, index) in indices.iter().enumerate() {
            if position <= chunk.len() {
                out.push(char::from(ALPHABET[*index as usize]));
            } else {
                out.push(char::from(PAD));
            }
        }
    }
    out
}

fn value_of(byte: u8) -> Option<u32> {
    let index = ALPHABET.iter().position(|candidate| *candidate == byte)?;
    Some(index as u32)
}

/// Decode `text`, rejecting anything that is not a canonical encoding.
pub fn decode(text: &str) -> Result<Vec<u8>, Base64Error> {
    let bytes = text.as_bytes();
    if !bytes.len().is_multiple_of(4) {
        return Err(Base64Error::Length(bytes.len()));
    }
    let groups = bytes.len() / 4;
    let mut out = Vec::with_capacity(groups * 3);
    for (index, group) in bytes.chunks_exact(4).enumerate() {
        let last = index + 1 == groups;
        let padding = group.iter().filter(|byte| **byte == PAD).count();
        if padding > 0 {
            // Padding is only ever the tail of the final group: never in a
            // group before it, never more than two characters, and never with
            // a data character after it.
            if !last || padding > 2 || group[4 - padding..].iter().any(|byte| *byte != PAD) {
                return Err(Base64Error::Padding);
            }
        }
        let mut values = [0u32; 4];
        for (position, byte) in group.iter().enumerate() {
            if *byte == PAD {
                break;
            }
            values[position] = value_of(*byte).ok_or(Base64Error::Character(*byte))?;
        }
        let combined = (values[0] << 18) | (values[1] << 12) | (values[2] << 6) | values[3];
        match padding {
            0 => {
                out.push((combined >> 16) as u8);
                out.push((combined >> 8) as u8);
                out.push(combined as u8);
            }
            1 => {
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

    /// RFC 4648 section 10.
    const VECTORS: &[(&str, &str)] = &[
        ("", ""),
        ("f", "Zg=="),
        ("fo", "Zm8="),
        ("foo", "Zm9v"),
        ("foob", "Zm9vYg=="),
        ("fooba", "Zm9vYmE="),
        ("foobar", "Zm9vYmFy"),
    ];

    #[test]
    fn matches_the_published_vectors() {
        for (plain, encoded) in VECTORS {
            assert_eq!(&encode(plain.as_bytes()), encoded, "encoding {plain:?}");
            assert_eq!(
                decode(encoded).expect("a published vector decodes"),
                plain.as_bytes(),
                "decoding {encoded:?}"
            );
        }
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
    fn a_wrong_length_is_refused() {
        assert_eq!(decode("Zm9"), Err(Base64Error::Length(3)));
        assert_eq!(decode("Zm9vY"), Err(Base64Error::Length(5)));
    }

    #[test]
    fn a_character_outside_the_alphabet_is_refused() {
        assert_eq!(decode("Zm9-"), Err(Base64Error::Character(b'-')));
        assert_eq!(decode("Zm9 "), Err(Base64Error::Character(b' ')));
        assert_eq!(decode("Zm9\n"), Err(Base64Error::Character(b'\n')));
    }

    #[test]
    fn misplaced_padding_is_refused() {
        assert_eq!(decode("Zg==Zg=="), Err(Base64Error::Padding));
        assert_eq!(decode("Z=g="), Err(Base64Error::Padding));
        assert_eq!(decode("===="), Err(Base64Error::Padding));
    }

    #[test]
    fn a_non_canonical_final_group_is_refused() {
        // `Zh==` and `Zg==` would both decode to `f` under a lax decoder.
        assert_eq!(decode("Zh=="), Err(Base64Error::NonCanonical));
        assert_eq!(decode("Zm9="), Err(Base64Error::NonCanonical));
    }
}
