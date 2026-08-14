//! What this thread has asked the filesystem for.
//!
//! Every read here is one of three acts: opening a file for its content,
//! stating a name, and taking a directory entry off a directory stream. This
//! module counts them as they happen, which is the only place they are all
//! visible — a caller sees the answers a walk or a read hands back and never how
//! many acts produced them.
//!
//! # The tally is this thread's, and a reader takes it rather than reading it
//!
//! The counts are thread-local. That is what makes them attributable: a caller
//! that runs one job on one thread takes the tally at the start and again at the
//! end, and what comes out is that job's filesystem cost rather than a shared
//! number two jobs moved at once. A process-wide count would need every other
//! thread to be idle to mean the same thing.
//!
//! [`take_tally`] empties the tally as it reads it, so the reading is *since the
//! last reading* and nothing has to remember what the previous one said.
//!
//! # This is evidence, not a bar
//!
//! Nothing here decides anything: no read is refused for having counted too
//! much, and no caller is asked to keep the tally small. It exists so that a
//! suite comparing two derivations of one vault can say what each of them spent,
//! which is a question no answer either derivation returns can settle.

use std::cell::Cell;

/// What one thread has asked the filesystem for.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ReadTally {
    /// Files opened for their content. A document read is one of these; so is a
    /// walk's own read of a file it enumerated.
    pub document_opens: u64,
    /// Names stated, however the stat was spelled: through a descriptor, through
    /// a parent directory, or by path.
    pub stats: u64,
    /// Directory entries taken off a directory stream, `.` and `..` excluded.
    pub walk_dirents: u64,
}

impl ReadTally {
    /// Whether this thread has asked the filesystem for nothing at all.
    pub fn is_empty(&self) -> bool {
        *self == ReadTally::default()
    }

    /// The two tallies added, which is how a caller folds a thread's reading
    /// into a longer-lived account.
    pub fn plus(self, other: ReadTally) -> ReadTally {
        ReadTally {
            document_opens: self.document_opens + other.document_opens,
            stats: self.stats + other.stats,
            walk_dirents: self.walk_dirents + other.walk_dirents,
        }
    }
}

thread_local! {
    static TALLY: Cell<ReadTally> = const { Cell::new(ReadTally {
        document_opens: 0,
        stats: 0,
        walk_dirents: 0,
    }) };
}

/// What this thread has asked the filesystem for since the last [`take_tally`].
pub fn tally() -> ReadTally {
    TALLY.with(Cell::get)
}

/// The same reading, with the tally emptied so the next one starts from zero.
pub fn take_tally() -> ReadTally {
    TALLY.with(|tally| tally.replace(ReadTally::default()))
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

    /// The tally is this thread's, so a case that empties it first reads only
    /// what it went on to spend.
    #[test]
    fn a_reading_empties_the_tally_it_reports() {
        let _ = take_tally();
        count_document_open();
        count_stat();
        count_dirents(4);
        let read = tally();
        assert_eq!(
            read,
            ReadTally {
                document_opens: 1,
                stats: 1,
                walk_dirents: 4
            }
        );
        assert_eq!(take_tally(), read);
        assert!(take_tally().is_empty());
    }

    #[test]
    fn two_tallies_fold_into_one() {
        let one = ReadTally {
            document_opens: 1,
            stats: 2,
            walk_dirents: 3,
        };
        assert_eq!(
            one.plus(one),
            ReadTally {
                document_opens: 2,
                stats: 4,
                walk_dirents: 6
            }
        );
        assert!(ReadTally::default().is_empty());
    }
}
