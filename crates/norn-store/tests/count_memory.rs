//! Process-native memory and descriptor prices for immutable count results.
//! Fixture generation belongs to the parent; SQLite allocations belong to the child.

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use norn_store::{
    Change, ContentModel, DerivationVersion, DocumentFacts, DocumentPath, FrontmatterValue,
    IncrementProvenance, PageRefusal, SnapshotReader, Store, StoredPathOrder,
};
use norn_testkit::process::{Run, Sandbox, open_fd_count};
use norn_wire::{CountParams, GroupKey, VaultAddress, VaultName};

const DATABASE_ENV: &str = "NORN_COUNT_RESOURCE_DATABASE";
const SIZE_ENV: &str = "NORN_COUNT_RESOURCE_DOCUMENTS";
const CASE: &str = "retained_results_bound_native_memory_and_price_their_descriptors";
// A whole-result Vec at the larger scale retains over 14 MiB more label bytes.
const MEMORY_GROWTH: u64 = 8 * 1024 * 1024;
const MEMORY_CEILING: u64 = 64 * 1024 * 1024;

fn model() -> ContentModel {
    ContentModel::none()
}
fn params() -> CountParams {
    CountParams::new(VaultAddress::name(VaultName::new("counts").unwrap()))
        .with_by([GroupKey::field("status"), GroupKey::field("n")])
        .with_limit(16)
}
fn snapshot(reader: &Arc<SnapshotReader>) -> norn_store::Snapshot {
    reader
        .try_take()
        .expect("no competing read")
        .establish()
        .expect("snapshot")
}

fn populate(path: &Path, size: usize) {
    let mut store = Store::open(path, StoredPathOrder::Sensitive, DerivationVersion::new(1))
        .expect("fixture store");
    let suffix = "x".repeat(4096);
    for start in (0..size).step_by(64) {
        let changes = (start..(start + 64).min(size)).map(|at| {
            let fields = vec![
                (
                    "status".to_string(),
                    FrontmatterValue::String(format!("{at:06}-{suffix}")),
                ),
                (
                    "n".to_string(),
                    FrontmatterValue::String(format!("{at:06}")),
                ),
            ];
            Change::Upsert(
                DocumentFacts::new(
                    DocumentPath::new(&format!("{at:06}.md")).unwrap(),
                    format!("hash-{at}"),
                    "body",
                    4,
                )
                .with_frontmatter(Some(FrontmatterValue::Map(fields)), &model()),
            )
        });
        store
            .begin_request()
            .apply_increment(IncrementProvenance::Derived, changes, &[], &model())
            .expect("populate fixture");
    }
}

#[allow(clippy::disallowed_macros)] // Machine-consumed child report.
fn child(path: &Path, size: usize) {
    let initial = open_fd_count().unwrap();
    let store = Store::open(path, StoredPathOrder::Sensitive, DerivationVersion::new(1))
        .expect("source store");
    let reader = Arc::new(store.open_reader().reader.expect("reader"));
    drop(snapshot(&reader));
    let base = open_fd_count().unwrap();
    let at = Instant::now();
    let mut request = params();
    let mut counted = snapshot(&reader)
        .count_with_time(&request, &model(), at)
        .expect("first page");
    let first = counted.next.clone().expect("more groups");
    let retained_fds = open_fd_count().unwrap() - base;
    assert_eq!(
        retained_fds, 1,
        "one spilled immutable result owns one descriptor"
    );
    let mut rows = 0;
    let mut vm_steps = 0;
    loop {
        rows += counted.tallies.len();
        vm_steps += counted.work.vm_steps;
        let Some(next) = counted.next else { break };
        request = request.with_after(next);
        counted = snapshot(&reader)
            .count_with_time(&request, &model(), at)
            .expect("retained page");
    }
    assert_eq!(rows, size);
    assert_eq!(
        open_fd_count().unwrap(),
        base + retained_fds,
        "draining accumulates no descriptors"
    );
    let resumed = params().with_after(first);
    assert!(matches!(
        snapshot(&reader).count_with_time(&resumed, &model(), at + Duration::from_secs(601)),
        Err(PageRefusal::CountCursorExpired)
    ));
    assert_eq!(
        open_fd_count().unwrap(),
        base,
        "observing expiry closes result storage"
    );
    drop(reader);
    drop(store);
    assert_eq!(
        open_fd_count().unwrap(),
        initial,
        "teardown returns every descriptor"
    );
    println!("count-resource rows={rows} fd_delta={retained_fds} vm_steps={vm_steps}");
}

#[test]
#[ignore = "memory-lane case: isolated native SQLite memory and descriptor pair"]
fn retained_results_bound_native_memory_and_price_their_descriptors() {
    if let Some(path) = std::env::var_os(DATABASE_ENV) {
        let size = std::env::var(SIZE_ENV).unwrap().parse().unwrap();
        child(Path::new(&path), size);
        return;
    }
    let mut peaks = Vec::new();
    for size in [512, 4096] {
        let sandbox =
            Sandbox::new(Path::new(env!("CARGO_TARGET_TMPDIR")), "count-resource").unwrap();
        let path = sandbox.work_dir().join("source.sqlite3");
        populate(&path, size);
        let executable = sandbox
            .install_binary(&std::env::current_exe().unwrap())
            .unwrap();
        let outcome = Run::new(&sandbox, &executable)
            .args([
                "--exact",
                CASE,
                "--ignored",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(DATABASE_ENV, &path)
            .env(SIZE_ENV, size.to_string())
            .deadline(Duration::from_secs(120))
            .wait()
            .unwrap();
        outcome.assert_success();
        assert!(!outcome.stdout_truncated);
        assert!(
            outcome
                .stdout_text()
                .contains(&format!("count-resource rows={size} fd_delta=1 vm_steps="))
        );
        eprintln!(
            "retained count at {size}: peak_rss_bytes={}, {}",
            outcome.peak_rss_bytes,
            outcome.stdout_text()
        );
        assert!(outcome.peak_rss_bytes <= MEMORY_CEILING);
        peaks.push(outcome.peak_rss_bytes);
    }
    assert!(
        peaks[1] <= peaks[0] + MEMORY_GROWTH,
        "native result memory followed result size: {peaks:?}"
    );
}
