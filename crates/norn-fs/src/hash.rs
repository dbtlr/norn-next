//! The content hash, and the reading that produces one.
//!
//! **Only a content hash concludes "unchanged".** A stat fingerprint may
//! prioritize work or raise suspicion; it never concludes. The asymmetry is
//! why: a false "unchanged" destroys work or backs a wrong answer, while a
//! false "changed" costs a re-derivation.
//!
//! Reading and hashing a file is **one act against one file descriptor**, so
//! the bytes hashed are provably the bytes read. [`hashed_from`] is that act: it
//! takes a handle rather than a path, which is what makes the guarantee
//! structural instead of a convention each call site keeps, and it is what every
//! consumer that needs a file's hash asks.
//!
//! [`ContentHash::of`] is the same guarantee arrived at from the other side — a
//! caller that already holds the bytes hashes those bytes, and the write kernel's
//! source-reading leg uses it over a buffer it read through one handle in one
//! act. What has no spelling anywhere is *open a path and hash whatever it turns
//! out to be*, because that is the act whose two halves can be about two files.

use std::fmt;
use std::io::{Read, Seek, SeekFrom};

use sha2::{Digest, Sha256};

/// How many bytes are read from a handle at a time while hashing it.
///
/// The hash is computed over a stream rather than over a buffer holding the
/// whole file, because peak memory is a function of the working set and not of
/// what a document happens to weigh.
const CHUNK: usize = 64 * 1024;

/// The SHA-256 of a document's bytes.
///
/// Compared rather than read: a caller matches one against another to decide
/// whether the content it observed is the content that is there now. The hex
/// spelling exists for diagnostics and for carrying the value across a seam
/// that has no bytes in it.
#[derive(Clone, Copy, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ContentHash([u8; 32]);

impl ContentHash {
    /// The hash of `bytes`.
    ///
    /// This is the composing side: a caller that has just built the content it
    /// wants at a path hashes it here and hands the value to the kernel.
    pub fn of(bytes: &[u8]) -> ContentHash {
        let mut hasher = Sha256::new();
        hasher.update(bytes);
        ContentHash(hasher.finalize().into())
    }

    /// The hash spelled as 64 lowercase hex digits.
    pub fn to_hex(self) -> String {
        let mut hex = String::with_capacity(64);
        for byte in self.0 {
            fmt::Write::write_fmt(&mut hex, format_args!("{byte:02x}")).expect("a string");
        }
        hex
    }

    /// The hash 64 hex digits spell, or `None` when they do not spell one.
    ///
    /// Refused rather than salvaged: a truncated or mis-cased value that parsed
    /// into *some* hash would compare unequal to everything and read as drift,
    /// which is a wrong answer wearing a correct shape.
    pub fn from_hex(hex: &str) -> Option<ContentHash> {
        if hex.len() != 64 {
            return None;
        }
        let mut bytes = [0u8; 32];
        for (byte, pair) in bytes.iter_mut().zip(hex.as_bytes().chunks(2)) {
            let digits = std::str::from_utf8(pair).ok()?;
            if digits.bytes().any(|d| d.is_ascii_uppercase()) {
                return None;
            }
            *byte = u8::from_str_radix(digits, 16).ok()?;
        }
        Some(ContentHash(bytes))
    }
}

impl fmt::Display for ContentHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_hex())
    }
}

/// The hex spelling, so a hash in a diagnostic is the value a caller can
/// compare by eye against the one it passed.
impl fmt::Debug for ContentHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ContentHash({})", self.to_hex())
    }
}

/// The hash of everything `handle` holds, and how many bytes that was.
///
/// **The handle is rewound first**, so a second reading through the same
/// descriptor hashes the whole file rather than the tail of it. That is what
/// makes the verify stage of the write protocol possible at all: the
/// precondition and the verification are two readings of **one** open file
/// description, and a re-hash that started from wherever the last one stopped
/// would hash nothing and conclude that everything matched. It also means the
/// position is left at the end of the file, which a caller that reads afterwards
/// has to know.
///
/// The whole file is read as a stream, so peak memory is one chunk rather than the
/// weight of the document. A read interrupted by a signal is retried, because
/// `EINTR` is not a failure to read and half a hash is not a smaller hash.
///
/// What this cannot promise is anything about the *name* the handle came from.
/// The bytes hashed are the bytes of the file this descriptor refers to; whether
/// some path still resolves to that file is a separate question, asked with a
/// stat comparison by whoever needs the answer.
///
/// ```
/// use norn_fs::{ContentHash, hashed_from};
///
/// let mut handle = std::io::Cursor::new(b"the bytes a reader read".to_vec());
/// let (hash, len) = hashed_from(&mut handle).expect("hashing a handle");
/// assert_eq!(hash, ContentHash::of(b"the bytes a reader read"));
/// assert_eq!(len, 23);
/// ```
pub fn hashed_from<H: Read + Seek>(handle: &mut H) -> std::io::Result<(ContentHash, u64)> {
    handle.seek(SeekFrom::Start(0))?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; CHUNK];
    let mut len = 0u64;
    loop {
        let read = match handle.read(&mut buffer) {
            Ok(read) => read,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        };
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        len += read as u64;
    }
    Ok((ContentHash(hasher.finalize().into()), len))
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

    /// The published SHA-256 of the empty input and of `abc`. A hash function
    /// that agrees with itself and with nothing else is a hash nobody else can
    /// check a value against.
    #[test]
    fn the_hash_is_sha_256() {
        assert_eq!(
            ContentHash::of(b"").to_hex(),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            ContentHash::of(b"abc").to_hex(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    /// Hashing a handle and hashing the same bytes agree, and the length comes
    /// back with the hash. A streaming reader that dropped a chunk boundary
    /// would disagree here.
    #[test]
    fn hashing_a_handle_agrees_with_hashing_the_bytes() {
        for len in [0usize, 1, CHUNK - 1, CHUNK, CHUNK + 1, CHUNK * 2 + 7] {
            let bytes: Vec<u8> = (0..len).map(|i| (i % 251) as u8).collect();
            let mut handle = Cursor::new(bytes.clone());
            let (hash, read) = hashed_from(&mut handle).expect("hashing a cursor");
            assert_eq!(hash, ContentHash::of(&bytes), "at {len} bytes");
            assert_eq!(read, len as u64, "at {len} bytes");
        }
    }

    /// **The bar on the second reading.** A handle already read to its end
    /// hashes the whole file again, not the nothing that is left after it.
    ///
    /// The forbidden shape is a re-hash that continues from wherever the last
    /// read stopped: it produces the hash of the empty input every time, which
    /// compares equal to itself, so a verify stage built on it concludes that
    /// nothing changed no matter what did.
    #[test]
    fn a_second_reading_of_one_handle_hashes_the_whole_file() {
        let bytes = b"the bytes a precondition read".to_vec();
        let mut handle = Cursor::new(bytes.clone());
        let (first, _) = hashed_from(&mut handle).expect("the first reading");
        let (second, len) = hashed_from(&mut handle).expect("the second reading");
        assert_eq!(first, second);
        assert_eq!(len, bytes.len() as u64);
        assert_ne!(second, ContentHash::of(b""));
    }

    /// **The bar on an interrupted read.** A read a signal cut short is asked
    /// again, so the hash is of the whole file.
    ///
    /// The forbidden shape is treating `EINTR` as a failure. A signal arriving
    /// mid-read is not a file that cannot be read, and the caller that meets it is
    /// a write's precondition or its verification — so the answer would be an
    /// environmental refusal on a machine where nothing is wrong, or worse, a
    /// short read taken for the file.
    #[test]
    fn a_read_a_signal_interrupts_is_asked_again() {
        /// A handle that is interrupted before every chunk it hands over.
        struct Interrupted {
            bytes: Cursor<Vec<u8>>,
            interrupt: bool,
        }

        impl Read for Interrupted {
            fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
                self.interrupt = !self.interrupt;
                if self.interrupt {
                    return Err(std::io::Error::from(std::io::ErrorKind::Interrupted));
                }
                self.bytes.read(buffer)
            }
        }

        impl Seek for Interrupted {
            fn seek(&mut self, to: SeekFrom) -> std::io::Result<u64> {
                self.bytes.seek(to)
            }
        }

        let content: Vec<u8> = (0..CHUNK * 2 + 7).map(|i| (i % 251) as u8).collect();
        let mut handle = Interrupted {
            bytes: Cursor::new(content.clone()),
            interrupt: false,
        };
        let (hash, len) = hashed_from(&mut handle).expect("a read that was interrupted");
        assert_eq!(hash, ContentHash::of(&content));
        assert_eq!(len, content.len() as u64);
    }

    #[test]
    fn the_hex_spelling_round_trips() {
        let hash = ContentHash::of(b"round trip");
        assert_eq!(ContentHash::from_hex(&hash.to_hex()), Some(hash));
    }

    /// A spelling that is not 64 lowercase hex digits is refused rather than
    /// coerced into some hash that compares equal to nothing.
    #[test]
    fn a_spelling_that_is_not_a_hash_is_refused() {
        let good = ContentHash::of(b"round trip").to_hex();
        for bad in [
            String::new(),
            good[..63].to_string(),
            format!("{good}0"),
            good.to_uppercase(),
            "g".repeat(64),
            format!("{}zz", &good[..62]),
        ] {
            assert_eq!(
                ContentHash::from_hex(&bad),
                None,
                "{bad:?} parsed as a hash"
            );
        }
    }

    /// The debug spelling carries the value, so an assertion failure names the
    /// hash instead of an opaque array.
    #[test]
    fn the_debug_spelling_carries_the_value() {
        let hash = ContentHash::of(b"abc");
        assert_eq!(format!("{hash:?}"), format!("ContentHash({hash})"));
    }
}
