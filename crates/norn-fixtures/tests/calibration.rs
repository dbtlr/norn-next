//! The calibration gate: a realistic-scale profile's shape statistics land
//! inside the checked-in envelope.
//!
//! What this catches is drift — a pool edit, a block-weight change or a
//! distribution tweak that quietly moves the generated corpus away from the
//! collection it is meant to stand in for. The envelope moves only when
//! somebody edits `probe::CALIBRATION`, which is a diff a reviewer reads.
//!
//! This suite's scratch trees are backed by `norn_testkit::process::Sandbox`,
//! which is unix-only.
#![cfg(unix)]

mod scratch;

use std::fs;
use std::path::{Path, PathBuf};

use norn_fixtures::probe::{self, CALIBRATION, VaultStats};
use norn_fixtures::{Manifest, Profile};

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
#[ignore = "soak-lane case: the >=5k profile is nightly work, not per-PR"]
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
    // A bounded uniform of a few short paragraphs means about 230 bytes. The
    // mixture's arithmetic mean is about twenty times that; a single sample of
    // it lands a few percent either side, because a tail this heavy moves the
    // mean around. The bound here is the order-of-magnitude claim, which is
    // what "the bodies are not tiny" means — the narrow band belongs to the
    // calibration envelope, where a drift of a few percent is the point.
    assert!(
        stats.document_bytes_mean > 15 * 230,
        "mean body {} sits near the scale a few short paragraphs produce",
        stats.document_bytes_mean
    );
}

/// The probe reads the tree back off disk; the manifest records what the
/// generator emitted. Agreement — including the exact Markdown byte total — is
/// the check that every emitted byte actually landed.
fn assert_agrees(name: &str, stats: &VaultStats, manifest: &Manifest) {
    assert_eq!(
        stats.documents as usize, manifest.documents,
        "{name}: documents"
    );
    assert_eq!(
        stats.directories as usize, manifest.directories,
        "{name}: directories"
    );
    assert_eq!(
        (stats.documents + stats.non_markdown_files) as usize,
        manifest.files,
        "{name}: files"
    );
    assert_eq!(stats.links as usize, manifest.links, "{name}: links");
    assert_eq!(
        stats.largest_stem_class as usize, manifest.largest_ambiguity_class,
        "{name}: largest stem class"
    );
    assert_eq!(
        stats.markdown_bytes as usize, manifest.markdown_bytes,
        "{name}: the Markdown bytes on disk must equal the bytes the generator emitted"
    );
    // Species by species, because the totals agreeing would let two species
    // swap: a dangling link written where an outbound one was planned keeps
    // the count and changes what the tree exercises.
    assert_eq!(
        [
            stats.in_vault_file_symlinks as usize,
            stats.in_vault_dir_symlinks as usize,
            stats.dangling_symlinks as usize,
            stats.outbound_symlinks as usize,
        ],
        [
            manifest.symlinks.in_vault_file,
            manifest.symlinks.in_vault_dir,
            manifest.symlinks.dangling,
            manifest.symlinks.outbound,
        ],
        "{name}: the links on disk must be the species the generator reported emitting"
    );
}

#[test]
fn the_probe_and_the_manifest_agree_on_the_tree() {
    for (name, seed) in [
        ("tiny", 3),
        ("small", 17),
        ("ambiguous", 5),
        ("realistic", 7),
    ] {
        let label = format!("cal-agree-{name}");
        let (stats, manifest) = generate_and_measure(&label, &profile(name), seed);
        assert_agrees(name, &stats, &manifest);
    }
}

#[test]
#[ignore = "soak-lane case: the >=5k profile is nightly work, not per-PR"]
fn the_probe_and_the_manifest_agree_on_the_soak_tree() {
    let (stats, manifest) = generate_and_measure("cal-agree-soak", &profile("soak"), 3);
    assert_agrees("soak", &stats, &manifest);
}

/// Every link a profile asks for lands, as the species it was asked for.
///
/// This is the masking inversion the platform posture exists to prevent, put
/// the other way round: a generation that quietly emitted fewer links than its
/// profile declares — or the same number in the wrong species — would report
/// success and leave every consumer measuring link handling measuring nothing.
#[test]
fn a_generation_emits_every_species_its_profile_declares() {
    for name in ["tiny", "small", "ambiguous", "realistic"] {
        let profile = profile(name);
        with_tree(
            &format!("cal-symlinks-{name}"),
            &profile,
            23,
            |dir, manifest| {
                assert_eq!(
                    manifest.symlinks, profile.symlinks,
                    "{name}: the manifest reports links the profile did not ask for"
                );
                let links = scratch::symlinks(dir);
                assert_eq!(
                    links.len(),
                    profile.symlinks.total(),
                    "{name}: the tree holds {} links against a declared {}",
                    links.len(),
                    profile.symlinks.total()
                );
            },
        );
    }
}

/// A link is never a document, whatever it is named or resolves to.
///
/// A `.md` link naming a document is the case: counted as a document it adds
/// one that was never generated, and the count on disk stops being the count
/// the profile declares.
#[test]
fn a_markdown_named_link_is_not_counted_as_a_document() {
    let profile = profile("small");
    with_tree("cal-link-not-document", &profile, 29, |dir, manifest| {
        let markdown_links = scratch::symlinks(dir)
            .iter()
            .filter(|(entry, _)| entry.rel.ends_with(".md"))
            .count();
        assert!(
            markdown_links > 0,
            "the profile emitted no `.md`-named link, so this case proves nothing"
        );
        assert_eq!(
            scratch::documents(dir).len(),
            manifest.documents,
            "the tree holds a different number of documents than the generator emitted"
        );
    });
}

/// The first directory link in the tree at `root` that a walk re-enters, named
/// by its relative path.
///
/// Every path here is canonical, so the question is put to the filesystem
/// rather than to the generator's own spelling of a target: a link's target is
/// a directory when `canonicalize` says so, and a directory is inside another
/// when its canonical path starts with the other's. A dangling or outbound
/// link canonicalizes to nothing and takes no part.
#[allow(clippy::disallowed_methods)] // Resolves the links of the tree the case walks.
fn re_entered_link(root: &Path) -> Option<String> {
    // Each directory link as (the directory holding it, the directory it
    // names, its relative path).
    let links: Vec<(PathBuf, PathBuf, String)> = scratch::symlinks(root)
        .into_iter()
        .filter_map(|(entry, _)| {
            let target = fs::canonicalize(&entry.path).ok()?;
            let holder = fs::canonicalize(entry.path.parent()?).ok()?;
            target.is_dir().then_some((holder, target, entry.rel))
        })
        .collect();

    links.iter().find_map(|(holder, target, rel)| {
        let mut reached = vec![target.clone()];
        let mut next = 0;
        while next < reached.len() {
            let here = reached[next].clone();
            next += 1;
            if holder.starts_with(&here) {
                return Some(rel.clone());
            }
            for (other_holder, other_target, _) in &links {
                if other_holder.starts_with(&here) && !reached.contains(other_target) {
                    reached.push(other_target.clone());
                }
            }
        }
        None
    })
}

/// No walk of a generated tree loops.
///
/// Two directory links naming each other's subtrees make a path of unbounded
/// length — the kernel answers `ELOOP` after about thirty hops, and every
/// consumer that follows links fails on a tree it was handed as well-formed.
/// Neither link sits inside the directory it names, so the pair passes any
/// check that looks at one link at a time. Seed 365 is a pair the generator
/// emitted while its check did.
#[test]
fn no_walk_of_a_generated_tree_loops() {
    with_tree("cal-loops", &profile("realistic"), 365, |dir, manifest| {
        assert!(
            manifest.symlinks.in_vault_dir > 1,
            "the profile carries one directory link, so this case proves nothing"
        );
        assert_eq!(
            re_entered_link(dir),
            None,
            "a walk of this tree descends the same link without end"
        );
    });
}

#[test]
fn the_dangling_share_lands_where_the_knob_puts_it() {
    // The generator reports how many links it made dangle; the tree names
    // those targets in a form nothing else emits, so the claim is checkable.
    let profile = profile("small");
    with_tree("cal-dangling", &profile, 19, |dir, manifest| {
        let counted: usize = scratch::document_texts(dir)
            .iter()
            .map(|text| text.matches("absent-").count())
            .sum();
        assert_eq!(
            counted, manifest.dangling_links,
            "the tree holds {counted} absent targets against a reported {}",
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
