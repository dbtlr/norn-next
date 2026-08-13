//! What an ordinary frontmatter block costs to read, as a function of how many
//! keys it holds.
//!
//! `FRONTMATTER_MAX_BYTES` bounds a block's length, which bounds what one read
//! can cost — but a ceiling is not a shape. A read whose cost is quadratic in
//! key count sits under that ceiling and still charges a heal worker orders of
//! magnitude more than the block it is reading is worth. What this suite pins
//! is the shape, in both places a block's keys are all resolved: reading the
//! block, and deriving every field text out of it, each grow with key count and
//! not with its square.
//!
//! The clock is why these are soak cases. Under [ADR 0004] counters and
//! structure gate per PR and clocks trend nightly, and every reading here is a
//! clock. The measurement that matters is the *ratio* between two scales taken
//! in the same run, which is what makes it a bar a hosted runner's throughput
//! does not move.
//!
//! [ADR 0004]: ../../../docs/decisions/0004-two-tier-measurement-and-authored-baselines.md

use std::time::Duration;

use norn_text::{Document, FRONTMATTER_MAX_BYTES};

mod baselines;

const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";

/// How many bytes an entry of the subject block occupies before its value:
/// `kxyz:` and its line break.
const ENTRY_BYTES: usize = 6;

/// The most keys the block bound admits at [`ENTRY_BYTES`] each. The scales
/// below are stated as fractions of this, so they follow the bound rather than
/// a number written twice.
const KEYS_AT_THE_BOUND: usize = FRONTMATTER_MAX_BYTES / ENTRY_BYTES;

/// The value the text subject writes under every key: one character, so the
/// entry stays short and the block still holds thousands of them.
const TEXT_VALUE: &str = "v";

/// How many bytes one entry of the text subject occupies: [`ENTRY_BYTES`], a
/// separating space, and the value.
const TEXT_ENTRY_BYTES: usize = ENTRY_BYTES + 1 + TEXT_VALUE.len();

/// The most keys the block bound admits at [`TEXT_ENTRY_BYTES`] each.
const TEXT_KEYS_AT_THE_BOUND: usize = FRONTMATTER_MAX_BYTES / TEXT_ENTRY_BYTES;

/// The subject: a flat mapping of `keys` distinct keys, each holding `value`.
///
/// Keys are a fixed width and so is the value, so the entries are a fixed length
/// and block length is proportional to key count — which is what makes the two
/// scales below a four-fold step in keys rather than in something else. Each key
/// opens with a letter, so every one of them resolves as a string and none is
/// dropped by the value model. An empty `value` is the densest ordinary entry a
/// block can carry and so the most keys the bound admits; a one-character one
/// gives every field a string to derive a text from.
fn flat_block(keys: usize, value: &str) -> String {
    let entry_bytes = if value.is_empty() {
        ENTRY_BYTES
    } else {
        ENTRY_BYTES + 1 + value.len()
    };
    let mut yaml = String::with_capacity(keys * entry_bytes);
    for index in 0..keys {
        yaml.push('k');
        let mut rest = index;
        for _ in 0..3 {
            yaml.push(ALPHABET[rest % ALPHABET.len()] as char);
            rest /= ALPHABET.len();
        }
        assert_eq!(rest, 0, "the key width admits every key this block holds");
        yaml.push(':');
        if !value.is_empty() {
            yaml.push(' ');
            yaml.push_str(value);
        }
        yaml.push('\n');
    }
    assert_eq!(yaml.len(), keys * entry_bytes, "entries are a fixed length");
    yaml
}

/// A document holding a `keys`-key block, which the bound admits whole.
fn subject(keys: usize, value: &str) -> String {
    let block = flat_block(keys, value);
    assert!(
        block.len() <= FRONTMATTER_MAX_BYTES,
        "a subject past the bound is refused unparsed, which measures nothing"
    );
    format!("---\n{block}---\n# body\n")
}

/// The best of five samples, after three that only warm the caches, where one
/// sample reads `source` `reads` times.
///
/// A block's keys are all resolved against the parsed mapping in two places, and
/// only the first is on the read this times: the field split resolves every
/// scanned key line, and [`Document::field_texts`] — which [`derive_cost`]
/// measures — resolves every field again.
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

/// What `walks` derives of every field text cost over one already-parsed
/// `source`, sampled the way [`parse_cost`] samples a read.
///
/// The parse is outside the clock on purpose. It is linear in key count on both
/// sides of the ratio and it is the larger share of an ordinary read, so timing
/// it here would dilute the walk's shape into a reading that passes a bar the
/// walk alone fails.
fn derive_cost(source: &str, keys: usize, walks: usize) -> Duration {
    let document = Document::parse(source);
    assert!(
        document.diagnostics().is_empty(),
        "the subject block reads clean, or this measures a refusal: {:?}",
        document.diagnostics()
    );
    assert_eq!(
        document.fields().len(),
        keys,
        "the subject block splits into one field per key, or this measures less work than the \
         block asks for"
    );
    let confirm = |texts: usize| {
        assert_eq!(
            texts, keys,
            "the subject block derives one text per key, or this walk is shorter than the \
             block's field count"
        );
    };
    for _ in 0..3 {
        confirm(document.field_texts().len());
    }
    (0..5)
        .map(|_| {
            let started = std::time::Instant::now();
            let counts: Vec<usize> = (0..walks).map(|_| document.field_texts().len()).collect();
            let elapsed = started.elapsed();
            counts.into_iter().for_each(confirm);
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

    let split = parse_cost(&subject(split_keys, ""), split_keys, 4);
    let whole = parse_cost(&subject(whole_keys, ""), whole_keys, 1);
    let reading = baselines::per_mille(whole.as_nanos() as u64, split.as_nanos() as u64);

    baselines::record(
        "frontmatter parse against key count",
        &[
            ("keys, split four ways", split_keys.to_string()),
            (
                "four reads of the split block (ms)",
                baselines::milliseconds(split),
            ),
            ("keys, whole", whole_keys.to_string()),
            (
                "one read of the whole block (ms)",
                baselines::milliseconds(whole),
            ),
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
    let elapsed = parse_cost(&subject(keys, ""), keys, 1);

    baselines::record(
        "frontmatter parse at the bound",
        &[
            ("keys", keys.to_string()),
            ("parse (ms)", baselines::milliseconds(elapsed)),
            (
                "parse ceiling (ms)",
                baselines::milliseconds(baselines::BOUND_PARSE_CEILING),
            ),
        ],
    );

    assert!(
        elapsed <= baselines::BOUND_PARSE_CEILING,
        "reading a {keys}-key block at the bound took {elapsed:?}, past the {:?} sanity ceiling",
        baselines::BOUND_PARSE_CEILING
    );
}

/// **The linearity invariant over the derive walk.** The parse is one of two
/// places a block's keys are all resolved; [`Document::field_texts`] is the
/// other, and it is what a caller reading tags or wikilinks out of a block goes
/// through. The arrangement is the one above — one block of `n` keys against
/// four of `n / 4` — and the subject gives every key a string value, so the walk
/// yields one text per field rather than skipping the fields it visits.
#[test]
#[ignore = "soak-lane case: a clock never gates a pull request"]
fn deriving_every_field_text_stays_linear_in_field_count() {
    let split_keys = TEXT_KEYS_AT_THE_BOUND / 4;
    let whole_keys = split_keys * 4;

    let split = derive_cost(&subject(split_keys, TEXT_VALUE), split_keys, 4);
    let whole = derive_cost(&subject(whole_keys, TEXT_VALUE), whole_keys, 1);
    let reading = baselines::per_mille(whole.as_nanos() as u64, split.as_nanos() as u64);

    baselines::record(
        "frontmatter text derive against key count",
        &[
            ("keys, split four ways", split_keys.to_string()),
            (
                "four derives over the split block (ms)",
                baselines::milliseconds(split),
            ),
            ("keys, whole", whole_keys.to_string()),
            (
                "one derive over the whole block (ms)",
                baselines::milliseconds(whole),
            ),
            ("whole against split", baselines::multiple(reading)),
            (
                "whole against split bar",
                baselines::multiple(baselines::DERIVE_WHOLE_AGAINST_SPLIT_PER_MILLE),
            ),
        ],
    );

    assert!(
        reading <= baselines::DERIVE_WHOLE_AGAINST_SPLIT_PER_MILLE,
        "deriving every text of one {whole_keys}-key block cost {}x the same over four \
         {split_keys}-key blocks, against a {}x bar, so the derive is growing faster than the \
         block's field count",
        baselines::multiple(reading),
        baselines::multiple(baselines::DERIVE_WHOLE_AGAINST_SPLIT_PER_MILLE)
    );
}
