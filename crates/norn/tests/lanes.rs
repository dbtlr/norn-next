//! Full regression adopts the compiled registry and inventory audits.

const LANE_BY_FILE_STEM: &[(&str, &str)] = &[
    ("regression", "audit-lane case:"),
    ("certification", "audit-lane case:"),
];

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

#[test]
fn the_lane_steps_ci_runs_are_the_stems_this_table_names() {
    norn_testkit::lanes::assert_lane_steps_agree(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")),
        env!("CARGO_PKG_NAME"),
        LANE_BY_FILE_STEM,
    );
}
