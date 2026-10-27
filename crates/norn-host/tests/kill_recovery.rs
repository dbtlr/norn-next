#![cfg(feature = "induced-failure")]
#![allow(clippy::disallowed_methods)] // subprocess and filesystem acceptance fixture.

// Compiled for two items: the budget every wait on a published label obeys,
// and the declaration the attached store pins. How long one such look may take
// is a fact about this crate's host and not about this suite, and the
// declaration a changeset is judged under is the pinned one in every suite, so
// both are composed where the other suites read them from.
mod attach;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use norn_config::ConfigDirs;
use norn_config::registry::{Entry, VaultRoot};
use norn_host::{
    Host, LifecyclePolicy, LoadingBoundary, LoadingProbe, ProductionEntryOps, ProductionPolicy,
    RegistryRead,
};
use norn_store::induced_failure::{ARM_HITS, INCREMENT_SEAM};
use norn_store::{
    Change, DocumentFacts, DocumentPath, IncrementProvenance, Store, StoredPathOrder,
};
use norn_testkit::attestation::{Attestation, SEAM};
use norn_testkit::isolation::{self, Lease};
use norn_testkit::scratch::Scratch;
use norn_testkit::wait::{Observed, wait_until};
use norn_wire::{ErrorEnvelope, ReasonCode, TrustState, VaultName};

const CHILD_ENV: &str = "NORN_HOST_TORN_INCREMENT_CHILD";
const DATABASE_ENV: &str = "NORN_HOST_TORN_INCREMENT_DATABASE";
const VAULT_ENV: &str = "NORN_HOST_TORN_INCREMENT_VAULT";
const BUILD_CHILD_ENV: &str = "NORN_HOST_KILLED_BUILD_CHILD";
const WAIT_LIMIT: Duration = Duration::from_secs(30);

/// The documents the killed build is over: enough for the build to cross
/// several pages of two.
const BUILD_DOCUMENTS: usize = 8;

#[test]
#[ignore = "fault-lane case: induced recovery runs in full regression"]
fn next_attach_converges_after_process_death_mid_increment() {
    if std::env::var_os(CHILD_ENV).is_some() {
        tear_increment();
    }

    let fixture = Fixture::new();
    fixture.write("a.md", "old a\n");
    fixture.write("b.md", "old b\n");

    attach_and_wait(fixture.host(), &fixture.name);

    let before = read_rows(&fixture.database, &fixture.vault);
    assert_eq!(before.len(), 2);
    let generation_before = before[0].generation;
    assert!(before.iter().all(|row| row.generation == generation_before));

    // Filesystem reality advances while the derived store is detached. The
    // dying process then starts one deliberately false changeset and is killed
    // after applying two entries but before SQLite can commit any of it.
    fixture.write("a.md", "real a after crash\n");
    fixture.write("b.md", "real b after crash\n");
    fixture.write("c.md", "real c after crash\n");
    // The record file exists before the child starts, so an absent record is
    // a fact about the seam rather than about a sink nothing could write.
    fs::write(&fixture.hits, "").expect("create the arm record file");
    let output = Command::new(std::env::current_exe().expect("test executable"))
        .args([
            "--exact",
            "next_attach_converges_after_process_death_mid_increment",
            "--nocapture",
            "--include-ignored",
        ])
        .env(CHILD_ENV, "1")
        .env(DATABASE_ENV, &fixture.database)
        .env(VAULT_ENV, &fixture.vault)
        .env(ARM_HITS, &fixture.hits)
        .output()
        .expect("run torn-increment subprocess");
    assert!(
        !output.status.success(),
        "induced process death unexpectedly succeeded\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    // A child that ends any other way than at the tear — a refused changeset
    // that panics, for one — also fails, and leaves the store as the tear
    // would. The record is what says the changeset was open when it ended.
    let torn = Attestation::read(&fixture.hits);
    torn.assert_reached(
        "a process ended between two entries of a changeset",
        &[(SEAM, INCREMENT_SEAM), ("boundary", "entries")],
    );
    torn.assert_count("a process ended between two entries of a changeset", 1);
    eprintln!("torn-increment child reached {INCREMENT_SEAM} exactly once before process death");

    // No prefix of the false changeset and no generation belonging to it is at
    // rest. This is checked before healing so convergence cannot hide a tear.
    let after_death = read_rows(&fixture.database, &fixture.vault);
    assert_eq!(after_death.len(), 2);
    for row in &after_death {
        assert_eq!(row.generation, generation_before);
        let old = format!("old {}\n", row.path.as_str().trim_end_matches(".md"));
        assert_eq!(
            row.content_hash,
            Some(norn_fs::ContentHash::of(old.as_bytes()).to_string())
        );
    }
    assert!(
        after_death.iter().all(|row| row.path.as_str() != "c.md"),
        "an uncommitted new document survived process death"
    );

    // The ordinary production attach owns recovery: its hash-authoritative
    // ordered heal must replace both stale rows and insert the new filesystem
    // document without any crash-specific repair path.
    attach_and_wait(fixture.host(), &fixture.name);
    let converged = read_rows(&fixture.database, &fixture.vault);
    assert_eq!(converged.len(), 3);
    let healed_generation = converged[0].generation;
    assert_eq!(
        healed_generation,
        generation_before + 1,
        "the torn transaction must not consume a partial generation"
    );
    assert!(
        converged
            .iter()
            .all(|row| row.generation == healed_generation)
    );
    for (path, body) in [
        ("a.md", "real a after crash\n"),
        ("b.md", "real b after crash\n"),
        ("c.md", "real c after crash\n"),
    ] {
        let row = converged
            .iter()
            .find(|row| row.path.as_str() == path)
            .expect("healed document");
        assert_eq!(
            row.content_hash,
            Some(norn_fs::ContentHash::of(body.as_bytes()).to_string())
        );
        assert_eq!(row.byte_length, Some(body.len() as u64));
    }
}

/// Ends the process at the second canonical page of the build it is installed on.
///
/// The canonical page boundary is the only one that counts: intake changesets
/// into the disposable metadata database advance the same changeset counter as
/// the canonical store's, so a counter-armed abort would fire inside them.
struct DieAtTheSecondPage(AtomicUsize);

impl LoadingProbe for DieAtTheSecondPage {
    fn at(&self, boundary: LoadingBoundary<'_>) {
        if matches!(boundary, LoadingBoundary::CanonicalPage)
            && self.0.fetch_add(1, Ordering::SeqCst) + 1 == 2
        {
            std::process::abort();
        }
    }
}

fn body_of(index: usize) -> String {
    format!("# Document {index}\n\nA body for document {index}.\n")
}

/// **A process killed mid-way through a vault's first build leaves the mark, and
/// the next attach builds from zero instead of resuming.**
///
/// The child attaches a fresh vault of eight documents through the real host,
/// two to a page, and ends at the second canonical page: at least one changeset
/// of the build has committed and the build has not ended. The parent reads the
/// store through a read-only handle before any attach — so the case cannot pass
/// by killing the child before it wrote anything, or after it finished — and
/// finds the mark standing over some but not all of the rows. The next attach
/// opens a rebuilt store (a new epoch, so no row was carried), reaches `Ready`,
/// and holds every document at the hash of its bytes with the mark gone.
#[test]
#[ignore = "fault-lane case: induced recovery runs in full regression"]
fn next_attach_rebuilds_from_zero_after_process_death_mid_build() {
    if let Some(root) = std::env::var_os(BUILD_CHILD_ENV) {
        die_mid_build(Path::new(&root));
    }

    let mut fixture = Fixture::unleased(Scratch::new("norn-host-kill-mid-build"));
    for index in 0..BUILD_DOCUMENTS {
        fixture.write(&format!("doc-{index}.md"), &body_of(index));
    }
    let output = Command::new(std::env::current_exe().expect("test executable"))
        .args([
            "--exact",
            "next_attach_rebuilds_from_zero_after_process_death_mid_build",
            "--nocapture",
            "--include-ignored",
        ])
        .env(BUILD_CHILD_ENV, fixture.root.root())
        .output()
        .expect("run killed-build subprocess");
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        assert_eq!(
            output.status.signal(),
            Some(6),
            "the child ended some way other than the abort the probe raises\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
    }

    // Read before any attach, through a handle that cannot write: the mark
    // stands over a partial derivation.
    let (marked, rows, epoch) = read_the_store_raw(&fixture.database);
    assert!(marked, "the killed build left no mark");
    assert!(
        0 < rows && rows < BUILD_DOCUMENTS,
        "the child ended with {rows} of {BUILD_DOCUMENTS} rows, so the kill did not land mid-build"
    );

    fixture.lease();
    attach_and_wait(fixture.host(), &fixture.name);

    let mut store = Store::open(
        &fixture.database,
        proven_order(&fixture.vault),
        norn_host::DERIVATION_VERSION,
    )
    .expect("open the rebuilt store");
    assert_eq!(*store.open_outcome(), norn_store::OpenOutcome::Reused);
    assert!(!store.under_construction());
    assert_ne!(
        store.epoch(),
        epoch,
        "the attach resumed over the killed build's store"
    );
    let rows = store
        .begin_request()
        .stored_documents_after_ordered(None, 16, StoredPathOrder::Sensitive)
        .expect("read stored documents");
    assert_eq!(rows.len(), BUILD_DOCUMENTS);
    for index in 0..BUILD_DOCUMENTS {
        let row = rows
            .iter()
            .find(|row| row.path.as_str() == format!("doc-{index}.md"))
            .expect("a derived document");
        assert_eq!(
            row.content_hash,
            Some(norn_fs::ContentHash::of(body_of(index).as_bytes()).to_string())
        );
    }
}

/// Whether the mark stands, how many document rows the store holds, and its
/// epoch — read through a read-only handle.
fn read_the_store_raw(database: &Path) -> (bool, usize, String) {
    let open = norn_db::open_read_only(database);
    let database = open.adopted.expect("a read-only open of the killed store");
    let connection = database.connection();
    let marked = norn_db::meta::get_meta::<norn_db::rusqlite::types::Value>(
        connection,
        "under_construction",
    )
    .expect("reading the mark")
    .is_some();
    let rows: i64 = connection
        .query_row("SELECT COUNT(*) FROM documents", [], |row| row.get(0))
        .expect("counting rows");
    let epoch: String = norn_db::meta::get_meta(connection, norn_db::meta::STORE_EPOCH)
        .expect("reading the epoch")
        .expect("a created store has an epoch");
    (marked, rows as usize, epoch)
}

/// The child: attach the vault under `root` and be killed by the probe.
fn die_mid_build(root: &Path) -> ! {
    let _lease = Lease::hold(
        isolation::REAL_WATCHER,
        isolation::acquisition_budget(attach::state_budget(WAIT_LIMIT)),
    );
    let name = VaultName::new("notes").expect("vault name");
    let host = production_host(
        root,
        &root.join("vault"),
        &name,
        ProductionPolicy::new(2, 2).unwrap(),
        |ops| ops.with_loading_probe(Arc::new(DieAtTheSecondPage(AtomicUsize::new(0)))),
    );
    attach_and_wait(host, &name);
    panic!("the build finished instead of being killed")
}

fn tear_increment() -> ! {
    let database =
        PathBuf::from(std::env::var_os(DATABASE_ENV).expect("database passed to child process"));
    let vault = PathBuf::from(std::env::var_os(VAULT_ENV).expect("vault passed to child process"));
    let mut store = Store::open(
        database,
        proven_order(&vault),
        norn_host::DERIVATION_VERSION,
    )
    .expect("open seeded derived store");
    norn_store::induced_failure::abort_after_changeset_entries(2);
    let changes = ["a.md", "b.md", "c.md"].map(|path| {
        Change::Upsert(DocumentFacts::new(
            DocumentPath::new(path).expect("document path"),
            format!("false-{path}"),
            "content that never committed",
            28,
        ))
    });
    // The attach pinned the vault schema, and a changeset judged under any
    // other declaration is refused before its first entry is written.
    let declared = attach::read::the_pinned_declaration(&mut store);
    let returned = store.begin_request().apply_increment(
        IncrementProvenance::Derived,
        changes,
        &[],
        &declared,
    );
    panic!("the changeset returned instead of tearing: {returned:?}")
}

fn attach_and_wait(host: Host<ProductionEntryOps>, name: &VaultName) {
    host.demand(name).expect("request production attachment");
    wait_until(
        &format!("the entry under `{name}` to serve the vault"),
        attach::state_budget(WAIT_LIMIT),
        || {
            let observed = host.state(name);
            if observed == Ok(TrustState::Ready) {
                return Observed::Met(());
            }
            assert!(
                !names_no_vault(&observed),
                "the host serves no vault under `{name}`: {observed:?}"
            );
            Observed::pending(format!("the state is {observed:?}"))
        },
    )
    .unwrap_or_else(|failure| panic!("{failure}"));
    drop(host);
}

/// Whether what the host answered is the refusal a name it holds no entry under
/// is refused with.
///
/// A wait polls an entry on its way somewhere. A name no entry stands behind is
/// a mistake in the case rather than a state converging, and it converges on
/// nothing, so it ends the wait where it is found instead of at the deadline.
fn names_no_vault(observed: &Result<TrustState, ErrorEnvelope>) -> bool {
    matches!(observed, Err(envelope) if envelope.code() == &ReasonCode::HostUnknownVault)
}

/// The order the attach opens a store over `vault` under: the one the root
/// proves. An open under the other one would rebuild the store it is reading.
fn proven_order(vault: &Path) -> StoredPathOrder {
    norn_host::stored_path_order(
        norn_fs::PathNormalizer::detect(vault)
            .expect("detect the vault's case behaviour")
            .case_sensitivity(),
    )
}

fn read_rows(database: &Path, vault: &Path) -> Vec<norn_store::StoredDocument> {
    let mut store = Store::open(database, proven_order(vault), norn_host::DERIVATION_VERSION)
        .expect("open derived store");
    store
        .begin_request()
        .stored_documents_after_ordered(None, 16, StoredPathOrder::Sensitive)
        .expect("read stored documents")
}

struct Fixture {
    // The naming and the removal are the scratch helper's; what this fixture
    // adds is the vault tree inside and the lease beside it.
    root: Scratch,
    vault: PathBuf,
    database: PathBuf,
    // Where the child's changeset tear records that it fired.
    hits: PathBuf,
    name: VaultName,
    // Both attaches below go through production entry operations, and each
    // installs a real platform watcher. The lease covers the fixture's whole
    // life, which spans both of them and the induced process death between.
    //
    // The child this case spawns takes none of its own: it opens the derived
    // store and dies inside a changeset, and it never attaches.
    //
    // A case whose child attaches takes none up front: the child holds the
    // machine's one live watcher while it runs, and the parent takes it back
    // with [`Fixture::lease`] once the child is gone.
    _watcher_lease: Option<Lease>,
}

impl Fixture {
    fn new() -> Self {
        let mut fixture = Self::unleased(Scratch::new("norn-host-kill-recovery"));
        fixture.lease();
        fixture
    }

    /// Take the real-watcher lease, for the rest of the fixture's life.
    fn lease(&mut self) {
        self._watcher_lease = Some(Lease::hold(
            isolation::REAL_WATCHER,
            isolation::acquisition_budget(attach::state_budget(WAIT_LIMIT)),
        ));
    }

    /// A fixture over `root` that holds no lease yet.
    fn unleased(root: Scratch) -> Self {
        let vault = root.join("vault");
        fs::create_dir_all(vault.join(".norn")).expect("create vault");
        fs::write(vault.join(".norn/schema.yaml"), "version: 1\n").expect("write schema");
        let name = VaultName::new("notes").expect("vault name");
        let dirs =
            ConfigDirs::new(root.join("config"), root.join("data")).expect("config directories");
        let database = dirs.derived_dir(&name).join("store.sqlite3");
        let hits = root.join("arm-hits");
        Self {
            root,
            vault,
            database,
            hits,
            name,
            _watcher_lease: None,
        }
    }

    fn write(&self, path: &str, body: &str) {
        fs::write(self.vault.join(path), body).expect("write vault document");
    }

    fn host(&self) -> Host<ProductionEntryOps> {
        // Eight exceeds this fixture's three documents, so healing uses
        // one increment and the generation assertion remains exact.
        self.host_over(ProductionPolicy::new(2, 8).unwrap(), |ops| ops)
    }

    /// A host whose entry operations `shape` may adjust, such as to install a probe.
    fn host_over(
        &self,
        policy: ProductionPolicy,
        shape: impl FnOnce(ProductionEntryOps) -> ProductionEntryOps,
    ) -> Host<ProductionEntryOps> {
        production_host(self.root.root(), &self.vault, &self.name, policy, shape)
    }
}

/// A production host over `vault`, with the machine-local directories under `root`.
fn production_host(
    root: &Path,
    vault: &Path,
    name: &VaultName,
    policy: ProductionPolicy,
    shape: impl FnOnce(ProductionEntryOps) -> ProductionEntryOps,
) -> Host<ProductionEntryOps> {
    let entry = Entry::new(name.clone(), VaultRoot::new(vault).expect("vault root"));
    let registry = RegistryRead::from_entries([entry]);
    let dirs = ConfigDirs::new(root.join("config"), root.join("data")).expect("config directories");
    Host::new(
        registry,
        shape(ProductionEntryOps::new(dirs, policy)),
        LifecyclePolicy {
            idle_after: Duration::from_secs(60),
            worker_slots: 1,
            watch_poll_interval: Duration::from_millis(5),
            read_settle_bound: norn_host::READ_SETTLE_BOUND,
        },
    )
    .expect("production host")
}
