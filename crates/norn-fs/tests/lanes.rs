//! The lane assignment is the `#[ignore]` reason, so the reason is checked.
//!
//! One CI lane adopts this crate's ignored cases by asking for them wholesale:
//! the per-PR memory job runs this crate's `walk_memory` target with
//! `--ignored` and asserts that a non-zero number of cases passed. That is what
//! makes a name filter unnecessary, and it is also what makes a stray
//! `#[ignore]` dangerous — an `#[ignore = "flaky"]` added for a local reason
//! would be silently adopted by that lane, and would run under a bar nobody
//! meant it to face.
//!
//! The walk that reads this crate's sources and holds every `#[ignore]` in them
//! to a lane is `norn_testkit::lanes`, shared with every other crate whose
//! cases a lane adopts. What lives here is the table it walks against: which
//! file carries which lane is this crate's own business and changes in this
//! crate's own diffs.

/// Which lane prefix a file's `#[ignore]` reasons must open with, keyed by
/// the file's stem — its name under `tests/` without the `.rs` extension.
///
/// - `memory-lane case:` — the per-PR `memory invariant` job's `--ignored`
///   step.
///
/// An ignored case in a new test file means adding that file's stem here, in
/// the same diff: a stem this table does not name is an error, not a case with
/// no lane to check against. Whether a workflow step runs the file is a pairing
/// this guard does not read and a reviewer does.
const LANE_BY_FILE_STEM: &[(&str, &str)] = &[("walk_memory", "memory-lane case:")];

#[test]
fn every_ignored_case_names_the_lane_that_adopts_it() {
    norn_testkit::lanes::assert_every_ignored_case_names_its_lane(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")),
        LANE_BY_FILE_STEM,
    );
}

#[test]
fn lane_prefixes_agree_with_testkit() {
    norn_testkit::lanes::assert_lane_prefixes_agree(env!("CARGO_PKG_NAME"), LANE_BY_FILE_STEM);
}
