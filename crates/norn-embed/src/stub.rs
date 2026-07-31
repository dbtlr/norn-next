//! The deterministic stub — the embedder a default build has.
//!
//! # What it computes
//!
//! A hashed bag of words. The text is split into tokens, each token is hashed
//! into one of `dimensions` buckets, the buckets are counted, and the counts
//! are scaled to unit length. Similar text lands in similar buckets, so
//! nearest-vector plumbing has something with structure to sort — but the
//! numbers carry no meaning a language model would recognize, and nothing
//! about the stub pretends otherwise.
//!
//! # Why it is exactly reproducible
//!
//! Same input, same vector, on every platform and every run. Three choices
//! carry that:
//!
//! - **Tokens are defined over bytes**, not over `char` classification. A
//!   token is a maximal run of bytes that are ASCII alphanumeric or non-ASCII,
//!   lowercased over ASCII only. Where the split falls therefore does not move
//!   when a toolchain's Unicode tables do.
//!
//!   The price is paid outside ASCII, and it is worth stating plainly. **Only
//!   ASCII punctuation, whitespace and control bytes separate tokens**: a
//!   non-ASCII byte is a token byte, so an ideographic comma, a non-breaking
//!   space and an em dash all join rather than split, and a CJK run between
//!   two ASCII separators is one token however many words it holds. There is
//!   no Unicode normalization either, so `café` written as one code point and
//!   the same word written as `e` plus a combining accent are two tokens and
//!   two vectors. A retrieval quality that depends on any of that is asking
//!   the stub for something it does not have; a real model runtime is what
//!   answers it.
//! - **Counting is integer arithmetic.** Bucket counts are `u64` and the sum
//!   of their squares is `u128`; neither rounds, so no accumulation order can
//!   change a result.
//! - **The float work is one square root and one division per value.** Both
//!   are correctly rounded by IEEE-754, as are the conversions feeding them,
//!   so every platform computes the same bits. No transcendental function
//!   appears: those are the operations a platform's math library is free to
//!   round differently.
//!
//! `tests/determinism.rs` pins the resulting vectors by digest. That test is
//! the mechanism behind the model-version rule: **changing what this computes
//! fails it, and the fix is a new model version**, because vectors already
//! derived under the old one describe a different function.
//!
//! # Its identity is an input, not a label
//!
//! The model's id and version seed every token hash, so two stubs under
//! different identities produce different vectors for the same text. A
//! `(model id, version)` column pair therefore says something true about the
//! bytes beside it, which is what a migration over those columns relies on.

use std::num::NonZeroUsize;

use crate::Embedder;
use crate::embedding::Embedding;
use crate::error::EmbedError;
use crate::model::Model;

/// The embedder a build without a model runtime has.
///
/// Cheap to construct, cheap to call, and total: it produces a vector for
/// every string and refuses nothing.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StubEmbedder {
    model: Model,
    dimensions: NonZeroUsize,
}

impl StubEmbedder {
    /// The pinned stub's model id.
    pub const ID: &'static str = "stub";

    /// The pinned stub's model version. It moves when what the stub computes
    /// moves, and never otherwise.
    pub const VERSION: &'static str = "2";

    /// The pinned stub's width. Its own number, chosen to be cheap — it
    /// imitates no real model's width, and nothing should read it as a
    /// prediction of one.
    pub const DIMENSIONS: usize = 64;

    /// The pinned stub: [`ID`](Self::ID) at [`VERSION`](Self::VERSION), over
    /// [`DIMENSIONS`](Self::DIMENSIONS) values.
    pub fn new() -> Self {
        let dimensions = NonZeroUsize::new(Self::DIMENSIONS).expect("the pinned width is not zero");
        Self::with_model(Model::new(Self::ID, Self::VERSION), dimensions)
            .expect("the pinned id at the pinned width")
    }

    /// A stub under a named identity and width, or `None` when `model` names
    /// [`ID`](Self::ID) at a width other than [`DIMENSIONS`](Self::DIMENSIONS).
    ///
    /// The identity is otherwise the caller's to choose, which is what lets a
    /// second stub stand in for a second model — the shape a migration over
    /// `(model id, version)` needs to be exercised against. Naming a real
    /// model's id here would put that name on vectors the real model did not
    /// produce, so callers name stubs.
    ///
    /// # Why the pinned id is reserved
    ///
    /// Width is a fourth input to the vector function and the only one that
    /// does not appear in a [`Model`]. Two stubs under the same id at two
    /// widths compute different functions while comparing equal, so derived
    /// state keyed on `(model id, version)` would hold both under one key and
    /// have nothing to tell them apart by. The id this crate pins is held to
    /// one width so that key stays honest; every other id is free at every
    /// width, because nothing has pinned what it means.
    pub fn with_model(model: Model, dimensions: NonZeroUsize) -> Option<Self> {
        if model.id() == Self::ID && dimensions.get() != Self::DIMENSIONS {
            return None;
        }
        Some(StubEmbedder { model, dimensions })
    }
}

impl Default for StubEmbedder {
    fn default() -> Self {
        Self::new()
    }
}

impl Embedder for StubEmbedder {
    fn model(&self) -> &Model {
        &self.model
    }

    fn dimensions(&self) -> NonZeroUsize {
        self.dimensions
    }

    fn embed(&self, text: &str) -> Result<Embedding, EmbedError> {
        let width = self.dimensions.get();
        let seed = seed(&self.model);

        let mut buckets = vec![0u64; width];
        // The model's own feature, present in every vector. It is what makes
        // the count non-zero for text with no tokens at all, so the scaling
        // below always divides by a positive norm.
        buckets[bucket(seed, width)] += 1;
        for token in Tokens::over(text) {
            buckets[bucket(fnv1a(seed, &token), width)] += 1;
        }

        Ok(Embedding::new(self.model.clone(), to_unit_length(&buckets)))
    }
}

/// Where a hash lands. `width` is non-zero: it comes from a [`NonZeroUsize`].
///
/// The hash is run through [`mix`] before the reduction, and that step is
/// load-bearing rather than decorative: `%` by a small width keeps the low
/// bits of its input and discards the rest, and FNV-1a's low bits are worth
/// very little on their own.
fn bucket(hash: u64, width: usize) -> usize {
    (mix(hash) % width as u64) as usize
}

/// A hash spread across all 64 of its bits.
///
/// FNV-1a's rounds are `xor` then multiply, and both carry information
/// upward only: bit *n* of the state is a function of bits 0..=*n* of the
/// input bytes, so the low bits of the hash are a closed function of the low
/// bits of the token. Reducing by a width of 64 keeps exactly six of them,
/// and the result is that tokens agreeing in the low six bits of every byte
/// share a bucket with probability 1 — every ASCII digit is an exact alias of
/// the letter `0x40` above it, `1` for `q` and `2` for `r` on down.
///
/// This is the finalizer that fixes it: two xor-shift-multiply rounds and a
/// last shift, each step a bijection, together carrying every input bit into
/// every output bit. Only wrapping integer arithmetic appears, so it computes
/// the same value everywhere the stub runs.
fn mix(hash: u64) -> u64 {
    let mut state = hash;
    state ^= state >> 30;
    state = state.wrapping_mul(0xbf58_476d_1ce4_e5b9);
    state ^= state >> 27;
    state = state.wrapping_mul(0x94d0_49bb_1331_11eb);
    state ^ (state >> 31)
}

/// The state every token hash starts from.
///
/// Each part is length-prefixed before its bytes, so no two identities reach
/// the same seed by concatenating differently — `("a", "bc")` and `("ab",
/// "c")` are different seeds. The length is written as a `u64` rather than a
/// `usize` so its width is the same on a 32-bit target as on a 64-bit one.
fn seed(model: &Model) -> u64 {
    let mut state = FNV_OFFSET_BASIS;
    for part in [model.id(), model.version()] {
        state = fnv1a(state, &(part.len() as u64).to_le_bytes());
        state = fnv1a(state, part.as_bytes());
    }
    state
}

const FNV_OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

/// FNV-1a, continued from `state`. Spelled out here rather than taken from
/// `std`: a `Hasher` from the standard library carries no guarantee that its
/// output is the same in the next release, and these hashes are pinned.
fn fnv1a(state: u64, bytes: &[u8]) -> u64 {
    bytes.iter().fold(state, |state, byte| {
        (state ^ u64::from(*byte)).wrapping_mul(FNV_PRIME)
    })
}

/// Counts scaled so the vector's length is 1.
///
/// The sum of squares is exact integer arithmetic and at least 1, so the norm
/// is positive and every stub vector is a unit vector. Cosine similarity
/// against these is a dot product, and no consumer has to handle a vector with
/// no direction.
fn to_unit_length(buckets: &[u64]) -> Vec<f32> {
    let sum_of_squares: u128 = buckets
        .iter()
        .map(|count| u128::from(*count) * u128::from(*count))
        .sum();
    let norm = (sum_of_squares as f64).sqrt() as f32;
    buckets.iter().map(|count| *count as f32 / norm).collect()
}

/// The token runs of a string, lowercased over ASCII.
struct Tokens<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Tokens<'a> {
    fn over(text: &'a str) -> Self {
        Tokens {
            bytes: text.as_bytes(),
            at: 0,
        }
    }
}

impl Iterator for Tokens<'_> {
    type Item = Vec<u8>;

    fn next(&mut self) -> Option<Vec<u8>> {
        while self.at < self.bytes.len() && !is_token_byte(self.bytes[self.at]) {
            self.at += 1;
        }
        if self.at == self.bytes.len() {
            return None;
        }

        let start = self.at;
        while self.at < self.bytes.len() && is_token_byte(self.bytes[self.at]) {
            self.at += 1;
        }
        Some(
            self.bytes[start..self.at]
                .iter()
                .map(u8::to_ascii_lowercase)
                .collect(),
        )
    }
}

/// A byte a token is made of: an ASCII letter or digit, or any byte outside
/// ASCII. **Only ASCII separates** — ASCII punctuation, whitespace and control
/// bytes, and nothing else. A non-ASCII separator is a token byte like any
/// other, so an ideographic comma or a non-breaking space joins the runs
/// around it rather than splitting them.
const fn is_token_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || !byte.is_ascii()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stub(id: &str, version: &str) -> StubEmbedder {
        StubEmbedder::with_model(
            Model::new(id, version),
            NonZeroUsize::new(16).expect("16 is not zero"),
        )
        .expect("an id of its own is free at every width")
    }

    fn values(embedder: &StubEmbedder, text: &str) -> Vec<f32> {
        embedder
            .embed(text)
            .expect("the stub refuses nothing")
            .values()
            .to_vec()
    }

    #[test]
    fn a_version_change_moves_the_vector() {
        let text = "the same words either way";
        assert_ne!(
            values(&stub("m", "1"), text),
            values(&stub("m", "2"), text),
            "the model version seeds the hash, so it is an input to the vector"
        );
    }

    #[test]
    fn an_id_change_moves_the_vector() {
        let text = "the same words either way";
        assert_ne!(
            values(&stub("one", "1"), text),
            values(&stub("two", "1"), text),
            "the model id seeds the hash, so it is an input to the vector"
        );
    }

    #[test]
    fn identity_parts_do_not_run_together() {
        let text = "the same words either way";
        assert_ne!(
            values(&stub("a", "bc"), text),
            values(&stub("ab", "c"), text),
            "length-prefixing keeps two identities from reaching one seed"
        );
    }

    #[test]
    fn only_ascii_that_is_not_a_letter_or_a_digit_splits_tokens() {
        let read = |text: &str| Tokens::over(text).collect::<Vec<_>>();
        assert_eq!(
            read("Alpha, beta!"),
            vec![b"alpha".to_vec(), b"beta".to_vec()]
        );
        assert_eq!(read("  \t\n "), Vec::<Vec<u8>>::new());
        assert_eq!(read("a1-b2"), vec![b"a1".to_vec(), b"b2".to_vec()]);
        assert_eq!(read("naïve"), vec!["naïve".as_bytes().to_vec()]);
    }

    /// The other half of the same rule, stated as its cost: a separator
    /// outside ASCII does not separate, so a run of CJK is one token however
    /// many words it holds, and a non-breaking space joins what an ASCII
    /// space would split.
    #[test]
    fn a_separator_outside_ascii_joins_rather_than_splits() {
        let read = |text: &str| Tokens::over(text).collect::<Vec<_>>();
        assert_eq!(
            read("日本語、テスト").len(),
            1,
            "an ideographic comma is a token byte, so the run around it is one token"
        );
        assert_eq!(
            read("a\u{a0}b").len(),
            1,
            "a non-breaking space is a token byte too"
        );
        assert_eq!(read("a b").len(), 2, "an ASCII space is a separator");
    }

    /// FNV-1a's low bits are a closed function of the input's low bits, so a
    /// reduction that keeps only the low bits puts every ASCII digit in the
    /// bucket of the letter `0x40` above it — ten pairs, all ten colliding,
    /// every time. [`mix`] is what breaks that: after it the ten pairs
    /// collide at chance, and `1` and `q` in particular do not.
    #[test]
    fn a_tokens_low_bits_do_not_decide_its_bucket() {
        let width = StubEmbedder::DIMENSIONS;
        let seed = seed(&Model::new(StubEmbedder::ID, StubEmbedder::VERSION));
        let bucket_of = |byte: u8| bucket(fnv1a(seed, &[byte]), width);

        assert_ne!(
            bucket_of(b'1'),
            bucket_of(b'q'),
            "`1` and `q` differ in one bit above the reduction's width"
        );

        let collisions = (b'0'..=b'9')
            .filter(|digit| bucket_of(*digit) == bucket_of(digit ^ 0x40))
            .count();
        assert_eq!(
            collisions, 0,
            "{collisions} of the ten digit/letter pairs share a bucket; ten of \
             ten is the aliasing this guards against"
        );
    }

    /// The pinned id names one function of one width. A width it does not
    /// have would put vectors that are not comparable under one
    /// `(model id, version)` key, with nothing in the key to tell them apart.
    #[test]
    fn the_pinned_id_is_refused_at_a_width_it_does_not_have() {
        let width = |n: usize| NonZeroUsize::new(n).expect("the widths tested are not zero");
        let pinned = Model::new(StubEmbedder::ID, StubEmbedder::VERSION);
        assert!(StubEmbedder::with_model(pinned.clone(), width(8)).is_none());
        assert!(
            StubEmbedder::with_model(pinned, width(StubEmbedder::DIMENSIONS)).is_some(),
            "the pinned width is the one width the pinned id has"
        );
        assert!(
            StubEmbedder::with_model(Model::new(StubEmbedder::ID, "99"), width(8)).is_none(),
            "the id is what is reserved, at every version of it"
        );
        assert!(
            StubEmbedder::with_model(Model::new("not-the-stub", "1"), width(8)).is_some(),
            "an id this crate has pinned nothing about is free at every width"
        );
    }

    #[test]
    fn case_is_folded_over_ascii() {
        assert_eq!(
            Tokens::over("MiXeD").collect::<Vec<_>>(),
            vec![b"mixed".to_vec()]
        );
    }
}
