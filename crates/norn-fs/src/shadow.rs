//! Shadows: where a write's bytes wait, what they are called, and why a
//! leaked one is harmless.
//!
//! A **shadow** is a staged, unpublished copy of one document write. It is
//! never a vault document, it is never surfaced by any reading surface, and one
//! that outlives its write attempt is inert.
//!
//! # Shadows live outside the vault
//!
//! The vault tree belongs to every consumer — snapshot automation, sync
//! clients, editors — and a mechanism file placed among documents gets
//! committed, synced and indexed by tools that cannot know to ignore it. So a
//! shadow is never a sibling of its destination. Its home is decided **per
//! vault** by [`ShadowHome::resolve`], because the atomic publish is a rename
//! and a rename cannot cross filesystems: a fixed global temporary location is
//! structurally impossible, not merely inconvenient.
//!
//! Two placements, and the device comparison chooses between them:
//!
//! - [`Placement::DataRoot`] — the norn data directory's per-vault temporary
//!   directory, which is where a shadow belongs: outside the vault entirely.
//! - [`Placement::VaultFallback`] — [`FALLBACK`] under the vault root, used
//!   only when the data directory sits on a different filesystem. It is a
//!   single dot-directory outside the vault-document boundary, excluded from
//!   the walk wholesale.
//!
//! The choice is a fact about a vault and is recorded as one by whoever
//! resolves it. A rename that reports `EXDEV` later is an
//! [environmental refusal](crate::Refusal::Environment) — a filesystem
//! arrangement that changed under a decision already taken, which is a
//! re-evaluation rather than a bad write.
//!
//! A shadow's invisibility means it is never surfaced as a document. It is not
//! confidentiality, which Norn does not claim: a shadow is as readable as the
//! vault it is about to join.
//!
//! # A name is unique per attempt — this is a contract clause
//!
//! A shadow's name carries **this process's identifier and a counter that only
//! ever increases within it**, and it is never derived from the destination's
//! own name. No write ever picks a name a previous write in the same process
//! was handed, and the shadow is opened with `create_new`, so a name that is
//! somehow already taken refuses the write rather than truncating whatever is
//! there.
//!
//! That clause is what makes a leaked shadow inert, and it is why cleanup is
//! allowed to fail quietly. A deterministic, stem-derived name is the forbidden
//! shape: a later write to the same destination would compute that exact name
//! and open it with truncating semantics, and a leaked shadow that had become a
//! second link to the live document's inode would take the truncation with it —
//! destroying the document before the write got as far as noticing the
//! destination already existed.
//!
//! # A leaked shadow is bounded by a sweep, in two tiers
//!
//! Nothing reopens a shadow, so a leaked one costs bytes and nothing else. Two
//! sweeps bound that cost, and both belong to the host that owns a vault's
//! lifecycle rather than to this kernel:
//!
//! - **At attach, under a freshly taken maintainer lock, the sweep is total.**
//!   Holding the lock means no other Norn host is writing this vault, so every
//!   shadow in the home is the residue of a write that is already over.
//! - **In life, on the host's maintenance schedule, the sweep is bounded by
//!   [`SHADOW_AGE_THRESHOLD`].** A live write's shadow exists for as long as it
//!   takes to write and fsync one document, so an age threshold far above that
//!   separates residue from work in flight without ever needing to ask which
//!   process a name belongs to.
//!
//! **No sweep ever breaks a lock, and no lock has a timeout.** The kernel
//! releases a `flock` when the holding process dies, so a lock still held is a
//! process still alive. Taking a lock away on age grounds is what manufactures
//! the dead-inode hazard [`crate::lock`] defends against; a wedged live holder
//! is a health finding, never something to steal from.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use crate::identity::identity_of;
use crate::refusal::{Refusal, environment};

/// What every shadow name begins with, and therefore the first thing
/// [`is_shadow_name`] asks about.
pub const SHADOW_PREFIX: &str = "norn-shadow-";

/// Where shadows go when the norn data directory is on another filesystem,
/// relative to the vault root.
///
/// One dot-directory rather than a temporary file among documents: the walk
/// excludes it wholesale, so nothing inside it is ever a candidate for being a
/// vault document.
pub const FALLBACK: &str = ".norn/tmp";

/// How old a shadow must be before an in-life sweep removes it.
///
/// A shadow exists for the length of one document's write and fsync. Ten
/// minutes is three orders of magnitude above that, which is the point: the
/// threshold is not a guess at how long a write takes, it is a margin wide
/// enough that no live write is ever inside it. Sweeping is bounded by age
/// rather than by asking which process owns a name, because a name's process
/// identifier is reused across boots and is not evidence of anything.
pub const SHADOW_AGE_THRESHOLD: Duration = Duration::from_secs(600);

/// Distinguishes two shadows staged by the same process.
static SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Where a vault's shadows are staged, and which of the two placements that
/// is.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ShadowHome {
    directory: PathBuf,
    placement: Placement,
}

/// Which of the two homes a vault's shadows landed in.
///
/// A typed fact about a vault rather than a detail of one write: it is decided
/// once, by a device comparison, and recorded by whoever resolved it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Placement {
    /// The norn data directory's per-vault temporary directory: outside the
    /// vault entirely, which is where a shadow belongs.
    DataRoot,
    /// [`FALLBACK`] under the vault root, because the data directory is on a
    /// different filesystem and a rename cannot cross one.
    VaultFallback,
}

impl ShadowHome {
    /// Decide where `vault_root`'s shadows go, given the data directory's
    /// per-vault temporary directory.
    ///
    /// `data_tmp` is created if it is not there — it is norn's own directory,
    /// and its device is what the decision reads. Where the two devices agree
    /// the answer is `data_tmp`; where they do not, [`FALLBACK`] under the vault
    /// root is created and returned instead.
    ///
    /// **The comparison is against the vault root**, so a separate mount
    /// *inside* a vault is not what this decides about. A swap into one reports
    /// `EXDEV`, which is an environmental refusal naming the path: the decision
    /// is made again rather than guessed at, because the alternative — falling
    /// back to copy-then-unlink — publishes bytes without a rename and gives up
    /// atomicity to avoid an error message.
    pub fn resolve(vault_root: &Path, data_tmp: &Path) -> Result<ShadowHome, Refusal> {
        let vault = device_of(vault_root)?;
        make_directory(data_tmp)?;
        let data = device_of(data_tmp)?;
        Self::resolve_where(vault_root, data_tmp, vault == data)
    }

    /// [`ShadowHome::resolve`], with the device comparison's answer passed in
    /// rather than read off the filesystem.
    ///
    /// The parameter is what makes the fallback a checkable claim on a machine
    /// with one filesystem: the case that matters is the one where the two
    /// devices differ, and a test that cannot arrange two mounts has no other
    /// way to reach it.
    fn resolve_where(
        vault_root: &Path,
        data_tmp: &Path,
        same_device: bool,
    ) -> Result<ShadowHome, Refusal> {
        if same_device {
            make_directory(data_tmp)?;
            return Ok(ShadowHome {
                directory: data_tmp.to_path_buf(),
                placement: Placement::DataRoot,
            });
        }
        let fallback = vault_root.join(FALLBACK);
        make_directory(&fallback)?;
        Ok(ShadowHome {
            directory: fallback,
            placement: Placement::VaultFallback,
        })
    }

    /// The directory shadows are staged in.
    pub fn directory(&self) -> &Path {
        &self.directory
    }

    /// Which of the two placements this is.
    pub fn placement(&self) -> Placement {
        self.placement
    }

    /// The path of a shadow nothing has taken yet.
    ///
    /// Each call yields a name no previous call in this process yielded. The
    /// destination is not a parameter, because a name derived from it is the
    /// forbidden shape this module's contract clause forbids.
    pub(crate) fn next_shadow(&self) -> PathBuf {
        self.directory.join(shadow_name())
    }
}

/// A shadow name nothing in this process has been handed before.
fn shadow_name() -> String {
    format!(
        "{SHADOW_PREFIX}{}-{}",
        std::process::id(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed)
    )
}

/// Whether `name` is a shadow this crate staged.
///
/// **This is how a shadow is recognized — not a leading dot.** The walk, the
/// event-suppression path and the sweep all ask this one question, so a shadow
/// is invisible as a document without any surface having to hide every hidden
/// file: a user's genuinely dot-prefixed document stays a document.
///
/// The shape is exact: the prefix, a process identifier, a hyphen, and a
/// counter, with nothing on either end. A name that merely starts the same way
/// is not one of ours, and treating it as one would have a sweep remove a file
/// somebody else put there.
///
/// ```
/// use std::ffi::OsStr;
///
/// use norn_fs::is_shadow_name;
///
/// assert!(is_shadow_name(OsStr::new("norn-shadow-4821-0")));
/// assert!(!is_shadow_name(OsStr::new("norn-shadow-4821-0.md")));
/// assert!(!is_shadow_name(OsStr::new("norn-shadow-notes")));
/// ```
pub fn is_shadow_name(name: &OsStr) -> bool {
    let Some(name) = name.to_str() else {
        return false;
    };
    let Some(rest) = name.strip_prefix(SHADOW_PREFIX) else {
        return false;
    };
    let Some((pid, sequence)) = rest.split_once('-') else {
        return false;
    };
    is_number(pid) && is_number(sequence)
}

/// Whether `text` is one or more ASCII digits and nothing else.
fn is_number(text: &str) -> bool {
    !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit())
}

/// Create `directory` and every directory on the way to it.
#[allow(clippy::disallowed_methods)] // The vault filesystem seam: this crate owns the shadow home.
fn make_directory(directory: &Path) -> Result<(), Refusal> {
    std::fs::create_dir_all(directory).map_err(|error| environment("creating", directory, &error))
}

/// Which device `path` sits on.
#[allow(clippy::disallowed_methods)] // The vault filesystem seam: this crate owns vault stat.
fn device_of(path: &Path) -> Result<u64, Refusal> {
    std::fs::metadata(path)
        .map(|metadata| identity_of(&metadata).dev)
        .map_err(|error| environment("reading the device of", path, &error))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scratch directory for the length of one test.
    struct Scratch(PathBuf);

    impl Scratch {
        #[allow(clippy::disallowed_methods)] // Harness scaffolding: the tree this test works over.
        fn new(label: &str) -> Scratch {
            let root = std::env::temp_dir().join(format!(
                "norn-fs-shadow-{label}-{}-{}",
                std::process::id(),
                SEQUENCE.fetch_add(1, Ordering::Relaxed)
            ));
            let _ = std::fs::remove_dir_all(&root);
            std::fs::create_dir_all(&root).expect("a scratch directory");
            Scratch(root)
        }

        fn path(&self, relative: &str) -> PathBuf {
            self.0.join(relative)
        }
    }

    impl Drop for Scratch {
        #[allow(clippy::disallowed_methods)] // Harness scaffolding: removing the tree this test made.
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// **The bar on unique-per-attempt naming.** Two shadows taken in one
    /// process never share a name, and neither name mentions a destination.
    ///
    /// A stem-derived name is the forbidden shape. Under it two writes to one
    /// document compute the same shadow path, the second opens it with
    /// truncating semantics, and a leaked shadow aliased onto the live inode
    /// takes that truncation into the document.
    #[test]
    fn two_shadows_never_share_a_name() {
        let home = ShadowHome {
            directory: PathBuf::from("/data/vaults/notes/tmp"),
            placement: Placement::DataRoot,
        };
        let mut seen = std::collections::BTreeSet::new();
        for _ in 0..1_000 {
            assert!(
                seen.insert(home.next_shadow()),
                "a shadow name was handed out twice"
            );
        }
        for name in &seen {
            let name = name.file_name().expect("a file name").to_string_lossy();
            assert!(
                is_shadow_name(OsStr::new(name.as_ref())),
                "{name} is not recognizable as a shadow"
            );
        }
    }

    /// A shadow name is a function of the process and the counter, and of
    /// nothing else. Two destinations get two names, which is what says the
    /// destination is not an input.
    #[test]
    fn a_shadow_name_does_not_come_from_its_destination() {
        let home = ShadowHome {
            directory: PathBuf::from("/data/vaults/notes/tmp"),
            placement: Placement::DataRoot,
        };
        let first = home.next_shadow();
        let second = home.next_shadow();
        assert_ne!(first, second);
        for taken in [&first, &second] {
            let name = taken.file_name().expect("a file name").to_string_lossy();
            assert!(
                !name.contains("glossary") && !name.contains(".md"),
                "{name} carries something from a destination"
            );
        }
    }

    /// **The bar on the predicate.** Every name this crate hands out is
    /// recognized, and every adversarial near-miss is not.
    ///
    /// The forbidden shape is a prefix test. Under it a sweep removes
    /// `norn-shadow-notes.md`, which is a document somebody wrote, and a walk
    /// hides it.
    #[test]
    fn the_predicate_admits_our_names_and_refuses_near_misses() {
        for ours in ["norn-shadow-1-0", "norn-shadow-4821-93", "norn-shadow-0-0"] {
            assert!(
                is_shadow_name(OsStr::new(ours)),
                "{ours} was not recognized"
            );
        }
        for theirs in [
            "",
            "norn-shadow",
            "norn-shadow-",
            "norn-shadow-1",
            "norn-shadow-1-",
            "norn-shadow--1",
            "norn-shadow-1-2-3",
            "norn-shadow-1-2.md",
            "norn-shadow-abc-1",
            "norn-shadow-1-abc",
            "norn-shadow-notes.md",
            "norn-shadow-1-0 ",
            " norn-shadow-1-0",
            "xnorn-shadow-1-0",
            "Norn-Shadow-1-0",
            ".norn-shadow-1-0",
            "norn-shadow-1-0.tmp",
            "norn-shadow-+1-0",
            "norn-shadow-1_0",
        ] {
            assert!(
                !is_shadow_name(OsStr::new(theirs)),
                "{theirs:?} was taken for a shadow"
            );
        }
    }

    /// A name that is not text is not one of ours: every name this crate hands
    /// out is ASCII.
    #[test]
    fn a_name_that_is_not_text_is_not_a_shadow() {
        use std::os::unix::ffi::OsStrExt;
        assert!(!is_shadow_name(OsStr::from_bytes(b"norn-shadow-1-\xff")));
    }

    /// One filesystem puts shadows outside the vault, which is where they
    /// belong.
    #[test]
    fn one_filesystem_stages_shadows_outside_the_vault() {
        let scratch = Scratch::new("one-device");
        let vault = scratch.path("vault");
        make_directory(&vault).expect("a vault root");
        let data_tmp = scratch.path("data/vaults/notes/tmp");

        let home = ShadowHome::resolve(&vault, &data_tmp).expect("a shadow home");
        assert_eq!(home.placement(), Placement::DataRoot);
        assert_eq!(home.directory(), data_tmp);
        assert!(
            !home.directory().starts_with(&vault),
            "a shadow home inside the vault on one filesystem"
        );
    }

    /// **The bar on the fallback.** Two filesystems put shadows under the
    /// vault's own dot-directory, because a rename cannot cross a filesystem.
    ///
    /// The forbidden shape is staging in the data directory regardless: every
    /// swap would then fail `EXDEV`, and a kernel that answered by copying
    /// instead would publish bytes without a rename.
    #[test]
    fn two_filesystems_fall_back_inside_the_vault() {
        let scratch = Scratch::new("two-devices");
        let vault = scratch.path("vault");
        make_directory(&vault).expect("a vault root");
        let data_tmp = scratch.path("data/vaults/notes/tmp");

        let home = ShadowHome::resolve_where(&vault, &data_tmp, false).expect("a shadow home");
        assert_eq!(home.placement(), Placement::VaultFallback);
        assert_eq!(home.directory(), vault.join(FALLBACK));
        #[allow(clippy::disallowed_methods)] // Asserting on what the resolution created.
        let made = std::fs::metadata(home.directory()).is_ok();
        assert!(made, "the fallback directory was not created");
    }

    /// The fallback is one dot-directory, so a walk excluding it excludes every
    /// shadow in it. A path with an undotted segment would need the walk to
    /// know a second rule.
    #[test]
    fn the_fallback_sits_under_a_single_dot_directory() {
        let mut segments = Path::new(FALLBACK).components();
        let first = segments.next().expect("a first segment");
        assert!(
            first.as_os_str().to_string_lossy().starts_with('.'),
            "{FALLBACK} does not begin with a dot-directory"
        );
        assert!(
            segments.next().is_some(),
            "{FALLBACK} is the dot-directory itself, which holds more than shadows"
        );
    }
}
