//! Linux's view of who is still in a process group, read from `/proc`.
//!
//! [`super::OwnedProcessGroup::close`] asks one question here: is the exited,
//! unreaped leader the only process left in the group? `killpg` cannot answer
//! it, because on Linux signalling a group that holds only a zombie succeeds.
//!
//! The answer is "yes" only when it is proved. Every doubt reads as "no", which
//! keeps the full signal-delivery window: `/proc` was mounted for another pid
//! namespace (its `self` link does not name this process), `/proc` cannot be
//! listed or an entry of the listing cannot be read, a member's `stat` cannot be
//! read for a reason other than the process having gone, a `stat` line does not
//! parse, the leader is not in the listing as a zombie of the group, or any
//! other process is in the group in any state.
//!
//! A zombie that is not the leader counts as a member, conservatively: it may be
//! a member the group's `SIGKILL` did not reach, because `/proc` shows only its
//! main thread's state and says nothing of whether the signal was delivered to
//! it. The leader's own zombie state carries no such doubt only because the
//! caller also has `waitid` report the leader ended, which the kernel does only
//! once every thread of it has exited. A zombie non-leader is reaped by its
//! parent or by `init`, so counting it costs at most a few more poll gaps of the
//! window.

use std::ffi::OsString;
use std::io;

use crate::process::identity::LinuxStat;

/// Whether `/proc` shows the group `pgid` holding nothing but its own leader,
/// as a zombie. One pass over `/proc`: a directory listing and one `stat` read
/// per process on the machine.
pub(super) fn only_the_exited_leader_remains(pgid: libc::pid_t) -> bool {
    let own_pid = libc::pid_t::try_from(std::process::id()).ok();
    own_pid.is_some_and(|own_pid| scan(&KernelProcessTable, own_pid, pgid))
}

/// What the scan reads of the kernel's process table. [`KernelProcessTable`]
/// reads `/proc`; the tests stand in for each way that read can fail.
trait ProcessTable {
    /// The process ID `/proc/self` names, which is the reader's own only when
    /// `/proc` was mounted for the reader's pid namespace.
    fn self_pid(&self) -> io::Result<libc::pid_t>;
    /// The names of the entries in `/proc`, each of which may fail to read.
    fn entries(&self) -> io::Result<Vec<io::Result<OsString>>>;
    /// The bytes of `/proc/<pid>/stat`.
    fn stat(&self, pid: libc::pid_t) -> io::Result<Vec<u8>>;
}

struct KernelProcessTable;

#[allow(clippy::disallowed_methods)] // Harness process cleanup: reading the kernel's process table.
impl ProcessTable for KernelProcessTable {
    fn self_pid(&self) -> io::Result<libc::pid_t> {
        std::fs::read_link("/proc/self")?
            .to_str()
            .and_then(|name| name.parse().ok())
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "/proc/self does not name a process ID",
                )
            })
    }

    fn entries(&self) -> io::Result<Vec<io::Result<OsString>>> {
        Ok(std::fs::read_dir("/proc")?
            .map(|entry| entry.map(|entry| entry.file_name()))
            .collect())
    }

    fn stat(&self, pid: libc::pid_t) -> io::Result<Vec<u8>> {
        std::fs::read(format!("/proc/{pid}/stat"))
    }
}

// `own_pid` is the scanning process's ID in its own pid namespace; a table whose
// `self` names another ID lists another namespace's processes and proves nothing.
fn scan(table: &impl ProcessTable, own_pid: libc::pid_t, pgid: libc::pid_t) -> bool {
    if table.self_pid().ok() != Some(own_pid) {
        return false;
    }
    let Ok(entries) = table.entries() else {
        return false;
    };
    let mut leader_seen = false;
    for entry in entries {
        let Ok(name) = entry else {
            return false;
        };
        let Some(pid) = name
            .to_str()
            .and_then(|name| name.parse::<libc::pid_t>().ok())
        else {
            continue;
        };
        let stat = match table.stat(pid) {
            Ok(stat) => stat,
            Err(error) if vanished(&error) => continue,
            Err(_) => return false,
        };
        let Ok(stat) = LinuxStat::parse(pid, &stat) else {
            return false;
        };
        let Ok(pgrp) = stat.pgrp() else {
            return false;
        };
        if pgrp != pgid {
            continue;
        }
        if pid != pgid || stat.state().ok() != Some(b'Z') {
            return false;
        }
        leader_seen = true;
    }
    leader_seen
}

// A process that ended between the listing and the read is gone, not unknown.
fn vanished(error: &io::Error) -> bool {
    error.kind() == io::ErrorKind::NotFound || error.raw_os_error() == Some(libc::ESRCH)
}

#[cfg(test)]
mod tests {
    use super::*;

    const OWN_PID: libc::pid_t = 10;
    const LEADER: libc::pid_t = 100;
    const OTHER: libc::pid_t = 200;

    // A process table that answers from fixed data, so each doubt the scan can
    // meet is set up on its own against a table that would otherwise prove the
    // leader alone.
    struct FixedTable {
        self_pid: io::Result<libc::pid_t>,
        listing: Option<Vec<Result<&'static str, libc::c_int>>>,
        stats: Vec<(libc::pid_t, Result<&'static [u8], libc::c_int>)>,
    }

    impl FixedTable {
        // The leader as a zombie of its own group, and one unrelated process.
        fn proving() -> FixedTable {
            FixedTable {
                self_pid: Ok(OWN_PID),
                listing: Some(vec![Ok("self"), Ok("100"), Ok("200")]),
                stats: vec![
                    (LEADER, Ok(b"100 (sh) Z 1 100 100 0")),
                    (OTHER, Ok(b"200 (init) S 0 200 200 0")),
                ],
            }
        }

        fn with_other_stat(mut self, stat: Result<&'static [u8], libc::c_int>) -> FixedTable {
            self.stats.retain(|(pid, _)| *pid != OTHER);
            self.stats.push((OTHER, stat));
            self
        }
    }

    impl ProcessTable for FixedTable {
        fn self_pid(&self) -> io::Result<libc::pid_t> {
            match &self.self_pid {
                Ok(pid) => Ok(*pid),
                Err(error) => Err(io::Error::new(error.kind(), error.to_string())),
            }
        }

        fn entries(&self) -> io::Result<Vec<io::Result<OsString>>> {
            let listing = self
                .listing
                .as_ref()
                .ok_or_else(|| io::Error::from_raw_os_error(libc::EACCES))?;
            Ok(listing
                .iter()
                .map(|entry| {
                    entry
                        .map(OsString::from)
                        .map_err(io::Error::from_raw_os_error)
                })
                .collect())
        }

        fn stat(&self, pid: libc::pid_t) -> io::Result<Vec<u8>> {
            match self.stats.iter().find(|(listed, _)| *listed == pid) {
                Some((_, Ok(stat))) => Ok(stat.to_vec()),
                Some((_, Err(errno))) => Err(io::Error::from_raw_os_error(*errno)),
                None => Err(io::Error::from_raw_os_error(libc::ENOENT)),
            }
        }
    }

    #[test]
    fn the_leader_alone_as_a_zombie_of_its_group_is_proved() {
        assert!(scan(&FixedTable::proving(), OWN_PID, LEADER));
    }

    #[test]
    fn a_proc_mounted_for_another_pid_namespace_proves_nothing() {
        let mut table = FixedTable::proving();
        table.self_pid = Ok(OWN_PID + 1);

        assert!(!scan(&table, OWN_PID, LEADER));
    }

    #[test]
    fn an_unreadable_proc_self_link_proves_nothing() {
        let mut table = FixedTable::proving();
        table.self_pid = Err(io::Error::from_raw_os_error(libc::ENOENT));

        assert!(!scan(&table, OWN_PID, LEADER));
    }

    #[test]
    fn an_unlistable_proc_proves_nothing() {
        let mut table = FixedTable::proving();
        table.listing = None;

        assert!(!scan(&table, OWN_PID, LEADER));
    }

    #[test]
    fn an_unreadable_listing_entry_proves_nothing() {
        let mut table = FixedTable::proving();
        table
            .listing
            .as_mut()
            .expect("a listing")
            .push(Err(libc::EIO));

        assert!(!scan(&table, OWN_PID, LEADER));
    }

    #[test]
    fn a_stat_read_failing_for_a_live_process_proves_nothing() {
        for errno in [libc::EACCES, libc::EIO] {
            let table = FixedTable::proving().with_other_stat(Err(errno));

            assert!(!scan(&table, OWN_PID, LEADER), "errno {errno}");
        }
    }

    #[test]
    fn a_process_gone_before_its_stat_was_read_is_skipped() {
        for errno in [libc::ENOENT, libc::ESRCH] {
            let table = FixedTable::proving().with_other_stat(Err(errno));

            assert!(scan(&table, OWN_PID, LEADER), "errno {errno}");
        }
    }

    #[test]
    fn an_unparseable_stat_line_proves_nothing() {
        for stat in [
            &b"200 (init"[..],
            b"200 (init) S 0",
            b"200 (init) S 0 x",
            b"200 (\xff) S 0 200 \xff",
        ] {
            let table = FixedTable::proving().with_other_stat(Ok(stat));

            assert!(!scan(&table, OWN_PID, LEADER), "{stat:?}");
        }
    }

    #[test]
    fn an_unreadable_leader_state_proves_nothing() {
        let mut table = FixedTable::proving();
        table.stats[0].1 = Ok(b"100 (sh) ZZ 1 100 100 0");

        assert!(!scan(&table, OWN_PID, LEADER));
    }

    #[test]
    fn a_leader_that_is_not_a_zombie_proves_nothing() {
        let mut table = FixedTable::proving();
        table.stats[0].1 = Ok(b"100 (sh) S 1 100 100 0");

        assert!(!scan(&table, OWN_PID, LEADER));
    }

    #[test]
    fn a_group_with_no_leader_in_the_listing_proves_nothing() {
        let mut table = FixedTable::proving();
        table.listing = Some(vec![Ok("self"), Ok("200")]);

        assert!(!scan(&table, OWN_PID, LEADER));
    }

    #[test]
    fn another_member_in_any_state_proves_nothing() {
        for stat in [
            &b"200 (sleep) S 100 100 100 0"[..],
            b"200 (sleep) Z 100 100 100 0",
        ] {
            let table = FixedTable::proving().with_other_stat(Ok(stat));

            assert!(!scan(&table, OWN_PID, LEADER), "{stat:?}");
        }
    }

    #[test]
    fn the_kernel_proc_self_link_names_this_process() {
        let own_pid = libc::pid_t::try_from(std::process::id()).expect("a pid_t");

        assert_eq!(
            KernelProcessTable.self_pid().expect("reading /proc/self"),
            own_pid
        );
    }

    #[test]
    fn this_live_process_is_a_live_member_of_its_own_group() {
        let pgid = unsafe { libc::getpgrp() };

        assert!(!only_the_exited_leader_remains(pgid));
    }
}
