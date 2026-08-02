#![cfg(feature = "induced-failure")]
#![allow(clippy::disallowed_methods)] // subprocess and filesystem acceptance fixture.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use norn_config::registry::{Entry, VaultRoot};
use norn_config::{ConfigDirs, VaultName};
use norn_host::{Host, LifecyclePolicy, ProductionEntryOps, ProductionPolicy, ServingRegistry};
use norn_store::{Change, DocumentFacts, DocumentPath, IncrementProvenance, Store};
use norn_wire::TrustState;

const CHILD_ENV: &str = "NORN_HOST_TORN_INCREMENT_CHILD";
const DATABASE_ENV: &str = "NORN_HOST_TORN_INCREMENT_DATABASE";
const WAIT_LIMIT: Duration = Duration::from_secs(30);

#[test]
fn next_attach_converges_after_process_death_mid_increment() {
    if std::env::var_os(CHILD_ENV).is_some() {
        tear_increment();
    }

    let fixture = Fixture::new();
    fixture.write("a.md", "old a\n");
    fixture.write("b.md", "old b\n");

    attach_and_wait(fixture.host(), &fixture.name);

    let before = read_rows(&fixture.database);
    assert_eq!(before.len(), 2);
    let generation_before = before[0].generation;
    assert!(before.iter().all(|row| row.generation == generation_before));

    // Filesystem reality advances while the derived store is detached. The
    // dying process then starts one deliberately false changeset and is killed
    // after applying two entries but before SQLite can commit any of it.
    fixture.write("a.md", "real a after crash\n");
    fixture.write("b.md", "real b after crash\n");
    fixture.write("c.md", "real c after crash\n");
    let output = Command::new(std::env::current_exe().expect("test executable"))
        .args([
            "--exact",
            "next_attach_converges_after_process_death_mid_increment",
            "--nocapture",
        ])
        .env(CHILD_ENV, "1")
        .env(DATABASE_ENV, &fixture.database)
        .output()
        .expect("run torn-increment subprocess");
    assert!(
        !output.status.success(),
        "induced process death unexpectedly succeeded\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    // No prefix of the false changeset and no generation belonging to it is at
    // rest. This is checked before healing so convergence cannot hide a tear.
    let after_death = read_rows(&fixture.database);
    assert_eq!(after_death.len(), 2);
    for row in &after_death {
        assert_eq!(row.generation, generation_before);
        let old = format!("old {}\n", row.path.as_str().trim_end_matches(".md"));
        assert_eq!(
            row.content_hash,
            norn_fs::ContentHash::of(old.as_bytes()).to_string()
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
    let converged = read_rows(&fixture.database);
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
            norn_fs::ContentHash::of(body.as_bytes()).to_string()
        );
        assert_eq!(row.byte_length, body.len() as u64);
    }
}

fn tear_increment() -> ! {
    let database =
        PathBuf::from(std::env::var_os(DATABASE_ENV).expect("database passed to child process"));
    let mut store = Store::open(database).expect("open seeded derived store");
    norn_store::induced_failure::abort_after_changeset_entries(2);
    let changes = ["a.md", "b.md", "c.md"].map(|path| {
        Change::Upsert(DocumentFacts::new(
            DocumentPath::new(path).expect("document path"),
            format!("false-{path}"),
            "content that never committed",
            28,
        ))
    });
    let _ = store
        .begin_request()
        .apply_increment(IncrementProvenance::Derived, changes);
    panic!("induced abort did not fire")
}

fn attach_and_wait(host: Host<ProductionEntryOps>, name: &VaultName) {
    host.demand(name).expect("request production attachment");
    let deadline = Instant::now() + WAIT_LIMIT;
    loop {
        let observed = host.state(name).expect("registered vault state");
        if observed == TrustState::Ready {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "attach did not converge: {observed:?}"
        );
        thread::sleep(Duration::from_millis(5));
    }
    drop(host);
}

fn read_rows(database: &Path) -> Vec<norn_store::StoredDocument> {
    let mut store = Store::open(database).expect("open derived store");
    store
        .begin_request()
        .stored_documents_after(None, 16)
        .expect("read stored documents")
}

struct Fixture {
    root: PathBuf,
    vault: PathBuf,
    database: PathBuf,
    name: VaultName,
}

impl Fixture {
    fn new() -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock after epoch")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "norn-host-kill-recovery-{}-{nonce}",
            std::process::id()
        ));
        let vault = root.join("vault");
        fs::create_dir_all(vault.join(".norn")).expect("create vault");
        fs::write(vault.join(".norn/schema.yaml"), "version: 1\n").expect("write schema");
        let name = VaultName::new("notes").expect("vault name");
        let dirs =
            ConfigDirs::new(root.join("config"), root.join("data")).expect("config directories");
        let database = dirs.derived_dir(&name).join("store.sqlite3");
        Self {
            root,
            vault,
            database,
            name,
        }
    }

    fn write(&self, path: &str, body: &str) {
        fs::write(self.vault.join(path), body).expect("write vault document");
    }

    fn host(&self) -> Host<ProductionEntryOps> {
        let entry = Entry::new(
            self.name.clone(),
            VaultRoot::new(&self.vault).expect("vault root"),
        );
        let registry = ServingRegistry::from_entries([entry.clone()]).expect("serving registry");
        let dirs = ConfigDirs::new(self.root.join("config"), self.root.join("data"))
            .expect("config directories");
        Host::new(
            registry,
            ProductionEntryOps::new([entry], dirs, ProductionPolicy::new(2, 8)),
            LifecyclePolicy {
                idle_after: Duration::from_secs(60),
                worker_slots: 1,
                watch_poll_interval: Duration::from_millis(5),
            },
        )
        .expect("production host")
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}
