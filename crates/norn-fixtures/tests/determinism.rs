//! The determinism gate: the same `(profile, seed)` produces the same tree,
//! byte for byte.
//!
//! Each case generates into a fresh directory, reduces the whole tree to one
//! digest, and deletes it before comparing — so two runs are compared through
//! their digests rather than by holding two copies of a multi-megabyte tree at
//! once, and a passing suite leaves nothing behind.
//!
//! The largest profile is `#[ignore]`d here and adopted by the soak lane,
//! which is the lane long-running work belongs in.

mod scratch;

use norn_fixtures::{Manifest, Profile, digest};

use scratch::generate_and_digest;

/// The profiles whose determinism is asserted on every pull request.
const PER_PR_PROFILES: &[&str] = &["tiny", "small", "ambiguous", "realistic"];

fn profile(name: &str) -> Profile {
    Profile::by_name(name).unwrap_or_else(|| panic!("no profile named `{name}`"))
}

#[test]
fn every_per_pr_profile_reproduces_its_tree() {
    for name in PER_PR_PROFILES {
        let profile = profile(name);
        let (first_digest, first) = generate_and_digest(&format!("det-{name}-a"), &profile, 7);
        let (second_digest, second) = generate_and_digest(&format!("det-{name}-b"), &profile, 7);
        assert_eq!(
            digest::hex(&first_digest),
            digest::hex(&second_digest),
            "`{name}` at seed 7 produced two different trees"
        );
        assert_eq!(first, second, "`{name}` at seed 7 reported two manifests");
    }
}

#[test]
#[ignore = "the >=5k-document profile is soak-lane work; NORN-13 activates it"]
fn the_soak_profile_reproduces_its_tree() {
    let profile = profile("soak");
    let (first, _) = generate_and_digest("det-soak-a", &profile, 3);
    let (second, _) = generate_and_digest("det-soak-b", &profile, 3);
    assert_eq!(
        digest::hex(&first),
        digest::hex(&second),
        "`soak` at seed 3 produced two different trees"
    );
}

#[test]
fn a_different_seed_produces_a_different_tree() {
    let profile = profile("small");
    let (one, _) = generate_and_digest("seed-one", &profile, 1);
    let (two, _) = generate_and_digest("seed-two", &profile, 2);
    assert_ne!(
        digest::hex(&one),
        digest::hex(&two),
        "two seeds produced identical trees, so the seed does nothing"
    );
}

#[test]
fn one_seed_produces_unrelated_trees_across_profiles() {
    let mut digests: Vec<String> = Vec::new();
    for name in ["tiny", "small", "ambiguous"] {
        let (tree, _) = generate_and_digest(&format!("cross-{name}"), &profile(name), 5);
        digests.push(digest::hex(&tree));
    }
    digests.sort();
    let total = digests.len();
    digests.dedup();
    assert_eq!(total, digests.len(), "two profiles produced the same tree");
}

#[test]
fn a_manifest_counts_what_the_tree_holds() {
    let profile = profile("small");
    let (_, manifest) = generate_and_digest("counts", &profile, 11);
    let Manifest {
        documents,
        files,
        directories,
        links,
        dangling_links,
        largest_ambiguity_class,
        ..
    } = manifest;
    assert_eq!(documents, profile.docs);
    assert!(files > documents, "no clutter or sentinel was written");
    assert!(directories > 0, "no directory was created");
    assert!(links > 0, "no link was emitted");
    assert!(dangling_links > 0, "no link was made to dangle");
    assert_eq!(largest_ambiguity_class, profile.ambiguity.k);
}
