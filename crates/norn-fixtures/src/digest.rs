//! SHA-256, and a tree digest for comparing two arbitrary directories.
//!
//! The hash is implemented here rather than pulled in because this crate is a
//! zero-dependency leaf. It is exercised against the published test vectors.
//!
//! # This is not where the determinism contract lives
//!
//! [`tree()`] walks a directory and digests what it finds, which means its path
//! keys are the names the filesystem hands back. On a filesystem that stores
//! names in a fixed Unicode normalization form, those are not the names that
//! were written. The contract digest is [`crate::Manifest::tree_digest`],
//! built at emission from the generator's own strings; the crate docs explain
//! why. Use [`tree()`] to ask what is in a directory, never to ask whether a
//! generation reproduced.

use std::fs;
use std::io;
use std::path::Path;

use crate::tree;

const H0: [u32; 8] = [
    0x6a09_e667,
    0xbb67_ae85,
    0x3c6e_f372,
    0xa54f_f53a,
    0x510e_527f,
    0x9b05_688c,
    0x1f83_d9ab,
    0x5be0_cd19,
];

#[rustfmt::skip]
const K: [u32; 64] = [
    0x428a_2f98, 0x7137_4491, 0xb5c0_fbcf, 0xe9b5_dba5, 0x3956_c25b, 0x59f1_11f1, 0x923f_82a4, 0xab1c_5ed5,
    0xd807_aa98, 0x1283_5b01, 0x2431_85be, 0x550c_7dc3, 0x72be_5d74, 0x80de_b1fe, 0x9bdc_06a7, 0xc19b_f174,
    0xe49b_69c1, 0xefbe_4786, 0x0fc1_9dc6, 0x240c_a1cc, 0x2de9_2c6f, 0x4a74_84aa, 0x5cb0_a9dc, 0x76f9_88da,
    0x983e_5152, 0xa831_c66d, 0xb003_27c8, 0xbf59_7fc7, 0xc6e0_0bf3, 0xd5a7_9147, 0x06ca_6351, 0x1429_2967,
    0x27b7_0a85, 0x2e1b_2138, 0x4d2c_6dfc, 0x5338_0d13, 0x650a_7354, 0x766a_0abb, 0x81c2_c92e, 0x9272_2c85,
    0xa2bf_e8a1, 0xa81a_664b, 0xc24b_8b70, 0xc76c_51a3, 0xd192_e819, 0xd699_0624, 0xf40e_3585, 0x106a_a070,
    0x19a4_c116, 0x1e37_6c08, 0x2748_774c, 0x34b0_bcb5, 0x391c_0cb3, 0x4ed8_aa4a, 0x5b9c_ca4f, 0x682e_6ff3,
    0x748f_82ee, 0x78a5_636f, 0x84c8_7814, 0x8cc7_0208, 0x90be_fffa, 0xa450_6ceb, 0xbef9_a3f7, 0xc671_78f2,
];

/// A streaming SHA-256.
pub struct Sha256 {
    state: [u32; 8],
    block: [u8; 64],
    filled: usize,
    total_bits: u64,
}

impl Default for Sha256 {
    fn default() -> Self {
        Self::new()
    }
}

impl Sha256 {
    pub fn new() -> Self {
        Sha256 {
            state: H0,
            block: [0; 64],
            filled: 0,
            total_bits: 0,
        }
    }

    pub fn update(&mut self, mut data: &[u8]) {
        self.total_bits = self.total_bits.wrapping_add((data.len() as u64) * 8);
        while !data.is_empty() {
            let take = (64 - self.filled).min(data.len());
            self.block[self.filled..self.filled + take].copy_from_slice(&data[..take]);
            self.filled += take;
            data = &data[take..];
            if self.filled == 64 {
                let block = self.block;
                self.compress(&block);
                self.filled = 0;
            }
        }
    }

    /// Absorb `data` behind its length, so two different splits of the same
    /// bytes cannot hash alike.
    pub fn update_framed(&mut self, data: &[u8]) {
        self.update(&(data.len() as u64).to_be_bytes());
        self.update(data);
    }

    pub fn finish(mut self) -> [u8; 32] {
        let bits = self.total_bits;
        self.update(&[0x80]);
        while self.filled != 56 {
            self.update(&[0x00]);
        }
        // `update` counted the padding; the recorded length is the message's.
        let block = self.block;
        let mut final_block = block;
        final_block[56..].copy_from_slice(&bits.to_be_bytes());
        self.compress(&final_block);

        let mut out = [0u8; 32];
        let (chunks, _) = out.as_chunks_mut::<4>();
        for (chunk, word) in chunks.iter_mut().zip(self.state.iter()) {
            *chunk = word.to_be_bytes();
        }
        out
    }

    fn compress(&mut self, block: &[u8; 64]) {
        let mut w = [0u32; 64];
        let (chunks, _) = block.as_chunks::<4>();
        for (i, chunk) in chunks.iter().enumerate() {
            w[i] = u32::from_be_bytes(*chunk);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }

        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = self.state;
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let t1 = h
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }

        for (slot, value) in self.state.iter_mut().zip([a, b, c, d, e, f, g, h]) {
            *slot = slot.wrapping_add(value);
        }
    }
}

/// Lowercase hexadecimal rendering of a digest.
pub fn hex(digest: &[u8; 32]) -> String {
    let mut out = String::with_capacity(64);
    for byte in digest {
        out.push(char::from_digit(u32::from(byte >> 4), 16).expect("nibble is a hex digit"));
        out.push(char::from_digit(u32::from(byte & 0x0f), 16).expect("nibble is a hex digit"));
    }
    out
}

/// The digest of a directory tree **as the filesystem reports it**: every
/// node's relative path, kind and contents, in sorted path order.
///
/// Two trees share a digest exactly when the filesystem reports the same
/// relative paths, the same directories, the same file bytes, and the same
/// link targets. Mode,
/// ownership and timestamps are outside it — they are properties of the run,
/// not of the tree. Filename *spelling* is inside it, which is what makes this
/// a diagnostic rather than the determinism contract: see the module docs.
///
/// A symbolic link contributes the target it names, read without following it.
/// Reading through one would digest a document twice under two names, and
/// would fail outright on a link whose target is not there.
#[allow(clippy::disallowed_methods)] // Reads the tree it digests, and the targets of the links in it.
pub fn tree(root: &Path) -> io::Result<[u8; 32]> {
    let mut hasher = Sha256::new();
    for node in tree::walk(root)? {
        hasher.update_framed(node.rel.as_bytes());
        match node.kind {
            tree::Kind::Dir => hasher.update(b"d"),
            tree::Kind::File => {
                hasher.update(b"f");
                hasher.update_framed(&fs::read(&node.path)?);
            }
            tree::Kind::Symlink => {
                hasher.update(b"l");
                hasher.update_framed(fs::read_link(&node.path)?.to_string_lossy().as_bytes());
            }
        }
    }
    Ok(hasher.finish())
}

/// [`tree()`], rendered as hexadecimal.
pub fn tree_hex(root: &Path) -> io::Result<String> {
    Ok(hex(&tree(root)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn digest_of(input: &[u8]) -> String {
        let mut h = Sha256::new();
        h.update(input);
        hex(&h.finish())
    }

    #[test]
    fn matches_the_published_vectors() {
        assert_eq!(
            digest_of(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            digest_of(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            digest_of(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
    }

    #[test]
    fn is_independent_of_how_the_input_is_split() {
        let mut whole = Sha256::new();
        whole.update(b"the quick brown fox jumps over the lazy dog");
        let mut split = Sha256::new();
        split.update(b"the quick brown ");
        split.update(b"fox jumps over ");
        split.update(b"the lazy dog");
        assert_eq!(whole.finish(), split.finish());
    }

    #[test]
    fn spans_a_multi_block_message() {
        // 1000 bytes crosses several 64-byte blocks and the padding boundary.
        let input = vec![b'a'; 1000];
        assert_eq!(
            digest_of(&input),
            "41edece42d63e8d9bf515a9ba6932e1c20cbc9f5a5d134645adb5db1b9737ea3"
        );
    }

    #[test]
    fn framing_separates_a_shared_concatenation() {
        let mut one = Sha256::new();
        one.update_framed(b"ab");
        one.update_framed(b"c");
        let mut two = Sha256::new();
        two.update_framed(b"a");
        two.update_framed(b"bc");
        assert_ne!(one.finish(), two.finish());
    }
}
