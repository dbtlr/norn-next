//! The per-PR memory bars: what generating a tree costs, measured rather than
//! asserted, at the profiles the per-PR lane owns.
//!
//! Two instruments, both counts and neither a clock. A **ceiling** holds the
//! ~2k-document `realistic` profile's peak resident set under a checked-in
//! number. A **flatness pair** holds the ratio between the 300-document
//! `ambiguous` profile and `realistic`, which is the sharper of the two: a
//! ceiling passes anything that fits under it, and a ratio fails the moment
//! two scales stop moving together. Both profiles are under 5k documents, so
//! both are this lane's work under ADR 0002's by-kind split; the ≥5k profile's
//! cases live in `memory_soak.rs`.
//!
//! **Every case here is `#[ignore]`d into the `memory-lane` lane**, and the CI
//! `memory invariant` job is the only thing that runs them. A measurement
//! running beside the workspace suite measures the workspace suite too, so
//! "build and test" stays free of measurement.
#![cfg(unix)]

mod baselines;
mod measure;

#[test]
#[ignore = "memory-lane case: runs in the ci memory job, not the workspace suite"]
fn the_gate_profile_generates_inside_its_memory_bar() {
    let reading = measure::generate("memory-gate", "realistic");
    baselines::record(
        "gate profile generation",
        &[
            (
                "peak resident set (MiB)",
                baselines::mebibytes(reading.peak_rss_bytes),
            ),
            (
                "peak resident set ceiling (MiB)",
                baselines::mebibytes(baselines::GATE_PEAK_RSS_CEILING_BYTES),
            ),
        ],
    );
    assert!(
        reading.peak_rss_bytes <= baselines::GATE_PEAK_RSS_CEILING_BYTES,
        "generating `realistic` peaked at {} MiB against a {} MiB bar",
        baselines::mebibytes(reading.peak_rss_bytes),
        baselines::mebibytes(baselines::GATE_PEAK_RSS_CEILING_BYTES)
    );
}

/// The memory invariant as a slope rather than a point, at per-PR scale.
///
/// `ambiguous` and `realistic` are both under 5k documents, and the clutter
/// between them spans 35x — 129 KiB against 4.34 MiB. A generator whose cost
/// were the tree would show that spread in its peak; this one shows a fraction
/// of it, because the only things that grow with the tree are one path and one
/// digest per emitted file.
#[test]
#[ignore = "memory-lane case: runs in the ci memory job, not the workspace suite"]
fn peak_memory_holds_flat_from_the_ambiguity_profile_to_the_gate_profile() {
    let small = measure::generate("memory-pair-ambiguous", "ambiguous");
    let gate = measure::generate("memory-pair-realistic", "realistic");
    measure::assert_flat(
        "peak memory across the two per-PR scales",
        ("ambiguous", small),
        ("realistic", gate),
        baselines::GATE_PAIR_PEAK_RSS_PER_MILLE,
    );
}
