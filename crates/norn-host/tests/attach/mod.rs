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
//! Both binaries that compile this module use part of it — the child harnesses
//! adopt a tree the parent generated rather than generating one — so an unused
//! remainder in either is the layout rather than a defect.
#![allow(dead_code)]
#![allow(clippy::disallowed_methods)] // Harness scaffolding: this suite's own generated tree.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use norn_config::registry::{Entry, VaultRoot};
use norn_config::{ConfigDirs, VaultName};
use norn_fixtures::Profile;
use norn_host::{Host, LifecyclePolicy, ProductionEntryOps, ProductionPolicy, ServingRegistry};
use norn_store::Store;
use norn_wire::TrustState;

/// The seed every generated tree is drawn at. One value, so two readings
/// differ by the profile alone.
pub const SEED: u64 = 7;

/// The vault the registry names. One entry per host, so the name is fixed.
const VAULT_NAME: &str = "notes";

/// The in-vault schema attachment pins.
const SCHEMA: &[u8] = b"version: 1\n";

/// How long a suite waits for an attachment to converge.
///
/// A bound on whether the state ever converged, and nothing on how quickly: the
/// profiles here reach thousands of documents, and a bound near the time an
/// attach usually takes would make machine speed a test result.
pub const READY_LIMIT: Duration = Duration::from_secs(600);

/// A directory one case owns, under this target's own temporary directory, and
/// which is removed with it.
///
/// The name carries the process id and a counter, so two cases in one run never
/// meet in a tree.
pub struct Scratch {
    root: PathBuf,
}

impl Scratch {
    pub fn new(label: &str) -> Scratch {
        static SERIAL: AtomicU64 = AtomicU64::new(0);
        let serial = SERIAL.fetch_add(1, Ordering::Relaxed);
        let root = Path::new(env!("CARGO_TARGET_TMPDIR"))
            .join(format!("{label}-{}-{serial}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("a scratch directory");
        Scratch { root }
    }

    pub fn path(&self) -> &Path {
        &self.root
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
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

    /// A host serving this vault, with the whole tree reachable in as few
    /// transactions as the store's own bounds allow.
    pub fn host(&self) -> Host<ProductionEntryOps> {
        let entry = Entry::new(
            self.name.clone(),
            VaultRoot::new(&self.vault).expect("vault root"),
        );
        let registry = ServingRegistry::from_entries([entry.clone()]).expect("serving registry");
        let policy = ProductionPolicy::new(1024, 1024).expect("production policy");
        Host::new(
            registry,
            ProductionEntryOps::new([entry], self.dirs(), policy),
            LifecyclePolicy {
                // Longer than any run here, so nothing detaches underneath a
                // measurement that did not ask for it.
                idle_after: Duration::from_secs(3600),
                worker_slots: 1,
                watch_poll_interval: Duration::from_millis(50),
            },
        )
        .expect("production host")
    }
}

/// Attach `vault` through `host` and wait for the state it converges to.
///
/// The lease is dropped here: what the suites measure is an attachment that
/// reached [`TrustState::Ready`], and holding demand past that only keeps the
/// reaper away from something no run here reaps.
pub fn attach_and_wait(host: &Host<ProductionEntryOps>, name: &VaultName) {
    host.demand(name).expect("request attachment");
    let deadline = Instant::now() + READY_LIMIT;
    loop {
        let observed = host.state(name).expect("registered vault state");
        if observed == TrustState::Ready {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "attach did not converge inside {READY_LIMIT:?}; observed {observed:?}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}
