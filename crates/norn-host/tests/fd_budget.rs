#![cfg(any(target_os = "linux", target_os = "macos"))]
#![allow(clippy::disallowed_methods)] // probe inspects process FDs; fixtures impersonate editors.

use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use norn_config::registry::{Entry, VaultRoot};
use norn_config::{ConfigDirs, VaultName};
use norn_host::{Host, LifecyclePolicy, ProductionEntryOps, ProductionPolicy, ServingRegistry};
use norn_wire::TrustState;

const PROBE_ENV: &str = "NORN_HOST_FD_BUDGET_PROBE";
const FD_BUDGET: usize = 12;
const LARGE_VAULT_DOCUMENTS: usize = 2_000;
const WAIT_LIMIT: Duration = Duration::from_secs(30);

#[test]
fn attached_entry_has_a_bounded_vault_size_independent_fd_cost() {
    if std::env::var_os(PROBE_ENV).is_some() {
        run_probe();
        return;
    }

    // Isolate the descriptor accounting from cargo's test harness and from
    // other tests that may open files concurrently in this process.
    let output = Command::new(std::env::current_exe().expect("test executable"))
        .args([
            "--exact",
            "attached_entry_has_a_bounded_vault_size_independent_fd_cost",
            "--nocapture",
        ])
        .env(PROBE_ENV, "1")
        .output()
        .expect("run descriptor probe subprocess");
    assert!(
        output.status.success(),
        "descriptor probe failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}

fn run_probe() {
    let fixture = Fixture::new();
    fixture.write_documents(1);
    let baseline = open_fd_count();

    let host = fixture.host();
    attach_and_wait(&host, &fixture.name);
    let one_document = open_fd_count();
    let one_document_delta = one_document.checked_sub(baseline).unwrap_or_else(|| {
        panic!("ready descriptor count {one_document} fell below baseline {baseline}")
    });
    assert!(
        one_document_delta <= FD_BUDGET,
        "one-document attachment used {one_document_delta} descriptors; budget is {FD_BUDGET}"
    );
    detach_and_wait(&host, &fixture.name);
    assert_eq!(
        open_fd_count(),
        baseline,
        "one-document detach retained descriptors"
    );

    fixture.write_documents(LARGE_VAULT_DOCUMENTS);
    attach_and_wait(&host, &fixture.name);
    let large_vault = open_fd_count();
    let large_vault_delta = large_vault.checked_sub(baseline).unwrap_or_else(|| {
        panic!("ready descriptor count {large_vault} fell below baseline {baseline}")
    });
    assert!(
        large_vault_delta <= FD_BUDGET,
        "2k-document attachment used {large_vault_delta} descriptors; budget is {FD_BUDGET}"
    );
    assert_eq!(
        large_vault_delta, one_document_delta,
        "descriptor cost changed with vault size"
    );
    detach_and_wait(&host, &fixture.name);
    assert_eq!(
        open_fd_count(),
        baseline,
        "2k-document detach retained descriptors"
    );

    // Exercise reattachment once more so a one-shot cleanup path cannot make
    // the two principal measurements pass accidentally.
    attach_and_wait(&host, &fixture.name);
    assert_eq!(
        open_fd_count().saturating_sub(baseline),
        large_vault_delta,
        "descriptor cost changed across attach cycles"
    );
    detach_and_wait(&host, &fixture.name);
    assert_eq!(
        open_fd_count(),
        baseline,
        "repeat detach retained descriptors"
    );
}

fn attach_and_wait(host: &Host<ProductionEntryOps>, name: &VaultName) {
    host.demand(name).expect("request attachment");
    wait_for_state(host, name, TrustState::Ready);
}

fn detach_and_wait(host: &Host<ProductionEntryOps>, name: &VaultName) {
    host.reap_idle(Instant::now() + Duration::from_secs(2))
        .expect("schedule idle detach");
    wait_for_state(host, name, TrustState::Unattached);
}

fn wait_for_state(host: &Host<ProductionEntryOps>, name: &VaultName, expected: TrustState) {
    let deadline = Instant::now() + WAIT_LIMIT;
    loop {
        let observed = host.state(name).expect("registered vault state");
        if observed == expected {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {expected:?}; observed {observed:?}"
        );
        thread::sleep(Duration::from_millis(5));
    }
}

fn open_fd_count() -> usize {
    #[cfg(target_os = "linux")]
    const FD_DIRECTORY: &str = "/proc/self/fd";
    #[cfg(target_os = "macos")]
    const FD_DIRECTORY: &str = "/dev/fd";

    let entries = fs::read_dir(FD_DIRECTORY).expect("open process descriptor directory");
    // Reading either descriptor directory holds one descriptor for the
    // iterator itself, and that descriptor is visible in the listing.
    entries
        .count()
        .checked_sub(1)
        .expect("descriptor directory iterator is represented in its listing")
}

struct Fixture {
    root: PathBuf,
    vault: PathBuf,
    name: VaultName,
}

impl Fixture {
    fn new() -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock after epoch")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "norn-host-fd-budget-{}-{nonce}",
            std::process::id()
        ));
        let vault = root.join("vault");
        fs::create_dir_all(vault.join(".norn")).expect("create vault");
        fs::write(vault.join(".norn/schema.yaml"), "version: 1\n").expect("write schema");
        Self {
            root,
            vault,
            name: VaultName::new("notes").expect("vault name"),
        }
    }

    fn host(&self) -> Host<ProductionEntryOps> {
        let entry = Entry::new(
            self.name.clone(),
            VaultRoot::new(&self.vault).expect("vault root"),
        );
        let registry = ServingRegistry::from_entries([entry.clone()]).expect("serving registry");
        let dirs = ConfigDirs::new(self.root.join("config"), self.root.join("data"))
            .expect("config directories");
        let ops = ProductionEntryOps::new([entry], dirs, ProductionPolicy::new(64, 64).unwrap());
        Host::new(
            registry,
            ops,
            LifecyclePolicy {
                idle_after: Duration::from_secs(1),
                worker_slots: 1,
                watch_poll_interval: Duration::from_millis(5),
            },
        )
        .expect("host")
    }

    fn write_documents(&self, count: usize) {
        for index in 0..count {
            let path = self.vault.join(format!("note-{index:04}.md"));
            fs::write(path, format!("# Note {index}\n")).expect("write document");
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}
