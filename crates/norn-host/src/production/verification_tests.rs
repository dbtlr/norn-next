use super::*;
use norn_store::{VerificationPoint, VerificationStop};
use std::sync::mpsc;

/// How long a held verification waits for its stop before it gives up: far
/// longer than any bound these cases assert, so a retirement that waited the
/// verification out instead of stopping it fails them.
const HELD_UNTIL_STOPPED: Duration = Duration::from_secs(30);

/// Hold every later verification of `verifications` at its first `at` point
/// until it is stopped: the receiver hears once one is held, with the name of
/// the thread it is held on.
fn hold_until_stopped(
    verifications: &crate::verification::StoreVerifications,
    at: VerificationPoint,
) -> mpsc::Receiver<Option<String>> {
    let (held, holding) = mpsc::channel();
    let held = Mutex::new(held);
    verifications.run_inside_verifications(move |point, stop: &VerificationStop| {
        if point != at {
            return;
        }
        let _ = held
            .lock()
            .unwrap()
            .send(thread::current().name().map(str::to_string));
        let began = Instant::now();
        while !stop.is_stopped() && began.elapsed() < HELD_UNTIL_STOPPED {
            thread::sleep(Duration::from_millis(1));
        }
    });
    holding
}

/// Leave the index of `name`'s store disagreeing with its documents behind
/// the host's back, through a connection of the case's own: damage only the
/// full-text check meets, and which the verification meets on its copy.
fn damage_the_full_text_index(f: &Fixture, name: &VaultName) {
    let store = ConfigDirs::new(f.root.join("config"), f.root.join("data"))
        .unwrap()
        .derived_dir(name)
        .join(STORE_FILE);
    let connection = norn_db::rusqlite::Connection::open(store).unwrap();
    connection
        .busy_timeout(Duration::from_secs(5))
        .unwrap();
    connection
        .execute_batch(
            "INSERT INTO documents_fts (rowid, body) VALUES (99999, 'a body no document holds')",
        )
        .unwrap();
}

/// Leave the full-text index config of `name`'s store holding a format
/// version FTS5 refuses, behind the host's back, through a connection of the
/// case's own: damage that fails every full-text read and that the driver
/// does not describe as a damaged file.
fn damage_the_full_text_config(f: &Fixture, name: &VaultName) {
    let store = ConfigDirs::new(f.root.join("config"), f.root.join("data"))
        .unwrap()
        .derived_dir(name)
        .join(STORE_FILE);
    let connection = norn_db::rusqlite::Connection::open(store).unwrap();
    connection
        .busy_timeout(Duration::from_secs(5))
        .unwrap();
    connection
        .execute_batch("UPDATE documents_fts_config SET v = 3 WHERE k = 'version'")
        .unwrap();
}

/// Bring `name`'s verification due and wake the scheduler, as its hourly
/// deadline would.
fn verification_comes_due<O: EntryOps>(
    host: &crate::Host<O>,
    verifications: &crate::verification::StoreVerifications,
    name: &VaultName,
) {
    verifications.bring_due(name);
    host.wake_background();
}

/// An apply of one `str_replace` on `note.md`, answered on another thread:
/// whether it applied, or an error where no answer came within 15 seconds.
fn apply_on_another_thread(
    host: &Arc<crate::Host<ProductionEntryOps>>,
    name: &VaultName,
    from: &str,
    to: &str,
) -> Result<bool, mpsc::RecvTimeoutError> {
    let (answered, answer) = mpsc::channel();
    let applying = Arc::clone(host);
    let vault = name.clone();
    let (from, to) = (from.to_string(), to.to_string());
    thread::spawn(move || {
        let plan = norn_wire::AuthoredPlan::new(
            norn_wire::VaultAddress::name(vault),
            vec![norn_wire::Operation::new(
                norn_wire::OperationKind::str_replace(
                    norn_wire::DocumentPath::new("note.md").unwrap(),
                    from,
                    to,
                ),
            )],
        );
        let applied = applying
            .apply(norn_wire::ApplyParams::new(
                norn_wire::ApplyMode::Apply,
                PlanDocument::operations(plan),
            ))
            .expect("an apply over a ready vault is admitted")
            .wait();
        let _ = answered.send(applied.is_ok());
    });
    answer.recv_timeout(Duration::from_secs(15))
}

/// **A write completes while its vault's verification is in flight**, on the
/// lane: held with its read-only snapshot open, and again with its copy
/// written, the verification holds no claim and no writer, so an apply
/// takes the claim, commits and answers each time. Both holds are on a lane
/// thread, never on a worker-pool leg.
#[test]
fn a_write_completes_while_its_vaults_verification_is_in_flight() {
    let f = Fixture::new("verification-write-during");
    write_a_vault_of_documents(&f, 8);
    fs::write(f.vault().join("note.md"), "status draft\n").unwrap();
    let ops = fixture_ops(&f);
    let verifications = Arc::clone(&ops.verifications);
    let (held, holding) = mpsc::channel();
    let (resume, resumed) = mpsc::channel::<()>();
    let (held, resumed) = (Mutex::new(held), Mutex::new(resumed));
    let holds = Mutex::new(Vec::new());
    verifications.run_inside_verifications(move |at, _| {
        let hold = matches!(at, VerificationPoint::Row | VerificationPoint::CopyCheck);
        let mut seen = holds.lock().unwrap();
        if !hold || seen.contains(&at) {
            return;
        }
        seen.push(at);
        drop(seen);
        held.lock()
            .unwrap()
            .send((at, thread::current().name().map(str::to_string)))
            .unwrap();
        let _ = resumed.lock().unwrap().recv_timeout(Duration::from_secs(30));
    });
    let (host, name, _lease) = ready_host(&f, ops);
    let host = Arc::new(host);
    verification_comes_due(&host, &verifications, &name);

    for (expected, from, to) in [
        (VerificationPoint::Row, "draft", "final"),
        (VerificationPoint::CopyCheck, "final", "done"),
    ] {
        let (at, thread) = holding
            .recv_timeout(Duration::from_secs(15))
            .expect("the verification to be held");
        assert_eq!(at, expected);
        assert_eq!(
            thread.as_deref(),
            Some(crate::lifecycle::LANE_THREAD),
            "the verification ran off the lane"
        );
        let applied = apply_on_another_thread(&host, &name, from, to);
        resume.send(()).unwrap();
        assert_eq!(
            applied,
            Ok(true),
            "the apply waited for the verification held at {at:?}"
        );
    }
    assert_eq!(
        fs::read_to_string(f.vault().join("note.md")).unwrap(),
        "status done\n"
    );
    wait_until("the verification to end", lifecycle_budget(), || {
        let reading = verifications.reading();
        if reading.sound == 1 {
            Observed::Met(())
        } else {
            Observed::Pending(format!("{reading:?}"))
        }
    })
    .unwrap_or_else(|failure| panic!("{failure}"));
    assert_eq!(host.maintenance_reading().units, 0, "a leg ran a unit");
}

/// **Damage the verification meets schedules the rebuild once.** The store's
/// index disagrees with its documents behind the host's back; the
/// verification meets it on its copy, the entry judges it for the store
/// lifetime the copy was taken from — which the ops answer for, since no
/// engine records the entry's store reading — and the rebuild runs once. The
/// rebuilt store verifies sound, and nothing schedules a second rebuild.
#[test]
fn damage_the_verification_meets_schedules_the_rebuild_once() {
    damage_met_schedules_the_rebuild_once("verification-damage", damage_the_full_text_index);
}

/// **A damaged full-text config is damage the verification meets.** The
/// config fails every full-text read as an ordinary error, so the
/// verification has to call it damage itself; the rebuild then runs once, as
/// it does for a disagreeing index.
#[test]
fn a_damaged_full_text_config_the_verification_meets_schedules_the_rebuild_once() {
    damage_met_schedules_the_rebuild_once("verification-config-damage", damage_the_full_text_config);
}

/// A ready vault damaged by `damage` behind the host's back, whose due
/// verification meets it: the rebuild runs once, the rebuilt store verifies
/// sound, and nothing schedules a second rebuild.
fn damage_met_schedules_the_rebuild_once(
    fixture: &str,
    damage: fn(&Fixture, &VaultName),
) {
    let f = Fixture::new(fixture);
    write_a_vault_of_documents(&f, 8);
    let ops = fixture_ops(&f);
    let verifications = Arc::clone(&ops.verifications);
    let (host, name, _lease) = ready_host(&f, ops);
    let rebuilds_before = host.ops().evidence.read().rebuilds_run;
    damage(&f, &name);
    verification_comes_due(&host, &verifications, &name);

    wait_until("the damage to be rebuilt", lifecycle_budget(), || {
        let reading = verifications.reading();
        let rebuilt = host.ops().evidence.read().rebuilds_run - rebuilds_before;
        match host.state(&name) {
            Ok(norn_wire::TrustState::Ready) if reading.damaged == 1 && rebuilt == 1 => {
                Observed::Met(())
            }
            state => Observed::Pending(format!("{state:?}, {rebuilt} rebuilt, {reading:?}")),
        }
    })
    .unwrap_or_else(|failure| panic!("{failure}"));

    verification_comes_due(&host, &verifications, &name);
    wait_until("the rebuilt store to verify", lifecycle_budget(), || {
        let reading = verifications.reading();
        if reading.sound == 1 {
            Observed::Met(())
        } else {
            Observed::Pending(format!("{reading:?}"))
        }
    })
    .unwrap_or_else(|failure| panic!("{failure}"));
    assert_eq!(verifications.reading().damaged, 1);
    assert_eq!(host.ops().evidence.read().rebuilds_run - rebuilds_before, 1);
    assert_eq!(host.state(&name), Ok(norn_wire::TrustState::Ready));
    assert_next_due_a_whole_interval_on(&verifications, &name, "a sound verdict");
}

/// That `vault`'s next verification is a whole interval away, not the lane's
/// retry backoff only damage over the bound lifetime earns: a sound or
/// refused verdict that came due again each second would rewrite the store's
/// copy every second.
fn assert_next_due_a_whole_interval_on(
    verifications: &crate::verification::StoreVerifications,
    vault: &VaultName,
    after: &str,
) {
    let due = verifications
        .deadline(vault)
        .unwrap_or_else(|| panic!("{after} left no next verification due"));
    assert!(
        due > Instant::now() + Duration::from_secs(60),
        "{after} brought the next verification due within a minute, at {:?} from now",
        due.saturating_duration_since(Instant::now())
    );
}

/// **A refused verification schedules nothing.** The copy's place is taken
/// by something that is not a file, so the verification is refused: no
/// verdict, no rebuild, and the entry keeps serving.
#[test]
fn a_refused_verification_schedules_nothing() {
    let f = Fixture::new("verification-refused");
    write_a_vault_of_documents(&f, 4);
    let ops = fixture_ops(&f);
    let verifications = Arc::clone(&ops.verifications);
    let (host, name, _lease) = ready_host(&f, ops);
    let rebuilds_before = host.ops().evidence.read().rebuilds_run;
    let derived = ConfigDirs::new(f.root.join("config"), f.root.join("data"))
        .unwrap()
        .derived_dir(&name);
    fs::create_dir_all(
        derived
            .join(STORE_FILE)
            .with_extension("verification-copy")
            .join("occupied"),
    )
    .unwrap();
    verification_comes_due(&host, &verifications, &name);
    wait_until("the verification to be refused", lifecycle_budget(), || {
        let reading = verifications.reading();
        if reading.refused == 1 {
            Observed::Met(())
        } else {
            Observed::Pending(format!("{reading:?}"))
        }
    })
    .unwrap_or_else(|failure| panic!("{failure}"));
    assert_eq!(verifications.reading().damaged, 0);
    assert_eq!(host.ops().evidence.read().rebuilds_run, rebuilds_before);
    assert_eq!(host.state(&name), Ok(norn_wire::TrustState::Ready));
    assert_next_due_a_whole_interval_on(&verifications, &name, "a refused verification");
}

/// **A rebuild's discard stops a verification in flight over the store it
/// discards, and the verdict is none.** The verification is held with its
/// copy of a damaged store written; the rebuild retires the store lifetime
/// before it discards the store, which stops the verification, and goes on
/// once it has returned. Its unit reports no damage, and the rebuilt store
/// is the lifetime the ops answer for.
#[test]
fn a_rebuild_stops_a_verification_in_flight_and_no_verdict_counts() {
    let f = Fixture::new("verification-rebuild-stops");
    write_a_vault_of_documents(&f, 8);
    let (ops, name) = f.ops(64);
    let ops = Arc::new(ops);
    let progress = ProgressReporter::disconnected();
    let mut attachment = ops.attach(&f.registration(), &progress).unwrap();
    norn_store::induced_failure::execute_out_of_band(
        &mut attachment.store,
        "INSERT INTO documents_fts (rowid, body) VALUES (99999, 'a body no document holds')",
    )
    .unwrap();
    let epoch = attachment.store.epoch().to_string();
    let holding = hold_until_stopped(&ops.verifications, VerificationPoint::CopyCheck);
    ops.verifications.bring_due(&name);
    let unit = {
        let ops = Arc::clone(&ops);
        let name = name.clone();
        thread::spawn(move || ops.background_unit(&name))
    };
    holding
        .recv_timeout(Duration::from_secs(15))
        .expect("the verification to be held");

    let began = Instant::now();
    let attachment = ops
        .rebuild(&name, attachment, &progress)
        .expect("rung 3 over a store whose verification is in flight");
    assert!(began.elapsed() < HELD_UNTIL_STOPPED / 2);
    assert_eq!(
        unit.join().unwrap(),
        crate::BackgroundUnit::Ran { owed: false },
        "a stopped verification answered a verdict"
    );
    let reading = ops.verification_reading();
    assert_eq!((reading.interrupted, reading.damaged), (1, 0), "{reading:?}");
    assert!(!ops.store_lifetime_current(&name, &epoch));
    assert!(ops.store_lifetime_current(&name, attachment.store.epoch()));
    ops.detach(&name, attachment);
}

/// **A detach stops a verification in flight and returns within the bound.**
/// The verification is held, with its copy of a damaged store written, until
/// it is stopped; an idle detach retires the store lifetime, which stops it,
/// and the entry is unattached long before the hold would have given up. The
/// stopped verification is no verdict, and no rebuild follows.
#[test]
fn a_detach_stops_a_verification_in_flight_and_returns_within_the_bound() {
    let f = Fixture::new("verification-detach-stops");
    write_a_vault_of_documents(&f, 8);
    let ops = fixture_ops(&f);
    let verifications = Arc::clone(&ops.verifications);
    let holding = hold_until_stopped(&verifications, VerificationPoint::CopyCheck);
    let (host, name, lease) = ready_host(&f, ops);
    let rebuilds_before = host.ops().evidence.read().rebuilds_run;
    damage_the_full_text_index(&f, &name);
    verification_comes_due(&host, &verifications, &name);
    holding
        .recv_timeout(Duration::from_secs(15))
        .expect("the verification to be held");

    let began = Instant::now();
    drop(lease);
    host.reap_idle(Instant::now() + Duration::from_secs(3600))
        .expect("an idle reap");
    wait_state(&host, &name, norn_wire::TrustState::Unattached);
    assert!(
        began.elapsed() < HELD_UNTIL_STOPPED / 2,
        "the detach waited {:?} for the verification",
        began.elapsed()
    );
    let reading = verifications.reading();
    assert_eq!((reading.interrupted, reading.damaged), (1, 0), "{reading:?}");
    assert_eq!(host.ops().evidence.read().rebuilds_run, rebuilds_before);
}

/// **Host shutdown does not wait for a full scan.** The verification is held
/// until it is stopped; dropping the host stops it and returns long before
/// the hold would have given up.
#[test]
fn host_shutdown_stops_a_verification_in_flight() {
    let f = Fixture::new("verification-shutdown-stops");
    write_a_vault_of_documents(&f, 8);
    let ops = fixture_ops(&f);
    let verifications = Arc::clone(&ops.verifications);
    let holding = hold_until_stopped(&verifications, VerificationPoint::Row);
    let (host, name, lease) = ready_host(&f, ops);
    verification_comes_due(&host, &verifications, &name);
    holding
        .recv_timeout(Duration::from_secs(15))
        .expect("the verification to be held");

    let began = Instant::now();
    drop(lease);
    drop(host);
    assert!(
        began.elapsed() < HELD_UNTIL_STOPPED / 2,
        "shutdown waited {:?} for the verification",
        began.elapsed()
    );
    assert_eq!(verifications.reading().interrupted, 1);
}

/// **An idle vault's verification comes due again, deadline after deadline,
/// with no wake and no poll.** Each verification moves its clock on one
/// interval, and the lane hands the clock's next reading back to the
/// scheduler as the turn ends, so the next verification starts at that
/// deadline: nothing else asks — the watcher poll is a minute away and the
/// vault takes no write.
#[test]
fn an_idle_vaults_verification_comes_due_again_at_each_deadline() {
    let f = Fixture::new("verification-due-again");
    write_a_vault_of_documents(&f, 4);
    let ops = fixture_ops(&f).with_verification_interval(Duration::from_millis(200));
    let verifications = Arc::clone(&ops.verifications);
    let (host, _name, _lease) = ready_host(&f, ops);
    let wakes = host.scheduler_reading().wakes;
    wait_until("three verifications to run", lifecycle_budget(), || {
        let reading = verifications.reading();
        if reading.sound >= 3 {
            Observed::Met(())
        } else {
            Observed::Pending(format!("{reading:?}"))
        }
    })
    .unwrap_or_else(|failure| panic!("{failure}"));
    let scheduler = host.scheduler_reading();
    assert_eq!(scheduler.wakes, wakes, "a wake asked: {scheduler:?}");
    assert!(scheduler.deadlines >= 3, "{scheduler:?}");
    assert_eq!(host.maintenance_reading().legs, 0);
}

/// **A copy a crash left beside the store is gone once the attach is
/// Ready**, before any verification of the store has run: the attach clears
/// it, rather than leaving it on disk for an hour until the first
/// verification clears the copy's place.
#[test]
fn a_copy_a_crash_left_is_cleared_at_attach() {
    let f = Fixture::new("verification-leftover-at-attach");
    write_a_vault_of_documents(&f, 4);
    let derived = ConfigDirs::new(f.root.join("config"), f.root.join("data"))
        .unwrap()
        .derived_dir(&f.registration().name);
    fs::create_dir_all(&derived).unwrap();
    let copy = derived.join(STORE_FILE).with_extension("verification-copy");
    fs::write(&copy, b"a copy a crash left").unwrap();
    let ops = fixture_ops(&f);
    let verifications = Arc::clone(&ops.verifications);
    let (_host, _name, _lease) = ready_host(&f, ops);
    assert_eq!(verifications.reading().runs, 0);
    assert!(!copy.exists(), "the attach left a crash's copy");
}

/// Production ops whose next watcher poll, once armed, reports an overflow,
/// and whose next reconcile, once armed, holds its claim until the case lets
/// it go: the stance an overflow publishes is held for as long as the case
/// needs it to refuse.
struct OverflowHeldOpen {
    inner: ProductionEntryOps,
    overflow: std::sync::atomic::AtomicBool,
    hold_reconcile: std::sync::atomic::AtomicBool,
    reconcile_held: Mutex<mpsc::Sender<()>>,
    reconcile_released: Mutex<mpsc::Receiver<()>>,
}

impl EntryOps for OverflowHeldOpen {
    type Attachment = ProductionAttachment;
    fn sweep_unregistered(&self, registered: &[VaultName]) {
        self.inner.sweep_unregistered(registered);
    }

    fn attach(
        &self,
        registration: &Registration,
        progress: &ProgressReporter<Self::Attachment>,
    ) -> Result<Self::Attachment, JobFailure> {
        self.inner.attach(registration, progress)
    }

    fn reconcile(
        &self,
        name: &VaultName,
        attachment: &mut Self::Attachment,
        work: ReconcileWork,
        progress: &ProgressReporter<Self::Attachment>,
    ) -> Result<(), JobFailure> {
        if self
            .hold_reconcile
            .swap(false, std::sync::atomic::Ordering::SeqCst)
        {
            self.reconcile_held.lock().unwrap().send(()).unwrap();
            let _ = self
                .reconcile_released
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(30));
        }
        self.inner.reconcile(name, attachment, work, progress)
    }

    fn recover(
        &self,
        name: &VaultName,
        attachment: &mut Self::Attachment,
        progress: &ProgressReporter<Self::Attachment>,
    ) -> Result<(), JobFailure> {
        self.inner.recover(name, attachment, progress)
    }

    fn rebuild(
        &self,
        name: &VaultName,
        attachment: Self::Attachment,
        progress: &ProgressReporter<Self::Attachment>,
    ) -> Result<Self::Attachment, JobFailure> {
        self.inner.rebuild(name, attachment, progress)
    }

    fn poll(
        &self,
        name: &VaultName,
        attachment: &mut Self::Attachment,
    ) -> Result<Option<norn_fs::Batch>, JobFailure> {
        let polled = self.inner.poll(name, attachment)?;
        if self.overflow.swap(false, std::sync::atomic::Ordering::SeqCst) {
            return Ok(Some(norn_fs::Batch::rescan(RescanScope::Vault)));
        }
        Ok(polled)
    }

    fn maintenance_deadline(&self, attachment: &Self::Attachment) -> Option<Instant> {
        self.inner.maintenance_deadline(attachment)
    }

    fn maintain_quiet(
        &self,
        name: &VaultName,
        attachment: &mut Self::Attachment,
        quiet: bool,
        turn: &mut MaintenanceTurn,
        progress: &ProgressReporter<Self::Attachment>,
    ) -> Result<(), JobFailure> {
        self.inner
            .maintain_quiet(name, attachment, quiet, turn, progress)
    }

    fn rolling_heal(&self, attachment: &Self::Attachment) -> Option<norn_wire::RollingHealStatus> {
        self.inner.rolling_heal(attachment)
    }

    fn detach(&self, name: &VaultName, attachment: Self::Attachment) {
        self.inner.detach(name, attachment);
    }

    fn discard(&self, name: &VaultName) {
        self.inner.discard(name);
    }

    fn count_leg_mint(&self, statements: u64) {
        self.inner.count_leg_mint(statements);
    }

    fn background_owed(&self, name: &VaultName) -> bool {
        self.inner.background_owed(name)
    }

    fn background_unit(&self, name: &VaultName) -> crate::BackgroundUnit {
        self.inner.background_unit(name)
    }

    fn background_deadline(&self, name: &VaultName) -> Option<Instant> {
        self.inner.background_deadline(name)
    }

    fn store_lifetime_current(&self, name: &VaultName, epoch: &str) -> bool {
        self.inner.store_lifetime_current(name, epoch)
    }

    fn retire_background(&self) {
        self.inner.retire_background();
    }
}

/// Whether `state` is the refusal a watcher overflow publishes.
fn refuses_for_an_overflow(state: &Result<norn_wire::TrustState, norn_wire::ErrorEnvelope>) -> bool {
    state.as_ref().is_err_and(|envelope| {
        matches!(
            envelope.detail(),
            norn_wire::ErrorDetail::EntryUntrusted {
                reason: norn_wire::UntrustedReason::WatcherOverflow,
                ..
            }
        )
    })
}

/// **Damage a verification brings back while an overflow's reconcile holds
/// the entry is rebuilt once that reconcile ends.** The verification is held
/// with its copy of a damaged store written; a watcher overflow then lands,
/// so the entry refuses reads while its reconcile — which keeps the store
/// lifetime the verification read — holds the claim. The verdict the
/// verification brings back meanwhile is about the store the entry still
/// stands on, and the reconcile's `Ready` does not answer it: the rebuild
/// runs, once, and the entry ends `Ready` over the rebuilt store.
#[test]
fn damage_a_verification_brings_back_during_an_overflow_is_rebuilt() {
    let f = Fixture::new("verification-damage-over-overflow");
    write_a_vault_of_documents(&f, 8);
    let inner = fixture_ops(&f);
    let verifications = Arc::clone(&inner.verifications);
    let (held, holding) = mpsc::channel();
    let (resume, resumed) = mpsc::channel::<()>();
    let (held, resumed) = (Mutex::new(Some(held)), Mutex::new(resumed));
    verifications.run_inside_verifications(move |at, _| {
        if at != VerificationPoint::CopyCheck {
            return;
        }
        // Only the first verification is held.
        let Some(held) = held.lock().unwrap().take() else {
            return;
        };
        held.send(()).unwrap();
        let _ = resumed.lock().unwrap().recv_timeout(Duration::from_secs(30));
    });
    let (reconcile_held, reconcile_holding) = mpsc::channel();
    let (release_reconcile, reconcile_released) = mpsc::channel();
    let registration = f.registration();
    let name = registration.name.clone();
    let host = crate::Host::new(
        crate::RegistryRead::from_entries([registration]),
        OverflowHeldOpen {
            inner,
            overflow: std::sync::atomic::AtomicBool::new(false),
            hold_reconcile: std::sync::atomic::AtomicBool::new(false),
            reconcile_held: Mutex::new(reconcile_held),
            reconcile_released: Mutex::new(reconcile_released),
        },
        crate::LifecyclePolicy {
            idle_after: Duration::from_secs(60),
            worker_slots: 1,
            watch_poll_interval: Duration::from_millis(5),
            read_settle_bound: crate::READ_SETTLE_BOUND,
            background_threads: crate::BACKGROUND_THREADS,
            background_turn_budget: crate::BACKGROUND_TURN_BUDGET,
        },
    )
    .unwrap();
    norn_fs::watch::settle_prior_changes(f.root.root(), lifecycle_budget().work()).unwrap();
    let _lease = host.demand(&name).unwrap();
    wait_state(&host, &name, norn_wire::TrustState::Ready);
    let rebuilds_before = host.ops().inner.evidence.read().rebuilds_run;
    damage_the_full_text_index(&f, &name);
    verification_comes_due(&host, &verifications, &name);
    holding
        .recv_timeout(Duration::from_secs(15))
        .expect("the verification to be held");

    host.ops()
        .hold_reconcile
        .store(true, std::sync::atomic::Ordering::SeqCst);
    host.ops()
        .overflow
        .store(true, std::sync::atomic::Ordering::SeqCst);
    reconcile_holding
        .recv_timeout(Duration::from_secs(15))
        .expect("the overflow's reconcile to hold the entry");
    assert!(refuses_for_an_overflow(&host.state(&name)));

    let turns = host.lane_reading().turns;
    resume.send(()).unwrap();
    wait_until("the verdict to be judged", lifecycle_budget(), || {
        let (reading, lane) = (verifications.reading(), host.lane_reading());
        if reading.damaged == 1 && lane.turns > turns {
            Observed::Met(())
        } else {
            Observed::Pending(format!("{reading:?}, {lane:?}"))
        }
    })
    .unwrap_or_else(|failure| panic!("{failure}"));
    assert!(refuses_for_an_overflow(&host.state(&name)));
    release_reconcile.send(()).unwrap();

    wait_until("the damage to be rebuilt", lifecycle_budget(), || {
        let rebuilt = host.ops().inner.evidence.read().rebuilds_run - rebuilds_before;
        match host.state(&name) {
            Ok(norn_wire::TrustState::Ready) if rebuilt == 1 => Observed::Met(()),
            state => Observed::Pending(format!(
                "{state:?}, {rebuilt} rebuilt, {:?}",
                verifications.reading()
            )),
        }
    })
    .unwrap_or_else(|failure| panic!("{failure}"));
    assert_eq!(
        host.ops().inner.evidence.read().rebuilds_run - rebuilds_before,
        1
    );
    // The verdict set aside left the verification owed, and it met the
    // damage again once the reconcile's `Ready` let the lane feed.
    assert_eq!(verifications.reading().damaged, 2);
}
