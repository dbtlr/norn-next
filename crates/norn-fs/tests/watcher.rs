//! Platform-real filesystem watcher contracts.
//!
//! These cases deliberately assert inclusion rather than exact backend event
//! shapes: over-reporting is safe, while a changed path going missing is not.

#![allow(clippy::disallowed_methods)] // Harness scaffolding simulates editors, sync tools and root loss.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use norn_fs::{Batch, CaseSensitivity, PathNormalizer, WatchError, watch};
use norn_testkit::wait::{Budget, Observed, wait_until};

static SERIAL: AtomicU64 = AtomicU64::new(0);

fn budget() -> Budget {
    Budget::new(Duration::from_secs(15), Duration::from_millis(250))
}

struct Scratch {
    root: PathBuf,
}

impl Scratch {
    fn new(label: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "norn-fs-watcher-{label}-{}-{}",
            std::process::id(),
            SERIAL.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("vault")).expect("a vault root");
        std::fs::create_dir_all(root.join("schema")).expect("a schema directory");
        std::fs::write(root.join("schema/schema.toml"), b"version = 1\n")
            .expect("an external schema");
        Self { root }
    }

    fn vault(&self) -> PathBuf {
        self.root.join("vault")
    }

    fn schema(&self) -> PathBuf {
        self.root.join("schema/schema.toml")
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[derive(Default)]
struct Seen {
    roots: BTreeSet<PathBuf>,
    schema_dirty: bool,
    terminal: Option<WatchError>,
}

impl Seen {
    fn add(&mut self, batch: Batch) {
        self.roots.extend(
            batch
                .vault_roots()
                .iter()
                .map(|path| path.as_path().to_owned()),
        );
        self.schema_dirty |= batch.schema_dirty();
    }
}

struct Collector {
    seen: Arc<Mutex<Seen>>,
    stop: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
}

impl Collector {
    fn start(vault: &Path, schema: &Path) -> Self {
        let (subscription, _own_writes) = watch(vault, schema).expect("watch coverage is active");
        let seen = Arc::new(Mutex::new(Seen::default()));
        let stop = Arc::new(AtomicBool::new(false));
        let captured = seen.clone();
        let stopping = stop.clone();
        let thread = thread::spawn(move || {
            while !stopping.load(Ordering::Acquire) {
                match subscription.recv_timeout(Duration::from_millis(20)) {
                    Ok(Some(batch)) => captured.lock().expect("captured state").add(batch),
                    Ok(None) => {}
                    Err(error) => {
                        captured.lock().expect("captured state").terminal = Some(error);
                        return;
                    }
                }
            }
        });
        Self {
            seen,
            stop,
            thread: Some(thread),
        }
    }

    fn wait_for_roots(&self, expected: &BTreeSet<PathBuf>, description: &'static str) {
        wait_until(description, budget(), || {
            let seen = self.seen.lock().expect("captured state");
            let missing: Vec<_> = expected
                .iter()
                .filter(|path| !seen.roots.iter().any(|root| path.starts_with(root)))
                .cloned()
                .collect();
            if missing.is_empty() {
                Observed::Met(())
            } else {
                Observed::Pending(format!(
                    "changed paths not covered by any invalidation root: {missing:?}; roots: {:?}",
                    seen.roots
                ))
            }
        })
        .unwrap_or_else(|failure| panic!("{failure}"));
    }
}

impl Drop for Collector {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            thread.join().expect("watch collector");
        }
    }
}

#[test]
fn a_burst_covers_every_changed_path_including_non_markdown_files() {
    let scratch = Scratch::new("burst");
    let collector = Collector::start(&scratch.vault(), &scratch.schema());
    let expected = BTreeSet::from([
        PathBuf::from("nested/one.md"),
        PathBuf::from("nested/two.txt"),
        PathBuf::from("elsewhere/three.md"),
    ]);

    for relative in &expected {
        let path = scratch.vault().join(relative);
        std::fs::create_dir_all(path.parent().expect("a parent")).expect("a directory");
        std::fs::write(path, b"first\n").expect("a changed file");
    }

    collector.wait_for_roots(&expected, "the burst to report every changed path");
}

#[test]
fn an_editor_atomic_save_reports_the_destination_path() {
    let scratch = Scratch::new("editor-save");
    let destination = scratch.vault().join("notes/document.md");
    std::fs::create_dir_all(destination.parent().expect("a parent")).expect("a directory");
    std::fs::write(&destination, b"old\n").expect("the original document");
    let collector = Collector::start(&scratch.vault(), &scratch.schema());

    let temporary = scratch.vault().join("notes/.document.md.swp");
    std::fs::write(&temporary, b"new\n").expect("the editor temporary");
    std::fs::rename(&temporary, &destination).expect("the atomic replacement");

    collector.wait_for_roots(
        &BTreeSet::from([PathBuf::from("notes/document.md")]),
        "the editor save to report its destination",
    );
}

#[test]
fn a_sync_catch_up_covers_every_path_across_settled_batches() {
    let scratch = Scratch::new("sync-catch-up");
    let collector = Collector::start(&scratch.vault(), &scratch.schema());
    let mut expected = BTreeSet::new();

    for index in 0..128 {
        let relative = PathBuf::from(format!("sync/{index:03}.md"));
        let path = scratch.vault().join(&relative);
        std::fs::create_dir_all(path.parent().expect("a parent")).expect("a directory");
        std::fs::write(path, format!("revision {index}\n")).expect("a synced document");
        expected.insert(relative);
        thread::sleep(Duration::from_millis(5));
    }

    collector.wait_for_roots(&expected, "the sync catch-up to report every changed path");
}

#[test]
fn external_schema_changes_and_vault_root_loss_are_reported() {
    let scratch = Scratch::new("coverage");
    let schema = scratch.schema();
    let schema_parent = schema.parent().expect("a schema parent");
    let watched_schema = match PathNormalizer::detect(schema_parent)
        .expect("schema-parent case behavior")
        .case_sensitivity()
    {
        // The configured spelling and callback spelling may differ while
        // identifying the same file. The macOS platform gate binds that path.
        CaseSensitivity::Insensitive => schema_parent.join("SCHEMA.TOML"),
        CaseSensitivity::Sensitive => schema.clone(),
    };
    let collector = Collector::start(&scratch.vault(), &watched_schema);

    let replacement = scratch.root.join("schema/.schema.toml.new");
    std::fs::write(&replacement, b"version = 2\n").expect("a replacement schema");
    std::fs::rename(&replacement, scratch.schema()).expect("the schema replacement");
    wait_until(
        "the external schema replacement to be reported",
        budget(),
        || {
            let seen = collector.seen.lock().expect("captured state");
            if seen.schema_dirty {
                Observed::Met(())
            } else {
                Observed::Pending("schema_dirty is false".to_owned())
            }
        },
    )
    .unwrap_or_else(|failure| panic!("{failure}"));

    let canonical_vault = std::fs::canonicalize(scratch.vault()).expect("the canonical vault root");
    std::fs::remove_dir_all(scratch.vault()).expect("removing the watched root");
    wait_until(
        "vault root loss to terminate the subscription",
        budget(),
        || {
            let seen = collector.seen.lock().expect("captured state");
            match &seen.terminal {
                Some(WatchError::CoverageLost(path)) if path == &canonical_vault => {
                    Observed::Met(())
                }
                terminal => Observed::Pending(format!("terminal state is {terminal:?}")),
            }
        },
    )
    .unwrap_or_else(|failure| panic!("{failure}"));
}
