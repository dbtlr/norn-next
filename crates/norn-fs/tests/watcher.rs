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

use norn_fs::{
    Batch, CaseSensitivity, PathNormalizer, RescanScope, Subscription, SubscriptionState,
    WatchError, watch,
};
use norn_testkit::isolation::{self, Lease};
use norn_testkit::wait::{Budget, Observed, wait_until};

static SERIAL: AtomicU64 = AtomicU64::new(0);

/// The wait a case gives coverage to cross its backend synchronization
/// boundary. It is the lifecycle's own authored deadline, so a case that
/// reaches it saw what a host would refuse an attach over.
const SYNCHRONIZATION_DEADLINE: Duration = Duration::from_secs(15);

/// The vault-relative path a collector writes to prove the backend is
/// reporting. Not a Markdown document, and named so a reader of a leftover
/// scratch tree knows what put it there.
const CANARY: &str = "watch-canary";

fn budget() -> Budget {
    Budget::new(Duration::from_secs(15), Duration::from_millis(250))
}

/// The bound on taking the real-watcher lease, derived from the window a case
/// here holds it for.
///
/// The queue depth and the wall it is capped against are the isolation
/// module's, because they are one machine's and not this target's: the cases
/// queued ahead of one here are mostly in another crate.
fn lease_budget() -> Budget {
    isolation::acquisition_budget(budget())
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

/// Every fact taken off one subscription, accumulated across batches.
///
/// The rescans are kept because they are how the backend reports that a
/// partition's exact path set was lost: [`RescanScope::Vault`] clears the dirty
/// roots of the batch carrying it and no root enters that batch afterwards. A
/// collector that dropped them would read the widening as paths going missing,
/// which is the opposite of what it means.
///
/// That makes a rescan an answer to one question and not the other, and the
/// two questions are separate methods. [`Seen::covers`] asks whether a path
/// was invalidated, which a rescan answers for every path at once.
/// [`Seen::reported`] asks whether the backend delivered an event for the path
/// itself, which a rescan answers for none: it says the path set was lost, so
/// it is precisely the report that carries no per-path fact.
#[derive(Default)]
struct Seen {
    roots: BTreeSet<PathBuf>,
    rescans: BTreeSet<RescanScope>,
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
        self.rescans.extend(batch.rescans().iter().copied());
        self.schema_dirty |= batch.schema_dirty();
    }

    /// Whether some reported invalidation makes `path` invalid: an
    /// invalidation root at or above it, or a [`RescanScope::Vault`] rescan,
    /// which is the widest invalidation the backend has and therefore covers
    /// every vault path.
    fn covers(&self, path: &Path) -> bool {
        self.rescans.contains(&RescanScope::Vault) || self.reported(path)
    }

    /// Whether the backend reported an invalidation root at or above `path` —
    /// a per-path delivery, with no rescan standing in for it.
    ///
    /// This is what readiness asks. A stream that is live reports the path
    /// that changed; a stream the platform has starved reports nothing, and a
    /// rescan admitting that the path set was lost is the one report that
    /// proves neither.
    fn reported(&self, path: &Path) -> bool {
        self.roots.iter().any(|root| path.starts_with(root))
    }

    /// What the collector has taken so far, for a wait that is about to fail.
    fn state(&self) -> String {
        format!(
            "roots: {:?}; rescans: {:?}; schema_dirty: {}; terminal: {:?}",
            self.roots, self.rescans, self.schema_dirty, self.terminal
        )
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
    // The platform serves every watcher on the machine from one service, and
    // the lease is what keeps this case's watcher the only live one. It is
    // held for as long as the subscription is, so the field outlives nothing
    // and is dropped with it.
    _watcher_lease: Lease,
}

impl Collector {
    /// Watch `vault`, and hand back a collector the backend is already
    /// reporting to, whose coverage state is empty.
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
    ///
    /// **Readiness is a per-path report and nothing else.** A
    /// [`RescanScope::Vault`] rescan covers the canary the way it covers every
    /// vault path, and it is the report the backend makes when the platform
    /// lost the path set — so a readiness check that accepted one would call a
    /// stream live on the strength of the one fact that says nothing came
    /// through it, and every later wait would be met by that rescan rather
    /// than by the change the case made.
    fn start(vault: &Path, schema: &Path) -> Self {
        let lease = Lease::hold(isolation::REAL_WATCHER, lease_budget());
        let (subscription, _own_writes) = watch(vault, schema).expect("watch coverage is active");
        let mut collector = Self::adopt(subscription, lease);
        let canary = vault.join(CANARY);
        wait_until("the backend to report a canary write", budget(), || {
            std::fs::write(&canary, b"canary\n").expect("the canary write");
            collector.drain();
            if collector.seen.reported(Path::new(CANARY)) {
                Observed::Met(())
            } else {
                Observed::Pending(format!(
                    "the backend reported no event for the canary itself; {}",
                    collector.seen.state()
                ))
            }
        })
        .unwrap_or_else(|failure| panic!("{failure}"));
        // The canary proves the stream is live and nothing else, so the case
        // starts from empty coverage: the canary's own root is not the case's
        // action, and neither is anything settling coverage reported beside
        // it. The rescans go with them, and that is the same rule the case's
        // own collection obeys from the other side. A rescan says the path set
        // was lost, and the paths it was lost for are the ones that predate
        // this line — the case has made no change yet, so there is nothing
        // here for it to widen. A rescan arriving during the collection below
        // is kept, because there it widens the very changes the case made.
        // A terminal error survives the reset: it is the last fact the
        // subscription carries, and a case waiting for one still has to see it.
        let terminal = collector.seen.terminal.take();
        collector.seen = Seen {
            terminal,
            ..Seen::default()
        };
        collector
    }

    /// Collect from a subscription the case already holds, with the lease that
    /// was taken before it.
    ///
    /// A case that builds its own subscription is one asserting something
    /// about establishing coverage, so it takes the lease first and hands both
    /// over together — the collector is what holds them for the same window.
    fn adopt(subscription: Subscription, lease: Lease) -> Self {
        Self {
            subscription,
            seen: Seen::default(),
            _watcher_lease: lease,
        }
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
    fn wait_for(&mut self, description: &str, condition: impl FnMut(&Seen) -> Observed<()>) {
        self.wait_within(budget(), description, condition);
    }

    /// The same wait, for a case whose bound is its own rather than the
    /// target's.
    fn wait_within(
        &mut self,
        budget: Budget,
        description: &str,
        mut condition: impl FnMut(&Seen) -> Observed<()>,
    ) {
        wait_until(description, budget, || {
            self.drain();
            condition(&self.seen)
        })
        .unwrap_or_else(|failure| panic!("{failure}"));
    }

    fn wait_for_roots(&mut self, expected: &BTreeSet<PathBuf>, description: &'static str) {
        self.wait_for(description, |seen| {
            let missing: Vec<_> = expected
                .iter()
                .filter(|path| !seen.covers(path))
                .cloned()
                .collect();
            if missing.is_empty() {
                Observed::Met(())
            } else {
                Observed::Pending(format!(
                    "changed paths covered by no reported invalidation: {missing:?}; {}",
                    seen.state()
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
            Observed::Pending(seen.state())
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

/// **The window synchronization closes.** A write issued the moment coverage
/// is installed — before the subscription is live — is reported for its own
/// path once it is.
///
/// This is the case a canary cannot stand in for. The canary is rewritten
/// until one of its writes is reported, so it proves the stream is live and
/// says nothing about the write that preceded it. Here the write happens once,
/// while the subscription is still synchronizing, and the report is required
/// afterwards: a backend whose stream starts at its own convenience never
/// observed it, and the case fails naming the state it saw at the write.
#[test]
fn a_write_issued_before_synchronization_is_reported_for_its_own_path() {
    let scratch = Scratch::new("synchronization-race");
    let vault = scratch.vault();
    let lease = Lease::hold(isolation::REAL_WATCHER, lease_budget());
    let (subscription, _own_writes) =
        watch(&vault, &scratch.schema()).expect("watch coverage is active");

    let racing = subscription.state();
    std::fs::write(vault.join("raced.md"), b"raced\n").expect("the racing write");

    subscription
        .synchronize(SYNCHRONIZATION_DEADLINE)
        .expect("coverage to become live");
    let mut collector = Collector::adopt(subscription, lease);
    // Every other case here retries its write until one is reported, so its
    // wait bounds a retry loop. This one writes once and cannot ask again, so
    // its wait bounds the platform's own delivery instead: a run that reaches
    // this is a stream that went silent, not a machine that was slow.
    collector.wait_within(
        Budget::new(Duration::from_secs(60), Duration::from_millis(250)),
        "the racing write to be reported",
        |seen| {
            if seen.reported(Path::new("raced.md")) {
                Observed::Met(())
            } else {
                Observed::Pending(format!(
                    "a write made while the subscription was {racing:?} went unreported; {}",
                    seen.state()
                ))
            }
        },
    );
    // What the write raced is the selected backend's boundary. On macOS the
    // stream has not started when registration returns, so the write lands in
    // the window the event-history replay covers. Where registration is itself
    // the boundary, the subscription is live before the write and the backend
    // has already queued it.
    let expected = match cfg!(target_os = "macos") {
        true => SubscriptionState::Synchronizing,
        false => SubscriptionState::Live,
    };
    assert_eq!(
        racing, expected,
        "installing coverage returned with the subscription in a state this backend's \
         synchronization boundary does not put it in, so this case raced nothing"
    );
}

/// **One boundary covers every volume the plan touches.** A schema edge on a
/// second volume synchronizes with the vault's own edge and reports.
///
/// The identifiers a native macOS stream replays from come from one per-host
/// source that advances across every attached volume, which is what makes a
/// single boundary a claim about the whole plan rather than about one
/// filesystem. A plan whose edges sit on different volumes is where that
/// claim is worth something, so it is the topology this case builds.
#[test]
fn a_plan_spanning_two_volumes_synchronizes_and_reports_from_both() {
    let scratch = Scratch::new("multi-volume");
    let volume = match Volume::provision("schema") {
        Ok(volume) => volume,
        Err(reason) => {
            eprintln!(
                "no second volume available, so the multi-volume plan is unasserted: {reason}"
            );
            return;
        }
    };
    let schema = volume.mount().join("schema.toml");
    std::fs::write(&schema, b"version = 1\n").expect("a schema on the second volume");

    let lease = Lease::hold(isolation::REAL_WATCHER, lease_budget());
    let (subscription, _own_writes) =
        watch(&scratch.vault(), &schema).expect("watch coverage is active across two volumes");
    subscription
        .synchronize(SYNCHRONIZATION_DEADLINE)
        .expect("one boundary to cover a plan spanning two volumes");
    let mut collector = Collector::adopt(subscription, lease);

    std::fs::write(scratch.vault().join("note.md"), b"note\n").expect("a document write");
    let replacement = volume.mount().join(".schema.toml.new");
    std::fs::write(&replacement, b"version = 2\n").expect("a replacement schema");
    std::fs::rename(&replacement, &schema).expect("the schema replacement");

    collector.wait_for("both volumes to report their own change", |seen| {
        match (seen.reported(Path::new("note.md")), seen.schema_dirty) {
            (true, true) => Observed::Met(()),
            _ => Observed::Pending(seen.state()),
        }
    });
}

/// A second mounted volume, for the coverage plan that needs one.
///
/// A RAM disk is the volume a test can mount and release without privileges.
/// Where the machine will not provide one the case that wanted it says so and
/// asserts nothing, because a volume is the environment's to give.
struct Volume {
    device: String,
    mount: PathBuf,
}

impl Volume {
    fn provision(label: &str) -> Result<Self, String> {
        // 65,536 512-byte sectors: 32 MiB, which holds a schema file and the
        // volume's own event log with room to spare.
        // 65,536 512-byte sectors: 32 MiB, which holds a schema file and the
        // volume's own event log with room to spare.
        let device = command("hdiutil", &["attach", "-nomount", "ram://65536"])?;
        // The device is attached from here on, so the volume owns it before
        // anything else can fail and leave it attached with no owner.
        let mut volume = Self {
            device: device.clone(),
            mount: PathBuf::new(),
        };
        let name = format!("norn-{label}-{}", std::process::id());
        command("diskutil", &["eraseVolume", "HFS+", &name, &device])?;
        volume.mount = command("diskutil", &["info", &device])?
            .lines()
            .find_map(|line| line.trim().strip_prefix("Mount Point:").map(str::trim))
            .filter(|mount| !mount.is_empty())
            .ok_or_else(|| format!("{device} reports no mount point"))?
            .into();
        Ok(volume)
    }

    fn mount(&self) -> &Path {
        &self.mount
    }
}

impl Drop for Volume {
    fn drop(&mut self) {
        let _ = command("hdiutil", &["detach", "-force", &self.device]);
    }
}

/// Run one provisioning command, reporting its output or why it gave none.
fn command(program: &str, arguments: &[&str]) -> Result<String, String> {
    let output = std::process::Command::new(program)
        .args(arguments)
        .output()
        .map_err(|error| format!("{program} could not run: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "{program} {} failed: {}",
            arguments.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

/// **The bar under the readiness check.** A vault-wide rescan covers the canary
/// and reports nothing about it.
///
/// Readiness asks whether the stream delivered an event for the canary itself,
/// and [`RescanScope::Vault`] is the one report that answers no per-path
/// question: it says the path set was lost. The forbidden shape is a readiness
/// check that reads coverage instead — under it a rescan settling before the
/// case has done anything at all declares the stream live, and every later wait
/// in the case is met by that rescan rather than by the change the case made.
///
/// Nothing here touches a platform: the two predicates are read off a
/// collector's accumulated facts, so this holds on a machine whose backend is
/// delivering nothing.
#[test]
fn a_vault_rescan_covers_the_canary_without_reporting_it() {
    let mut seen = Seen::default();
    seen.rescans.insert(RescanScope::Vault);

    assert!(
        seen.covers(Path::new(CANARY)),
        "a vault-wide rescan is the widest invalidation there is and it did not cover the canary"
    );
    assert!(
        !seen.reported(Path::new(CANARY)),
        "a vault-wide rescan was read as the backend having reported the canary itself"
    );
}

/// **The bar on the hold window.** A collector holds the real-watcher lease for
/// as long as it holds its subscription, and lets go with it.
///
/// The forbidden shape is the lease released early — at the end of the
/// readiness phase, say, or anywhere else before the subscription is dropped.
/// It costs nothing visible and it speeds this target up, because every case
/// then runs its watcher beside every sibling's; what it buys is the starvation
/// the lease exists to prevent, showing up somewhere else as paths that never
/// arrived.
///
/// The exclusion is read from this process, which is where a file lock excludes
/// per open file description rather than per process. The reacquisition after
/// the drop takes the ordinary queueing bound, because a sibling case is
/// entitled to be next in line.
#[test]
fn a_collector_holds_the_watcher_lease_until_its_subscription_is_dropped() {
    let scratch = Scratch::new("lease-window");
    let collector = Collector::start(&scratch.vault(), &scratch.schema());

    let contested = Lease::try_hold(
        isolation::REAL_WATCHER,
        Budget::new(Duration::from_millis(50), Duration::from_millis(250)),
    );
    assert!(
        contested.is_err(),
        "the lease was free while a collector was watching, so this case's watcher runs beside \
         every sibling's"
    );

    drop(collector);
    drop(Lease::hold(isolation::REAL_WATCHER, lease_budget()));
}
