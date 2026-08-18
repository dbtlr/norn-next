//! A temporary directory one case owns, named so nothing else can meet it.
//!
//! This is the naming and the lifecycle, and only those. What a case arranges
//! inside its tree is the case's own — a vault root, a shadow home, a schema
//! source, a sandbox's environment bases — and each suite keeps the harness
//! that arranges it. A helper that tried to absorb those arrangements would be
//! a parameter list per caller, which is the copying it exists to remove
//! wearing one name.
//!
//! # What the name carries
//!
//! A scratch root is `<label>-<process id>-<counter>`. The process id
//! separates concurrent test binaries, which the runner starts several of; the
//! counter separates two roots taken inside one of them. A clock reading
//! separates neither reliably: two cases on two threads read the same
//! nanosecond often enough to collide, and the loser meets a directory that
//! already exists.
//!
//! The name is cleared before it is created, because a process id is reused
//! across runs. A case that started from a previous run's residue is judging a
//! tree it did not arrange.
//!
//! # What removal survives
//!
//! A tree goes away when its [`Scratch`] drops. A case that made a directory
//! unsearchable — to reach a refusal that needs one — leaves a tree the plain
//! removal cannot enter, so a removal that fails puts the modes back and tries
//! once more. The retry is second because the mode walk costs a read of every
//! directory in the tree and almost no case needs it. A tree that still will
//! not go is left behind rather than panicking a case that has already
//! reported its verdict.

use std::ffi::OsStr;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// Distinguishes two scratch roots taken in the same process.
static SERIAL: AtomicU64 = AtomicU64::new(0);

/// A directory name nothing else in this run holds: `label`, this process's
/// id, and a counter.
///
/// Callers that own a directory want [`Scratch`], which creates and removes
/// one. This is for the callers that only need the name — a mount point, a
/// volume label, a path handed to something else to create.
///
/// **`label` is a name, not a path, and one that is not is a panic.** What
/// this returns is one path component and nothing more: a caller joins it onto
/// a base and gets a child of that base, or hands it over as a single name a
/// mount point or a volume can be called. A label carrying a separator, a
/// root, or a parent component returns a string neither holds — one that
/// resolves somewhere else when joined, and that is malformed where a single
/// name is what was asked for.
pub fn unique_name(label: &str) -> String {
    assert!(
        is_one_component(label),
        "a scratch label is one path component, and {label:?} is not"
    );
    format!(
        "{label}-{}-{}",
        std::process::id(),
        SERIAL.fetch_add(1, Ordering::Relaxed)
    )
}

/// Whether `label` spells exactly one ordinary path component and nothing
/// else — no separator, no root, no `.` or `..`, and not empty.
fn is_one_component(label: &str) -> bool {
    let mut components = Path::new(label).components();
    let Some(Component::Normal(only)) = components.next() else {
        return false;
    };
    only == OsStr::new(label) && components.next().is_none()
}

/// A directory one case owns, removed when this is dropped.
pub struct Scratch {
    root: PathBuf,
}

impl Scratch {
    /// A fresh directory under the system temporary directory.
    ///
    /// Panics when the directory cannot be made, because a case with no tree
    /// to work in has nothing to report but that.
    pub fn new(label: &str) -> Scratch {
        Scratch::under(&std::env::temp_dir(), label)
            .unwrap_or_else(|e| panic!("a scratch directory named {label}: {e}"))
    }

    /// A fresh directory under `base`, for a caller that names its own base —
    /// `CARGO_TARGET_TMPDIR`, or a tree it already owns.
    #[allow(clippy::disallowed_methods)] // Harness scaffolding: the tree a case works in.
    pub fn under(base: &Path, label: &str) -> io::Result<Scratch> {
        let root = base.join(unique_name(label));
        std::fs::remove_dir_all(&root).or_else(ignore_missing)?;
        std::fs::create_dir_all(&root)?;
        Ok(Scratch { root })
    }

    /// The tree's own path.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// A path under the tree. Nothing is created.
    pub fn join(&self, relative: impl AsRef<Path>) -> PathBuf {
        self.root.join(relative)
    }
}

impl Drop for Scratch {
    #[allow(clippy::disallowed_methods)] // Harness scaffolding: removing the tree a case made.
    fn drop(&mut self) {
        if std::fs::remove_dir_all(&self.root).is_ok() {
            return;
        }
        restore_modes(&self.root);
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// Put every directory under `root` back to something removable.
#[cfg(unix)]
#[allow(clippy::disallowed_methods)] // Harness scaffolding: undoing what a case arranged.
fn restore_modes(root: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(root, std::fs::Permissions::from_mode(0o755));
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            restore_modes(&entry.path());
        }
    }
}

/// Nothing to put back where a directory's removability is not a mode.
#[cfg(not(unix))]
fn restore_modes(_root: &Path) {}

fn ignore_missing(error: io::Error) -> io::Result<()> {
    if error.kind() == io::ErrorKind::NotFound {
        Ok(())
    } else {
        Err(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **Two roots for one label never collide, and each carries this
    /// process's id.** The counter is what separates them; the process id is
    /// what separates this binary's roots from a concurrent binary's.
    #[test]
    fn two_roots_for_one_label_differ_and_both_carry_the_process_id() {
        let first = Scratch::new("norn-testkit-scratch-uniqueness");
        let second = Scratch::new("norn-testkit-scratch-uniqueness");
        assert_ne!(first.root(), second.root());
        let pid = std::process::id().to_string();
        for root in [first.root(), second.root()] {
            let name = root.file_name().expect("a scratch root name");
            assert!(
                name.to_string_lossy().contains(&pid),
                "a scratch root does not carry this process's id: {}",
                root.display()
            );
        }
    }

    /// **A tree is there while its handle is, and gone after.**
    #[test]
    #[allow(clippy::disallowed_methods)] // Judging what the harness left behind.
    fn a_tree_lasts_exactly_as_long_as_its_handle() {
        let root = {
            let scratch = Scratch::new("norn-testkit-scratch-lifetime");
            let root = scratch.root().to_path_buf();
            assert!(std::fs::symlink_metadata(&root).is_ok(), "no tree was made");
            root
        };
        assert!(
            std::fs::symlink_metadata(&root).is_err(),
            "the tree outlived its handle"
        );
    }

    /// **A tree holding a directory nothing can enter is still removed.** A
    /// case reaching a refusal that needs an unsearchable directory would
    /// otherwise leave its whole tree on the machine.
    #[cfg(unix)]
    #[test]
    #[allow(clippy::disallowed_methods)] // Arranging the mode this case is about.
    fn an_unsearchable_directory_does_not_keep_a_tree_alive() {
        use std::os::unix::fs::PermissionsExt;

        let root = {
            let scratch = Scratch::new("norn-testkit-scratch-unsearchable");
            let closed = scratch.join("closed");
            std::fs::create_dir_all(closed.join("inside")).expect("a nested directory");
            std::fs::set_permissions(&closed, std::fs::Permissions::from_mode(0o000))
                .expect("closing the directory");
            scratch.root().to_path_buf()
        };
        assert!(
            std::fs::symlink_metadata(&root).is_err(),
            "an unsearchable directory kept the tree alive"
        );
    }

    /// **A label that is not one path component is refused.** The name
    /// decides what the drop removes, so a label spelling its way out of the
    /// base — absolutely, through a parent, or by carrying a separator at all
    /// — would have a tree nobody named removed instead of the one that was.
    #[test]
    fn a_label_that_is_not_one_component_is_refused() {
        for label in [
            "",
            ".",
            "..",
            "/tmp/external",
            "../external",
            "nested/../external",
            "nested/child",
            "nested/",
            "/",
        ] {
            assert!(
                !is_one_component(label),
                "the label {label:?} was taken for one path component"
            );
        }
        for label in ["safe", "norn-fs-watcher-burst", "one.two"] {
            assert!(
                is_one_component(label),
                "the label {label:?} was refused as a path component"
            );
        }
        assert!(
            std::panic::catch_unwind(|| unique_name("../external")).is_err(),
            "a label that leaves the base was accepted as a scratch name"
        );
    }

    /// **A tree is empty when a case gets it.** The residue clearing this
    /// stands on has no in-process arrangement — the name is fresh in every
    /// call — so what a case can be held to is the state it starts from.
    #[test]
    #[allow(clippy::disallowed_methods)] // Judging the tree the harness handed over.
    fn a_fresh_tree_holds_nothing() {
        let scratch = Scratch::new("norn-testkit-scratch-fresh");
        assert!(
            std::fs::read_dir(scratch.root())
                .expect("the fresh tree")
                .next()
                .is_none(),
            "a fresh tree carries entries nobody put there"
        );
    }
}
