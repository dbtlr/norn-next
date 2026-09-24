//! The one keyset page every read builder reads: section after section, one
//! row past its bound.

use super::{Ran, Stepped};
use crate::error::StoreError;
use crate::store::Snapshot;

/// A page of rows a builder read, and where the next page continues.
pub(crate) struct KeysetPage<T> {
    /// The rows, at most the page's bound of them, in the order the sections
    /// read them.
    pub(crate) rows: Vec<T>,
    /// The last row kept, where a next page exists, and `None` where this
    /// page is the last.
    pub(crate) next: Option<T>,
    /// The rows the section statements handed back: one past the bound where
    /// a next page exists.
    pub(crate) read: u64,
    /// What SQLite counted stepping the section statements, summed.
    pub(crate) stepped: Stepped,
}

impl Snapshot {
    /// One page of at most `limit` rows, read from `sections` in order.
    ///
    /// Each section is asked for one row more than the page still has room
    /// for, and a section after the page is full is never read, so a page reads
    /// at most one row past its bound. **That row is what says a next page
    /// exists**: it is dropped, and the last row kept is where the next page
    /// continues. The builder states the rest: `read` answers a section with
    /// at most the rows it is handed, running each statement it reads through
    /// at the end of `record` — or none, where what the section reads is held
    /// in memory — and what SQLite counted stepping the statements a section
    /// ran is summed into the page's.
    pub(crate) fn read_page<S, T: Clone>(
        &self,
        sections: impl IntoIterator<Item = S>,
        limit: usize,
        record: &mut Vec<Ran>,
        mut read: impl FnMut(&mut Vec<Ran>, S, usize) -> Result<Vec<T>, StoreError>,
    ) -> Result<KeysetPage<T>, StoreError> {
        let mut rows: Vec<T> = Vec::new();
        let mut stepped = Stepped::default();
        for section in sections {
            let room = limit + 1 - rows.len();
            if room == 0 {
                break;
            }
            let ran_before = record.len();
            rows.extend(read(record, section, room)?);
            for ran in &record[ran_before..] {
                stepped.add(ran.stepped);
            }
        }
        let read = rows.len() as u64;
        let next = if rows.len() > limit {
            rows.truncate(limit);
            rows.last().cloned()
        } else {
            None
        };
        Ok(KeysetPage {
            rows,
            next,
            read,
            stepped,
        })
    }
}
