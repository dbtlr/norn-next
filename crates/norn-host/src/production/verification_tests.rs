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
    let f = Fixture::new("verification-damage");
    write_a_vault_of_documents(&f, 8);
    let ops = fixture_ops(&f);
    let verifications = Arc::clone(&ops.verifications);
    let (host, name, _lease) = ready_host(&f, ops);
    let rebuilds_before = host.ops().evidence.read().rebuilds_run;
    damage_the_full_text_index(&f, &name);
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
