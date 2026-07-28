//! The calibration gate: a realistic-scale profile's shape statistics land
//! inside the checked-in envelope.
//!
//! What this catches is drift — a pool edit, a block-weight change or a
//! distribution tweak that quietly moves the generated corpus away from the
//! collection it is meant to stand in for. The envelope moves only when
//! somebody edits `probe::CALIBRATION`, which is a diff a reviewer reads.

mod scratch;

use std::fs;

use norn_fixtures::Profile;
use norn_fixtures::probe::{self, CALIBRATION};

use scratch::{generate_and_measure, with_tree};

fn profile(name: &str) -> Profile {
    Profile::by_name(name).unwrap_or_else(|| panic!("no profile named `{name}`"))
}

fn assert_calibrated(label: &str, name: &str, seed: u64) {
    let (stats, _) = generate_and_measure(label, &profile(name), seed);
    let deviations = probe::check(&stats, CALIBRATION);
    assert!(
        deviations.is_empty(),
        "`{name}` at seed {seed} left the calibration envelope:\n{}",
        deviations
            .iter()
            .map(|d| format!("  {d}"))
            .collect::<Vec<_>>()
            .join("\n")
    );
}

#[test]
fn the_gate_profile_stays_inside_the_envelope() {
    // More than one seed, because a single one can flatter a distribution.
    for seed in [1, 7] {
        assert_calibrated(&format!("cal-realistic-{seed}"), "realistic", seed);
    }
}

#[test]
#[ignore = "the >=5k-document profile is soak-lane work; NORN-13 activates it"]
fn the_soak_profile_stays_inside_the_envelope() {
    assert_calibrated("cal-soak", "soak", 3);
}

#[test]
fn the_body_distribution_is_long_tailed_rather_than_uniform() {
    // The defect this crate exists to have fixed, asserted as a property
    // rather than as a range: a bounded-uniform generator puts its mean next
    // to its median and has no upper tail worth the name.
    let (stats, _) = generate_and_measure("cal-tail", &profile("realistic"), 13);
    assert!(
        stats.document_bytes_mean > stats.document_bytes_median * 3 / 2,
        "mean {} is not meaningfully above median {}",
        stats.document_bytes_mean,
        stats.document_bytes_median
    );
    assert!(
        stats.document_bytes_max > stats.document_bytes_median * 10,
        "largest document {} is only {}x the median",
        stats.document_bytes_max,
        stats.document_bytes_max / stats.document_bytes_median.max(1)
    );
    assert!(
        stats.document_bytes_mean > 20 * 230,
        "mean body {} sits at the scale a few short paragraphs produce",
        stats.document_bytes_mean
    );
}

#[test]
fn the_probe_and_the_manifest_agree_on_the_tree() {
    let (stats, manifest) = generate_and_measure("cal-agree", &profile("small"), 17);
    assert_eq!(stats.documents as usize, manifest.documents);
    assert_eq!(stats.directories as usize, manifest.directories);
    assert_eq!(
        (stats.documents + stats.non_markdown_files) as usize,
        manifest.files
    );
    assert_eq!(stats.links as usize, manifest.links);
    assert_eq!(
        stats.largest_stem_class as usize,
        manifest.largest_ambiguity_class
    );
}

#[test]
fn the_dangling_share_lands_where_the_knob_puts_it() {
    // The generator reports how many links it made dangle; the tree names
    // those targets in a form nothing else emits, so the claim is checkable.
    let profile = profile("small");
    with_tree("cal-dangling", &profile, 19, |dir, manifest| {
        let mut counted = 0;
        for entry in walk_markdown(dir) {
            counted += fs::read_to_string(&entry)
                .expect("reading a generated document")
                .matches("[[absent-")
                .count();
        }
        assert_eq!(
            counted, manifest.dangling_links,
            "the tree holds {counted} dangling links against a reported {}",
            manifest.dangling_links
        );
        let per_mille = manifest.dangling_links * 1000 / manifest.links.max(1);
        let declared = profile.links.dangling_per_mille as usize;
        assert!(
            per_mille >= declared / 3 && per_mille <= declared * 3,
            "{per_mille} per mille dangling against a declared {declared}"
        );
    });
}

fn walk_markdown(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        for entry in fs::read_dir(&current).expect("reading a generated directory") {
            let entry = entry.expect("reading a generated directory entry");
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "md") {
                out.push(path);
            }
        }
    }
    out
}
