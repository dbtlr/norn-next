//! The determinism gate: the same `(profile, seed)` produces the same tree,
//! byte for byte.
//!
//! Each case generates into a fresh directory, takes the contract digest, and
//! deletes the tree before comparing — so two runs are compared through their
//! digests rather than by holding two copies of a multi-megabyte tree at once,
//! and a passing suite leaves nothing behind.
//!
//! The largest profile is `#[ignore]`d here and adopted by the soak lane. Not
//! because it is expensive — it bills about nine seconds — but because the
//! lanes divide by *kind*: the per-PR lane runs the profiles the per-PR gates
//! are to measure against, and the >=5k profile belongs to the lane that runs >=5k
//! work. Splitting on lane discipline rather than on a stopwatch keeps the
//! rule from drifting every time a machine gets faster.

mod scratch;

use std::fs;
use std::path::{Path, PathBuf};

use norn_fixtures::{Manifest, Profile, digest, generate};

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

/// The contract digest is a property of the generator, not of the filesystem.
///
/// Some filesystems store file names in a fixed Unicode normalization form and
/// hand back a different spelling than the one written, so a digest whose path
/// keys came from `readdir` would differ between two machines generating the
/// same tree. This case demonstrates the separation directly: an NFD-spelled
/// name is written into a finished tree, and afterwards
///
/// - the contract digest of a fresh generation is unchanged, because it never
///   asked the filesystem how anything is spelled, while
/// - the walk-based diagnostic digest has moved, because it did.
///
/// The generated profile carries non-ASCII document names of its own, so the
/// class of name at issue is in the tree either way.
#[test]
fn the_contract_digest_does_not_depend_on_what_the_filesystem_reports() {
    let profile = profile("small");
    let dir = scratch::fresh("normalization");
    let before = generate(&profile, 5, &dir).expect("generating a scratch tree");
    let walked_before = digest::tree(&dir).expect("digesting a scratch tree");

    assert!(
        non_ascii_document_count(&dir) > 0,
        "the profile emitted no non-ASCII names, so this case proves nothing"
    );

    fs::write(dir.join(NFD_NAME), NFD_BODY).expect("writing an NFD name");

    let after = generate(&profile, 5, &scratch::fresh("normalization-again"))
        .expect("generating a scratch tree");
    let walked_after = digest::tree(&dir).expect("digesting a scratch tree");

    assert_eq!(
        digest::hex(&before.tree_digest),
        digest::hex(&after.tree_digest),
        "the contract digest moved when only the directory's contents changed"
    );
    assert_ne!(
        digest::hex(&walked_before),
        digest::hex(&walked_after),
        "the walk-based digest ignored files that were added to the tree"
    );

    fs::remove_dir_all(&dir).expect("removing a scratch tree");
    fs::remove_dir_all(PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("normalization-again"))
        .expect("removing a scratch tree");
}

fn non_ascii_document_count(dir: &Path) -> usize {
    let mut count = 0;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        for entry in fs::read_dir(&current).expect("reading a generated directory") {
            let entry = entry.expect("reading a generated directory entry");
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if !entry.file_name().to_string_lossy().is_ascii() {
                count += 1;
            }
        }
    }
    count
}

/// "cafe" followed by U+0301 COMBINING ACUTE ACCENT — the decomposed spelling
/// of a name a collection would ordinarily write as "café.md".
const NFD_NAME: &str = "cafe\u{301}.md";
/// The same name precomposed: a single U+00E9. Character for character equal
/// to [`NFD_NAME`], byte for byte different.
const NFC_NAME: &str = "caf\u{e9}.md";
const NFD_BODY: &[u8] = b"---\ntitle: Decomposed\n---\n";
const NFC_BODY: &[u8] = b"---\ntitle: Composed\n---\n";

/// How a name written in one normalization form comes back is the
/// filesystem's business, and this case records that it is.
///
/// Both spellings of one name are written into an empty directory. A
/// byte-preserving filesystem then holds two files; a normalizing one holds
/// one, because the second write landed on the first. **Neither outcome is
/// asserted** — the platform decides, and that is exactly the point: a
/// determinism contract stated over what a walk reports would inherit that
/// decision, so this crate's contract is stated over what it emits instead.
///
/// What is asserted is that the phenomenon is real here: the walk reports
/// names, and reading a name back gives whichever body last landed under it.
#[test]
fn a_name_written_in_one_normalization_form_is_the_filesystems_business() {
    let dir = scratch::fresh("normalization-forms");
    fs::create_dir_all(&dir).expect("creating a scratch directory");
    fs::write(dir.join(NFD_NAME), NFD_BODY).expect("writing the decomposed spelling");
    fs::write(dir.join(NFC_NAME), NFC_BODY).expect("writing the composed spelling");

    let names: Vec<String> = fs::read_dir(&dir)
        .expect("reading the scratch directory")
        .map(|entry| {
            entry
                .expect("reading a directory entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();

    assert!(
        names.len() == 1 || names.len() == 2,
        "expected the filesystem to hold one or two spellings, found {names:?}"
    );
    for name in &names {
        assert!(
            name.starts_with("caf"),
            "an unrelated entry appeared: {name}"
        );
        let bytes = fs::read(dir.join(name)).expect("reading back a written name");
        assert!(
            bytes == NFD_BODY || bytes == NFC_BODY,
            "a written name came back holding something else"
        );
    }
    if names.len() == 1 {
        // A normalizing filesystem: the two spellings are one file, and the
        // second write won. This is the case that would break a walk-keyed
        // digest across machines.
        assert_eq!(
            fs::read(dir.join(NFD_NAME)).expect("reading the surviving name"),
            NFC_BODY
        );
    }

    fs::remove_dir_all(&dir).expect("removing a scratch tree");
}
