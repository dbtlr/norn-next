#![forbid(unsafe_code)]
//! Text in, vector out, with the model that produced it named.
//!
//! This crate is one seam — [`Embedder`] — and one implementation of it,
//! [`StubEmbedder`], which is what a build without a model runtime has. Its
//! whole job is turning a string into numbers.
//!
//! # It is blind, and that is the point
//!
//! `norn-embed` has **no workspace dependencies at all**: it cannot reach the
//! store, the filesystem, the vault, or anything that decides. Semantic search
//! is a query surface and never a correctness input, and the reason it can
//! never become one is structural rather than intentional — the code that
//! would have to be written does not compile. The architecture gate holds the
//! absent edges (boundary invariant 12 in `docs/architecture.md`), and
//! `tests/isolation.rs` holds the other half: **no third-party dependency
//! either**, so a model runtime cannot enter a default build.
//!
//! The one effect this crate is ever permitted is acquiring model weights — an
//! opt-in fetch and load, at a path the host injects. It arrives with the
//! runtime that needs it; nothing here touches a filesystem today.
//!
//! # Model identity is first-class
//!
//! A vector is a pure function of `(model id, model version, content)`. All
//! three appear in the API rather than being assumed: an [`Embedder`] names
//! its [`Model`], and every [`Embedding`] carries that model with it. Derived
//! state keyed on the pair therefore says something true, and a model upgrade
//! is a migration over `(model id, version)` — never a reinterpretation of
//! bytes already stored.
//!
//! # The default build is the stub
//!
//! [`StubEmbedder`] produces the same vector for the same input on every
//! platform and every run, so tests and gates over vector state are
//! reproducible without a model, and no development build pays for one. It is
//! a hashed bag of words: structured enough to sort by, and meaningless as
//! language. A real pinned runtime compiles only behind the release/soak
//! feature, which arrives with it.
//!
//! # Where to start
//!
//! - [`Embedder`] — the seam, and what an implementation promises.
//! - [`StubEmbedder`] — the default build's embedder, and how it stays
//!   reproducible.
//! - [`Model`] — the identity that travels with every vector.

use std::num::NonZeroUsize;

mod embedding;
mod error;
mod model;
mod stub;

pub use embedding::Embedding;
pub use error::EmbedError;
pub use model::Model;
pub use stub::StubEmbedder;

/// Something that turns text into a vector.
///
/// # What an implementation promises
///
/// - **A vector is a pure function of `(model id, model version, text)`.** The
///   same embedder given the same text answers the same way, for as long as it
///   exists. An implementation whose answers drift is one whose model version
///   should have moved.
/// - **Every [`Embedding`] it returns names [`model`](Self::model)** and holds
///   [`dimensions`](Self::dimensions) values.
/// - **A failure is a refusal, not a panic.** Loading a model happens before an
///   embedder exists, so nothing here reports an unavailable model; what
///   [`EmbedError`] carries is a model that was asked and did not answer. The
///   stub refuses nothing, and a caller still has to handle a refusal, because
///   a runtime session can fail on an input.
///
/// # Shared by construction
///
/// `Send + Sync` are supertraits, so a `dyn Embedder` is shareable without a
/// caller wrapping it. Embedding is the work of a pool of workers; an
/// implementation whose model handle is not itself shareable owns that
/// problem — one embedder is one model, and serializing access to it inside
/// the implementation is a decision only the implementation can make well.
pub trait Embedder: Send + Sync {
    /// Which model this embedder speaks for. Every [`Embedding`] it produces
    /// carries this identity.
    fn model(&self) -> &Model;

    /// How many values its vectors hold.
    ///
    /// The type carries the guarantee rather than the prose: a width of zero
    /// is a vector with no values, which no consumer of one has a meaning
    /// for, and an implementation outside this crate cannot report one.
    fn dimensions(&self) -> NonZeroUsize;

    /// The vector for `text`.
    ///
    /// # Errors
    ///
    /// [`EmbedError`] when the model was asked and produced nothing.
    fn embed(&self, text: &str) -> Result<Embedding, EmbedError>;
}
