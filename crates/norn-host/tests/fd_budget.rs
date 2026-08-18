#![cfg(any(target_os = "linux", target_os = "macos"))]
#![allow(clippy::disallowed_methods)] // probe inspects process FDs; fixtures impersonate editors.

use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use norn_config::ConfigDirs;
use norn_config::registry::{Entry, VaultRoot};
use norn_host::{
    AttachMode, DemandLease, Host, LifecyclePolicy, ProductionEntryOps, ProductionPolicy,
    RegistryRead,
};
use norn_testkit::isolation::{self, Lease};
use norn_testkit::wait::{Budget, Observed, wait_until};
use norn_wire::{ErrorEnvelope, ReasonCode, TrustState, VaultName};

const PROBE_ENV: &str = "NORN_HOST_FD_BUDGET_PROBE";

/// How many descriptors one served attachment may hold.
///
/// **An authored threshold, and it moves only by a reviewed edit with grounds**
/// — the same discipline the bands in `tests/baselines/mod.rs` carry, under
/// [ADR 0007](../../../docs/decisions/0007-authored-measurement-thresholds.md).
/// It sits here rather than in that module because its subject is this file's
/// probe: a count of this process's own descriptors, which no other suite
/// takes.
///
/// The count is taken after the attachment reaches `Ready`, so what it bounds
/// is the steady-state cost of a served attachment rather than the high-water
/// mark of the heal walk that got there. Observed on macos-arm64 over two
/// consecutive runs, identical in both: **4 descriptors per attachment at 1
/// document and 4 at 2000** — the readings this file records through
/// `norn_testkit::readings::record` below. The budget is 12, three times the
/// measured cost, so a subject that starts holding one more handle per
/// subscription or per store file is caught while a run whose runner hands the
/// process an extra descriptor at the sampling instant is not.
///
/// The bar the vault-size claim rests on is not this ceiling: the two deltas
/// are asserted **equal**, which fails the moment the cost starts moving with
/// the vault at any height under the budget.
const FD_BUDGET: usize = 12;
const LARGE_VAULT_DOCUMENTS: usize = 2_000;
const WAIT_LIMIT: Duration = Duration::from_secs(30);

/// How long one look at what the host publishes may take.
///
/// A look is a lock and a label, so this separates a machine that has stopped
/// answering from a state that has not arrived — which is [`WAIT_LIMIT`]'s
/// question and not this one's.
const STATE_PROBE: Duration = Duration::from_millis(250);

/// The line the probe prints its measurement on, which the parent records.
///
/// The probe's own output never reaches a person: it runs as a subprocess whose
/// streams the parent captures and prints on failure alone. **A bar that passes
/// says only that the cost fit**, and what the cost was is the reading this
/// budget exists to hold — so the number crosses back out of the probe and is
/// recorded beside the budget it was judged against.
const MEASUREMENT_PREFIX: &str = "fd-budget ";

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
    record_the_measurement(&String::from_utf8_lossy(&output.stdout));
}

/// Record what the probe measured, off the line it printed.
///
/// A probe that passed its bars and printed no measurement is a reading lost
/// rather than a bar failed, so this fails saying so: the parent has no other
/// way to learn what the attachment cost.
fn record_the_measurement(reported: &str) {
    let measurement = reported
        .lines()
        .find_map(|line| line.strip_prefix(MEASUREMENT_PREFIX))
        .unwrap_or_else(|| {
            panic!(
                "the descriptor probe passed its bars and printed no `{MEASUREMENT_PREFIX}` line, \
                 so what the attachment cost is not recorded anywhere: {reported}"
            )
        });
    let reading = |key: &str| {
        field(measurement, key)
            .unwrap_or_else(|| panic!("`{measurement}` does not carry `{key}`"))
            .to_string()
    };
    norn_testkit::readings::record(
        "attach descriptor cost",
        &[
            ("descriptors before attaching", reading("baseline=")),
            (
                "descriptors per attachment, 1 document",
                reading("one_document="),
            ),
            (
                "descriptors per attachment, 2000 documents",
                reading("large_vault="),
            ),
            ("budget", FD_BUDGET.to_string()),
        ],
    );
}

/// The value of `key` in the probe's measurement line, up to the next space.
fn field<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let (_, rest) = line.split_once(key)?;
    Some(rest.split_whitespace().next().unwrap_or(rest))
}

fn run_probe() {
    let fixture = Fixture::new();
    fixture.write_documents(1);
    let baseline = open_fd_count();

    let host = fixture.host();
    let lease = attach_and_wait(&host, &fixture.name);
    let one_document = open_fd_count();
    let one_document_delta = one_document.checked_sub(baseline).unwrap_or_else(|| {
        panic!("ready descriptor count {one_document} fell below baseline {baseline}")
    });
    assert!(
        one_document_delta <= FD_BUDGET,
        "one-document attachment used {one_document_delta} descriptors; budget is {FD_BUDGET}"
    );
    drop(lease);
    detach_and_wait(&host, &fixture.name);
    assert_eq!(
        open_fd_count(),
        baseline,
        "one-document detach retained descriptors"
    );

    fixture.write_documents(LARGE_VAULT_DOCUMENTS);
    let lease = attach_and_wait(&host, &fixture.name);
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
    drop(lease);
    detach_and_wait(&host, &fixture.name);
    assert_eq!(
        open_fd_count(),
        baseline,
        "2k-document detach retained descriptors"
    );

    // Exercise reattachment once more so a one-shot cleanup path cannot make
    // the two principal measurements pass accidentally.
    let lease = attach_and_wait(&host, &fixture.name);
    assert_eq!(
        open_fd_count().saturating_sub(baseline),
        large_vault_delta,
        "descriptor cost changed across attach cycles"
    );
    drop(lease);
    detach_and_wait(&host, &fixture.name);
    assert_eq!(
        open_fd_count(),
        baseline,
        "repeat detach retained descriptors"
    );

    report_the_measurement(baseline, one_document_delta, large_vault_delta);
}

/// What the probe hands its parent: the counts behind the bars above.
///
/// Printed last, so a line reaching the parent is a measurement of a probe that
/// ran every one of its assertions rather than of one that stopped part-way.
#[allow(clippy::disallowed_macros)] // The probe's measurement is a machine-consumed stream its parent reads.
fn report_the_measurement(baseline: usize, one_document: usize, large_vault: usize) {
    println!(
        "{MEASUREMENT_PREFIX}baseline={baseline} one_document={one_document} \
         large_vault={large_vault} budget={FD_BUDGET}"
    );
}

#[must_use]
fn attach_and_wait(
    host: &Host<ProductionEntryOps>,
    name: &VaultName,
) -> DemandLease<ProductionEntryOps> {
    let lease = host
        .demand(name, AttachMode::Durable)
        .expect("request attachment");
    wait_for_state(host, name, TrustState::Ready);
    lease
}

fn detach_and_wait(host: &Host<ProductionEntryOps>, name: &VaultName) {
    host.reap_idle(Instant::now() + Duration::from_secs(2))
        .expect("schedule idle detach");
    wait_for_state(host, name, TrustState::Unattached);
}

/// Wait for the host to answer one exact trust state.
///
/// **The state waited for is one that crosses as a label.** A state a poll does
/// not walk out of is answered as an envelope carrying its reason, so it never
/// equals the label this compares against and a caller naming one would spend
/// the whole budget before saying so. The probe waits for `Ready` and
/// `Unattached`, which are the two the descriptor readings are taken at.
fn wait_for_state(host: &Host<ProductionEntryOps>, name: &VaultName, expected: TrustState) {
    debug_assert!(
        expected.refusal().is_none(),
        "{expected:?} crosses as a refusal, so no label ever equals it"
    );
    wait_until(
        &format!("the entry under `{name}` to publish {expected:?}"),
        Budget::new(WAIT_LIMIT, STATE_PROBE),
        || {
            let observed = host.state(name);
            if observed.as_ref() == Ok(&expected) {
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
}

/// Whether what the host answered is the refusal a name it holds no entry under
/// is refused with.
///
/// A wait polls an entry on its way somewhere. A name no entry stands behind is
/// a mistake in the probe rather than a state converging, and it converges on
/// nothing, so it ends the wait where it is found instead of at the deadline.
fn names_no_vault(observed: &Result<TrustState, ErrorEnvelope>) -> bool {
    matches!(observed, Err(envelope) if envelope.code() == &ReasonCode::HostUnknownVault)
}

fn open_fd_count() -> usize {
    norn_testkit::process::open_fd_count().expect("this process's descriptor count")
}

struct Fixture {
    root: PathBuf,
    vault: PathBuf,
    name: VaultName,
    // The probe attaches through production entry operations, and an
    // attachment installs a real platform watcher. The lease makes this
    // process's the only live one on the machine for as long as the fixture
    // lasts, which is longer than any host it builds.
    //
    // It is taken here rather than around each attach so that the descriptor
    // it costs is inside the baseline every delta below is measured against.
    _watcher_lease: Lease,
}

impl Fixture {
    fn new() -> Self {
        let lease = Lease::hold(
            isolation::REAL_WATCHER,
            isolation::acquisition_budget(Budget::new(WAIT_LIMIT, STATE_PROBE)),
        );
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
            _watcher_lease: lease,
        }
    }

    fn host(&self) -> Host<ProductionEntryOps> {
        let entry = Entry::new(
            self.name.clone(),
            VaultRoot::new(&self.vault).expect("vault root"),
        );
        let registry = RegistryRead::from_entries([entry.clone()]);
        let dirs = ConfigDirs::new(self.root.join("config"), self.root.join("data"))
            .expect("config directories");
        let ops = ProductionEntryOps::new(dirs, ProductionPolicy::new(64, 64).unwrap());
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
