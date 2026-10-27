//! Registration-time first-index candidacy and ordinary reused-store reads.
#![cfg(all(unix, feature = "induced-failure"))]
#![allow(clippy::disallowed_methods)]
use norn_config::ConfigDirs;
use norn_config::registry::VaultRoot;
use norn_host::{
    ActiveFingerprints, ApplyEnd, ApplyProgress, AttachmentAdvisory, EntryOps, EntryReloadFailure,
    Host, JobFailure, LifecyclePolicy, PlanGround, ProductionEntryOps, ProductionPolicy,
    ProgressReporter, ReconcileWork, RecordRefusal, Registration, RegistryRead, ReloadJudgment,
    RetireRefusal, SemanticEngines,
};
use norn_store::ContentModel;
use norn_testkit::isolation::{self, Lease};
use norn_testkit::scratch::Scratch;
use norn_testkit::wait::{Budget, Observed, wait_until};
use norn_wire::{
    FindParams, GetParams, PlanDocument, ResolutionTarget, RollingHealStatus, TrustState,
    UntrustedReason, VaultAddress, VaultName,
};
use std::path::Path;
use std::sync::{Arc, Condvar, Mutex, atomic::AtomicBool};
use std::time::{Duration, Instant};

const BUDGET: Budget = Budget::new(Duration::from_secs(30), Duration::from_millis(50));

struct PreHeld {
    inner: ProductionEntryOps,
    hold: AtomicBool,
    gate: Arc<(Mutex<bool>, Condvar)>,
}
macro_rules! delegate {
    ($(fn $method:ident(&self $(, $arg:ident: $ty:ty)*) $(-> $ret:ty)?;)*) => {
        $(fn $method(&self $(, $arg: $ty)*) $(-> $ret)? {
            self.inner.$method($($arg),*)
        })*
    };
}

impl EntryOps for PreHeld {
    type Attachment = <ProductionEntryOps as EntryOps>::Attachment;
    #[cfg(feature = "induced-failure")]
    fn loading_read_boundary(&self, completed: bool) {
        self.inner.loading_read_boundary(completed);
    }

    fn attach(
        &self,
        registration: &Registration,
        progress: &ProgressReporter<Self::Attachment>,
    ) -> Result<Self::Attachment, JobFailure> {
        if self.hold.load(std::sync::atomic::Ordering::SeqCst) {
            let mut released = self.gate.0.lock().unwrap();
            while !*released {
                released = self.gate.1.wait(released).unwrap();
            }
        }
        self.inner.attach(registration, progress)
    }

    delegate! {
        fn first_index_reads(&self) -> bool;
        fn first_index_candidate(&self, registration: &Registration) -> bool;
        fn trim_readers(&self, now: Instant);
        fn reconcile(&self, name: &VaultName, attachment: &mut Self::Attachment, work: ReconcileWork, progress: &ProgressReporter<Self::Attachment>) -> Result<(), JobFailure>;
        fn recover(&self, name: &VaultName, attachment: &mut Self::Attachment, progress: &ProgressReporter<Self::Attachment>) -> Result<(), JobFailure>;
        fn rebuild(&self, name: &VaultName, attachment: Self::Attachment, progress: &ProgressReporter<Self::Attachment>) -> Result<Self::Attachment, JobFailure>;
        fn poll(&self, name: &VaultName, attachment: &mut Self::Attachment) -> Result<Option<norn_fs::Batch>, JobFailure>;
        fn reload(&self, name: &VaultName, attachment: &mut Self::Attachment, progress: &ProgressReporter<Self::Attachment>) -> Result<ReloadJudgment, EntryReloadFailure>;
        fn judge_reload(&self, name: &VaultName, attachment: &Self::Attachment) -> Result<ReloadJudgment, EntryReloadFailure>;
        fn active_fingerprints(&self, attachment: &Self::Attachment) -> Option<ActiveFingerprints>;
        fn active_content_model(&self, attachment: &Self::Attachment) -> Arc<ContentModel>;
        fn control_root(&self, attachment: &Self::Attachment) -> Option<std::path::PathBuf>;
        fn plan_ground(&self, attachment: &Self::Attachment) -> Option<PlanGround>;
        fn apply(&self, name: &VaultName, attachment: &mut Self::Attachment, plan: PlanDocument, apply: &ApplyProgress, progress: &ProgressReporter<Self::Attachment>) -> Result<ApplyEnd, JobFailure>;
        fn withheld_trust(&self, attachment: &Self::Attachment) -> Option<UntrustedReason>;
        fn advisories(&self, attachment: &Self::Attachment) -> Vec<AttachmentAdvisory>;
        fn count_leg_mint(&self, statements: u64);
        fn semantic(&self) -> Option<&SemanticEngines>;
        fn rolling_heal(&self, attachment: &Self::Attachment) -> Option<RollingHealStatus>;
        fn maintenance_due(&self, name: &VaultName, attachment: &Self::Attachment) -> bool;
        fn maintain_quiet(&self, name: &VaultName, attachment: &mut Self::Attachment, quiet: bool, progress: &ProgressReporter<Self::Attachment>) -> Result<(), JobFailure>;
        fn maintain(&self, name: &VaultName, attachment: &mut Self::Attachment) -> Result<(), JobFailure>;
        fn detach(&self, name: &VaultName, attachment: Self::Attachment);
        fn discard(&self, name: &VaultName);
        fn record(&self, registration: &Registration) -> Result<(), RecordRefusal>;
        fn retire(&self, registration: &Registration, keep_state: bool, standing: &[Registration]) -> Result<(), RetireRefusal>;
        fn amend(&self, current: &Registration, amended: &Registration, standing: &[Registration]) -> Result<(), RetireRefusal>;
    }
}

fn host(
    dirs: &ConfigDirs,
    root: &Path,
    hold: bool,
    gate: Arc<(Mutex<bool>, Condvar)>,
) -> Host<PreHeld> {
    let registration = Registration::new(
        VaultName::new("notes").unwrap(),
        VaultRoot::new(root).unwrap(),
    );
    // The fixture's scratch directory, outside the vault, takes the sentinel.
    let scratch = root
        .parent()
        .expect("the vault sits in its scratch directory");
    norn_fs::watch::settle_prior_changes(scratch, BUDGET.work()).unwrap();
    Host::new(
        RegistryRead::from_entries([registration]),
        PreHeld {
            inner: ProductionEntryOps::new(dirs.clone(), ProductionPolicy::new(2, 2).unwrap()),
            hold: AtomicBool::new(hold),
            gate,
        },
        LifecyclePolicy {
            idle_after: Duration::from_secs(7200),
            worker_slots: 1,
            watch_poll_interval: Duration::from_millis(25),
            read_settle_bound: norn_host::READ_SETTLE_BOUND,
        },
    )
    .unwrap()
}

/// A canonical-only (non-fresh) first attach of this host: does an eligible Get
/// refuse at once (as a canonical first-open request), or wait for provisional
/// first-index admission and then expire with the first-index message?
#[test]
fn probe_reused_store_first_attach_provisional_wait() {
    let scratch = Scratch::under(Path::new(env!("CARGO_TARGET_TMPDIR")), "review-reused").unwrap();
    let root = scratch.join("vault");
    std::fs::create_dir_all(root.join(".norn")).unwrap();
    std::fs::write(root.join(".norn/schema.yaml"), "version: 1\n").unwrap();
    for n in 0..6 {
        std::fs::write(root.join(format!("doc{n}.md")), format!("# Doc {n}\n")).unwrap();
    }
    let dirs = ConfigDirs::new(scratch.join("config"), scratch.join("data")).unwrap();
    let _lease = Lease::hold(
        isolation::REAL_WATCHER,
        isolation::acquisition_budget(BUDGET),
    );
    let name = VaultName::new("notes").unwrap();
    let address = VaultAddress::name(name.clone());
    // First host: build the canonical store, then drop it (store is now Reused).
    {
        let gate = Arc::new((Mutex::new(true), Condvar::new()));
        let host = host(&dirs, &root, false, gate);
        let _d = host.demand(&name).unwrap();
        wait_until("first Ready", BUDGET, || match host.state(&name) {
            Ok(TrustState::Ready) => Observed::Met(()),
            r => Observed::pending(format!("{r:?}")),
        })
        .unwrap();
    }
    let gate = Arc::new((Mutex::new(false), Condvar::new()));
    let host = host(&dirs, &root, true, gate.clone());
    let params = GetParams::new(address.clone(), ResolutionTarget::new("doc1").unwrap());
    let get = host.get(&params);
    let find = host.find(&FindParams::new(address.clone()));
    let count = host.count(&norn_wire::CountParams::new(address.clone()));
    let waits = host.read_evidence().settle_waits;
    *gate.0.lock().unwrap() = true;
    gate.1.notify_all();
    wait_until("second Ready", BUDGET, || match host.state(&name) {
        Ok(TrustState::Ready) => Observed::Met(()),
        r => Observed::pending(format!("{r:?}")),
    })
    .unwrap();
    assert!(host.get(&params).is_ok());
    for error in [get.unwrap_err(), find.unwrap_err(), count.unwrap_err()] {
        assert_eq!(error.code(), &norn_wire::ReasonCode::HostEntryNotReady);
        assert!(!error.message().contains("first time"), "{error:?}");
    }
    assert_eq!(
        waits, 0,
        "eligible requests waited during a reused store's first attach prologue"
    );
}

fn release(gate: &Arc<(Mutex<bool>, Condvar)>) {
    *gate.0.lock().unwrap_or_else(|p| p.into_inner()) = true;
    gate.1.notify_all();
}
struct ReleaseGate<'a>(&'a Arc<(Mutex<bool>, Condvar)>);
impl Drop for ReleaseGate<'_> {
    fn drop(&mut self) {
        release(self.0);
    }
}

#[derive(Default)]
struct CanonicalPause {
    reached: AtomicBool,
    gate: Arc<(Mutex<bool>, Condvar)>,
}
impl norn_host::LoadingProbe for CanonicalPause {
    fn at(&self, boundary: norn_host::LoadingBoundary<'_>) {
        if matches!(boundary, norn_host::LoadingBoundary::MetadataComplete) {
            self.reached
                .store(true, std::sync::atomic::Ordering::Release);
            let held = self.gate.0.lock().unwrap();
            let (_held, timeout) = self
                .gate
                .1
                .wait_timeout_while(held, BUDGET.work(), |released| !*released)
                .unwrap();
            assert!(
                !timeout.timed_out(),
                "canonical construction was never released"
            );
        }
    }
}

fn fixture(label: &str) -> (Scratch, std::path::PathBuf, ConfigDirs, Lease) {
    let scratch = Scratch::under(Path::new(env!("CARGO_TARGET_TMPDIR")), label).unwrap();
    let root = scratch.join("vault");
    std::fs::create_dir_all(root.join(".norn")).unwrap();
    std::fs::write(root.join(".norn/schema.yaml"), "version: 1\n").unwrap();
    std::fs::write(root.join("doc1.md"), "# Document\nWhole body.\n").unwrap();
    let dirs = ConfigDirs::new(scratch.join("config"), scratch.join("data")).unwrap();
    let lease = Lease::hold(
        isolation::REAL_WATCHER,
        isolation::acquisition_budget(BUDGET),
    );
    (scratch, root, dirs, lease)
}
fn name() -> VaultName {
    VaultName::new("notes").unwrap()
}
fn get() -> GetParams {
    GetParams::new(
        VaultAddress::name(name()),
        ResolutionTarget::new("doc1").unwrap(),
    )
}
fn assert_ordinary_refusals(host: &Host<PreHeld>) {
    let get = host.get(&get()).unwrap_err();
    let find = host
        .find(&FindParams::new(VaultAddress::name(name())))
        .unwrap_err();
    let count = host
        .count(&norn_wire::CountParams::new(VaultAddress::name(name())))
        .unwrap_err();
    for error in [get, find, count] {
        assert_eq!(error.code(), &norn_wire::ReasonCode::HostEntryNotReady);
        assert!(!error.message().contains("first time"), "{error:?}");
    }
    assert_eq!(host.read_evidence().settle_waits, 0);
    assert_eq!(host.read_evidence().settle_expiries, 0);
}

#[test]
fn a_fresh_first_public_caller_triggers_attachment_and_waits_for_its_own_capability() {
    let (_scratch, root, dirs, _lease) = fixture("registration-fresh-control");
    let preattach = Arc::new((Mutex::new(false), Condvar::new()));
    let canonical = Arc::new(CanonicalPause::default());
    let host = Host::new(
        RegistryRead::from_entries([Registration::new(name(), VaultRoot::new(&root).unwrap())]),
        PreHeld {
            inner: ProductionEntryOps::new(dirs, ProductionPolicy::new(2, 2).unwrap())
                .with_loading_probe(canonical.clone()),
            hold: AtomicBool::new(true),
            gate: preattach.clone(),
        },
        LifecyclePolicy {
            idle_after: Duration::from_secs(7200),
            worker_slots: 1,
            watch_poll_interval: Duration::from_millis(25),
            read_settle_bound: norn_host::READ_SETTLE_BOUND,
        },
    )
    .unwrap();
    std::thread::scope(|scope| {
        let _pre_release = ReleaseGate(&preattach);
        let _canonical_release = ReleaseGate(&canonical.gate);
        let caller = scope.spawn(|| host.get(&get()));
        wait_until("first caller waiting before inner attach", BUDGET, || {
            if host.read_evidence().settle_waits > 0 {
                Observed::Met(())
            } else {
                Observed::pending("caller has not waited")
            }
        })
        .unwrap();
        assert!(!canonical.reached.load(std::sync::atomic::Ordering::Acquire));
        release(&preattach);
        wait_until("metadata complete before canonical work", BUDGET, || {
            if canonical.reached.load(std::sync::atomic::Ordering::Acquire) {
                Observed::Met(())
            } else {
                Observed::pending("metadata not complete")
            }
        })
        .unwrap();
        let answer = caller.join().unwrap().unwrap();
        assert!(matches!(
            answer.answer.reading.trust,
            TrustState::Warming { .. }
        ));
        assert!(
            serde_json::to_string(&answer.answer.report)
                .unwrap()
                .contains("Whole body.")
        );
        assert_eq!(host.read_evidence().settle_expiries, 0);
        release(&canonical.gate);
    });
}

#[test]
fn dynamic_registration_and_replacement_reobserve_storage_before_any_read() {
    use norn_wire::{RegisterParams, UnregisterParams, VaultChange, VaultReplace, VaultSetParams};
    for change in ["register", "reregister", "same-root", "move-root"] {
        let (scratch, root, dirs, _lease) = fixture("registration-composition");
        {
            let first = host(
                &dirs,
                &root,
                false,
                Arc::new((Mutex::new(true), Condvar::new())),
            );
            let _demand = first.demand(&name()).unwrap();
            wait_until("seed Ready", BUDGET, || {
                if first.state(&name()).unwrap() == TrustState::Ready {
                    Observed::Met(())
                } else {
                    Observed::pending("seed not ready")
                }
            })
            .unwrap();
        }
        let gate = Arc::new((Mutex::new(false), Condvar::new()));
        let canonical = Arc::new(CanonicalPause::default());
        let host = Host::new(
            RegistryRead::from_entries([]),
            PreHeld {
                inner: ProductionEntryOps::new(dirs.clone(), ProductionPolicy::new(2, 2).unwrap())
                    .with_loading_probe(canonical.clone()),
                hold: AtomicBool::new(true),
                gate: gate.clone(),
            },
            LifecyclePolicy {
                idle_after: Duration::from_secs(7200),
                worker_slots: 1,
                watch_poll_interval: Duration::from_millis(25),
                read_settle_bound: norn_host::READ_SETTLE_BOUND,
            },
        )
        .unwrap();
        let _release = ReleaseGate(&gate);
        let _release_canonical = ReleaseGate(&canonical.gate);
        let registration = Registration::new(name(), VaultRoot::new(&root).unwrap());
        host.vault_register(&RegisterParams::new(registration.clone()))
            .unwrap();
        match change {
            "reregister" => {
                host.vault_unregister(&UnregisterParams::new(name()).keeping_state())
                    .unwrap();
                host.vault_register(&RegisterParams::new(registration))
                    .unwrap();
            }
            "same-root" => {
                host.vault_set(
                    &VaultSetParams::new(name()).with_poll_backend(VaultChange::set(
                        norn_config::registry::PollBackend::Poll,
                    )),
                )
                .unwrap();
            }
            "move-root" => {
                let moved = scratch.join("other-vault");
                std::fs::create_dir_all(moved.join(".norn")).unwrap();
                std::fs::write(moved.join(".norn/schema.yaml"), "version: 1\n").unwrap();
                std::fs::write(moved.join("doc1.md"), "# Moved\nWhole body.\n").unwrap();
                host.vault_set(
                    &VaultSetParams::new(name())
                        .with_root(VaultReplace::set(VaultRoot::new(&moved).unwrap())),
                )
                .unwrap();
            }
            _ => {}
        }
        if change == "move-root" {
            std::thread::scope(|scope| {
                let caller = scope.spawn(|| host.get(&get()));
                wait_until("replacement fresh caller waiting", BUDGET, || {
                    if host.read_evidence().settle_waits > 0 {
                        Observed::Met(())
                    } else {
                        Observed::pending("caller not waiting")
                    }
                })
                .unwrap();
                release(&gate);
                wait_until("fresh replacement metadata", BUDGET, || {
                    if canonical.reached.load(std::sync::atomic::Ordering::Acquire) {
                        Observed::Met(())
                    } else {
                        Observed::pending("metadata not complete")
                    }
                })
                .unwrap();
                assert!(matches!(
                    caller.join().unwrap().unwrap().answer.reading.trust,
                    TrustState::Warming { .. }
                ));
                release(&canonical.gate);
            });
        } else {
            assert_ordinary_refusals(&host);
            release(&gate);
        }
    }
}

#[test]
fn foreign_and_inaccessible_storage_cannot_make_a_provisional_waiter() {
    use std::os::unix::fs::PermissionsExt;
    for state in ["foreign", "directory", "dangling-link", "unreadable"] {
        let (_scratch, root, dirs, _lease) = fixture("registration-conservative");
        let derived = dirs.derived_dir(&name());
        let ops = ProductionEntryOps::new(dirs.clone(), ProductionPolicy::new(2, 2).unwrap());
        std::fs::create_dir_all(&derived).unwrap();
        let store = derived.join("store.sqlite3");
        match state {
            "foreign" => std::fs::write(&store, "foreign storage").unwrap(),
            "directory" => std::fs::create_dir(&store).unwrap(),
            "dangling-link" => std::os::unix::fs::symlink("missing", &store).unwrap(),
            "unreadable" => {
                std::fs::set_permissions(&derived, std::fs::Permissions::from_mode(0o000)).unwrap()
            }
            _ => unreachable!(),
        }
        let gate = Arc::new((Mutex::new(false), Condvar::new()));
        let host = Host::new(
            RegistryRead::from_entries([Registration::new(name(), VaultRoot::new(&root).unwrap())]),
            PreHeld {
                inner: ops,
                hold: AtomicBool::new(true),
                gate: gate.clone(),
            },
            LifecyclePolicy {
                idle_after: Duration::from_secs(7200),
                worker_slots: 1,
                watch_poll_interval: Duration::from_millis(25),
                read_settle_bound: norn_host::READ_SETTLE_BOUND,
            },
        )
        .unwrap();
        // Restore permissions before unwinding or releasing attach; the candidate
        // was already observed by Host::new while the directory was unreadable.
        std::fs::set_permissions(&derived, std::fs::Permissions::from_mode(0o700)).unwrap();
        let _release = ReleaseGate(&gate);
        assert_ordinary_refusals(&host);
        match state {
            "directory" => std::fs::remove_dir(&store).unwrap(),
            "unreadable" => {}
            _ => std::fs::remove_file(&store).unwrap(),
        }
        release(&gate);
    }
}
