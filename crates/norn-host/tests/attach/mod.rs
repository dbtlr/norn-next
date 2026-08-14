//! A generated vault, and a real host attached over it.
//!
//! The measurement suites in this crate share one composition: a
//! `norn-fixtures` tree on disk, a serving registry naming it, production entry
//! operations over machine-local directories beside it, and a [`Host`] that
//! attaches the pair. What each suite adds is what it measures — derivation
//! counters, the peak resident set of the process that attached, a slope over a
//! long run — so the composition itself lives here once.
//!
//! **The tree is generated at a fixed seed and the profiles are named**, so a
//! reading differs between two runs by the subject and never by the corpus.
//!
//! A generated tree carries no vault schema, because a schema is a vault's own
//! declaration rather than part of the corpus a generator draws. Attachment
//! pins one, so [`Vault::generate`] writes the minimal schema beside the tree.
//!
//! Each binary that compiles this module uses part of it — the child harnesses
//! adopt a tree the parent generated rather than generating one — so an unused
//! remainder in any of them is the layout rather than a defect.
#![allow(dead_code)]
#![allow(clippy::disallowed_methods)] // Harness scaffolding: this suite's own generated tree.

use std::ffi::OsStr;
use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use norn_config::ConfigDirs;
use norn_config::registry::{Entry, VaultRoot};
use norn_fixtures::Profile;
use norn_host::{
    AttachMode, DemandLease, Host, LifecyclePolicy, ProductionEntryOps, ProductionPolicy,
    RegistryRead,
};
use norn_store::{DocumentPath, Store, StoredPathOrder};
use norn_testkit::isolation::{self, Lease};
use norn_testkit::wait::Budget;
use norn_wire::{TrustState, VaultName};

/// The seed every generated tree is drawn at. One value, so two readings
/// differ by the profile alone.
pub const SEED: u64 = 7;

/// The vault the registry names. One entry per host, so the name is fixed.
const VAULT_NAME: &str = "notes";

/// The in-vault schema attachment pins.
const SCHEMA: &[u8] = b"version: 1\n";

/// How long a suite waits for an attachment to converge.
///
/// A runaway bound rather than a bar on speed: an attachment approaching this
/// is stuck, not slow, and how long one takes is a clock these lanes do not
/// read. It sits below the deadline the process harness gives a child that
/// attaches, so a stuck attach fails here — naming the state it was last seen
/// in — rather than being killed from outside with nothing to say.
pub const READY_LIMIT: Duration = Duration::from_secs(240);

/// How long an entry nothing demands may sit attached before the host reaps it.
///
/// Longer than any load these suites run, so an attachment is torn down when
/// its host drops and at no other moment. A run that works the host past this
/// holds a demand lease for the whole of it, and this is the second guard
/// behind that one.
pub const IDLE_AFTER: Duration = Duration::from_secs(7200);

/// The file a parent writes beside a generated tree to say it issued the
/// harness run that adopts it.
const TOKEN_FILE: &str = "harness-token";

/// Issue the token for the tree at `root`, and hand back its text.
///
/// **A harness variable alone is not an invitation.** These suites put a child
/// in harness mode with an environment variable naming a tree; the same
/// variable already present in the environment the *parent* runs under would
/// turn every bar here into a harness run that reports an attachment and
/// evaluates nothing. So the parent writes a token inside the tree it generated
/// and passes the same text in a second variable, and the child refuses the
/// role unless the two agree.
pub fn issue_harness_token(root: &Path) -> String {
    static SERIAL: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("a clock past the epoch")
        .as_nanos();
    let token = format!(
        "{}-{}-{nanos}",
        std::process::id(),
        SERIAL.fetch_add(1, Ordering::Relaxed)
    );
    std::fs::write(root.join(TOKEN_FILE), &token).expect("write the harness token");
    token
}

/// The root a child adopts, taken only where the token pairs with it.
///
/// Every way the pair can fail to hold is a panic naming what was wrong: a
/// leaked variable names a root with no token beside it, or one whose token is
/// another run's. Falling through to the bars instead is what would let a
/// leaked variable pass a lane having measured nothing.
pub fn accepted_harness_root(root: &OsStr, token_variable: &str) -> PathBuf {
    let root = PathBuf::from(root);
    let issued = std::fs::read_to_string(root.join(TOKEN_FILE)).unwrap_or_else(|e| {
        panic!(
            "this suite was put in harness mode naming {}, and reading the harness token there \
             failed: {e}. The token is what says a parent in this run issued the harness; without \
             it the bars this suite carries would not run at all.",
            root.display()
        )
    });
    let presented = std::env::var(token_variable).unwrap_or_else(|e| {
        panic!(
            "this suite was put in harness mode naming {}, and `{token_variable}` does not carry \
             the token that goes with it: {e}",
            root.display()
        )
    });
    assert_eq!(
        presented.trim(),
        issued.trim(),
        "`{token_variable}` carries a token the tree at {} was not issued, so the harness mode \
         this suite was put in belongs to another run",
        root.display()
    );
    root
}

/// The vault tree, and the machine-local directories a host serves it from.
///
/// Nothing here removes the tree: it is placed under a directory the caller
/// owns — a testkit sandbox, which is removed with the run it belongs to.
pub struct Vault {
    root: PathBuf,
    vault: PathBuf,
    name: VaultName,
}

impl Vault {
    /// Generate `profile`'s tree under `root` and write the vault schema
    /// beside it.
    pub fn generate(root: &Path, profile: &str) -> Vault {
        let profile =
            Profile::by_name(profile).unwrap_or_else(|| panic!("no profile named `{profile}`"));
        let vault = root.join("vault");
        norn_fixtures::generate(&profile, SEED, &vault)
            .unwrap_or_else(|e| panic!("generating `{}`: {e}", profile.name));
        std::fs::create_dir_all(vault.join(".norn")).expect("create the vault's schema directory");
        std::fs::write(vault.join(".norn/schema.yaml"), SCHEMA).expect("write the vault schema");
        Vault::adopt(root)
    }

    /// The vault a tree already at `root` names.
    ///
    /// This is the child harness's way in: generation happens in the parent, so
    /// what the child costs is attachment alone.
    pub fn adopt(root: &Path) -> Vault {
        Vault {
            root: root.to_path_buf(),
            vault: root.join("vault"),
            name: VaultName::new(VAULT_NAME).expect("vault name"),
        }
    }

    pub fn name(&self) -> &VaultName {
        &self.name
    }

    pub fn path(&self) -> &Path {
        &self.vault
    }

    fn dirs(&self) -> ConfigDirs {
        ConfigDirs::new(self.root.join("config"), self.root.join("data"))
            .expect("config directories")
    }

    /// Where the attachment's derived database sits.
    ///
    /// A host never hands out the store it attached, so a suite that reads what
    /// an attachment derived opens the file itself — at the path the config
    /// directories put it, which is the same path the host resolved.
    pub fn database(&self) -> PathBuf {
        self.dirs().derived_dir(&self.name).join("store.sqlite3")
    }

    /// The derived store, opened directly.
    pub fn store(&self) -> Store {
        Store::open(self.database()).expect("open the derived store")
    }

    /// A host serving this vault, holding the real-watcher lease for as long
    /// as it serves.
    ///
    /// **The store page and the changeset are bounded well below the smallest
    /// profile these suites attach**, so every scale commits its heal in units
    /// of the same size. A bound a small vault never reaches is a working set
    /// that grows with the vault up to that bound, and a flatness pair drawn
    /// across it would be measuring the bound rather than the invariant.
    pub fn host(&self) -> ServingHost {
        let entry = Entry::new(
            self.name.clone(),
            VaultRoot::new(&self.vault).expect("vault root"),
        );
        let registry = RegistryRead::from_entries([entry.clone()]);
        let policy = ProductionPolicy::new(128, 128).expect("production policy");
        // Taken before the host exists, because the watcher is installed by
        // the attach the host runs and there is no later moment that is still
        // ahead of it.
        let lease = Lease::hold(isolation::REAL_WATCHER, lease_budget());
        let host = Host::new(
            registry,
            ProductionEntryOps::new(self.dirs(), policy),
            LifecyclePolicy {
                idle_after: IDLE_AFTER,
                worker_slots: 1,
                watch_poll_interval: Duration::from_millis(50),
            },
        )
        .expect("production host");
        ServingHost {
            host,
            _watcher_lease: lease,
        }
    }
}

/// The bound on taking the real-watcher lease here.
///
/// The hold window is a whole attachment rather than one wait: the lease is
/// taken before the host is built and let go when it drops, and what happens
/// in between is [`READY_LIMIT`] at its widest. One look at the lock is a
/// syscall, so the probe bound is the syscall's and not the attachment's.
fn lease_budget() -> Budget {
    isolation::acquisition_budget(Budget::new(READY_LIMIT, Duration::from_millis(250)))
}

/// A host, and the real-watcher lease that covers the watcher it installs.
///
/// **Every attachment these suites make installs a real platform watcher**,
/// because they attach through [`ProductionEntryOps`]; that watcher is a
/// subscription to the one service the operating system runs for the whole
/// machine, and past some number of live subscriptions the service reports
/// nothing at all to some of them. So the lease and the host are one value: a
/// host cannot be built here without the lease, and the lease cannot be let go
/// while the host that installed the watcher is still serving.
///
/// The lease is not reentrant, so this is the only place these suites take it.
/// A caller holds what this hands back and takes none of its own.
pub struct ServingHost {
    host: Host<ProductionEntryOps>,
    // Dropped after `host` by declaration order, which is the order that
    // matters: the watcher goes with the host, and the lease covers it.
    _watcher_lease: Lease,
}

impl Deref for ServingHost {
    type Target = Host<ProductionEntryOps>;

    fn deref(&self) -> &Self::Target {
        &self.host
    }
}

/// Attach `vault` through `host`, wait for it to become ready, and hand back
/// the demand that got it there.
///
/// **The lease comes back with the attachment rather than being dropped here.**
/// Demand is what an entry's idle reaper counts: an entry with no lease on it is
/// detached once the idle interval passes, and a run that keeps working the
/// host past that interval would lose the attachment underneath itself. Demand
/// is also the only thing that re-attaches an entry that went untrusted, so a
/// caller that means to keep being served holds one.
///
/// A caller that only wants the derived store on disk lets the lease drop: the
/// host it attached through is dropped in the same breath, and that detaches
/// the entry outright.
pub fn attach_and_wait(
    host: &Host<ProductionEntryOps>,
    name: &VaultName,
) -> DemandLease<ProductionEntryOps> {
    let lease = host
        .demand(name, AttachMode::Durable)
        .expect("request attachment");
    let deadline = Instant::now() + READY_LIMIT;
    loop {
        let observed = host.state(name);
        if observed == Ok(TrustState::Ready) {
            return lease;
        }
        assert!(
            Instant::now() < deadline,
            "attach did not converge inside {READY_LIMIT:?}; observed {observed:?}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// How many documents a store holds, read a bounded page at a time.
///
/// The page is far below the changeset a heal itself holds, so counting what an
/// attachment derived cannot be what a memory reading measures.
pub fn derived_documents(store: &mut Store) -> usize {
    let mut counted = 0;
    for_each_derived_path(store, |_| counted += 1);
    counted
}

/// Every derived path, handed over one at a time, a bounded page behind.
pub fn for_each_derived_path(store: &mut Store, mut visit: impl FnMut(&DocumentPath)) {
    /// How many derived rows are read at a time.
    const PAGE: usize = 64;

    let request = store.begin_request();
    let mut after: Option<DocumentPath> = None;
    loop {
        let page = request
            .stored_documents_after_ordered(after.as_ref(), PAGE, StoredPathOrder::Sensitive)
            .expect("reading a page of derived documents");
        let Some(last) = page.last() else {
            return;
        };
        after = Some(last.path.clone());
        for document in &page {
            visit(&document.path);
        }
    }
}
