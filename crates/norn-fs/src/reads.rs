//! What this thread asked the filesystem for while a window stood over it.
//!
//! A caller sees the answers a walk or a read hands back and never how many
//! acts produced them. This module counts some of those acts as they happen,
//! and hands the count to a caller through [`ReadWindow`].
//!
//! # The counted set is narrow, and the fields say what is in it
//!
//! This is not every stat the crate takes. Three acts are counted — the opens
//! `open_regular_at` performs, the stats that act and the walk take
//! along the way, and the directory entries a walk pulls off a stream — and
//! [`ReadTally`]'s fields name them one at a time, each with what it leaves
//! out. An act outside those fields is outside the tally by construction: no
//! call site elsewhere reaches the counters.
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
    /// Files opened for their content through `open_regular_at`, which
    /// is the one route both a document read and a walk's own read of a file it
    /// enumerated take. A directory opened to descend into is not one of these,
    /// and neither is a file opened by any other protocol in this crate.
    pub document_opens: u64,
    /// The stats those same two acts take, and only those: the `fstat`
    /// `open_regular_at` reads a reached file's kind from, the `statat`
    /// it tells a symbolic link from a non-directory with, [`crate::path_kind`]'s
    /// stat of the root an invalidation names, and the walk's own five — the
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
    pub walk_dirents: u64,
}

thread_local! {
    static TALLY: Cell<ReadTally> = const { Cell::new(ReadTally {
        document_opens: 0,
        stats: 0,
        walk_dirents: 0,
    }) };
    /// Whether a window already stands on this thread.
    static STANDING: Cell<bool> = const { Cell::new(false) };
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
    /// `norn_host::JobEvidence::attributing` is the one caller in the tree that
    /// opens a window, so the whole of the bookkeeping lives in one place; a
    /// second opener elsewhere turns a bookkeeping conflict into a panic on the
    /// worker thread that hits it.
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
}

impl Drop for ReadWindow {
    fn drop(&mut self) {
        TALLY.set(ReadTally::default());
        STANDING.set(false);
    }
}

pub(crate) fn count_document_open() {
    bump(|tally| tally.document_opens += 1);
}

pub(crate) fn count_stat() {
    bump(|tally| tally.stats += 1);
}

pub(crate) fn count_dirents(entries: u64) {
    bump(|tally| tally.walk_dirents += entries);
}

fn bump(change: impl FnOnce(&mut ReadTally)) {
    TALLY.with(|cell| {
        let mut tally = cell.get();
        change(&mut tally);
        cell.set(tally);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A window reports what the thread read while it stood, and nothing it
    /// read beforehand.
    #[test]
    fn a_window_reports_the_reads_made_under_it() {
        count_stat();
        let window = ReadWindow::open();
        count_document_open();
        count_stat();
        count_dirents(4);
        assert_eq!(
            window.finish(),
            ReadTally {
                document_opens: 1,
                stats: 1,
                walk_dirents: 4
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
                document_opens: 0,
                stats: 0,
                walk_dirents: 2
            }
        );
    }

    #[test]
    #[should_panic(expected = "a read window already stands on this thread")]
    fn one_thread_carries_one_window() {
        let _standing = ReadWindow::open();
        let _second = ReadWindow::open();
    }
}
