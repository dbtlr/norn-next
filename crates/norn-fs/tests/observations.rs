#![cfg(unix)]
#![allow(clippy::disallowed_methods)] // Contract scaffolding arranges filesystem states.

use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::time::Duration;

use norn_fs::{ContentHash, Refusal, path_identity, read_and_hash, read_optional_and_hash};

/// Distinguishes two scratch trees taken in the same process.
static SERIAL: AtomicU64 = AtomicU64::new(0);

/// Long enough that a machine under load does not fail the case, short enough
/// that a read waiting on a writer that will never come is reported instead of
/// hanging the suite.
const ANSWER_BUDGET: Duration = Duration::from_secs(20);

struct Scratch(PathBuf);

impl Scratch {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "norn-fs-observations-{label}-{}-{}",
            std::process::id(),
            SERIAL.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir(&path).expect("scratch directory");
        Self(path)
    }

    fn anchor(&self) -> &Path {
        &self.0
    }

    fn fifo(&self, name: &str) {
        let status = std::process::Command::new("mkfifo")
            .arg(self.0.join(name))
            .status()
            .expect("run mkfifo");
        assert!(status.success(), "mkfifo failed");
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::set_permissions(&self.0, fs::Permissions::from_mode(0o700));
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Answers what the read returns, or says that it never returned.
///
/// The read runs on its own thread because the condition under test is a read
/// that does not come back: a name that holds the caller inside `open` fails
/// this as a report rather than as a suite that never finishes.
fn within_budget<T: Send + 'static>(
    read: impl FnOnce() -> T + Send + 'static,
) -> Result<T, mpsc::RecvTimeoutError> {
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = sender.send(read());
    });
    receiver.recv_timeout(ANSWER_BUDGET)
}

#[test]
fn configured_file_bytes_and_fingerprint_are_one_observation() {
    let scratch = Scratch::new("schema");
    let schema = scratch.0.join("schema.toml");
    fs::write(&schema, b"version = 1\n").expect("schema bytes");

    let observed =
        read_and_hash(scratch.anchor(), Path::new("schema.toml")).expect("schema observation");

    assert_eq!(observed.path(), schema);
    assert_eq!(observed.bytes(), b"version = 1\n");
    assert_eq!(observed.content_hash(), ContentHash::of(observed.bytes()));
    let (bytes, fingerprint) = observed.into_parts();
    assert_eq!(fingerprint, ContentHash::of(&bytes));
}

/// **The bar on a name that is a symbolic link.** A configured file is the file
/// at its own name. A link there is refused whether or not it resolves, and the
/// refusal says so rather than reporting the target's absence.
///
/// The forbidden shape is following it. A link at a configured name has this
/// process read one file while every other consumer — the watcher covering the
/// name, an operator editing it — is looking at another.
#[test]
fn a_missing_name_and_a_linked_name_are_distinct_refusals() {
    let scratch = Scratch::new("missing-schema");
    let missing = scratch.0.join("missing.toml");
    let dangling = scratch.0.join("dangling.toml");
    let resolving = scratch.0.join("resolving.toml");
    symlink(&missing, &dangling).expect("dangling schema name");
    fs::write(scratch.0.join("target.toml"), b"version = 1\n").expect("link target");
    symlink("target.toml", &resolving).expect("resolving schema name");

    let Refusal::Environment {
        operation: missing_operation,
        kind: missing_kind,
        path: missing_path,
        ..
    } = read_and_hash(scratch.anchor(), Path::new("missing.toml"))
        .expect_err("missing schema must refuse")
    else {
        panic!("missing schema was not an environmental refusal")
    };
    assert_eq!(missing_kind, std::io::ErrorKind::NotFound);
    assert_eq!(missing_path, missing);
    assert_eq!(missing_operation, "opening");

    for (name, path) in [("dangling.toml", dangling), ("resolving.toml", resolving)] {
        let refusal =
            read_and_hash(scratch.anchor(), Path::new(name)).expect_err("a link must refuse");
        let Refusal::Environment {
            kind,
            path: refused,
            message,
            ..
        } = &refusal
        else {
            panic!("a linked schema name was not an environmental refusal")
        };
        assert_eq!(*kind, std::io::ErrorKind::InvalidInput, "{refusal:?}");
        assert_eq!(*refused, path);
        assert!(message.contains("symbolic link"), "{refusal:?}");
    }
}

/// **The bar on a link at any component.** Containment is per component: the
/// last name is not the only one opened without following a link, because a
/// single multi-component open would resolve the rest in the kernel.
#[test]
fn no_component_below_the_anchor_is_followed_through_a_link() {
    let scratch = Scratch::new("linked-ancestor");
    fs::create_dir_all(scratch.0.join("real/sub")).expect("real subtree");
    fs::write(scratch.0.join("real/sub/doc.md"), b"body").expect("document bytes");
    symlink("real", scratch.0.join("link")).expect("ancestor link");

    assert_eq!(
        read_optional_and_hash(scratch.anchor(), Path::new("real/sub/doc.md"))
            .expect("the contained spelling reads")
            .expect("the document is there")
            .bytes(),
        b"body"
    );
    assert!(
        read_optional_and_hash(scratch.anchor(), Path::new("link/sub/doc.md"))
            .expect("a linked ancestor is an answer, not a fault")
            .is_none(),
        "a document was read through a linked ancestor"
    );

    let refusal = read_and_hash(scratch.anchor(), Path::new("link/sub/doc.md"))
        .expect_err("the required read must refuse");
    assert!(
        matches!(&refusal, Refusal::Environment { message, .. } if message.contains("symbolic link")),
        "{refusal:?}"
    );
}

/// **The bar on what a relative name may say.** Containment is this seam's own
/// property, not a promise about the callers it has today. A name that is not
/// below the anchor is refused before anything is opened: an absolute name
/// resolves from the filesystem root and ignores the descriptor it is handed,
/// and a parent name walks out of the anchor with nothing to report.
///
/// It is a refusal rather than absence, because a caller that spelled such a
/// path is wrong about what it asked for — answering "nothing is there" would
/// have it converge on that answer and delete what it has.
#[test]
fn a_name_that_leaves_the_anchor_is_refused_rather_than_resolved() {
    let scratch = Scratch::new("uncontained");
    let anchor = scratch.0.join("vault");
    fs::create_dir(&anchor).expect("anchor directory");
    fs::write(scratch.0.join("outside.md"), b"outside").expect("outside canary");
    let absolute = scratch.0.join("outside.md");

    for relative in [Path::new("../outside.md"), absolute.as_path()] {
        let refusal = read_optional_and_hash(&anchor, relative)
            .expect_err("a path that leaves the anchor must refuse");
        assert!(
            matches!(&refusal, Refusal::Environment { kind, .. }
                if *kind == std::io::ErrorKind::InvalidInput),
            "{refusal:?}"
        );
        assert!(
            read_and_hash(&anchor, relative)
                .expect_err("a path that leaves the anchor must refuse")
                .to_string()
                .contains("leaves the directory"),
            "the refusal does not say what is wrong with the path"
        );
    }
}

/// **The bar on a name that is not a regular file.** Opening it must not wait
/// on anybody: a FIFO with no writer holds an ordinary `open` until one arrives,
/// which parks the worker that read it for as long as the pipe sits there.
///
/// The forbidden shape is deciding by a stat of the name and then opening it.
/// That is two acts on a name a writer can change in between, and the open is
/// the one that blocks.
#[test]
fn a_pipe_at_a_name_is_refused_without_waiting_for_a_writer() {
    let scratch = Scratch::new("fifo-schema");
    scratch.fifo("pipe.toml");
    let anchor = scratch.anchor().to_owned();

    let required = within_budget(move || read_and_hash(&anchor, Path::new("pipe.toml")))
        .expect("the read never returned: a pipe held it inside open")
        .expect_err("a pipe is not configured-file bytes");
    assert!(
        matches!(&required, Refusal::Environment { kind, .. }
            if *kind == std::io::ErrorKind::InvalidData),
        "{required:?}"
    );

    let anchor = scratch.anchor().to_owned();
    let optional = within_budget(move || read_optional_and_hash(&anchor, Path::new("pipe.toml")))
        .expect("the read never returned: a pipe held it inside open")
        .expect("a pipe is an answer rather than a fault");
    assert!(optional.is_none(), "a pipe was read as document bytes");
}

#[test]
fn a_directory_cannot_masquerade_as_configured_file_bytes() {
    let scratch = Scratch::new("directory-schema");
    fs::create_dir(scratch.0.join("folder")).expect("directory");
    let refusal =
        read_and_hash(scratch.anchor(), Path::new("folder")).expect_err("a directory must refuse");
    assert!(
        matches!(refusal, Refusal::Environment { kind, .. }
            if kind == std::io::ErrorKind::InvalidData),
        "{refusal:?}"
    );
}

/// **The bar on the split between an answer and a refusal.** An absent name is
/// an answer to the optional read; a machine that will not let this account
/// look is not. Reporting the second as absence would have a transient fault
/// delete derived state.
#[test]
fn unreadable_configured_file_is_an_environmental_refusal() {
    let scratch = Scratch::new("unreadable-schema");
    let schema = scratch.0.join("schema.toml");
    fs::write(&schema, b"secret").expect("schema bytes");
    fs::set_permissions(&schema, fs::Permissions::from_mode(0o000)).expect("remove access");

    #[allow(clippy::disallowed_types)] // Proves this account is subject to the arranged mode.
    let actually_blocked = fs::File::open(&schema).is_err();
    assert!(
        actually_blocked,
        "this account can read a mode-000 file, so the refusal case proves nothing"
    );

    for refusal in [
        read_and_hash(scratch.anchor(), Path::new("schema.toml"))
            .expect_err("unreadable schema must refuse"),
        read_optional_and_hash(scratch.anchor(), Path::new("schema.toml"))
            .expect_err("an unreadable file is not an absent one"),
    ] {
        assert!(
            matches!(refusal, Refusal::Environment { kind, ref path, .. }
                if kind == std::io::ErrorKind::PermissionDenied && *path == schema),
            "{refusal:?}"
        );
    }
}

#[test]
fn root_identity_detects_dot_and_symbolic_link_aliases() {
    let scratch = Scratch::new("root-aliases");
    let vault = scratch.0.join("vault");
    let alias = scratch.0.join("alias");
    fs::create_dir(&vault).expect("vault root");
    symlink(&vault, &alias).expect("root alias");

    let identity = path_identity(&vault).expect("root observation");
    assert_eq!(path_identity(&vault.join(".")).unwrap(), identity);
    assert_eq!(path_identity(&alias).unwrap(), identity);
}

#[test]
fn absent_and_dangling_roots_have_no_identity() {
    let scratch = Scratch::new("missing-root");
    let missing = scratch.0.join("missing");
    let dangling = scratch.0.join("dangling");
    symlink(&missing, &dangling).expect("dangling root name");

    assert_eq!(path_identity(&missing).unwrap(), None);
    assert_eq!(path_identity(&dangling).unwrap(), None);
}
