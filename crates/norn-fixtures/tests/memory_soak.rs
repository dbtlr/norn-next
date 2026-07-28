//! The ≥5k profile's memory bars and its wall clock, in the lane that owns
//! them.
//!
//! A separate binary from `memory.rs` rather than a separate `#[ignore]`
//! reason inside it, because a lane selects a suite's ignored cases wholesale:
//! one binary holding both lanes' cases would run the wall-clock assertion in
//! the per-PR lane, which is the one thing the two-tier posture forbids there.
//! The split is what lets each lane ask for everything a target has.
//!
//! The wall clock is legal here and only here, and it is a sanity ceiling
//! rather than a bar on speed — the value the lane produces is the recorded
//! duration in the job summary, not the pass.
#![cfg(unix)]

mod baselines;
mod measure;

#[test]
#[ignore = "soak-lane case: the >=5k profile is nightly work, not per-PR"]
fn the_soak_profile_generates_inside_its_baselines() {
    let reading = measure::generate("memory-soak", "soak");
    baselines::record(
        "soak profile generation",
        &[
            (
                "peak resident set (MiB)",
                baselines::mebibytes(reading.peak_rss_bytes),
            ),
            (
                "peak resident set ceiling (MiB)",
                baselines::mebibytes(baselines::SOAK_PEAK_RSS_CEILING_BYTES),
            ),
            (
                "wall clock (s)",
                format!(
                    "{}.{:03}",
                    reading.elapsed.as_secs(),
                    reading.elapsed.subsec_millis()
                ),
            ),
            (
                "wall clock ceiling (s)",
                baselines::SOAK_WALL_CLOCK_CEILING.as_secs().to_string(),
            ),
        ],
    );
    assert!(
        reading.peak_rss_bytes <= baselines::SOAK_PEAK_RSS_CEILING_BYTES,
        "generating `soak` peaked at {} MiB against a {} MiB bar",
        baselines::mebibytes(reading.peak_rss_bytes),
        baselines::mebibytes(baselines::SOAK_PEAK_RSS_CEILING_BYTES)
    );
    assert!(
        reading.elapsed <= baselines::SOAK_WALL_CLOCK_CEILING,
        "generating `soak` took {:?}, past the {:?} sanity ceiling",
        reading.elapsed,
        baselines::SOAK_WALL_CLOCK_CEILING
    );
}

/// The flatness pair one scale up, and a soak case because the ≥5k profile is
/// the scheduled lane's work whatever it costs to run.
#[test]
#[ignore = "soak-lane case: the >=5k profile is nightly work, not per-PR"]
fn peak_memory_holds_flat_from_the_gate_profile_to_the_soak_profile() {
    let gate = measure::generate("memory-pair-gate", "realistic");
    let soak = measure::generate("memory-pair-soak", "soak");
    measure::assert_flat(
        "peak memory across the gate and soak scales",
        ("realistic", gate),
        ("soak", soak),
        baselines::SOAK_TO_GATE_PEAK_RSS_PER_MILLE,
    );
}
