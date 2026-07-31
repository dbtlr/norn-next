//! The seam the host consumes: a shareable trait object, and a typed refusal.

use std::error::Error;
use std::sync::Arc;
use std::thread;

use norn_embed::{EmbedError, Embedder, Model, StubEmbedder};

/// The shape a worker pool holds an embedder in: one model, many callers, no
/// wrapper at the call site. A trait object that were not object-safe, or not
/// `Send + Sync`, would not compile here.
#[test]
fn an_embedder_is_a_shareable_trait_object() {
    let embedder: Arc<dyn Embedder> = Arc::new(StubEmbedder::new());
    let expected = embedder
        .embed("a document")
        .expect("the stub refuses nothing");

    let workers: Vec<_> = (0..4)
        .map(|_| {
            let embedder = Arc::clone(&embedder);
            thread::spawn(move || {
                let embedding = embedder
                    .embed("a document")
                    .expect("the stub refuses nothing");
                assert_eq!(embedding.model(), embedder.model());
                assert_eq!(embedding.dimensions(), embedder.dimensions());
                embedding
            })
        })
        .collect();

    for worker in workers {
        assert_eq!(worker.join().expect("a worker panicked"), expected);
    }
}

/// A caller decides from the variant and its fields. Reaching for the message
/// would be the forbidden shape: prose is not an interface, and matching on it
/// breaks the moment the wording improves.
#[test]
fn a_refusal_is_read_from_its_shape_and_not_from_its_message() {
    let model = Model::new("a-model", "7");
    let refusal = EmbedError::runtime(model.clone(), "the session did not return");

    assert_eq!(refusal.model(), &model);
    match &refusal {
        EmbedError::Runtime { model: named, .. } => assert_eq!(named, &model),
        _ => panic!("a refusal carried no model"),
    }

    let rendered = refusal.to_string();
    assert!(
        rendered.contains("a-model/7"),
        "a refusal renders the model it came from: {rendered}"
    );

    let as_error: &dyn Error = &refusal;
    assert_eq!(as_error.to_string(), rendered);
}

/// The stub is total. A refusal is a runtime's to make, and there is no
/// runtime in a default build.
#[test]
fn the_stub_refuses_nothing() {
    let embedder = StubEmbedder::new();
    for text in ["", " ", "\u{0}", "ordinary text", &"x".repeat(1_000_000)] {
        assert!(
            embedder.embed(text).is_ok(),
            "the stub refused a {}-byte input",
            text.len()
        );
    }
}
