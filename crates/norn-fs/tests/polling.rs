//! Polling spends content reads only on entries whose stat changed.
#![allow(clippy::disallowed_methods, clippy::disallowed_types)] // Fixture writes simulate external editors.

use std::fs::{self, File};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant, SystemTime};

use norn_testkit::scratch::Scratch;
use notify::{Config, Event, PollWatcher, RecursiveMode, Watcher};

// An external directory supplies a completion event without reading file content.
// Taking the counter's lock after that event waits for the entire tick to finish.
fn tick(
    watcher: &PollWatcher,
    events: &Receiver<notify::Result<Event>>,
    clock: &Path,
    number: u64,
) -> Vec<PathBuf> {
    tick_reports(watcher, events, clock, number)
        .into_iter()
        .flat_map(|report| report.unwrap().paths)
        .collect()
}

/// Every report one manual tick delivers, errors included, along with any
/// report still queued from before it.
fn tick_reports(
    watcher: &PollWatcher,
    events: &Receiver<notify::Result<Event>>,
    clock: &Path,
    number: u64,
) -> Vec<notify::Result<Event>> {
    File::open(clock)
        .unwrap()
        .set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(10_000 + number))
        .unwrap();
    watcher.poll().unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut reports = Vec::new();
    loop {
        let report = events
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .expect("manual tick completes within five seconds");
        let completed = report
            .as_ref()
            .is_ok_and(|event| event.paths.iter().any(|path| path == clock));
        reports.push(report);
        if completed {
            let _ = watcher.content_reads();
            reports.extend(events.try_iter());
            return reports;
        }
    }
}

#[test]
fn unchanged_poll_ticks_read_no_content_and_stat_changes_are_reported() {
    let scratch = Scratch::new("norn-fs-polling-stat");
    let vault = scratch.join("vault");
    let clock = scratch.join("clock");
    fs::create_dir(&vault).unwrap();
    fs::create_dir(&clock).unwrap();
    File::open(&clock)
        .unwrap()
        .set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(9_999))
        .unwrap();
    for index in 0..50 {
        fs::write(vault.join(format!("{index}.md")), b"original").unwrap();
    }
    let target = vault.join("0.md");
    let (tx, events) = mpsc::channel();
    let mut watcher = PollWatcher::new(
        tx,
        Config::default()
            .with_manual_polling()
            .with_compare_contents(true),
    )
    .unwrap();
    watcher.watch(&vault, RecursiveMode::Recursive).unwrap();
    watcher.watch(&clock, RecursiveMode::NonRecursive).unwrap();
    assert!(
        watcher.content_reads() >= 50,
        "counter observes real hashes"
    );
    tick(&watcher, &events, &clock, 0);
    let before = watcher.content_reads();
    for number in 1..=3 {
        let paths = tick(&watcher, &events, &clock, number);
        assert!(!paths.iter().any(|path| path.starts_with(&vault)));
    }
    assert_eq!(
        watcher.content_reads() - before,
        0,
        "three unchanged ticks over 50 files must read no content"
    );

    // Each edit below can also move ctime on a nanosecond-timestamp volume; the
    // whole-second case holds size, mtime and inode to a change of their own.
    fs::write(&target, b"longer content").unwrap();
    assert!(tick(&watcher, &events, &clock, 4).contains(&target), "size");
    assert_eq!(watcher.content_reads() - before, 1);

    let old = fs::metadata(&target).unwrap();
    fs::write(&target, b"same-size edit").unwrap();
    File::open(&target)
        .unwrap()
        .set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(20_000))
        .unwrap();
    let new = fs::metadata(&target).unwrap();
    assert_eq!(old.len(), new.len());
    assert_ne!(old.modified().unwrap(), new.modified().unwrap());
    assert!(
        tick(&watcher, &events, &clock, 5).contains(&target),
        "mtime"
    );
    assert_eq!(watcher.content_reads() - before, 2);

    // Linux stamps ctime from a coarse clock (before 6.13, or on a filesystem
    // without multigrain timestamps), so a chmod in the same tick as the edit
    // above leaves ctime where it was. Of the fields polling compares, a chmod
    // changes only ctime, so the same mode is reapplied until ctime moves.
    let old = fs::metadata(&target).unwrap();
    let mode = fs::Permissions::from_mode(old.mode() ^ 0o100);
    // Two seconds cross a ctime tick even where ctime keeps whole seconds.
    let deadline = Instant::now() + Duration::from_secs(2);
    let new = loop {
        fs::set_permissions(&target, mode.clone()).unwrap();
        let new = fs::metadata(&target).unwrap();
        let moved = (old.ctime(), old.ctime_nsec()) != (new.ctime(), new.ctime_nsec());
        if moved || Instant::now() >= deadline {
            break new;
        }
        std::thread::sleep(Duration::from_millis(1));
    };
    assert_eq!(
        (old.len(), old.modified().unwrap(), old.ino()),
        (new.len(), new.modified().unwrap(), new.ino())
    );
    assert_ne!(
        (old.ctime(), old.ctime_nsec()),
        (new.ctime(), new.ctime_nsec()),
        "ctime did not move within two seconds of reapplied chmods"
    );
    assert!(
        tick(&watcher, &events, &clock, 6).contains(&target),
        "ctime-only chmod"
    );
    assert_eq!(watcher.content_reads() - before, 3);

    let replacement = scratch.join("replacement.md");
    fs::write(&replacement, b"same-size edit").unwrap();
    File::open(&replacement)
        .unwrap()
        .set_modified(new.modified().unwrap())
        .unwrap();
    fs::rename(&replacement, &target).unwrap();
    assert_ne!(new.ino(), fs::metadata(&target).unwrap().ino());
    assert!(
        tick(&watcher, &events, &clock, 7).contains(&target),
        "new inode"
    );
    assert_eq!(watcher.content_reads() - before, 4);
}

/// **An entry the scan lists but cannot stat is gone, not a failure.** A
/// scan lists a directory and then reads each entry's metadata, so a file an
/// editor deletes between the two, or a symbolic link whose target does not
/// exist, names nothing by the time it is read. Either is reported as an
/// error, and an error from a watcher is terminal to its consumer, so one
/// temporary file or one dangling link would end coverage for good. The
/// entry is skipped instead, and a tick that no longer finds a path it held
/// reports the removal.
///
/// The dangling link is read with links followed, because that makes the
/// scan meet a missing entry on every tick rather than only inside a race.
///
/// **The watched root itself is not an entry.** A root that is gone is the
/// loss of the whole watch, and it is still reported as an error.
#[test]
fn an_entry_gone_before_its_stat_is_skipped_but_a_missing_root_is_reported() {
    let scratch = Scratch::new("norn-fs-polling-vanished");
    let vault = scratch.join("vault");
    let clock = scratch.join("clock");
    fs::create_dir(&vault).unwrap();
    fs::create_dir(&clock).unwrap();
    fs::write(vault.join("note.md"), b"note").unwrap();
    std::os::unix::fs::symlink(scratch.join("missing"), vault.join("dangling")).unwrap();
    let (tx, events) = mpsc::channel();
    let mut watcher = PollWatcher::new(
        tx,
        Config::default()
            .with_manual_polling()
            .with_compare_contents(true)
            .with_follow_symlinks(true),
    )
    .unwrap();
    watcher.watch(&vault, RecursiveMode::Recursive).unwrap();
    watcher.watch(&clock, RecursiveMode::NonRecursive).unwrap();

    for number in 0..2 {
        let errors: Vec<_> = tick_reports(&watcher, &events, &clock, number)
            .into_iter()
            .filter_map(Result::err)
            .collect();
        assert!(errors.is_empty(), "tick {number} reported {errors:?}");
    }

    fs::remove_dir_all(&vault).unwrap();
    assert!(
        tick_reports(&watcher, &events, &clock, 2)
            .iter()
            .any(Result::is_err),
        "a watched root that is gone is reported as an error"
    );
}

#[test]
fn a_polled_vault_with_a_denied_subdirectory_stays_live() {
    let scratch = Scratch::new("norn-fs-polling-denied");
    let vault = scratch.join("vault");
    let denied = vault.join("D");
    let clock = scratch.join("clock");
    fs::create_dir_all(&denied).unwrap();
    fs::create_dir(&clock).unwrap();
    let hidden = denied.join("hidden.md");
    let readable = vault.join("readable.md");
    fs::write(&hidden, b"hidden").unwrap();
    fs::write(&readable, b"before").unwrap();
    fs::set_permissions(&denied, fs::Permissions::from_mode(0o000)).unwrap();
    if fs::read_dir(&denied).is_ok() {
        eprintln!("this account bypasses directory modes; polling denial is unasserted");
        return;
    }

    let (tx, events) = mpsc::channel();
    let mut watcher = PollWatcher::new(
        tx,
        Config::default()
            .with_manual_polling()
            .with_compare_contents(true),
    )
    .unwrap();
    watcher.watch(&vault, RecursiveMode::Recursive).unwrap();
    watcher.watch(&clock, RecursiveMode::NonRecursive).unwrap();
    for number in 0..2 {
        let reports = tick_reports(&watcher, &events, &clock, number);
        assert!(reports.iter().all(Result::is_ok), "{reports:?}");
        assert!(
            reports
                .iter()
                .all(|report| !report.as_ref().unwrap().paths.contains(&hidden))
        );
    }

    fs::write(&readable, b"after").unwrap();
    assert!(tick(&watcher, &events, &clock, 2).contains(&readable));

    let old = fs::metadata(&denied).unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        fs::set_permissions(&denied, fs::Permissions::from_mode(0o755)).unwrap();
        let new = fs::metadata(&denied).unwrap();
        if (old.ctime(), old.ctime_nsec()) != (new.ctime(), new.ctime_nsec()) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "directory chmod did not move ctime"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    let reports = tick_reports(&watcher, &events, &clock, 3);
    let events: Vec<_> = reports.into_iter().collect::<notify::Result<_>>().unwrap();
    assert!(
        events.iter().any(|event| {
            event.paths.contains(&denied)
                && matches!(
                    event.kind,
                    notify::EventKind::Modify(notify::event::ModifyKind::Metadata(_))
                )
        }),
        "the denied directory's own stat must survive: {events:?}"
    );
    assert!(events.iter().any(|event| event.paths.contains(&hidden)));
}

/// The one stat field a whole-second case changes.
#[cfg(target_os = "macos")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Field {
    Size,
    Mtime,
    Inode,
}

#[cfg(target_os = "macos")]
impl Field {
    fn name(self) -> &'static str {
        match self {
            Field::Size => "size",
            Field::Mtime => "mtime",
            Field::Inode => "inode",
        }
    }
}

/// The four fields polling compares, as one value: size, mtime, ctime, inode.
#[cfg(target_os = "macos")]
fn stat(path: &Path) -> (u64, (i64, i64), (i64, i64), u64) {
    let metadata = fs::metadata(path).unwrap();
    (
        metadata.len(),
        (metadata.mtime(), metadata.mtime_nsec()),
        (metadata.ctime(), metadata.ctime_nsec()),
        metadata.ino(),
    )
}

/// The fields that differ between two stats.
#[cfg(target_os = "macos")]
fn differing(
    old: (u64, (i64, i64), (i64, i64), u64),
    new: (u64, (i64, i64), (i64, i64), u64),
) -> Vec<&'static str> {
    [
        ("size", old.0 != new.0),
        ("mtime", old.1 != new.1),
        ("ctime", old.2 != new.2),
        ("inode", old.3 != new.3),
    ]
    .into_iter()
    .filter_map(|(name, differs)| differs.then_some(name))
    .collect()
}

/// Sleep until the wall clock is early in a second, so a case's write and its
/// edit land within one whole-second timestamp.
#[cfg(target_os = "macos")]
fn start_of_a_second() {
    let into = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .subsec_millis();
    if into > 100 {
        std::thread::sleep(Duration::from_millis(u64::from(1_005 - into)));
    }
}

/// On a volume that keeps timestamps to the whole second, an edit made in the
/// second its file was written can change size, mtime or inode while leaving
/// ctime where it was. Each of those fields alone is a reported change, and
/// the one entry it changed is the one whose content polling reads again.
#[cfg(target_os = "macos")]
#[test]
fn a_change_to_size_mtime_or_inode_alone_is_reported_on_a_whole_second_volume() {
    let volume = norn_testkit::volume::Volume::provision("polling-stat")
        .unwrap_or_else(|reason| panic!("this case needs a whole-second volume: {reason}"));
    // The clock stays on the scratch volume, whose nanosecond ctime moves on
    // every tick, so a tick completes whatever this volume's stat reports.
    let scratch = Scratch::new("norn-fs-polling-whole-second");
    let vault = volume.mount().join("vault");
    let clock = scratch.join("clock");
    fs::create_dir(&vault).unwrap();
    fs::create_dir(&clock).unwrap();
    let (tx, events) = mpsc::channel();
    let mut watcher = PollWatcher::new(
        tx,
        Config::default()
            .with_manual_polling()
            .with_compare_contents(true),
    )
    .unwrap();
    watcher.watch(&vault, RecursiveMode::Recursive).unwrap();
    watcher.watch(&clock, RecursiveMode::NonRecursive).unwrap();
    let mut ticks = 0..;

    for field in [Field::Size, Field::Mtime, Field::Inode] {
        // A second boundary crossed between the write and the edit moves ctime
        // too; such an attempt proves nothing about the field and is retried.
        let observed = (0..5).find_map(|attempt| {
            let target = vault.join(format!("{field:?}-{attempt}.md"));
            start_of_a_second();
            fs::write(&target, b"aaaa").unwrap();
            tick(&watcher, &events, &clock, ticks.next().unwrap());
            let old = stat(&target);
            let before = watcher.content_reads();
            match field {
                Field::Size => {
                    use std::io::Write;
                    File::options()
                        .append(true)
                        .open(&target)
                        .unwrap()
                        .write_all(b"b")
                        .unwrap();
                }
                Field::Mtime => {
                    fs::write(&target, b"bbbb").unwrap();
                    File::open(&target)
                        .unwrap()
                        .set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(5_000))
                        .unwrap();
                }
                Field::Inode => {
                    let replacement = volume.mount().join(format!("replacement-{attempt}.md"));
                    fs::write(&replacement, b"bbbb").unwrap();
                    fs::rename(&replacement, &target).unwrap();
                }
            }
            let changed = differing(old, stat(&target));
            let paths = tick(&watcher, &events, &clock, ticks.next().unwrap());
            let reads = watcher.content_reads() - before;
            (changed == [field.name()]).then(|| (paths.contains(&target), reads))
        });
        assert_eq!(
            observed,
            Some((true, 1)),
            "a change to {} alone must be reported and re-read once \
             (None: no attempt changed it alone)",
            field.name()
        );
    }
}
