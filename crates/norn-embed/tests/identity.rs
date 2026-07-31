//! Model identity is first-class: an input to the vector, and attached to it.
//!
//! Derived vector state is keyed on `(model id, model version)`, and a
//! migration between models is a rewrite over that key. Both halves of the
//! claim under that are asserted here — that the pair actually selects the
//! function, and that it travels with every vector so a caller never has to
//! remember which embedder answered.

use std::num::NonZeroUsize;

use norn_embed::{Embedder, Embedding, Model, StubEmbedder};

fn stub(id: &str, version: &str) -> StubEmbedder {
    StubEmbedder::with_model(
        Model::new(id, version),
        NonZeroUsize::new(32).expect("32 is not zero"),
    )
    .expect("an id of its own is free at every width")
}

fn embed(embedder: &StubEmbedder, text: &str) -> Embedding {
    embedder.embed(text).expect("the stub refuses nothing")
}

const TEXT: &str = "the same words, whichever model reads them";

#[test]
fn an_embedding_names_the_model_that_made_it() {
    let embedder = stub("some-model", "3");
    let embedding = embed(&embedder, TEXT);
    assert_eq!(
        embedding.model(),
        embedder.model(),
        "an embedding that does not name its model leaves the key to derived \
         state up to a caller's memory"
    );
    assert_eq!(embedding.model().id(), "some-model");
    assert_eq!(embedding.model().version(), "3");
}

#[test]
fn a_version_change_moves_the_vector() {
    assert_ne!(
        embed(&stub("a-model", "1"), TEXT).values(),
        embed(&stub("a-model", "2"), TEXT).values(),
        "a version that does not change the vector is a label, and a \
         migration over it would be rewriting bytes to themselves"
    );
}

#[test]
fn an_id_change_moves_the_vector() {
    assert_ne!(
        embed(&stub("one-model", "1"), TEXT).values(),
        embed(&stub("another-model", "1"), TEXT).values(),
        "two models that agree on every vector are one model"
    );
}

#[test]
fn two_identities_do_not_run_together_into_one() {
    assert_ne!(
        embed(&stub("a", "bc"), TEXT).values(),
        embed(&stub("ab", "c"), TEXT).values(),
        "identity parts are length-prefixed, so a concatenation cannot \
         collide two models onto one function"
    );
}

#[test]
fn an_embedding_is_as_wide_as_the_embedder_that_made_it() {
    for width in [1usize, 2, 7, 32, 384] {
        let embedder = StubEmbedder::with_model(
            Model::new("width", "1"),
            NonZeroUsize::new(width).expect("the widths tested are not zero"),
        )
        .expect("an id of its own is free at every width");
        let embedding = embed(&embedder, TEXT);
        assert_eq!(embedder.dimensions().get(), width);
        assert_eq!(embedding.dimensions(), width);
        assert_eq!(embedding.values().len(), width);
    }
}

/// Width is an input to the vector function that a [`Model`] does not carry,
/// so the one id this crate pins is held to the one width it was pinned at.
/// Every other id stays free, which is what keeps a second stub available to
/// stand in for a second model.
#[test]
fn the_pinned_id_names_one_width_and_every_other_id_names_any() {
    let width = |n: usize| NonZeroUsize::new(n).expect("the widths tested are not zero");
    assert!(
        StubEmbedder::with_model(
            Model::new(StubEmbedder::ID, StubEmbedder::VERSION),
            width(8)
        )
        .is_none(),
        "two widths under one `(model id, version)` key compute two functions \
         and compare equal, and derived state keyed on the pair could not tell \
         them apart"
    );
    assert!(StubEmbedder::with_model(Model::new("a-model", "1"), width(8)).is_some());
}

#[test]
fn a_model_renders_as_id_over_version() {
    assert_eq!(Model::new("a-model", "2").to_string(), "a-model/2");
}
