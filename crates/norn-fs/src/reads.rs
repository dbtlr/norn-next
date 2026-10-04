//! What this thread asked the filesystem for while a window stood over it.
//!
//! A caller sees the answers a walk or a read hands back and never how many
//! acts produced them. This module counts some of those acts as they happen,
//! and hands the count to a caller through [`ReadWindow`].
//!
//! # The counted set is narrow, and the fields say what is in it
//!
//! This is not every stat the crate takes. Five acts are counted — the reads
//! of a file's content through a descriptor `open_regular_at` handed back (and
//! the write kernel's copy of a create's source, which is the same act), the
//! stats that open and the walk take along the way, the directory entries a
//! walk pulls off a stream, and the write kernel's reads of a target and of a
//! staged shadow — and [`ReadTally`]'s fields name them one at a time, each
//! with what it leaves out. An act outside those fields is outside the tally
//! by construction: no call site elsewhere reaches the counters.
//!
//! **A read is counted by the act that reads the bytes**, not by the open
//! that precedes it: the function that reads a file's content and hashes it
//! counts itself, so a descriptor read and hashed twice counts twice, and no
//! counted reader can hash a byte it did not count.
//!
//! # Which file was read, under `induced-failure`
//!
//! A count says how many reads a thread took and not of what. A build with
//! `induced-failure` also records, per counted read, the act that took it and
//! the path it read (`FileRead`), while a `FileRecording` is armed and a
//! window stands on the thread; `ReadWindow::finish_with_files` hands both
//! back together. A build without the feature carries none of it: no type,
//! no storage and no code at a read site.
//!
//! What is counted is what the churn suite's cost bars are stated over, so the
//! set is widened by a bar that needs an act it does not hold, and widening it
//! means changing the field that names the act.
//!
//! # A caller opens a window rather than reading a number
//!
//! The counts are thread-local, and that is what makes them attributable: what
//! a window reports is what one thread read while the window stood, rather than
//! a shared number two jobs moved at once. A process-wide count would need
//! every other thread to be idle to mean the same thing.
//!
//! One window stands per thread and reports once — it empties the counts as it
//! opens and is consumed by [`ReadWindow::finish`] — so a reading cannot be
//! taken twice, and a second reader cannot take the reading the window's owner
//! is standing on.
//!
//! # This is evidence, not a bar
//!
//! Nothing here decides anything: no read is refused for having counted too
//! much, and no caller is asked to keep the count small. It exists so that a
//! suite comparing two derivations of one vault can say what each of them
//! spent, which is a question no answer either derivation returns can settle.

use std::cell::Cell;
use std::marker::PhantomData;

/// What one thread read while a window stood over it.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ReadTally {
    /// Files read for their content through a descriptor `open_regular_at`
    /// handed back, which is the one route both a document read and a walk's
    /// own read of a file it enumerated take: one per read of the file's
    /// content, counted by the read itself, so a descriptor read twice counts
    /// twice. Every reader takes one open and reads it once, so the count is
    /// also the opens that reached a regular file and were read. The write
    /// kernel's copy of a create's source is one too: it reads a vault file
    /// for its content, whole and once, through the same anchored descent,
    /// though the open is the kernel's own. A directory opened to descend into
    /// is not one of these, and neither is a file any other protocol in this
    /// crate opens — a target hashed to judge it, or a shadow.
    pub document_opens: u64,
    /// The stats those same two acts take, and only those: the `fstat`
    /// `open_regular_at` reads a reached file's kind from, the `statat`
    /// it tells a symbolic link from a non-directory with, [`crate::Vault::reach`]'s
    /// stat of the last name of a path a caller supplied, and the walk's own five — the
    /// frontier entry it is about to classify, a directory entry whose kind the
    /// stream did not report, the re-stat that pages an entry, the target of
    /// a symbolic link it is classifying, and the `fstat` of a file the walk
    /// opened for its content, which reads that file's size and mtime.
    ///
    /// Deliberately outside: the registry's classification of served roots,
    /// the normalization probes under path handling, and every stat the shadow,
    /// lock and write protocols take. Those are acts of other kinds, and a lock
    /// acquisition inside a derivation's reading would make the reading answer
    /// a different question.
    pub stats: u64,
    /// Directory entries a walk took off a directory stream, `.` and `..`
    /// excluded. Enumeration only: what the walk then does with an entry is
    /// counted by the two fields above or by nothing.
    ///
    /// A walk takes entries for two reasons and both are here: the pages it
    /// enumerates, and the listing a descent on a folding root reads to confirm
    /// one caller-supplied name. They share a field because they are one act
    /// against one stream and because no bar reads this to mean paging alone —
    /// the crate's own paging bar counts a paging loop it drives itself, and
    /// the host's churn bounds are stated over documents opened and rows
    /// written. A reader that needs the two apart splits the field rather than
    /// inferring the split.
    pub walk_dirents: u64,
    /// Targets the write kernel read to judge their state: one per hash of a
    /// regular file opened at a target's name, taken by staging, by
    /// publication's verification again, and by a landing's confirmation. A
    /// name holding no regular file — a create's absent target among them —
    /// opens nothing to hash and is not one.
    ///
    /// Apart from [`ReadTally::document_opens`] because the act is another
    /// protocol's: the kernel reads a target to judge a transition, through
    /// its own no-follow open, and no byte of it reaches derivation. The
    /// hashing counts itself, so a descriptor hashed twice counts twice.
    pub target_reads: u64,
    /// Staged shadows the write kernel read to confirm, just before a
    /// publication act, that the shadow is still the file staging made and
    /// still holds what staging wrote: one per hash of a confirmation, counted
    /// by the hashing itself. A shadow lives in the shadow home, under no name
    /// of the vault's.
    pub shadow_reads: u64,
}

thread_local! {
    static TALLY: Cell<ReadTally> = const { Cell::new(ReadTally {
        document_opens: 0,
        stats: 0,
        walk_dirents: 0,
        target_reads: 0,
        shadow_reads: 0,
    }) };
    /// Whether a window already stands on this thread.
    static STANDING: Cell<bool> = const { Cell::new(false) };
}

/// Which counted act read a file: the [`ReadTally`] field the read is in.
#[cfg(feature = "induced-failure")]
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum ReadAct {
    /// A read of a file's content, [`ReadTally::document_opens`].
    Document,
    /// The write kernel's read of a target, [`ReadTally::target_reads`].
    Target,
    /// The write kernel's read of a staged shadow, [`ReadTally::shadow_reads`].
    Shadow,
}

/// One counted read of one file: the act that took it and the path it read,
/// spelled as the read site holds it — the anchor joined with the relative
/// name below it for a contained read, the walked root joined with the
/// walked path for a walk's read, the vault root joined with the target's
/// path — or a create's source's — for the write kernel, and the shadow home's own name for a shadow.
#[cfg(feature = "induced-failure")]
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct FileRead {
    /// Which counted act read the file.
    pub act: ReadAct,
    /// The file it read.
    pub path: std::path::PathBuf,
}

/// How many [`FileRecording`]s stand in the process.
#[cfg(feature = "induced-failure")]
static RECORDING: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

#[cfg(feature = "induced-failure")]
thread_local! {
    /// The files this thread's counted reads read while a window stood and a
    /// recording was armed.
    static FILES: std::cell::RefCell<Vec<FileRead>> = const {
        std::cell::RefCell::new(Vec::new())
    };
}

/// While one stands, every window in the process records the file each of
/// its counted reads read, beside the count.
///
/// **Process-wide, and opt-in.** The windows it arms are on whichever threads
/// run jobs, which a harness never holds, so the arm is the process's; it is
/// off unless asked for so a long run under `induced-failure` — a soak — pays
/// no allocation per read and holds no record it never reads. A harness that
/// arms it reads one case at a time.
#[cfg(feature = "induced-failure")]
#[must_use = "the recording stands only while this is held"]
pub struct FileRecording {
    _armed: (),
}

/// Arm the recording of which files counted reads read, for as long as the
/// answer is held.
#[cfg(feature = "induced-failure")]
pub fn record_files() -> FileRecording {
    RECORDING.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    FileRecording { _armed: () }
}

#[cfg(feature = "induced-failure")]
impl Drop for FileRecording {
    fn drop(&mut self) {
        RECORDING.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }
}

/// One thread's window over its own reads.
///
/// Opening empties this thread's counts, so what the window reports is what the
/// thread read while the window stood and never what it read before. Closing —
/// through [`ReadWindow::finish`] or by dropping — empties them again, so reads
/// made under no window belong to no window rather than to the next one.
pub struct ReadWindow {
    /// The counts a window reports live in the storage of the thread that
    /// opened it, so a window carried to another thread would report what the
    /// wrong thread read. This is what keeps it where it was opened.
    _thread_bound: PhantomData<*const ()>,
}

impl ReadWindow {
    /// Open a window over this thread's reads.
    ///
    /// **One window stands per thread.** A second one here would empty the
    /// counts the first is standing on and report them as its own, so it is
    /// refused: two windows over one thread are two answers to "what did this
    /// thread read", and only one of them could be right.
    ///
    /// `norn_host::JobEvidence::attributing` is the one caller in production
    /// that opens a window, so the whole of that bookkeeping lives in one place.
    /// A suite measuring its own reads opens one too, on the thread the case
    /// runs on and around work that opens none. Anywhere a second window could
    /// stand inside a first, the assertion below turns the bookkeeping conflict
    /// into a panic on the thread that hits it rather than into two answers.
    ///
    /// The six `norn_host::EntryOps` entry points that open a window do not
    /// nest, and the assertion below is what makes that a runtime invariant of
    /// production code rather than a claim about the call graph.
    pub fn open() -> ReadWindow {
        let standing = STANDING.replace(true);
        assert!(
            !standing,
            "a read window already stands on this thread, and opening a second would take the \
             reads the first is standing on"
        );
        TALLY.set(ReadTally::default());
        #[cfg(feature = "induced-failure")]
        FILES.with_borrow_mut(Vec::clear);
        ReadWindow {
            _thread_bound: PhantomData,
        }
    }

    /// What this thread read while the window stood, and the end of the window.
    ///
    /// It consumes the window, so one window reports once and there is no
    /// second reading for a caller to fold twice.
    pub fn finish(self) -> ReadTally {
        TALLY.get()
    }

    /// What this thread read while the window stood, the file each counted
    /// read read — empty where no [`FileRecording`] was armed — and the end
    /// of the window.
    #[cfg(feature = "induced-failure")]
    pub fn finish_with_files(self) -> (ReadTally, Vec<FileRead>) {
        (TALLY.get(), FILES.with_borrow_mut(std::mem::take))
    }
}

impl Drop for ReadWindow {
    fn drop(&mut self) {
        TALLY.set(ReadTally::default());
        #[cfg(feature = "induced-failure")]
        FILES.with_borrow_mut(Vec::clear);
        STANDING.set(false);
    }
}

/// Count one read of a file's content at `path`, by the act that reads it.
pub(crate) fn count_document_read(path: &std::path::Path) {
    bump(|tally| tally.document_opens += 1);
    #[cfg(feature = "induced-failure")]
    record(ReadAct::Document, path);
    #[cfg(not(feature = "induced-failure"))]
    let _ = path;
}

pub(crate) fn count_stat() {
    bump(|tally| tally.stats += 1);
}

pub(crate) fn count_dirents(entries: u64) {
    bump(|tally| tally.walk_dirents += entries);
}

/// Count one hash of the target at `path`, by the act that hashes it.
pub(crate) fn count_target_read(path: &std::path::Path) {
    bump(|tally| tally.target_reads += 1);
    #[cfg(feature = "induced-failure")]
    record(ReadAct::Target, path);
    #[cfg(not(feature = "induced-failure"))]
    let _ = path;
}

/// Count one hash of the staged shadow at `path`, by the act that hashes it.
pub(crate) fn count_shadow_read(path: &std::path::Path) {
    bump(|tally| tally.shadow_reads += 1);
    #[cfg(feature = "induced-failure")]
    record(ReadAct::Shadow, path);
    #[cfg(not(feature = "induced-failure"))]
    let _ = path;
}

/// Record which file a counted read read, where a recording is armed and a
/// window stands to hand it back. A thread with no window records nothing,
/// so a thread that never opens one holds nothing however long it reads.
#[cfg(feature = "induced-failure")]
fn record(act: ReadAct, path: &std::path::Path) {
    if RECORDING.load(std::sync::atomic::Ordering::SeqCst) == 0 || !STANDING.get() {
        return;
    }
    FILES.with_borrow_mut(|files| {
        files.push(FileRead {
            act,
            path: path.to_path_buf(),
        });
    });
}

fn bump(change: impl FnOnce(&mut ReadTally)) {
    TALLY.with(|cell| {
        let mut tally = cell.get();
        change(&mut tally);
        cell.set(tally);
    });
}

/// Take the turn every unit test in this crate that arms a recording, or
/// reads a window unarmed, holds for its whole case: the arm is the
/// process's, so one case's arm reaches another case's window on another
/// thread.
#[cfg(all(test, feature = "induced-failure"))]
pub(crate) fn recording_cases() -> std::sync::MutexGuard<'static, ()> {
    static RECORDING_CASES: std::sync::Mutex<()> = std::sync::Mutex::new(());
    RECORDING_CASES
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    /// A window reports what the thread read while it stood, and nothing it
    /// read beforehand.
    #[test]
    fn a_window_reports_the_reads_made_under_it() {
        count_stat();
        let window = ReadWindow::open();
        count_document_read(Path::new("note.md"));
        count_stat();
        count_dirents(4);
        count_target_read(Path::new("note.md"));
        count_shadow_read(Path::new("shadow"));
        assert_eq!(
            window.finish(),
            ReadTally {
                document_opens: 1,
                stats: 1,
                walk_dirents: 4,
                target_reads: 1,
                shadow_reads: 1,
            }
        );
    }

    /// Reads made between two windows are neither window's.
    #[test]
    fn a_second_window_reports_only_its_own() {
        let first = ReadWindow::open();
        count_stat();
        assert_eq!(first.finish().stats, 1);
        count_stat();
        count_stat();
        let second = ReadWindow::open();
        count_dirents(2);
        assert_eq!(
            second.finish(),
            ReadTally {
                walk_dirents: 2,
                ..ReadTally::default()
            }
        );
    }

    /// An armed window names the file each counted read read, by the act that
    /// read it, once per read: a file read twice is named twice.
    #[cfg(feature = "induced-failure")]
    #[test]
    fn an_armed_window_names_each_file_its_reads_read() {
        let _serial = recording_cases();
        let _recording = record_files();
        let window = ReadWindow::open();
        count_document_read(Path::new("note.md"));
        count_target_read(Path::new("note.md"));
        count_target_read(Path::new("note.md"));
        count_shadow_read(Path::new("shadow"));
        let (tally, files) = window.finish_with_files();
        assert_eq!(
            (tally.document_opens, tally.target_reads, tally.shadow_reads),
            (1, 2, 1)
        );
        let read = |act, path: &str| FileRead {
            act,
            path: path.into(),
        };
        assert_eq!(
            files,
            vec![
                read(ReadAct::Document, "note.md"),
                read(ReadAct::Target, "note.md"),
                read(ReadAct::Target, "note.md"),
                read(ReadAct::Shadow, "shadow"),
            ]
        );
    }

    /// No file is named where no recording is armed, nor on a thread with no
    /// window, nor before the window that reports opened.
    #[cfg(feature = "induced-failure")]
    #[test]
    fn a_window_names_files_only_while_armed_and_standing() {
        let _serial = recording_cases();
        let unarmed = ReadWindow::open();
        count_document_read(Path::new("unarmed.md"));
        let (tally, files) = unarmed.finish_with_files();
        assert_eq!(tally.document_opens, 1);
        assert!(files.is_empty(), "an unarmed window named {files:?}");

        let _recording = record_files();
        count_document_read(Path::new("no-window.md"));
        let window = ReadWindow::open();
        count_document_read(Path::new("windowed.md"));
        let (_, files) = window.finish_with_files();
        assert_eq!(
            files,
            vec![FileRead {
                act: ReadAct::Document,
                path: "windowed.md".into(),
            }]
        );
    }

    #[test]
    #[should_panic(expected = "a read window already stands on this thread")]
    fn one_thread_carries_one_window() {
        let _standing = ReadWindow::open();
        let _second = ReadWindow::open();
    }
}
