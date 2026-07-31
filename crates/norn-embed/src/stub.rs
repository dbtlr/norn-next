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
    pub const VERSION: &'static str = "1";

    /// The pinned stub's width. Its own number, chosen to be cheap — it
    /// imitates no real model's width, and nothing should read it as a
    /// prediction of one.
    pub const DIMENSIONS: usize = 64;

    /// The pinned stub: [`ID`](Self::ID) at [`VERSION`](Self::VERSION), over
    /// [`DIMENSIONS`](Self::DIMENSIONS) values.
    pub fn new() -> Self {
        let dimensions = NonZeroUsize::new(Self::DIMENSIONS).expect("the pinned width is not zero");
        Self::with_model(Model::new(Self::ID, Self::VERSION), dimensions)
    }

    /// A stub under a named identity and width.
    ///
    /// The identity is the caller's to choose, which is what lets a second
    /// stub stand in for a second model — the shape a migration over
    /// `(model id, version)` needs to be exercised against. Naming a real
    /// model's id here would put that name on vectors the real model did not
    /// produce, so callers name stubs.
    pub fn with_model(model: Model, dimensions: NonZeroUsize) -> Self {
        StubEmbedder { model, dimensions }
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

    fn dimensions(&self) -> usize {
        self.dimensions.get()
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
fn bucket(hash: u64, width: usize) -> usize {
    (hash % width as u64) as usize
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

/// A byte a token is made of: an ASCII letter or digit, or any byte of a
/// multi-byte UTF-8 sequence. Everything else — punctuation, whitespace,
/// control bytes — separates.
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
    fn tokens_split_on_everything_that_is_not_a_letter_or_a_digit() {
        let read = |text: &str| Tokens::over(text).collect::<Vec<_>>();
        assert_eq!(
            read("Alpha, beta!"),
            vec![b"alpha".to_vec(), b"beta".to_vec()]
        );
        assert_eq!(read("  \t\n "), Vec::<Vec<u8>>::new());
        assert_eq!(read("a1-b2"), vec![b"a1".to_vec(), b"b2".to_vec()]);
        assert_eq!(read("naïve"), vec!["naïve".as_bytes().to_vec()]);
    }

    #[test]
    fn case_is_folded_over_ascii() {
        assert_eq!(
            Tokens::over("MiXeD").collect::<Vec<_>>(),
            vec![b"mixed".to_vec()]
        );
    }
}
