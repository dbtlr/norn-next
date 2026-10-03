#![cfg(any(target_os = "linux", target_os = "macos"))]
#![allow(clippy::disallowed_methods)] // probe inspects process FDs; fixtures impersonate editors.

// Compiled for one item: the budget every wait on a published label obeys. How
// long one such look may take is a fact about this crate's host and not about
// this suite, so it is composed where the other suites read it from.
mod attach;
// The descriptor budget this file is judged against sits with the crate's other
// authored bands, because the file that holds them is the trend's whole memory
// and a reviewer reads it as one diff.
mod baselines;

use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

use norn_config::ConfigDirs;
use norn_config::registry::{Entry, VaultRoot};
use norn_host::{
    AttachMode, DemandLease, Host, LifecyclePolicy, ProductionEntryOps, ProductionPolicy,
    RegistryRead,
};
use norn_testkit::isolation::{self, Lease};
use norn_testkit::scratch::Scratch;
use norn_testkit::wait::{Observed, wait_until};
use norn_wire::{
    ApplyMode, ApplyReport, AuthoredValue, ErrorEnvelope, FieldChange, Predicate, ReasonCode,
    SetParams, TrustState, VaultAddress, VaultName, WriteTarget,
};

const PROBE_ENV: &str = "NORN_HOST_FD_BUDGET_PROBE";

const LARGE_VAULT_DOCUMENTS: usize = 2_000;
const WAIT_LIMIT: Duration = Duration::from_secs(30);

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
    baselines::record(
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
            ("budget", baselines::FD_BUDGET.to_string()),
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
        baselines::fits(one_document_delta, baselines::FD_BUDGET),
        "one-document attachment used {one_document_delta} descriptors; budget is {}",
        baselines::FD_BUDGET
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
        baselines::fits(large_vault_delta, baselines::FD_BUDGET),
        "2k-document attachment used {large_vault_delta} descriptors; budget is {}",
        baselines::FD_BUDGET
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

const APPLY_PROBE_ENV: &str = "NORN_HOST_FD_BUDGET_APPLY_PROBE";

/// How many documents the apply probe's `where` target matches.
const FLAGGED_DOCUMENTS: usize = 8;

/// How many applies the apply probe runs after its first.
const LATER_APPLIES: usize = 3;

/// **An apply that mints its own reader stays inside the descriptor budget
/// and accumulates nothing.** A `set` whose target is a `where`, sent
/// straight to apply, has its match run inside the apply job on one
/// short-lived read connection the store mints for that job. After the
/// apply is answered and the entry has settled, the descriptors the process
/// holds fit the budget an attachment is held to, every later such apply
/// settles at that same count, and detaching gives back every descriptor.
///
/// **The count is not held to what the attachment alone held**, because it
/// does not return there: on Linux it was measured settling one above it
/// after the first apply, the extra descriptor on the store's database file.
/// SQLite's unix layer keeps a closed connection's descriptor open while
/// another connection of the process holds a lock on the same file, and
/// reuses it at the next open, so later mints add nothing to it — which is
/// what this holds — and it goes with the detach.
#[test]
fn an_apply_minting_a_reader_stays_in_budget_and_accumulates_no_descriptor() {
    if std::env::var_os(APPLY_PROBE_ENV).is_some() {
        run_apply_probe();
        return;
    }

    // Isolated as the attachment probe is, for the same reason.
    let output = Command::new(std::env::current_exe().expect("test executable"))
        .args([
            "--exact",
            "an_apply_minting_a_reader_stays_in_budget_and_accumulates_no_descriptor",
            "--nocapture",
        ])
        .env(APPLY_PROBE_ENV, "1")
        .output()
        .expect("run descriptor probe subprocess");
    assert!(
        output.status.success(),
        "apply descriptor probe failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}

fn run_apply_probe() {
    let fixture = Fixture::new();
    fixture.write_flagged_documents(FLAGGED_DOCUMENTS);
    let baseline = open_fd_count();

    let host = fixture.host();
    let lease = attach_and_wait(&host, &fixture.name);
    let attached = open_fd_count();
    assert!(
        attached >= baseline,
        "ready descriptor count {attached} fell below baseline {baseline}"
    );

    let mut wave = String::from("flip");
    let mut flip = |round: usize| {
        let next = format!("flip-{round}");
        let answered = host
            .set(SetParams::new(
                VaultAddress::name(fixture.name.clone()),
                ApplyMode::Apply,
                WriteTarget::matching([Predicate::equal_to("wave", wave.as_str())]),
                vec![FieldChange::set(
                    "wave",
                    AuthoredValue::string(next.as_str()),
                )],
            ))
            .expect("the apply is admitted")
            .wait()
            .expect("the apply lands");
        let ApplyReport::Applied { targets, .. } = answered.report else {
            panic!("the apply answered {:?}", answered.report);
        };
        assert_eq!(
            targets.len(),
            FLAGGED_DOCUMENTS,
            "round {round}: {targets:?}"
        );
        wave = next;
    };

    flip(0);
    let settled = settled_fd_count();
    let settled_delta = settled - baseline;
    assert!(
        baselines::fits(settled_delta, baselines::FD_BUDGET),
        "an attachment that applied holds {settled_delta} descriptors; budget is {}",
        baselines::FD_BUDGET
    );
    for round in 1..=LATER_APPLIES {
        flip(round);
        assert_eq!(
            settled_fd_count(),
            settled,
            "apply {round} left the descriptor count other than the first apply did"
        );
    }

    drop(lease);
    detach_and_wait(&host, &fixture.name);
    assert_eq!(
        open_fd_count(),
        baseline,
        "detach after the applies retained descriptors"
    );
}

/// The descriptor count once it has held still for a moment.
///
/// The own writes an apply landed reach the watcher after the answer, and
/// what the entry does with them may hold a descriptor briefly, so a reading
/// is taken once the count has stopped moving rather than at the answer.
fn settled_fd_count() -> usize {
    const STILL_FOR: Duration = Duration::from_millis(500);
    let mut last = open_fd_count();
    let mut still_since = Instant::now();
    wait_until(
        "the descriptor count to hold still after an apply",
        attach::state_budget(WAIT_LIMIT),
        || {
            let count = open_fd_count();
            if count != last {
                last = count;
                still_since = Instant::now();
            }
            if still_since.elapsed() >= STILL_FOR {
                Observed::Met(count)
            } else {
                Observed::pending(format!(
                    "{count} descriptors, still for {:?}",
                    still_since.elapsed()
                ))
            }
        },
    )
    .unwrap_or_else(|failure| panic!("{failure}"))
}

/// What the probe hands its parent: the counts behind the bars above.
///
/// Printed last, so a line reaching the parent is a measurement of a probe that
/// ran every one of its assertions rather than of one that stopped part-way.
#[allow(clippy::disallowed_macros)] // The probe's measurement is a machine-consumed stream its parent reads.
fn report_the_measurement(baseline: usize, one_document: usize, large_vault: usize) {
    println!(
        "{MEASUREMENT_PREFIX}baseline={baseline} one_document={one_document} \
         large_vault={large_vault} budget={}",
        baselines::FD_BUDGET
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
        attach::state_budget(WAIT_LIMIT),
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
    // The naming and the removal are the scratch helper's; what this fixture
    // adds is the vault tree inside and the lease beside it.
    root: Scratch,
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
            isolation::acquisition_budget(attach::state_budget(WAIT_LIMIT)),
        );
        let root = Scratch::new("norn-host-fd-budget");
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
                read_settle_bound: norn_host::READ_SETTLE_BOUND,
            },
        )
        .expect("host")
    }

    /// Write `count` documents each carrying `wave: flip`, which the apply
    /// probe's `where` target matches.
    fn write_flagged_documents(&self, count: usize) {
        for index in 0..count {
            let path = self.vault.join(format!("flagged-{index:04}.md"));
            fs::write(path, format!("---\nwave: flip\n---\n# Flagged {index}\n"))
                .expect("write document");
        }
    }

    fn write_documents(&self, count: usize) {
        for index in 0..count {
            let path = self.vault.join(format!("note-{index:04}.md"));
            fs::write(path, format!("# Note {index}\n")).expect("write document");
        }
    }
}
