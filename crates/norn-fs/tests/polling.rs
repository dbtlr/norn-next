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
    File::open(clock)
        .unwrap()
        .set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(10_000 + number))
        .unwrap();
    watcher.poll().unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut paths = Vec::new();
    loop {
        let event = events
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .expect("manual tick completes within five seconds")
            .unwrap();
        let completed = event.paths.iter().any(|path| path == clock);
        paths.extend(event.paths);
        if completed {
            let _ = watcher.content_reads();
            paths.extend(events.try_iter().flat_map(|event| event.unwrap().paths));
            return paths;
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

    let old = fs::metadata(&target).unwrap();
    fs::set_permissions(&target, fs::Permissions::from_mode(old.mode() ^ 0o100)).unwrap();
    let new = fs::metadata(&target).unwrap();
    assert_eq!(
        (old.len(), old.modified().unwrap(), old.ino()),
        (new.len(), new.modified().unwrap(), new.ino())
    );
    assert_ne!(
        (old.ctime(), old.ctime_nsec()),
        (new.ctime(), new.ctime_nsec())
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
