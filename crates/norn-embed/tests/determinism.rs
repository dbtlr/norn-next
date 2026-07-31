//! Same input, same vector — pinned.
//!
//! The bar the stub exists to hold is reproducibility: a vector derived on a
//! developer's machine, in CI, and on a user's laptop is the same vector, so
//! anything built on top of derived vector state can be asserted. The pinned
//! digests below are what makes that a gate rather than a hope — they were
//! computed once and are compared against on every platform the suite runs on.
//!
//! **A digest that no longer matches is not a test to update.** It says the
//! stub computes something different from what it computed before, and vectors
//! already stored under `stub/1` describe the old function. The fix is a new
//! model version, and the new digests belong to it.

use std::sync::Arc;
use std::thread;

use norn_embed::{Embedder, Model, StubEmbedder};

/// A vector's bytes, hashed. The encoding is explicitly little-endian, so the
/// digest says nothing about the byte order of the machine that ran the test —
/// only about the values.
fn digest(values: &[f32]) -> String {
    const OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;

    let mut state = OFFSET_BASIS;
    for byte in values.iter().flat_map(|value| value.to_le_bytes()) {
        state = (state ^ u64::from(byte)).wrapping_mul(PRIME);
    }
    format!("{state:016x}")
}

fn embed(text: &str) -> Vec<f32> {
    StubEmbedder::new()
        .embed(text)
        .expect("the stub refuses nothing")
        .values()
        .to_vec()
}

/// The texts the digests below are pinned over, each with the digest of the
/// vector `stub/1` produces for it.
const PINNED: &[(&str, &str)] = &[
    ("", "21d2b700faab7628"),
    ("the quick brown fox", "69bc398d7ae8c3f6"),
    ("The Quick Brown Fox", "69bc398d7ae8c3f6"),
    (
        "# A heading\n\nA paragraph with [[a wikilink]] and a #tag.\n",
        "810f525183f771b2",
    ),
    ("naïve — с кириллицей 🌍", "c9e914a53cde78f1"),
];

#[test]
fn the_stub_pins_the_vector_it_produces() {
    for (text, expected) in PINNED {
        let values = embed(text);
        assert_eq!(
            &digest(&values),
            expected,
            "the vector for {text:?} moved. Vectors already derived under \
             {}/{} describe the function this one replaced: the change needs a \
             new model version, not a new digest. Got {values:?}",
            StubEmbedder::ID,
            StubEmbedder::VERSION,
        );
    }
}

#[test]
fn the_pinned_stub_keeps_its_identity_and_width() {
    let stub = StubEmbedder::new();
    assert_eq!(stub.model(), &Model::new("stub", "1"));
    assert_eq!(stub.dimensions(), 64);
    assert_eq!(embed("anything").len(), 64);
}

#[test]
fn the_same_text_embeds_the_same_way_twice() {
    let one = StubEmbedder::new();
    let another = StubEmbedder::new();
    for (text, _) in PINNED {
        let first = one.embed(text).expect("the stub refuses nothing");
        let second = one.embed(text).expect("the stub refuses nothing");
        let third = another.embed(text).expect("the stub refuses nothing");
        assert_eq!(first, second, "one embedder answered {text:?} two ways");
        assert_eq!(first, third, "two embedders answered {text:?} two ways");
    }
}

#[test]
fn concurrent_callers_get_the_same_vector() {
    let stub = Arc::new(StubEmbedder::new());
    let expected = stub.embed("shared").expect("the stub refuses nothing");

    let workers: Vec<_> = (0..8)
        .map(|_| {
            let stub = Arc::clone(&stub);
            thread::spawn(move || stub.embed("shared").expect("the stub refuses nothing"))
        })
        .collect();

    for worker in workers {
        let seen = worker.join().expect("a worker panicked");
        assert_eq!(seen, expected, "a concurrent caller saw a different vector");
    }
}

#[test]
fn every_vector_has_unit_length() {
    for text in adversarial_inputs() {
        let values = embed(&text);
        let length: f32 = values.iter().map(|value| value * value).sum::<f32>().sqrt();
        assert!(
            (length - 1.0).abs() < 1e-5,
            "the vector for a {}-byte input has length {length}, not 1",
            text.len()
        );
    }
}

#[test]
fn case_is_folded_over_ascii() {
    assert_eq!(
        embed("the quick brown fox"),
        embed("The Quick Brown Fox"),
        "ASCII case is folded, which is why two of the pins above agree"
    );
}

#[test]
fn word_order_and_repetition_are_the_only_things_a_bag_of_words_can_see() {
    assert_eq!(
        embed("alpha beta"),
        embed("beta alpha"),
        "a bag of words does not see order"
    );
    assert_ne!(
        embed("alpha beta"),
        embed("alpha beta beta"),
        "a bag of words does see how often a word appears"
    );
}

#[test]
fn the_stub_answers_for_every_input_shape() {
    for text in adversarial_inputs() {
        let embedding = StubEmbedder::new().embed(&text);
        assert!(
            embedding.is_ok(),
            "the stub refused a {}-byte input; it is total",
            text.len()
        );
    }
}

/// Inputs chosen to break a tokenizer: nothing to tokenize, separators only,
/// bytes outside ASCII, a byte no text editor writes on purpose, and a body
/// large enough that accumulation order would show if it mattered.
fn adversarial_inputs() -> Vec<String> {
    vec![
        String::new(),
        " ".to_string(),
        "\u{0}\u{1}\u{7f}".to_string(),
        "-—·/\\|,.;:!?".to_string(),
        "\r\n\r\n".to_string(),
        "🌍🌎🌏".to_string(),
        "с кириллицей и с ударением: naïve".to_string(),
        "a".repeat(100_000),
        "word ".repeat(50_000),
    ]
}
