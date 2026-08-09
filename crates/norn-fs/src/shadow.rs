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
//! shadow is never a sibling of its destination. Its home is decided by
//! [`ShadowHome::resolve`] **per vault root and per
//! [maintainership](MaintainershipKey)**, because the atomic publish is a
//! rename and a rename cannot cross filesystems: a fixed global temporary
//! location is structurally impossible, not merely inconvenient.
//!
//! Two placements, and the device comparison chooses between them:
//!
//! - [`Placement::DataRoot`] — the temporary directory inside the derived
//!   directory the key names, which is where a shadow belongs: outside the
//!   vault entirely.
//! - [`Placement::VaultFallback`] — the key's own directory under [`FALLBACK`]
//!   beneath the vault root, used only when the data directory sits on a
//!   different filesystem. [`FALLBACK`] is a single dot-directory outside the
//!   vault-document boundary, excluded from the walk wholesale, and the key
//!   beneath it is what keeps two maintainerships over one vault root in two
//!   homes.
//!
//! The choice is a fact about a vault root and is recorded as one by whoever
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
//! Nothing reopens a shadow, and a name that is somehow taken is skipped rather
//! than opened, so a leaked one costs bytes and nothing else. Two sweeps bound
//! that cost. [`ShadowHome::sweep`] is the act; *when* it runs belongs to the
//! host that owns a vault's lifecycle rather than to this kernel:
//!
//! - **At attach, under a freshly taken maintainer lock, the sweep is total.**
//!   The home is keyed by the same key the lock is taken under, so holding the
//!   lock means no other Norn host writes through this home and every shadow in
//!   it is the residue of a write that is already over.
//! - **In life, on the host's maintenance schedule, the sweep is bounded by
//!   [`SHADOW_AGE_THRESHOLD`].** A live write's shadow exists for as long as it
//!   takes to write and fsync one document, so an age threshold far above that
//!   separates residue from work in flight without ever needing to ask which
//!   process a name belongs to.
//!
//! The sweep is exported here because it has to be: no other crate may touch
//! vault mechanism files, so no other crate could implement one — and a bound
//! that nothing can enforce is not a bound.
//!
//! **No sweep ever breaks a lock, and no lock has a timeout.** The kernel
//! releases a `flock` when the holding process dies, so a lock still held is a
//! process still alive. Taking a lock away on age grounds is what manufactures
//! the dead-inode hazard [`crate::lock`] defends against; a wedged live holder
//! is a health finding, never something to steal from.

use std::ffi::OsStr;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

use crate::identity::identity_of;
use crate::refusal::{Refusal, environment};

/// What every shadow name begins with, and therefore the first thing
/// [`is_shadow_name`] asks about.
///
/// In-crate: the predicate is the surface, so nothing outside has a reason to
/// know the spelling, and one that matched the prefix itself would take a
/// document called `norn-shadow-notes.md` for residue.
pub(crate) const SHADOW_PREFIX: &str = "norn-shadow-";

/// Where shadow homes go when the norn data directory is on another
/// filesystem, relative to the vault root.
///
/// One dot-directory rather than a temporary file among documents: the walk
/// excludes it wholesale, so nothing inside it is ever a candidate for being a
/// vault document. The homes beneath it are one per
/// [maintainership](MaintainershipKey), so a vault root reached by two of them
/// holds two homes and neither sweeps the other's.
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

/// How many shadow names one write tries before the shadow home is declared
/// unusable.
///
/// A name is unique per attempt, so one taken name is residue and the next name
/// is free — this is the house bound on retrying, not an expectation. Reaching it
/// means this many distinct names are all taken, which is a directory somebody is
/// filling rather than a name that was unlucky.
pub(crate) const NAME_ATTEMPTS: usize = 64;

/// Distinguishes two shadows staged by the same process.
static SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Where one maintainership's shadows are staged, and which of the two
/// placements that is.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ShadowHome {
    directory: PathBuf,
    placement: Placement,
}

/// Which of the two homes a maintainership's shadows landed in.
///
/// A typed fact about a vault root rather than a detail of one write: it is
/// decided once, by a device comparison, and recorded by whoever resolved it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Placement {
    /// The temporary directory inside the derived directory the
    /// [key](MaintainershipKey) names: outside the vault entirely, which is
    /// where a shadow belongs.
    DataRoot,
    /// The key's own directory under [`FALLBACK`] beneath the vault root,
    /// because the data directory is on a different filesystem and a rename
    /// cannot cross one.
    VaultFallback,
}

/// Which maintainership a shadow home serves: the channel a build keeps its
/// state under, and the registered name of the vault whose derived state that
/// maintainership maintains.
///
/// **This is the key the maintainer lock is taken under.** The lock sits inside
/// the derived directory this pair names, so two hosts holding two keys hold
/// two locks over two derived stores, and two hosts reaching for one key
/// contend for one lock. A shadow home carries the same key wherever it is
/// placed, which is what makes "the lock is held" and "nothing else writes
/// through this home" the same fact — the premise the total attach-time
/// [sweep](ShadowHome::sweep) rests on.
///
/// Each part is one ordinary path component, checked here rather than assumed:
/// the key becomes a directory under a vault root, and a part carrying a
/// separator or naming a parent would place a home somewhere else entirely.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MaintainershipKey(PathBuf);

impl MaintainershipKey {
    /// The key `channel` and `vault` spell, or `None` when either is not a
    /// single ordinary path component.
    ///
    /// The two parts stay two components rather than being joined into one
    /// name. A concatenated name is ambiguous wherever a part may contain the
    /// joiner — a `norn` channel serving `dev-notes` and a `norn-dev` channel
    /// serving `notes` would spell one name — and two keys sharing one home is
    /// the exact outcome this key exists to prevent.
    pub fn new(channel: &str, vault: &str) -> Option<MaintainershipKey> {
        let channel = component(channel)?;
        let vault = component(vault)?;
        Some(MaintainershipKey(Path::new(channel).join(vault)))
    }

    /// The relative directory this key names.
    fn as_path(&self) -> &Path {
        &self.0
    }
}

/// `part`, when it is one ordinary path component and nothing else.
///
/// The whole test is that the path `part` spells is one `Normal` component that
/// reads back as `part` itself: that refuses the empty string, `.`, `..`,
/// anything holding a separator, and the trailing-separator spellings a plain
/// character scan would let through.
fn component(part: &str) -> Option<&OsStr> {
    let mut components = Path::new(part).components();
    match (components.next(), components.next()) {
        (Some(Component::Normal(one)), None) if one == OsStr::new(part) => Some(one),
        _ => None,
    }
}

impl ShadowHome {
    /// Decide where `key`'s shadows go under `vault_root`, given the temporary
    /// directory inside the derived directory `key` names.
    ///
    /// `data_tmp` is created if it is not there — it is norn's own directory,
    /// and its device is what the decision reads. Where the two devices agree
    /// the answer is `data_tmp`; where they do not, `key`'s directory under
    /// [`FALLBACK`] beneath the vault root is created and returned instead.
    ///
    /// **A home's key is the maintainership key.** `data_tmp` sits inside the
    /// derived directory the key names and is keyed by being there; the vault
    /// root is shared ground, reachable by every host serving it under any key,
    /// so the fallback is keyed here. One key resolves to one home every run,
    /// and two keys resolve to two homes that share no entry — which is what
    /// makes every shadow a total [sweep](ShadowHome::sweep) finds the residue
    /// of the lock holder's own writes.
    ///
    /// **The comparison is against the vault root**, so a separate mount
    /// *inside* a vault is not what this decides about. A swap into one reports
    /// `EXDEV`, which is an environmental refusal naming the path: the decision
    /// is made again rather than guessed at, because the alternative — falling
    /// back to copy-then-unlink — publishes bytes without a rename and gives up
    /// atomicity to avoid an error message.
    pub fn resolve(
        vault_root: &Path,
        data_tmp: &Path,
        key: &MaintainershipKey,
    ) -> Result<ShadowHome, Refusal> {
        let vault = device_of(vault_root)?;
        make_directory(data_tmp)?;
        let data = device_of(data_tmp)?;
        Self::resolve_where(vault_root, data_tmp, key, vault == data)
    }

    /// [`ShadowHome::resolve`], with the device comparison's answer passed in
    /// rather than read off the filesystem.
    ///
    /// The parameter is what makes the fallback a checkable claim on a machine
    /// with one filesystem: the case that matters is the one where the two
    /// devices differ, and a test that cannot arrange two mounts has no other
    /// way to reach it.
    ///
    /// `data_tmp` is expected to exist already — reading its device is what
    /// [`ShadowHome::resolve`] decides `same_device` from, so it has been made by
    /// the time an answer is available.
    fn resolve_where(
        vault_root: &Path,
        data_tmp: &Path,
        key: &MaintainershipKey,
        same_device: bool,
    ) -> Result<ShadowHome, Refusal> {
        if same_device {
            return Ok(ShadowHome {
                directory: data_tmp.to_path_buf(),
                placement: Placement::DataRoot,
            });
        }
        let fallback = vault_root.join(FALLBACK).join(key.as_path());
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

    /// Remove the shadows in this home that are at least `older_than` old.
    ///
    /// **Only entries [`is_shadow_name`] accepts are candidates.** Everything
    /// else in the directory is somebody else's and is counted by nobody: the
    /// home is Norn's, but a sweep that removed whatever it found would be a
    /// sweep that removed a near-miss the moment the predicate and the naming
    /// scheme drifted apart.
    ///
    /// `older_than` is what makes this the same act at both tiers:
    ///
    /// - **`Duration::ZERO` is the total attach-time sweep.** Every shadow goes,
    ///   whatever its age. That is only sound under a *freshly taken* maintainer
    ///   lock: the home is keyed by the same [key](MaintainershipKey) the lock is
    ///   taken under, so holding the lock means no other Norn host writes
    ///   through *this home* — not that no other host writes this vault, which a
    ///   vault served under a second key is a live counterexample to — and every
    ///   shadow in it is residue of a write that is already over. A caller that
    ///   has not just won the lock must not pass zero.
    /// - **[`SHADOW_AGE_THRESHOLD`] is the in-life sweep**, on the host's
    ///   maintenance schedule, and its margin is what keeps a live write's shadow
    ///   out of reach.
    ///
    /// A removal the filesystem refuses is left rather than reported, for the
    /// reason a write's cleanup failure is: residue is bounded by this sweep, and
    /// the next one comes round.
    #[allow(clippy::disallowed_methods)] // The vault filesystem seam: this crate owns the shadow home.
    pub fn sweep(&self, older_than: Duration) -> Result<Swept, Refusal> {
        let entries = std::fs::read_dir(&self.directory)
            .map_err(|error| environment("reading", &self.directory, &error))?;
        let now = SystemTime::now();
        let mut swept = Swept {
            removed: 0,
            left: 0,
        };
        for entry in entries {
            let entry = entry.map_err(|error| environment("reading", &self.directory, &error))?;
            if !is_shadow_name(&entry.file_name()) {
                continue;
            }
            // `DirEntry::metadata` does not follow links, so a link left at a
            // shadow's name is aged and removed as itself.
            if age_of(&entry, now).is_none_or(|age| age < older_than) {
                swept.left += 1;
                continue;
            }
            if std::fs::remove_file(entry.path()).is_ok() {
                swept.removed += 1;
            } else {
                swept.left += 1;
            }
        }
        Ok(swept)
    }
}

/// What one sweep of a shadow home did.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Swept {
    /// How many shadows the sweep removed.
    pub removed: usize,
    /// How many shadows the sweep recognized and left: younger than the
    /// threshold, or a removal the filesystem refused. Residue that persists
    /// across sweeps is a health finding rather than an error here.
    pub left: usize,
}

/// How long ago `entry` was last modified, or `None` when the filesystem will
/// not say.
///
/// An entry whose age cannot be established is never swept: age is the whole
/// basis for calling something residue, and a missing answer is not a small age.
#[allow(clippy::disallowed_methods)] // The vault filesystem seam: this crate owns the shadow home.
fn age_of(entry: &std::fs::DirEntry, now: SystemTime) -> Option<Duration> {
    let modified = entry.metadata().ok()?.modified().ok()?;
    // A shadow stamped in the future is not old, and is not an error either.
    Some(now.duration_since(modified).unwrap_or(Duration::ZERO))
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
    use crate::scratch::{Scratch, key};

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
        for theirs in NEAR_MISSES {
            assert!(
                !is_shadow_name(OsStr::new(theirs)),
                "{theirs:?} was taken for a shadow"
            );
        }
    }

    /// Names that are not shadows, several of which are one character away from
    /// being one. Shared by the predicate's bar and the sweep's.
    const NEAR_MISSES: &[&str] = &[
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
    ];

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
        let scratch = Scratch::new("shadow-one-device");
        let vault = scratch.directory("other-vault");
        let data_tmp = scratch.path("other-data/vaults/notes/tmp");

        let home = ShadowHome::resolve(&vault, &data_tmp, &key()).expect("a shadow home");
        assert_eq!(home.placement(), Placement::DataRoot);
        assert_eq!(home.directory(), data_tmp);
        assert!(
            !home.directory().starts_with(&vault),
            "a shadow home inside the vault on one filesystem"
        );
    }

    /// **The bar on the fallback.** Two filesystems put shadows under the
    /// vault's own dot-directory, in the directory this maintainership is keyed
    /// by, because a rename cannot cross a filesystem.
    ///
    /// The forbidden shape is staging in the data directory regardless: every
    /// swap would then fail `EXDEV`, and a kernel that answered by copying
    /// instead would publish bytes without a rename.
    #[test]
    fn two_filesystems_fall_back_inside_the_vault() {
        let scratch = Scratch::new("shadow-two-devices");
        let vault = scratch.directory("other-vault");
        let data_tmp = scratch.path("other-data/vaults/notes/tmp");

        let home =
            ShadowHome::resolve_where(&vault, &data_tmp, &key(), false).expect("a shadow home");
        assert_eq!(home.placement(), Placement::VaultFallback);
        assert_eq!(
            home.directory(),
            vault.join(FALLBACK).join("norn-dev/notes")
        );
        assert!(
            home.directory().starts_with(vault.join(FALLBACK)),
            "the keyed home sits outside the dot-directory the walk excludes"
        );
        assert!(
            scratch.exists(home.directory()),
            "the fallback directory was not created"
        );
    }

    /// **The bar on the fallback's key.** Two maintainerships over one vault
    /// root fall back into two homes, and one key names one home every time it
    /// is resolved.
    ///
    /// The forbidden shape is an unkeyed fallback: co-maintainers of two derived
    /// stores over one root would then stage into one directory, and the sweep
    /// bar below is what that costs.
    #[test]
    fn two_keys_over_one_vault_root_fall_back_into_two_homes() {
        let scratch = Scratch::new("shadow-two-keys");
        let vault = scratch.directory("other-vault");
        let mine = MaintainershipKey::new("norn-dev", "notes").expect("a key");
        let theirs = MaintainershipKey::new("norn", "notes").expect("a key");

        let home = |key: &MaintainershipKey, tmp: &str| {
            ShadowHome::resolve_where(&vault, &scratch.path(tmp), key, false)
                .expect("a shadow home")
        };
        let ours = home(&mine, "dev-data/vaults/notes/tmp");
        let others = home(&theirs, "live-data/vaults/notes/tmp");

        assert_ne!(
            ours.directory(),
            others.directory(),
            "two maintainerships share one fallback home"
        );
        assert_eq!(
            home(&mine, "dev-data/vaults/notes/tmp").directory(),
            ours.directory(),
            "one key named two homes across two resolves"
        );
    }

    /// **The bar the key buys.** A total sweep of one home leaves a shadow
    /// standing in another key's home over the same vault root.
    ///
    /// The forbidden shape is the attach-time sweep reaching another
    /// maintainership's live write: `Duration::ZERO` is sound because the lock
    /// holder is the only writer of *this home*, and an unkeyed fallback would
    /// make one host's attach delete a shadow another host's write is still
    /// holding.
    #[test]
    fn a_total_sweep_of_one_home_leaves_another_key_s_shadow_standing() {
        let scratch = Scratch::new("shadow-sweep-two-keys");
        let vault = scratch.directory("other-vault");
        let mine = MaintainershipKey::new("norn-dev", "notes").expect("a key");
        let theirs = MaintainershipKey::new("norn", "notes").expect("a key");
        let ours = ShadowHome::resolve_where(&vault, &scratch.path("dev/tmp"), &mine, false)
            .expect("a shadow home");
        let others = ShadowHome::resolve_where(&vault, &scratch.path("live/tmp"), &theirs, false)
            .expect("a shadow home");

        let live = others.next_shadow();
        #[allow(clippy::disallowed_methods)]
        // Harness scaffolding: another host's write in flight.
        std::fs::write(&live, b"another maintainer's write in flight").expect("a live shadow");
        let residue = ours.next_shadow();
        #[allow(clippy::disallowed_methods)] // Harness scaffolding: our own residue.
        std::fs::write(&residue, b"our residue").expect("residue");

        let swept = ours.sweep(Duration::ZERO).expect("a total sweep");

        assert_eq!(
            swept,
            Swept {
                removed: 1,
                left: 0
            }
        );
        assert!(!scratch.exists(&residue), "our own residue survived");
        assert!(
            scratch.exists(&live),
            "an attach sweep took another maintainership's live shadow"
        );
    }

    /// **The bar on what a key admits.** Every part is one ordinary path
    /// component, so a key can only ever name a directory directly under the
    /// fallback root.
    ///
    /// The forbidden shape is a key that escapes: a part spelling `..` or
    /// carrying a separator would place a home outside the dot-directory the
    /// walk excludes, among documents.
    #[test]
    fn a_key_part_that_is_not_one_component_spells_no_key() {
        assert!(MaintainershipKey::new("norn-dev", "notes").is_some());
        for part in [
            "",
            ".",
            "..",
            "/",
            "/notes",
            "notes/",
            "notes/deep",
            "../notes",
        ] {
            assert!(
                MaintainershipKey::new("norn-dev", part).is_none(),
                "{part:?} was taken for a key part"
            );
            assert!(
                MaintainershipKey::new(part, "notes").is_none(),
                "{part:?} was taken for a key part"
            );
        }
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

    /// **The bar on what a sweep is allowed to touch.** Only names the predicate
    /// accepts are removed, at every threshold including the total one.
    ///
    /// The forbidden shape is a sweep that empties the directory. The home is
    /// Norn's, but the predicate and the naming scheme are two pieces of code, and
    /// a sweep that trusted the directory instead of the name would remove
    /// whatever a near-miss turned out to be the moment they drifted apart.
    #[test]
    fn a_sweep_removes_only_what_the_predicate_recognizes() {
        let scratch = Scratch::new("shadow-sweep-names");
        let home = scratch.shadows();
        for theirs in NEAR_MISSES {
            if theirs.is_empty() || theirs.contains('/') {
                continue;
            }
            #[allow(clippy::disallowed_methods)] // Harness scaffolding: somebody else's files.
            std::fs::write(home.directory().join(theirs), b"somebody else's bytes")
                .unwrap_or_else(|e| panic!("placing {theirs:?}: {e}"));
        }
        // A name no near-miss above collides with, including on a
        // case-insensitive filesystem, where `Norn-Shadow-1-0` and
        // `norn-shadow-1-0` are one entry.
        let residue = home.directory().join("norn-shadow-77-3");
        #[allow(clippy::disallowed_methods)] // Harness scaffolding: our own residue.
        std::fs::write(&residue, b"residue").expect("residue");

        let swept = home.sweep(Duration::ZERO).expect("a total sweep");

        assert_eq!(
            swept,
            Swept {
                removed: 1,
                left: 0
            }
        );
        for theirs in NEAR_MISSES {
            if theirs.is_empty() || theirs.contains('/') {
                continue;
            }
            assert!(
                scratch.exists(&home.directory().join(theirs)),
                "the sweep removed {theirs:?}, which is not one of ours"
            );
        }
        assert!(
            !scratch.exists(&residue),
            "our own residue survived a total sweep"
        );
    }

    /// **The bar on the thresholded sweep.** A shadow younger than the threshold
    /// survives, and the same shadow aged past it does not.
    ///
    /// The forbidden shape is an in-life sweep that removes whatever it finds: a
    /// live write's shadow exists for the length of one document's write and
    /// fsync, and a sweep that took it would make every concurrent write a race
    /// against the maintenance schedule.
    #[test]
    fn a_thresholded_sweep_spares_a_shadow_younger_than_its_threshold() {
        let scratch = Scratch::new("shadow-sweep-age");
        let home = scratch.shadows();
        let young = home.directory().join("norn-shadow-1-0");
        #[allow(clippy::disallowed_methods)] // Harness scaffolding: a write in flight.
        std::fs::write(&young, b"a live write's bytes").expect("a young shadow");

        let spared = home.sweep(SHADOW_AGE_THRESHOLD).expect("an in-life sweep");
        assert_eq!(
            spared,
            Swept {
                removed: 0,
                left: 1
            }
        );
        assert!(
            scratch.exists(&young),
            "an in-life sweep took a shadow a live write could still be holding"
        );

        // The same shadow, aged past the threshold, is residue and goes. The age
        // is arranged rather than waited for: ten minutes of a test's time buys
        // nothing the timestamp does not.
        let aged = std::time::SystemTime::now() - SHADOW_AGE_THRESHOLD - Duration::from_secs(60);
        #[allow(clippy::disallowed_methods, clippy::disallowed_types)]
        // Harness scaffolding: aging the residue.
        std::fs::File::open(&young)
            .expect("the shadow")
            .set_times(std::fs::FileTimes::new().set_modified(aged))
            .expect("aging the shadow");

        let swept = home.sweep(SHADOW_AGE_THRESHOLD).expect("an in-life sweep");
        assert_eq!(
            swept,
            Swept {
                removed: 1,
                left: 0
            }
        );
        assert!(!scratch.exists(&young), "aged residue survived a sweep");
    }
}
