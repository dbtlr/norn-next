//! Symbolic links: the four species a generated tree carries, where each one
//! sits, and what it names.
//!
//! One module defines a species for both directions — [`plan`] emits them and
//! [`classify`] reads them back off a tree — so what the generator claims it
//! wrote and what a walk finds are the same vocabulary rather than two
//! descriptions that can drift apart.
//!
//! # Targets are relative, and never leave the generator's own tree
//!
//! Every target is a forward-slash relative path spelled from the directory
//! holding the link, so a target is a byte string that is a function of
//! `(profile, seed)` alone: an absolute target would put the output directory —
//! a caller's argument, different on every machine — inside the tree, and the
//! determinism contract would stop holding across machines. An outbound link
//! names a location above the vault root without anything being created there:
//! where the generator writes is inside its own root, and what a link *names*
//! is a string.
//!
//! # Following a link is never required, and no walk of the tree can loop
//!
//! A walk that descends every directory and follows every directory link
//! reaches each link at most once. [`plan`] places a directory link only where
//! the links planned before it cannot lead back to the directory holding it,
//! and puts it at the vault root otherwise — the root is reachable through no
//! link, because every target is a subdirectory. Two links naming each other's
//! subtrees are the case a per-link check misses: each one sits outside the
//! directory it names and the pair still makes a path of unbounded length.
//!
//! Nothing here needs a reader to follow anything — the probe classifies by
//! one stat of the target — but a tree carrying a traversal cycle is a trap
//! for every consumer that does: the kernel answers `ELOOP` after about thirty
//! hops, and the walk fails on a tree it was handed as well-formed.

use std::io;
use std::path::Path;

use crate::layout::Placed;
use crate::profile::Symlinks;
use crate::rng::Rng;
use crate::words::{CLUTTER_STEMS, DOC_WORDS, SUB_DIRS};

/// Whether this platform can create a symbolic link.
pub const SUPPORTED: bool = cfg!(unix);

/// What a symbolic link points at.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Species {
    /// A second name for a file inside the vault.
    InVaultFile,
    /// A second name for a directory inside the vault.
    InVaultDir,
    /// A target inside the vault that is not there.
    Dangling,
    /// A target above the vault root.
    Outbound,
}

/// One planned symbolic link: where it sits, what it names, and which species
/// that makes it.
pub struct PlannedLink {
    /// Vault-relative path, forward-slash.
    pub path: String,
    /// The target as written into the link: relative to the link's own
    /// directory.
    pub target: String,
    pub species: Species,
}

/// The refusal a generation earns when its profile asks for symbolic links and
/// the platform cannot create one.
///
/// `None` when nothing is asked for, or when links can be created here.
/// Answering `None` in the remaining case would emit a tree missing everything
/// the knob asked for while reporting success — the same profile and seed
/// naming two different trees, with the platform as the hidden third input.
pub fn platform_refusal(profile: &str, requested: usize, supported: bool) -> Option<io::Error> {
    (requested > 0 && !supported).then(|| {
        io::Error::new(
            io::ErrorKind::Unsupported,
            format!(
                "profile `{profile}` asks for {requested} symbolic links and this platform \
                 cannot create one; refusing rather than emitting a tree without them"
            ),
        )
    })
}

/// The refusal a generation earns when the filesystem under `dir` cannot carry
/// the symbolic links its profile asks for.
pub fn filesystem_refusal(profile: &str, requested: usize, dir: &Path) -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        format!(
            "profile `{profile}` asks for {requested} symbolic links and the filesystem at {} \
             cannot create one; refusing rather than emitting a tree without them",
            dir.display()
        ),
    )
}

/// Name of the link [`creatable_in`] makes and removes. Dot-prefixed, so a
/// walk that skips hidden entries would not see it even if a removal failed.
const PROBE_LINK: &str = ".norn-fixtures-symlink-probe";

/// Whether a symbolic link can be created inside `dir`.
///
/// [`SUPPORTED`] is a property of the build; this is a property of the
/// filesystem the directory sits on, and the two disagree. A build that can
/// call `symlink(2)` still meets filesystems that answer it with an error —
/// exFAT, FAT32, and some network and overlay mounts — and trying is the only
/// way to find out.
///
/// A removal that fails is returned as an error rather than as `false`: a
/// stray entry left in the target directory is a different problem from a
/// filesystem that carries no links, and reporting it as the second would
/// leave the first behind unmentioned.
#[allow(clippy::disallowed_methods)] // Makes and removes the one link the answer is read from.
pub fn creatable_in(dir: &Path) -> io::Result<bool> {
    let probe = dir.join(PROBE_LINK);
    match create("probe-target", &probe) {
        Ok(()) => {
            std::fs::remove_file(&probe)?;
            Ok(true)
        }
        Err(_) => Ok(false),
    }
}

/// Create the symbolic link `at`, naming `target`.
#[cfg(unix)]
#[allow(clippy::disallowed_methods)] // The generator's own writer: it creates the links it emits.
pub fn create(target: &str, at: &Path) -> io::Result<()> {
    std::os::unix::fs::symlink(target, at)
}

/// Create the symbolic link `at`, naming `target`.
///
/// Unreachable in a generation: [`platform_refusal`] fails the run before a
/// byte is written where [`SUPPORTED`] is false.
#[cfg(not(unix))]
pub fn create(target: &str, at: &Path) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        format!(
            "cannot create a symbolic link at {} naming `{target}` on this platform",
            at.display()
        ),
    ))
}

/// Plan every symbolic link `symlinks` asks for, placed across `dirs` and
/// naming `docs`.
///
/// Species are planned in a fixed order and each draws the same values in the
/// same sequence, so the set is a function of the generator's state on entry.
/// A tree with no directory or no document holds no link: there would be
/// nowhere to put one and nothing for it to name.
pub fn plan(
    rng: &mut Rng,
    symlinks: &Symlinks,
    dirs: &[String],
    docs: &[Placed],
) -> Vec<PlannedLink> {
    if dirs.is_empty() || docs.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::with_capacity(symlinks.total());

    for index in 0..symlinks.in_vault_file {
        let dir = rng.pick(dirs).as_str();
        let name = format!("{}-alias-{index:03}.md", rng.pick(DOC_WORDS));
        let document = rng.pick(docs).path.as_str();
        out.push(PlannedLink {
            path: join(dir, &name),
            target: relative(dir, document),
            species: Species::InVaultFile,
        });
    }

    // Where each directory link sits and what it names, for the cycle check
    // the next one is put through. Each link is placed against an acyclic set,
    // so the set stays acyclic.
    let mut dir_links: Vec<(&str, &str)> = Vec::with_capacity(symlinks.in_vault_dir);
    for index in 0..symlinks.in_vault_dir {
        let drawn = rng.pick(dirs).as_str();
        let target = rng.pick(dirs).as_str();
        // The vault root is reachable through no link, because every target is
        // a subdirectory, so placing the link there is the spelling that
        // always breaks the loop.
        let dir = if reentered(drawn, target, &dir_links) {
            ""
        } else {
            drawn
        };
        let name = format!("{}-shortcut-{index:03}", rng.pick(SUB_DIRS));
        out.push(PlannedLink {
            path: join(dir, &name),
            target: relative(dir, target),
            species: Species::InVaultDir,
        });
        dir_links.push((dir, target));
    }

    for index in 0..symlinks.dangling {
        let dir = rng.pick(dirs).as_str();
        let name = format!("{}-missing-{index:03}.md", rng.pick(DOC_WORDS));
        out.push(PlannedLink {
            path: join(dir, &name),
            // A sibling of the link that no profile ever emits: document names
            // carry a four-digit index, so this three-digit one is nothing the
            // generator can also write.
            target: format!("removed-{index:03}.md"),
            species: Species::Dangling,
        });
    }

    for index in 0..symlinks.outbound {
        let dir = rng.pick(dirs).as_str();
        let name = format!("{}-external-{index:03}", rng.pick(CLUTTER_STEMS));
        let stem = rng.pick(CLUTTER_STEMS);
        out.push(PlannedLink {
            path: join(dir, &name),
            // One level above the vault root, from wherever the link sits.
            target: format!(
                "{}../shared-attachments/{stem}-{index:03}.pdf",
                "../".repeat(depth(dir))
            ),
            species: Species::Outbound,
        });
    }

    out
}

/// Which species the link at `link_rel` naming `target` is, in the tree at
/// `root`.
///
/// The target is stat'd through, so a link naming a link that dangles is
/// dangling: what a species says is where following the link ends up.
#[allow(clippy::disallowed_methods)] // Stats the target of the link it is classifying.
pub fn classify(root: &Path, link_rel: &str, target: &str) -> Species {
    let Some(resolved) = resolved_in_vault(link_rel, target) else {
        return Species::Outbound;
    };
    match std::fs::metadata(root.join(resolved)) {
        Ok(metadata) if metadata.is_dir() => Species::InVaultDir,
        Ok(_) => Species::InVaultFile,
        Err(_) => Species::Dangling,
    }
}

/// The vault-relative path `target` names when followed from the link at
/// `link_rel`, or `None` when following it leaves the vault.
///
/// Lexical throughout: an absolute target leaves by definition, and a `..`
/// that runs off the front of the path is the escape. Nothing is stat'd, so a
/// target that escapes is named as such whether or not anything is there.
fn resolved_in_vault(link_rel: &str, target: &str) -> Option<String> {
    if target.starts_with('/') {
        return None;
    }
    let mut stack: Vec<&str> = components(parent_of(link_rel));
    for part in target.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                stack.pop()?;
            }
            other => stack.push(other),
        }
    }
    Some(stack.join("/"))
}

/// The directory holding `rel`, as a vault-relative path. The vault root is
/// the empty string.
fn parent_of(rel: &str) -> &str {
    match rel.rsplit_once('/') {
        Some((parent, _)) => parent,
        None => "",
    }
}

fn components(path: &str) -> Vec<&str> {
    if path.is_empty() {
        Vec::new()
    } else {
        path.split('/').collect()
    }
}

fn depth(dir: &str) -> usize {
    components(dir).len()
}

fn join(dir: &str, name: &str) -> String {
    if dir.is_empty() {
        name.to_string()
    } else {
        format!("{dir}/{name}")
    }
}

/// Whether `path` is `ancestor` or sits underneath it.
fn is_inside(path: &str, ancestor: &str) -> bool {
    ancestor.is_empty() || path == ancestor || path.starts_with(&format!("{ancestor}/"))
}

/// Whether a directory link held in `dir` and naming `target` is one a walk
/// re-enters, given the directory links in `planned` — each a pair of the
/// directory holding it and the directory it names.
///
/// A walk descending `target` reaches every directory beneath it, follows any
/// planned link held in one of those, and repeats. Re-entry is that closure
/// reaching `dir`: the walk arrives back at the directory holding this link,
/// descends it again, and the path it is building has no end. `dir` inside
/// `target` is the one-link case and falls out of the first step.
fn reentered<'a>(dir: &str, target: &'a str, planned: &[(&'a str, &'a str)]) -> bool {
    let mut reached = vec![target];
    let mut next = 0;
    while next < reached.len() {
        let here = reached[next];
        next += 1;
        if is_inside(dir, here) {
            return true;
        }
        for &(held_in, names) in planned {
            if is_inside(held_in, here) && !reached.contains(&names) {
                reached.push(names);
            }
        }
    }
    false
}

/// The spelling of `to` relative to the directory `from_dir`, both
/// vault-relative.
fn relative(from_dir: &str, to: &str) -> String {
    let from = components(from_dir);
    let to = components(to);
    let common = from
        .iter()
        .zip(to.iter())
        .take_while(|(here, there)| here == there)
        .count();
    let mut out = "../".repeat(from.len() - common);
    out.push_str(&to[common..].join("/"));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout;
    use crate::profile::Profile;

    fn sample(symlinks: Symlinks, seed: u64) -> Vec<PlannedLink> {
        let profile = Profile::by_name("small").expect("small profile");
        let dirs = layout::directories(&profile.dirs);
        let mut rng = Rng::new(seed);
        let docs = layout::plan(&mut rng, &profile, &dirs);
        plan(&mut rng, &symlinks, &dirs, &docs)
    }

    const FOUR: Symlinks = Symlinks {
        in_vault_file: 3,
        in_vault_dir: 3,
        dangling: 3,
        outbound: 3,
    };

    #[test]
    fn every_requested_link_is_planned_once() {
        let links = sample(FOUR, 3);
        assert_eq!(links.len(), FOUR.total());
        let mut paths: Vec<&str> = links.iter().map(|l| l.path.as_str()).collect();
        paths.sort_unstable();
        let total = paths.len();
        paths.dedup();
        assert_eq!(paths.len(), total, "a link path repeats");
    }

    #[test]
    fn the_knob_turned_off_plans_nothing() {
        assert!(sample(Symlinks::NONE, 3).is_empty());
    }

    /// The species a link is planned as is the species it reads back as,
    /// judged lexically. The two filesystem-answered species are separated by
    /// [`classify`] against a real tree, in the integration suite.
    #[test]
    fn an_outbound_target_leaves_the_vault_and_the_others_do_not() {
        for link in sample(FOUR, 5) {
            let resolved = resolved_in_vault(&link.path, &link.target);
            match link.species {
                Species::Outbound => assert!(
                    resolved.is_none(),
                    "`{}` -> `{}` was planned outbound and stays inside the vault",
                    link.path,
                    link.target
                ),
                _ => assert!(
                    resolved.is_some(),
                    "`{}` -> `{}` leaves the vault and was not planned outbound",
                    link.path,
                    link.target
                ),
            }
        }
    }

    /// More directory links than any shipped profile carries. A traversal
    /// cycle needs two links naming each other's subtrees, which is rare in
    /// the one to six a profile asks for and common at eight.
    const DENSE_DIR_LINKS: Symlinks = Symlinks {
        in_vault_file: 1,
        in_vault_dir: 8,
        dangling: 1,
        outbound: 1,
    };

    /// Every directory link as the pair (the directory holding it, the
    /// directory it names), both vault-relative.
    fn directory_edges(links: &[PlannedLink]) -> Vec<(String, String)> {
        links
            .iter()
            .filter(|link| link.species == Species::InVaultDir)
            .map(|link| {
                let target = resolved_in_vault(&link.path, &link.target)
                    .expect("an in-vault directory link stays inside the vault");
                (parent_of(&link.path).to_string(), target)
            })
            .collect()
    }

    /// The first directory link a walk re-enters, if any.
    ///
    /// Computed over the whole planned set as a fixed point, where [`plan`]
    /// places each link against the ones before it — so a placement that
    /// reasons wrongly about the set it has already built cannot pass by
    /// reasoning the same way twice.
    fn re_entered(edges: &[(String, String)]) -> Option<&(String, String)> {
        edges.iter().find(|(dir, target)| {
            let mut reached = vec![target.as_str()];
            let mut next = 0;
            while next < reached.len() {
                let here = reached[next];
                next += 1;
                if is_inside(dir, here) {
                    return true;
                }
                for (held_in, names) in edges {
                    if is_inside(held_in, here) && !reached.contains(&names.as_str()) {
                        reached.push(names);
                    }
                }
            }
            false
        })
    }

    /// No seed plans a set of directory links a walk can re-enter.
    ///
    /// The one-link case — a link inside the directory it names — is the
    /// obvious half. The half this is wide for is the mutual pair: one link in
    /// `a` naming `b` and another in `b` naming `a` are each outside the
    /// directory they name, and together they make a path of unbounded length.
    #[test]
    fn no_seed_plans_a_walk_that_loops() {
        for seed in 0..400 {
            let links = sample(DENSE_DIR_LINKS, seed);
            let edges = directory_edges(&links);
            assert!(
                re_entered(&edges).is_none(),
                "seed {seed} planned a directory link a walk re-enters: {:?} among {edges:?}",
                re_entered(&edges)
            );
        }
    }

    #[test]
    fn a_dangling_target_names_a_path_no_profile_emits() {
        for link in sample(FOUR, 9) {
            if link.species == Species::Dangling {
                assert!(link.target.starts_with("removed-"), "{}", link.target);
            }
        }
    }

    #[test]
    fn one_seed_plans_one_set_of_links() {
        let first = sample(FOUR, 21);
        let second = sample(FOUR, 21);
        let spelling = |links: &[PlannedLink]| {
            links
                .iter()
                .map(|l| format!("{} -> {}", l.path, l.target))
                .collect::<Vec<_>>()
        };
        assert_eq!(spelling(&first), spelling(&second));
    }

    #[test]
    fn relative_targets_climb_only_as_far_as_the_common_prefix() {
        assert_eq!(relative("notes", "notes/alpha.md"), "alpha.md");
        assert_eq!(relative("notes/drafts", "notes/alpha.md"), "../alpha.md");
        assert_eq!(relative("", "notes/alpha.md"), "notes/alpha.md");
        assert_eq!(
            relative("a/b/c", "d/e.md"),
            "../../../d/e.md",
            "a target sharing no prefix climbs to the root"
        );
    }

    #[test]
    fn resolution_follows_a_relative_target_back_to_a_vault_path() {
        assert_eq!(
            resolved_in_vault("notes/drafts/link.md", "../alpha.md"),
            Some("notes/alpha.md".to_string())
        );
        assert_eq!(
            resolved_in_vault("link.md", "./notes/alpha.md"),
            Some("notes/alpha.md".to_string())
        );
    }

    #[test]
    fn a_target_that_climbs_past_the_root_or_starts_at_one_leaves_the_vault() {
        assert_eq!(resolved_in_vault("notes/link", "../../outside"), None);
        assert_eq!(resolved_in_vault("link", "../outside"), None);
        assert_eq!(resolved_in_vault("notes/link", "/etc/passwd"), None);
    }

    #[test]
    fn an_outbound_target_escapes_from_however_deep_the_link_sits() {
        for dir in ["", "notes", "notes/drafts", "a/b/c/d"] {
            let target = format!("{}../shared-attachments/x.pdf", "../".repeat(depth(dir)));
            assert_eq!(
                resolved_in_vault(&join(dir, "link"), &target),
                None,
                "a link in `{dir}` did not escape with `{target}`"
            );
        }
    }

    #[test]
    fn a_platform_without_symlinks_refuses_instead_of_emitting_a_short_tree() {
        let refusal = platform_refusal("tiny", 4, false)
            .expect("a platform that cannot create links must refuse");
        assert_eq!(refusal.kind(), io::ErrorKind::Unsupported);
        let message = refusal.to_string();
        assert!(message.contains("tiny"), "{message}");
        assert!(message.contains('4'), "{message}");
    }

    #[test]
    fn a_platform_that_can_create_links_and_a_profile_asking_for_none_both_proceed() {
        assert!(platform_refusal("tiny", 4, true).is_none());
        assert!(platform_refusal("tiny", 0, false).is_none());
    }

    #[test]
    fn a_filesystem_refusal_names_the_profile_the_count_and_the_directory() {
        let refusal = filesystem_refusal("tiny", 4, Path::new("/mnt/exfat/vault"));
        assert_eq!(refusal.kind(), io::ErrorKind::Unsupported);
        let message = refusal.to_string();
        assert!(message.contains("tiny"), "{message}");
        assert!(message.contains('4'), "{message}");
        assert!(message.contains("/mnt/exfat/vault"), "{message}");
    }

    /// The probe answers for the filesystem it is pointed at, and the
    /// directory it answered about is exactly as it found it.
    #[test]
    #[allow(clippy::disallowed_methods)] // Makes and removes the directory it probes.
    fn the_probe_answers_and_leaves_the_directory_as_it_found_it() {
        let dir = std::env::temp_dir().join(format!(
            "norn-fixtures-probe-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("creating a scratch directory");

        assert_eq!(
            creatable_in(&dir).expect("probing a scratch directory"),
            SUPPORTED,
            "the probe disagreed with the build about this platform"
        );
        assert_eq!(
            std::fs::read_dir(&dir)
                .expect("listing a scratch directory")
                .count(),
            0,
            "the probe left an entry behind"
        );

        std::fs::remove_dir_all(&dir).expect("removing a scratch directory");
    }

    /// A directory that is not there carries no link. The probe answers `no`
    /// rather than failing, because the answer it is asked for is whether a
    /// link can be created and the answer is the same either way.
    #[test]
    fn a_probe_of_a_directory_that_is_not_there_answers_no() {
        let absent = Path::new("/norn-fixtures-no-such-directory/inner");
        assert!(!creatable_in(absent).expect("probing an absent directory"));
    }
}
