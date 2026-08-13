//! What an ordinary frontmatter block costs to read, as a function of how many
//! keys it holds.
//!
//! `FRONTMATTER_MAX_BYTES` bounds a block's length, which bounds what one read
//! can cost — but a ceiling is not a shape. A read whose cost is quadratic in
//! key count sits under that ceiling and still charges a heal worker orders of
//! magnitude more than the block it is reading is worth. What this suite pins
//! is the shape: parse cost grows with key count, not with its square.
//!
//! The clock is why these are soak cases. Under [ADR 0004] counters and
//! structure gate per PR and clocks trend nightly, and both readings here are
//! clocks. The measurement that matters is the *ratio* between two scales taken
//! in the same run, which is what makes it a bar a hosted runner's throughput
//! does not move.
//!
//! [ADR 0004]: ../../../docs/decisions/0004-two-tier-measurement-and-authored-baselines.md

use std::time::Duration;

use norn_text::{Document, FRONTMATTER_MAX_BYTES};

mod baselines;

const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";

/// How many bytes one entry of the subject block occupies: `kxyz:` and its line
/// break.
const ENTRY_BYTES: usize = 6;

/// The most keys the block bound admits at [`ENTRY_BYTES`] each. The scales
/// below are stated as fractions of this, so they follow the bound rather than
/// a number written twice.
const KEYS_AT_THE_BOUND: usize = FRONTMATTER_MAX_BYTES / ENTRY_BYTES;

/// The subject: a flat mapping of `keys` distinct keys.
///
/// Keys are a fixed width, so the entries are a fixed length and block length is
/// proportional to key count — which is what makes the two scales below a
/// four-fold step in keys rather than in something else. Each key opens with a
/// letter, so every one of them resolves as a string and none is dropped by the
/// value model; values are empty, which is the densest ordinary entry a block
/// can carry and so the most keys the bound admits.
fn flat_block(keys: usize) -> String {
    let mut yaml = String::with_capacity(keys * ENTRY_BYTES);
    for index in 0..keys {
        yaml.push('k');
        let mut rest = index;
        for _ in 0..3 {
            yaml.push(ALPHABET[rest % ALPHABET.len()] as char);
            rest /= ALPHABET.len();
        }
        assert_eq!(rest, 0, "the key width admits every key this block holds");
        yaml.push_str(":\n");
    }
    assert_eq!(yaml.len(), keys * ENTRY_BYTES, "entries are a fixed length");
    yaml
}

/// A document holding a `keys`-key block, which the bound admits whole.
fn subject(keys: usize) -> String {
    let block = flat_block(keys);
    assert!(
        block.len() <= FRONTMATTER_MAX_BYTES,
        "a subject past the bound is refused unparsed, which measures nothing"
    );
    format!("---\n{block}---\n# body\n")
}

/// The best of five samples, after three that only warm the caches, where one
/// sample reads `source` `reads` times.
///
/// The best rather than the mean: a descheduled sample measures the machine, and
/// the ratio this suite asserts on is between two of these, so one inflated
/// sample on either side is a reading about the runner.
fn parse_cost(source: &str, keys: usize, reads: usize) -> Duration {
    let confirm = |document: Document<'_>| {
        assert!(
            document.diagnostics().is_empty(),
            "the subject block reads clean, or this measures a refusal: {:?}",
            document.diagnostics()
        );
        assert_eq!(
            document.fields().len(),
            keys,
            "the subject block splits into one field per key, or this measures less work than \
             the block asks for"
        );
    };
    for _ in 0..3 {
        confirm(Document::parse(source));
    }
    (0..5)
        .map(|_| {
            let started = std::time::Instant::now();
            let documents: Vec<Document<'_>> =
                (0..reads).map(|_| Document::parse(source)).collect();
            let elapsed = started.elapsed();
            documents.into_iter().for_each(confirm);
            elapsed
        })
        .min()
        .expect("five samples")
}

/// **The linearity invariant.** One block of `n` keys costs no more than four
/// blocks of `n / 4` keys — the same keys either way, so a read that is linear
/// in key count charges the same for both arrangements and the split one pays
/// three extra fixed parse overheads on top. A read quadratic in key count
/// charges the whole block four times what the split costs.
///
/// A ceiling cannot say this: a quadratic read of a block at the bound fits
/// under any ceiling loose enough to survive a hosted runner. Both sides are
/// also sampled over comparable wall time, which a bare per-parse ratio between
/// two scales is not — the smaller parse would be short enough for the timer's
/// own noise to move the answer.
#[test]
#[ignore = "soak-lane case: a clock never gates a pull request"]
fn reading_an_ordinary_block_stays_linear_in_its_key_count() {
    let split_keys = KEYS_AT_THE_BOUND / 4;
    let whole_keys = split_keys * 4;

    let split = parse_cost(&subject(split_keys), split_keys, 4);
    let whole = parse_cost(&subject(whole_keys), whole_keys, 1);
    let reading = baselines::per_mille(whole.as_nanos() as u64, split.as_nanos() as u64);

    baselines::record(
        "frontmatter parse against key count",
        &[
            ("keys, split four ways", split_keys.to_string()),
            ("four reads of the split block (ms)", millis(split)),
            ("keys, whole", whole_keys.to_string()),
            ("one read of the whole block (ms)", millis(whole)),
            ("whole against split", baselines::multiple(reading)),
            (
                "whole against split bar",
                baselines::multiple(baselines::WHOLE_AGAINST_SPLIT_PER_MILLE),
            ),
        ],
    );

    assert!(
        reading <= baselines::WHOLE_AGAINST_SPLIT_PER_MILLE,
        "one {whole_keys}-key block cost {}x four {split_keys}-key blocks against a {}x bar, so \
         the parse is growing faster than the block's key count",
        baselines::multiple(reading),
        baselines::multiple(baselines::WHOLE_AGAINST_SPLIT_PER_MILLE)
    );
}

/// The sanity ceiling beside the ratio: the number the lane records is the
/// duration, and what this forbids is a block at the bound costing a heal
/// worker something it notices.
#[test]
#[ignore = "soak-lane case: a clock never gates a pull request"]
fn a_block_at_the_bound_reads_inside_its_ceiling() {
    let keys = KEYS_AT_THE_BOUND;
    let elapsed = parse_cost(&subject(keys), keys, 1);

    baselines::record(
        "frontmatter parse at the bound",
        &[
            ("keys", keys.to_string()),
            ("parse (ms)", millis(elapsed)),
            ("parse ceiling (ms)", millis(baselines::BOUND_PARSE_CEILING)),
        ],
    );

    assert!(
        elapsed <= baselines::BOUND_PARSE_CEILING,
        "reading a {keys}-key block at the bound took {elapsed:?}, past the {:?} sanity ceiling",
        baselines::BOUND_PARSE_CEILING
    );
}

/// Milliseconds to three places, by integer arithmetic: a bar is a whole number
/// of nanoseconds, so a reading formatted through a float would print a value
/// the comparison never made.
fn millis(duration: Duration) -> String {
    format!(
        "{}.{:03}",
        duration.as_millis(),
        duration.as_nanos() / 1_000 % 1_000
    )
}
