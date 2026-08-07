//! Platform-real filesystem watcher contracts.
//!
//! These cases deliberately assert inclusion rather than exact backend event
//! shapes: over-reporting is safe, while a changed path going missing is not.

#![allow(clippy::disallowed_methods)] // Harness scaffolding simulates editors, sync tools and root loss.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::Duration;

use norn_fs::{Batch, CaseSensitivity, PathNormalizer, Subscription, WatchError, watch};
use norn_testkit::wait::{Budget, Observed, wait_until};

static SERIAL: AtomicU64 = AtomicU64::new(0);

/// The vault-relative path a collector writes to prove the backend is
/// reporting. Not a Markdown document, and named so a reader of a leftover
/// scratch tree knows what put it there.
const CANARY: &str = "watch-canary";

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

/// One subscription and everything taken off it so far.
///
/// The subscription hands facts over one settled batch at a time and only to a
/// caller that asks, so the asking happens inside the wait: every probe drains
/// what has settled since the last one and then reads the accumulated state.
/// A batch that arrives while nothing is asking waits in the delivery slot and
/// the coalescer keeps merging behind it, so a probe that runs late sees wider
/// batches rather than fewer facts.
struct Collector {
    subscription: Subscription,
    seen: Seen,
}

impl Collector {
    /// Watch `vault`, and hand back a collector the backend is already
    /// reporting to.
    ///
    /// **Coverage is installed before [`watch`] returns; the platform stream
    /// behind it starts reporting a moment later.** A change made in that window
    /// is never reported at all, because what a stream carries is what happened
    /// after it started — so a case that writes the instant `watch` returns is
    /// asserting on facts the backend was never asked for. The collector
    /// therefore writes a canary of its own and waits for it to come back, which
    /// is the one observation that separates a live stream from a slow machine.
    /// The canary is rewritten on every look, so a write that lands in the dead
    /// window costs a retry rather than the whole wait; it stays where it is
    /// afterwards, an extra reported path no case asserts the absence of.
    fn start(vault: &Path, schema: &Path) -> Self {
        let (subscription, _own_writes) = watch(vault, schema).expect("watch coverage is active");
        let mut collector = Self {
            subscription,
            seen: Seen::default(),
        };
        let canary = vault.join(CANARY);
        wait_until("the backend to report a canary write", budget(), || {
            std::fs::write(&canary, b"canary\n").expect("the canary write");
            collector.drain();
            if collector
                .seen
                .roots
                .iter()
                .any(|root| Path::new(CANARY).starts_with(root))
            {
                Observed::Met(())
            } else {
                Observed::Pending(format!(
                    "the canary is covered by no invalidation root; roots: {:?}, terminal: {:?}",
                    collector.seen.roots, collector.seen.terminal
                ))
            }
        })
        .unwrap_or_else(|failure| panic!("{failure}"));
        collector
    }

    /// Take every settled batch the subscription is holding, and stop at the
    /// first terminal error — which is the last thing it ever reports.
    fn drain(&mut self) {
        while self.seen.terminal.is_none() {
            match self.subscription.try_recv() {
                Ok(Some(batch)) => self.seen.add(batch),
                Ok(None) => return,
                Err(error) => self.seen.terminal = Some(error),
            }
        }
    }

    /// Drain and judge until the condition holds over what has been seen.
    fn wait_for(&mut self, description: &str, mut condition: impl FnMut(&Seen) -> Observed<()>) {
        wait_until(description, budget(), || {
            self.drain();
            condition(&self.seen)
        })
        .unwrap_or_else(|failure| panic!("{failure}"));
    }

    fn wait_for_roots(&mut self, expected: &BTreeSet<PathBuf>, description: &'static str) {
        self.wait_for(description, |seen| {
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
        });
    }
}

#[test]
fn a_burst_covers_every_changed_path_including_non_markdown_files() {
    let scratch = Scratch::new("burst");
    let mut collector = Collector::start(&scratch.vault(), &scratch.schema());
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
    let mut collector = Collector::start(&scratch.vault(), &scratch.schema());

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
    let mut collector = Collector::start(&scratch.vault(), &scratch.schema());
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
    let mut collector = Collector::start(&scratch.vault(), &watched_schema);

    let replacement = scratch.root.join("schema/.schema.toml.new");
    std::fs::write(&replacement, b"version = 2\n").expect("a replacement schema");
    std::fs::rename(&replacement, scratch.schema()).expect("the schema replacement");
    collector.wait_for("the external schema replacement to be reported", |seen| {
        if seen.schema_dirty {
            Observed::Met(())
        } else {
            Observed::Pending("schema_dirty is false".to_owned())
        }
    });

    let canonical_vault = std::fs::canonicalize(scratch.vault()).expect("the canonical vault root");
    std::fs::remove_dir_all(scratch.vault()).expect("removing the watched root");
    collector.wait_for(
        "vault root loss to terminate the subscription",
        |seen| match &seen.terminal {
            Some(WatchError::CoverageLost(path)) if path == &canonical_vault => Observed::Met(()),
            terminal => Observed::Pending(format!("terminal state is {terminal:?}")),
        },
    );
}
