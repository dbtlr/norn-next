//! The determinism gate: the same `(profile, seed)` produces the same emitted
//! tree, byte for byte — the paths the generator writes and the bytes it
//! writes at them. What a filesystem reports those paths to be called is
//! outside the contract, and the case at the end of this file is where that
//! boundary is exercised.
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

use norn_fixtures::{Manifest, Profile, Symlinks, digest, generate};

use scratch::generate_and_digest;

/// The profiles whose determinism is asserted on every pull request.
const PER_PR_PROFILES: &[&str] = &["tiny", "small", "ambiguous", "realistic"];

/// The contract digest each named `(profile, seed)` produces, pinned.
///
/// Two generations agreeing catches a generator that is unstable inside one
/// run. It cannot catch one that moved every tree consistently, and this table
/// is what does: a value here is **read from a reviewed diff, never derived**,
/// so a revision that changes what a pair produces arrives as an edit somebody
/// made and a reviewer read rather than as a silent change of subject. The
/// identity it holds is scoped to a build, per [ADR 0002][identity] — the
/// table says what this generator produces today, not what any build of it
/// must produce.
///
/// [identity]: https://github.com/dbtlr/norn/blob/main/docs/decisions/0002-fixture-determinism-and-calibration.md
const PINNED_TREE_DIGESTS: &[(&str, u64, &str)] = &[
    (
        "tiny",
        7,
        "9a2a61028bdf73140c6607a6961c332b537d87b63f1e6ad75f33efdaf8c80d1b",
    ),
    (
        "small",
        7,
        "77c985a7fb3530da37a3b8193b022d6a50a70346b6a14c0777c281a3aa99d818",
    ),
    (
        "ambiguous",
        7,
        "dec7aa1017d9067a308386f82a374fb32aecfe48652198b1c359dd8a23d1f3e5",
    ),
    (
        "realistic",
        7,
        "de5bbcdc1a3f2fcc7dcaba4dcaad1fb7a65d01b606242d4767ba47017d8c2398",
    ),
    (
        "soak",
        3,
        "3988ec54df577dbb0b824afd0a2fb8701b659ccc9bc5e8037533787872a53197",
    ),
];

fn profile(name: &str) -> Profile {
    Profile::by_name(name).unwrap_or_else(|| panic!("no profile named `{name}`"))
}

/// The pinned digest for one pair. A pair with no entry is an error rather
/// than a skip: a case that asks for a pin and quietly compares nothing is the
/// failure the table exists to prevent.
fn pinned(name: &str, seed: u64) -> &'static str {
    PINNED_TREE_DIGESTS
        .iter()
        .find(|(profile, at, _)| *profile == name && *at == seed)
        .map(|(_, _, hex)| *hex)
        .unwrap_or_else(|| panic!("no pinned digest for `{name}` at seed {seed}"))
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

/// Each per-PR pair produces the tree the pinned table says it produces.
///
/// The failure this catches is the one two agreeing generations cannot: a
/// generator change that moves every tree consistently. Updating the constant
/// is the act that turns such a change from an accident into a decision
/// somebody made and a reviewer read.
#[test]
fn every_per_pr_profile_produces_its_pinned_tree() {
    for name in PER_PR_PROFILES {
        let (tree, _) = generate_and_digest(&format!("pin-{name}"), &profile(name), 7);
        assert_eq!(
            digest::hex(&tree),
            pinned(name, 7),
            "`{name}` at seed 7 no longer produces its pinned tree"
        );
    }
}

#[test]
#[ignore = "soak-lane case: the >=5k profile is nightly work, not per-PR"]
fn the_soak_profile_reproduces_its_tree() {
    let profile = profile("soak");
    let (first, _) = generate_and_digest("det-soak-a", &profile, 3);
    let (second, _) = generate_and_digest("det-soak-b", &profile, 3);
    assert_eq!(
        digest::hex(&first),
        digest::hex(&second),
        "`soak` at seed 3 produced two different trees"
    );
    assert_eq!(
        digest::hex(&first),
        pinned("soak", 3),
        "`soak` at seed 3 no longer produces its pinned tree"
    );
}

/// The symbolic links are inside the contract digest, and each setting of the
/// knob reproduces itself.
///
/// Turning the knob off changes nothing else: the links are planned after
/// every document and clutter file is drawn, and a knob at zero draws nothing,
/// so the two trees hold the same directories and the same bytes at the same
/// paths and differ only by the links. A digest that skipped the links would
/// therefore report the two trees as one, and a generation could lose every
/// link it was asked for without moving a single hash.
#[test]
fn the_symlink_knob_is_inside_the_contract_digest() {
    let with_links = profile("small");
    assert!(
        with_links.symlinks.total() > 0,
        "the sample profile asks for no link, so this case proves nothing"
    );
    let mut without_links = with_links;
    without_links.symlinks = Symlinks::NONE;

    let (on_first, on) = generate_and_digest("knob-symlinks-on-a", &with_links, 13);
    let (on_second, _) = generate_and_digest("knob-symlinks-on-b", &with_links, 13);
    let (off_first, off) = generate_and_digest("knob-symlinks-off-a", &without_links, 13);
    let (off_second, _) = generate_and_digest("knob-symlinks-off-b", &without_links, 13);

    assert_eq!(
        digest::hex(&on_first),
        digest::hex(&on_second),
        "the knob on produced two different trees at one seed"
    );
    assert_eq!(
        digest::hex(&off_first),
        digest::hex(&off_second),
        "the knob off produced two different trees at one seed"
    );
    assert_ne!(
        digest::hex(&on_first),
        digest::hex(&off_first),
        "the same tree with and without its symbolic links shares a contract \
         digest, so the digest cannot see them"
    );
    assert_eq!(on.symlinks, with_links.symlinks);
    assert_eq!(off.symlinks, Symlinks::NONE);
    assert_eq!(
        (on.documents, on.files, on.markdown_bytes),
        (off.documents, off.files, off.markdown_bytes),
        "turning the knob moved something other than the links, so the two \
         trees differ for a second reason"
    );
}

/// A symbolic link contributes the target it names, and the walk digest never
/// reads through one.
///
/// Three trees hold the same one document. Two add a link to it under one
/// name, spelled two ways that resolve to the same file with the same bytes;
/// the third puts a regular file of that name holding the target string as its
/// contents. A digest that followed the links would report the first two as
/// one tree, and a digest that dropped the node kind would report the third as
/// one of them.
#[cfg(unix)]
#[test]
#[allow(clippy::disallowed_methods)] // The case builds the three trees it digests.
fn the_walk_digest_reads_a_links_target_and_never_through_it() {
    const DOCUMENT: &[u8] = b"---\ntitle: Target\n---\n";

    let plain = link_case("walk-digest-plain", |dir| {
        fs::write(dir.join("alias.md"), b"doc.md").expect("writing a regular file")
    });
    let direct = link_case("walk-digest-direct", |dir| {
        std::os::unix::fs::symlink("doc.md", dir.join("alias.md")).expect("creating a link")
    });
    let dotted = link_case("walk-digest-dotted", |dir| {
        std::os::unix::fs::symlink("./doc.md", dir.join("alias.md")).expect("creating a link")
    });

    assert_ne!(
        direct, dotted,
        "two spellings of one target share a digest, so the target is outside it"
    );
    assert_ne!(
        direct, plain,
        "a link and a regular file holding the same bytes share a digest, so \
         the node kind is outside it"
    );

    // Nothing is at the far end of this one, so a digest that read through a
    // link could not produce a value at all.
    let dangling = link_case("walk-digest-dangling", |dir| {
        std::os::unix::fs::symlink("removed-000.md", dir.join("alias.md")).expect("creating a link")
    });
    assert_ne!(dangling, direct);

    /// Build a tree holding one document plus whatever `add` puts in it, and
    /// return its walk digest.
    #[allow(clippy::disallowed_methods)] // Builds and removes its own scratch tree.
    fn link_case(label: &str, add: impl FnOnce(&Path)) -> String {
        let dir = scratch::fresh(label);
        fs::create_dir_all(&dir).expect("creating a scratch directory");
        fs::write(dir.join("doc.md"), DOCUMENT).expect("writing the document");
        add(&dir);
        let digest = digest::tree_hex(&dir).expect("digesting a scratch tree");
        fs::remove_dir_all(&dir).expect("removing a scratch tree");
        digest
    }
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

/// Renaming a generated file to the other spelling of its own name moves the
/// walk-based digest and cannot move the contract digest.
///
/// This is the whole hazard on one axis. Nothing is added, removed or edited:
/// one file keeps its bytes and changes only how its name is spelled, from a
/// precomposed character to the combining sequence that means the same thing.
/// A digest keyed on what `readdir` reports has to move; a digest keyed on the
/// strings the generator emitted has nothing to move it, and the contract is
/// stated over the second.
///
/// Whether the rename changes what the directory reports is the filesystem's
/// decision — one that stores names in a fixed normalization form will have
/// stored the same spelling before and after — so the walk assertion runs only
/// when the directory actually reports something new. The contract assertion
/// runs either way, because it is the one that must not depend on the answer.
#[test]
#[allow(clippy::disallowed_methods)] // The case renames and re-reads the tree it generated.
fn respelling_a_name_moves_the_walk_digest_and_not_the_contract_digest() {
    let profile = profile("small");
    let dir = scratch::fresh("respelling");
    let manifest = generate(&profile, 5, &dir).expect("generating a scratch tree");

    // Not every non-ASCII name has a second spelling this test can produce —
    // the decomposition table covers the Latin precomposed characters only —
    // so the target is the first document whose name it actually changes.
    let (target, respelled) = non_ascii_documents(&dir)
        .into_iter()
        .find_map(|path| {
            let name = path.file_name()?.to_string_lossy().into_owned();
            let other = decompose(&name);
            if other == name {
                return None;
            }
            let respelled = path.parent()?.join(other);
            Some((path, respelled))
        })
        .expect("the profile emitted no name the decomposition table can respell");

    let contents = fs::read(&target).expect("reading the document about to be renamed");
    let walked_before = digest::tree(&dir).expect("digesting a scratch tree");
    let names_before = reported_names(&dir);

    fs::rename(&target, &respelled).expect("renaming to the other spelling");

    let names_after = reported_names(&dir);
    let walked_after = digest::tree(&dir).expect("digesting a scratch tree");
    let fresh = generate(&profile, 5, &scratch::fresh("respelling-again"))
        .expect("generating a scratch tree");

    assert_eq!(
        fs::read(&respelled).expect("reading the renamed document"),
        contents,
        "the rename changed the file's bytes, so this case is measuring the wrong thing"
    );
    assert_eq!(
        digest::hex(&manifest.tree_digest),
        digest::hex(&fresh.tree_digest),
        "the contract digest disagreed with an untouched generation of the same pair"
    );
    if names_before == names_after {
        // The filesystem folded both spellings onto one stored name, so the
        // walk has nothing to notice. The contract assertion above is the one
        // that mattered, and it held.
        eprintln!("this filesystem reports one spelling for both forms; walk check skipped");
    } else {
        assert_ne!(
            digest::hex(&walked_before),
            digest::hex(&walked_after),
            "the directory reports a different name and the walk digest did not move"
        );
    }

    fs::remove_dir_all(&dir).expect("removing a scratch tree");
    fs::remove_dir_all(PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("respelling-again"))
        .expect("removing a scratch tree");
}

/// The decomposed spelling of the precomposed characters this crate's name
/// pool emits. Deliberately a table of exactly those characters rather than a
/// general normalizer: the crate holds no Unicode tables, and the test needs
/// only the letters it actually writes.
fn decompose(name: &str) -> String {
    name.chars()
        .flat_map(|c| match c {
            '\u{fc}' => vec!['u', '\u{308}'],
            '\u{e9}' => vec!['e', '\u{301}'],
            '\u{ef}' => vec!['i', '\u{308}'],
            '\u{f1}' => vec!['n', '\u{303}'],
            '\u{f3}' => vec!['o', '\u{301}'],
            other => vec![other],
        })
        .collect()
}

/// Every name the directory tree reports, sorted.
fn reported_names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = scratch::walk(dir)
        .iter()
        .map(|entry| entry.name().to_string())
        .collect();
    names.sort();
    names
}

/// Generated documents whose file name is not pure ASCII.
fn non_ascii_documents(dir: &Path) -> Vec<PathBuf> {
    scratch::documents(dir)
        .into_iter()
        .filter(|entry| !entry.name().is_ascii())
        .map(|entry| entry.path)
        .collect()
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
/// Both spellings of one name are written into an empty directory. Whether the
/// directory then holds one file or two depends on whether the filesystem
/// treats the two spellings as one name — and that is a separate question from
/// whether it stores the bytes it was given, which is why the two cannot be
/// read off each other. **Neither outcome is asserted** here: the platform
/// decides, and that is exactly the point. A determinism contract stated over
/// what a walk reports would inherit that decision, so this crate's contract
/// is stated over what it emits instead.
///
/// What is asserted is that the phenomenon is real here: the walk reports
/// names, and reading a name back gives whichever body last landed under it.
#[test]
#[allow(clippy::disallowed_methods)] // The case writes both spellings itself and reads them back.
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
        // The filesystem treated the two spellings as one name, so the second
        // write landed on the first. Which spelling it stored is its own
        // business and is not asserted — only that one write won. This is the
        // case that would break a walk-keyed digest across machines.
        assert_eq!(
            fs::read(dir.join(NFD_NAME)).expect("reading the surviving name"),
            NFC_BODY
        );
    }

    fs::remove_dir_all(&dir).expect("removing a scratch tree");
}
