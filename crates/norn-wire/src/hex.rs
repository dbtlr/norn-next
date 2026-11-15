//! Lowercase hexadecimal, strict in both directions.
//!
//! The one hex codec in the workspace. A stored token's secret, the bearer a
//! request presents, and the hello's nonce and MAC are all written in it, so
//! the writer of a token file and the reader of a bearer cannot disagree about
//! what a spelling means.
//!
//! **The decoder is strict, and that is what makes the encoding canonical.**
//! It refuses an odd number of digits, a character outside `0-9a-f`, and
//! uppercase, which is not what the encoder writes. One byte sequence
//! therefore has exactly one spelling, so two spellings of one credential
//! cannot compare unequal.

/// The lowercase hex spelling of `bytes`: two digits per byte, high nibble
/// first.
pub fn encode(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut text = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        text.push(char::from(DIGITS[usize::from(byte >> 4)]));
        text.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    text
}

/// The bytes `text` spells, or `None` when it spells none.
///
/// Read as bytes rather than as characters: the length, the pairing and the
/// digit lookup are then all in the same unit, and a multi-byte character
/// cannot make a string of `n` digits decode as anything but `n / 2` bytes.
pub fn decode(text: &str) -> Option<Vec<u8>> {
    let bytes = text.as_bytes();
    if !bytes.len().is_multiple_of(2) {
        return None;
    }
    let (pairs, _) = bytes.as_chunks::<2>();
    pairs.iter().map(|pair| pair_value(*pair)).collect()
}

/// The `N` bytes `text` spells, or `None` when it spells none or spells some
/// other number of bytes.
///
/// For a fixed-width value — a nonce, a MAC — whose length is part of its
/// grammar: `2 * N` digits exactly.
pub fn decode_array<const N: usize>(text: &str) -> Option<[u8; N]> {
    let bytes = text.as_bytes();
    if bytes.len() != N * 2 {
        return None;
    }
    let (pairs, _) = bytes.as_chunks::<2>();
    let mut out = [0u8; N];
    for (slot, pair) in out.iter_mut().zip(pairs) {
        *slot = pair_value(*pair)?;
    }
    Some(out)
}

/// The byte two lowercase digits spell.
fn pair_value([high, low]: [u8; 2]) -> Option<u8> {
    Some((digit(high)? << 4) | digit(low)?)
}

/// The value of one lowercase hexadecimal digit.
fn digit(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_byte_round_trips() {
        let bytes: Vec<u8> = (0..=255u8).collect();
        let text = encode(&bytes);
        assert_eq!(text.len(), 512);
        assert_eq!(decode(&text).expect("a decoding"), bytes);
    }

    #[test]
    fn the_spelling_is_lowercase_high_nibble_first() {
        assert_eq!(encode(&[0x00, 0x0f, 0xa5, 0xff]), "000fa5ff");
        assert_eq!(encode(&[]), "");
        assert_eq!(decode(""), Some(Vec::new()));
    }

    #[test]
    fn text_that_is_not_lowercase_hex_decodes_to_nothing() {
        assert_eq!(decode("abc"), None, "an odd number of digits");
        assert_eq!(decode("zz"), None, "a digit outside the alphabet");
        assert_eq!(
            decode("AB"),
            None,
            "uppercase, which is not what is written"
        );
        assert_eq!(
            decode("é"),
            None,
            "two bytes that are one character, and neither of them a digit"
        );
        assert_eq!(decode(" 0a"), None, "whitespace is not a digit");
    }

    #[test]
    fn a_fixed_width_value_decodes_only_at_its_width() {
        assert_eq!(decode_array::<2>("0aff"), Some([0x0a, 0xff]));
        assert_eq!(decode_array::<2>("0a"), None, "too short");
        assert_eq!(decode_array::<2>("0aff00"), None, "too long");
        assert_eq!(decode_array::<2>("0aFF"), None, "uppercase");
        assert_eq!(decode_array::<2>("0aé"), None, "a multi-byte character");
        let thirty_two = [0x5a; 32];
        assert_eq!(decode_array::<32>(&encode(&thirty_two)), Some(thirty_two));
    }

    /// The bytes just outside each digit range are not digits, in either
    /// nibble: `/` and `:` around `0-9`, `` ` `` and `g` around `a-f`, and
    /// `@` and `G` around the uppercase range the decoder also refuses.
    #[test]
    fn the_bytes_beside_each_digit_range_are_refused_in_both_nibbles() {
        for outside in ['/', ':', '`', 'g', '@', 'G'] {
            for pair in [format!("{outside}0"), format!("0{outside}")] {
                assert_eq!(decode(&pair), None, "`{pair}` decoded");
                assert_eq!(decode_array::<1>(&pair), None, "`{pair}` decoded");
            }
        }
        for inside in ['0', '9', 'a', 'f'] {
            assert!(decode(&format!("{inside}{inside}")).is_some(), "`{inside}`");
        }
    }
}
