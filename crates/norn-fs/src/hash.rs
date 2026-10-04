//! The content hash, and the reading that produces one.
//!
//! **Only a content hash concludes "unchanged".** A stat fingerprint may
//! prioritize work or raise suspicion; it never concludes. The asymmetry is
//! why: a false "unchanged" destroys work or backs a wrong answer, while a
//! false "changed" costs a re-derivation.
//!
//! Reading and hashing a file is **one act against one file descriptor**, so
//! the bytes hashed are provably the bytes read. The streaming form, for a
//! caller holding a handle, is private to this module and reached only by
//! name: the write kernel's reads of a target and of a shadow through
//! [`target_hashed_from`] and [`shadow_hashed_from`], which count themselves,
//! the watcher's echo check through [`uncounted_echo_hashed_from`], which
//! says in its name that it is not counted, and a streamed observation
//! through [`hashed_and_checked_from`].
//! [`crate::read_and_hash`] is the configured-path form which returns both
//! the bytes and their hash from one opening, and
//! [`crate::stream_optional_and_hash`] the form that keeps no bytes: the hash
//! and whether the bytes are UTF-8, from one streamed reading.
//!
//! [`ContentHash::of`] is the same guarantee arrived at from the other side — a
//! caller that already holds the bytes hashes those bytes, and the write kernel
//! uses it over the content a caller stages, which is the content it writes.
//! What has no spelling anywhere is *open once to hash, then again to
//! read*, because those two halves can be about two files.

use std::fmt;
use std::io::{Read, Seek, SeekFrom};

use sha2::{Digest, Sha256};

/// How many bytes are read from a handle at a time while hashing it.
///
/// The hash is computed over a stream rather than over a buffer holding the
/// whole file, because peak memory is a function of the working set and not of
/// what a document happens to weigh.
const CHUNK: usize = 64 * 1024;

/// One forward pass over `reader`, the file at `path`; consumers receive the
/// same bytes the hash saw.
///
/// **The read counts itself** ([`crate::reads::ReadTally::document_opens`]):
/// every call is one counted read of `path`, so a descriptor read and hashed
/// twice is two reads in the tally rather than one open.
pub(crate) fn read_bytes_and_hash(
    reader: &mut impl Read,
    path: &std::path::Path,
) -> std::io::Result<(Vec<u8>, ContentHash)> {
    crate::reads::count_document_read(path);
    let mut bytes = Vec::new();
    reader.read_to_end(&mut bytes)?;
    let hash = ContentHash::of(&bytes);
    Ok((bytes, hash))
}

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
            *byte = (nibble(pair[0])? << 4) | nibble(pair[1])?;
        }
        Some(ContentHash(bytes))
    }
}

/// The value one lowercase hex digit spells, and `None` for every other byte.
///
/// Decoded by hand because the digits are the whole grammar: an integer parser
/// would also take a leading sign, and `"+a"` read as `0x0a` is a salvage the
/// doc on [`ContentHash::from_hex`] rules out.
fn nibble(digit: u8) -> Option<u8> {
    match digit {
        b'0'..=b'9' => Some(digit - b'0'),
        b'a'..=b'f' => Some(digit - b'a' + 10),
        _ => None,
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
/// The whole file is read as a stream ([`streamed`]), so peak memory is one
/// chunk rather than the weight of the document.
///
/// **This form is not counted** in any [`crate::reads::ReadTally`] field, so
/// it is private: a caller hashes through a wrapper whose name says whether
/// it counts, and a new uncounted hash is a call review can see.
///
/// What this cannot promise is anything about the *name* the handle came from.
/// The bytes hashed are the bytes of the file this descriptor refers to; whether
/// some path still resolves to that file is a separate question, asked with a
/// stat comparison by whoever needs the answer.
fn hashed_from<H: Read + Seek>(handle: &mut H) -> std::io::Result<(ContentHash, u64)> {
    handle.seek(SeekFrom::Start(0))?;
    streamed(handle, &mut |_| Ok(())).map_err(CopyFailed::into_error)
}

/// One forward pass over `reader` from where it stands, [`CHUNK`] bytes at a
/// time, handing each chunk to `each` once it is hashed: the hash and the
/// length of everything read.
///
/// **The one streaming loop.** Every streamed reading in this crate — a
/// target's and a shadow's hash, the echo check and a streamed observation —
/// is this loop and differs only in what it does
/// with a chunk, so peak memory is one chunk wherever a file is streamed. A
/// read interrupted by a signal is retried, because `EINTR` is not a failure
/// to read and half a hash is not a smaller hash.
fn streamed(
    reader: &mut impl Read,
    each: &mut dyn FnMut(&[u8]) -> std::io::Result<()>,
) -> Result<(ContentHash, u64), CopyFailed> {
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; CHUNK];
    let mut len = 0u64;
    loop {
        let read = match reader.read(&mut buffer) {
            Ok(read) => read,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(CopyFailed::Reading(error)),
        };
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        each(&buffer[..read]).map_err(CopyFailed::Writing)?;
        len += read as u64;
    }
    Ok((ContentHash(hasher.finalize().into()), len))
}

/// Which side of a streamed pass failed: the read of the file, or what was
/// done with a chunk of it.
#[derive(Debug)]
pub(crate) enum CopyFailed {
    Reading(std::io::Error),
    Writing(std::io::Error),
}

impl CopyFailed {
    /// The error, for a pass whose chunks go nowhere that can fail.
    fn into_error(self) -> std::io::Error {
        match self {
            CopyFailed::Reading(error) | CopyFailed::Writing(error) => error,
        }
    }
}

/// What one streamed reading of a file found: its content hash, its length,
/// and whether its bytes are UTF-8 — each the answer the whole buffer gives.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Checked {
    pub(crate) hash: ContentHash,
    pub(crate) len: u64,
    pub(crate) utf8: bool,
}

/// A streamed observation's read of the file at `path` through `reader`: its
/// hash, its length and whether it is UTF-8, from one forward pass that holds
/// one chunk and no more.
///
/// **The read counts itself** as one [`crate::reads::ReadTally::document_opens`],
/// as [`read_bytes_and_hash`] does: the two are the same act — a file read for
/// what its content is — and differ only in whether the bytes are kept.
pub(crate) fn hashed_and_checked_from(
    reader: &mut impl Read,
    path: &std::path::Path,
) -> std::io::Result<Checked> {
    crate::reads::count_document_read(path);
    let mut utf8 = Utf8Stream::default();
    let (hash, len) = streamed(reader, &mut |chunk| {
        utf8.feed(chunk);
        Ok(())
    })
    .map_err(CopyFailed::into_error)?;
    Ok(Checked {
        hash,
        len,
        utf8: utf8.finish(),
    })
}

/// Whether a stream of bytes handed over in chunks is UTF-8, judged as the
/// chunks arrive and without holding them: the verdict
/// [`std::str::from_utf8`] gives the whole buffer.
///
/// **A character split between two chunks is carried, not judged.** A chunk
/// that ends inside a multi-byte sequence leaves at most three bytes over, and
/// they are judged with the start of the next chunk; the stream ending while
/// bytes are still carried is a sequence cut short, which is not UTF-8.
#[derive(Debug, Default)]
pub(crate) struct Utf8Stream {
    /// The start of a character the last chunk ended inside.
    carry: [u8; 4],
    carried: usize,
    /// A byte that can begin no valid sequence was seen; nothing undoes it.
    invalid: bool,
}

impl Utf8Stream {
    /// Judge the next `chunk` of the stream.
    pub(crate) fn feed(&mut self, mut chunk: &[u8]) {
        if self.invalid {
            return;
        }
        if self.carried > 0 {
            // A carried start is the prefix of a sequence its lead byte gives
            // the width of; the standard decoder carried nothing else.
            let width = match self.carry[0] {
                0xf0..=0xff => 4,
                0xe0..=0xef => 3,
                _ => 2,
            };
            let take = (width - self.carried).min(chunk.len());
            self.carry[self.carried..self.carried + take].copy_from_slice(&chunk[..take]);
            self.carried += take;
            chunk = &chunk[take..];
            if self.carried < width {
                return;
            }
            self.carried = 0;
            if std::str::from_utf8(&self.carry[..width]).is_err() {
                self.invalid = true;
                return;
            }
        }
        if let Err(error) = std::str::from_utf8(chunk) {
            match error.error_len() {
                Some(_) => self.invalid = true,
                None => {
                    let tail = &chunk[error.valid_up_to()..];
                    self.carry[..tail.len()].copy_from_slice(tail);
                    self.carried = tail.len();
                }
            }
        }
    }

    /// Whether everything fed was UTF-8, with no character left unfinished.
    pub(crate) fn finish(self) -> bool {
        !self.invalid && self.carried == 0
    }
}

/// The write kernel's read of the target at `full`: the stream hash over its
/// descriptor, counted as one [`crate::reads::ReadTally::target_reads`] by
/// the act itself, so a target hashed twice counts twice.
pub(crate) fn target_hashed_from<H: Read + Seek>(
    handle: &mut H,
    full: &std::path::Path,
) -> std::io::Result<(ContentHash, u64)> {
    crate::reads::count_target_read(full);
    hashed_from(handle)
}

/// The write kernel's read of the staged shadow at `path`: the stream hash
/// over its descriptor, counted as one
/// [`crate::reads::ReadTally::shadow_reads`] by the act itself.
pub(crate) fn shadow_hashed_from<H: Read + Seek>(
    handle: &mut H,
    path: &std::path::Path,
) -> std::io::Result<(ContentHash, u64)> {
    crate::reads::count_shadow_read(path);
    hashed_from(handle)
}

/// The watcher's echo check: the stream hash over a file it opened itself,
/// **counted in no [`crate::reads::ReadTally`] field**. The check runs on the
/// watcher's thread, outside every job's account, so there is no window its
/// read could be counted in; nothing else calls this.
pub(crate) fn uncounted_echo_hashed_from<H: Read + Seek>(
    handle: &mut H,
) -> std::io::Result<(ContentHash, u64)> {
    hashed_from(handle)
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;
    use std::path::Path;

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

    /// Byte strings whose UTF-8 verdict turns on a multi-byte sequence: valid
    /// two-, three- and four-byte characters, a stray continuation byte, an
    /// overlong encoding, an encoded surrogate, and each width of sequence cut
    /// short at the end.
    fn utf8_cases() -> Vec<Vec<u8>> {
        let mut cases: Vec<Vec<u8>> = vec![
            Vec::new(),
            b"plain ascii".to_vec(),
            "a\u{e9}\u{20ac}\u{1f600}z".as_bytes().to_vec(),
            "\u{1f600}\u{1f600}".as_bytes().to_vec(),
            b"a\x80b".to_vec(),
            b"\xc0\x80".to_vec(),
            b"\xed\xa0\x80".to_vec(),
            b"ok \xff".to_vec(),
        ];
        for character in ["\u{e9}", "\u{20ac}", "\u{1f600}"] {
            let bytes = character.as_bytes();
            for cut in 1..bytes.len() {
                cases.push([b"x".as_slice(), &bytes[..cut]].concat());
                cases.push([&bytes[..cut], b"x".as_slice()].concat());
            }
        }
        cases
    }

    /// **The bar on a character split between chunks.** Fed in chunks of any
    /// size, the streamed check answers what one check over the whole buffer
    /// answers: a sequence split across a boundary is the character it spells,
    /// and a sequence the input ends inside is not UTF-8.
    ///
    /// The forbidden shapes are the two halves of checking each chunk on its
    /// own — a valid character split by a boundary read as two invalid
    /// fragments, and an incomplete tail carried forward and never judged.
    #[test]
    fn the_streamed_utf8_check_agrees_with_the_whole_buffer_at_every_split() {
        for case in utf8_cases() {
            let whole = std::str::from_utf8(&case).is_ok();
            for size in 1..=5 {
                let mut check = Utf8Stream::default();
                case.chunks(size).for_each(|chunk| check.feed(chunk));
                assert_eq!(check.finish(), whole, "{case:?} in chunks of {size}");
            }
            for at in 0..=case.len() {
                let mut check = Utf8Stream::default();
                check.feed(&case[..at]);
                check.feed(&case[at..]);
                assert_eq!(check.finish(), whole, "{case:?} split at {at}");
            }
        }
    }

    /// The streamed pass over a reader answers the hash and the UTF-8 verdict
    /// of the whole buffer, at the reader's own chunk size: a four-byte
    /// character straddling the first chunk boundary is UTF-8, and the same
    /// stream cut inside its last character is not.
    #[test]
    fn a_streamed_pass_hashes_and_checks_across_its_own_chunks() {
        let straddling = [vec![b'a'; CHUNK - 2], "\u{1f600}".as_bytes().to_vec()].concat();
        let truncated = straddling[..straddling.len() - 1].to_vec();
        let invalid_late = [vec![b'a'; CHUNK * 2 + 3], vec![0xff]].concat();
        for (bytes, utf8) in [
            (Vec::new(), true),
            (vec![b'a'; CHUNK], true),
            (straddling, true),
            (truncated, false),
            (invalid_late, false),
        ] {
            let streamed = hashed_and_checked_from(&mut Cursor::new(bytes.clone()), Path::new("x"))
                .expect("streaming a cursor");
            assert_eq!(
                streamed.hash,
                ContentHash::of(&bytes),
                "{} bytes",
                bytes.len()
            );
            assert_eq!(streamed.len, bytes.len() as u64);
            assert_eq!(streamed.utf8, utf8, "{} bytes", bytes.len());
            assert_eq!(streamed.utf8, std::str::from_utf8(&bytes).is_ok());
        }
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
            format!("+{}", &good[1..]),
            format!("{}+a", &good[..62]),
            format!("-{}", &good[1..]),
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
