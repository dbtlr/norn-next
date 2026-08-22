//! The sub-fingerprints a document row carries beside its content hash.
//!
//! A content hash answers whether the *document* changed. A change-feed consumer
//! derives from one part of it — the body, or the frontmatter projection — and
//! needs to know whether **that** part changed, so the row states each part
//! separately and the consumer triages before it fetches anything.
//!
//! One function computes all of them, from the value that is being stored,
//! inside the write that stores it. Two spellings of "the hash of this column"
//! is two answers a bad write could make disagree, and a hash taken anywhere but
//! at the write is a value the row it describes can move out from under.
//!
//! **The same value class as `documents.content_hash`**: SHA-256, spelled as 64
//! lowercase hex digits, which is the form the filesystem seam hands over. So a
//! consumer comparing a body hash against a content hash compares two hashes of
//! bytes rather than meeting a type error dressed as an answer. The two are
//! equal where the body is the whole document — one carrying no frontmatter
//! block — and that costs nothing, because the two values answer
//! different questions and neither is read as the other.

use std::fmt::Write as _;

use sha2::{Digest, Sha256};

/// The hash of one stored text value.
pub(crate) fn sub_fingerprint(value: &str) -> String {
    let digest = Sha256::digest(value.as_bytes());
    let mut hex = String::with_capacity(64);
    for byte in digest {
        write!(&mut hex, "{byte:02x}").expect("a string");
    }
    hex
}

#[cfg(test)]
mod tests {
    use super::sub_fingerprint;

    /// The value class the column holds: 64 lowercase hex digits, one value per
    /// input, and the same value every time for one input.
    #[test]
    fn a_sub_fingerprint_is_sixty_four_lowercase_hex_digits() {
        let hashed = sub_fingerprint("a body\n");
        assert_eq!(hashed.len(), 64, "`{hashed}` is not 64 digits");
        assert!(
            hashed
                .chars()
                .all(|digit| digit.is_ascii_digit() || ('a'..='f').contains(&digit)),
            "`{hashed}` is not lowercase hex"
        );
        assert_eq!(
            hashed,
            sub_fingerprint("a body\n"),
            "it is not deterministic"
        );
        assert_ne!(hashed, sub_fingerprint("another body\n"));
        assert_ne!(
            sub_fingerprint(""),
            sub_fingerprint(" "),
            "an empty value hashes to a value of its own"
        );
    }
}
