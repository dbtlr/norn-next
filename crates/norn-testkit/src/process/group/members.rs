//! Linux's view of who is still in a process group, read from `/proc`.
//!
//! [`super::OwnedProcessGroup::close`] asks one question here: is the exited,
//! unreaped leader the only process left in the group? `killpg` cannot answer
//! it, because on Linux signalling a group that holds only a zombie succeeds.
//! Recovery asks a second: how many processes are in a group
//! ([`process_group_count`]).
//!
//! The leader question's answer is "yes" only when it is proved. Every doubt
//! reads as "no", which keeps the full signal-delivery window: `/proc` was
//! mounted for another pid
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

/// What the scan and the count read of the kernel's process table. [`KernelProcessTable`]
/// reads `/proc`; the tests stand in for each way that read can fail.
trait ProcessTable {
    /// The process ID `/proc/self` names, which is the reader's own only when
    /// `/proc` was mounted for the reader's pid namespace.
    fn self_pid(&self) -> io::Result<libc::pid_t>;
    /// The names of the entries in `/proc`, each of which may fail to read.
    fn entries(&self) -> io::Result<Vec<io::Result<OsString>>>;
    /// The bytes of `/proc/<pid>/stat`.
    fn stat(&self, pid: libc::pid_t) -> io::Result<Vec<u8>>;
    /// The process group of one process, as the kernel's `getpgid` answers it.
    fn process_group(&self, pid: libc::pid_t) -> io::Result<libc::pid_t>;
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

    fn process_group(&self, pid: libc::pid_t) -> io::Result<libc::pid_t> {
        // SAFETY: `getpgid` takes a plain integer and touches no memory.
        match unsafe { libc::getpgid(pid) } {
            -1 => Err(io::Error::last_os_error()),
            pgid => Ok(pgid),
        }
    }
}

/// Count members of `pgid` through the guarded kernel process table.
///
/// A process that vanishes between listing and stat (`NotFound` or `ESRCH`)
/// is skipped. A process whose `stat` the reader is refused (`EPERM` or
/// `EACCES`, as a `/proc` mounted with `hidepid=1` does for another user's
/// process) is placed by `getpgid` instead, which still answers there; a
/// process that vanishes during that call is skipped too. A pid-namespace
/// mismatch, failed listing or listing entry, any other stat-read failure, any
/// other `getpgid` failure, or an unparseable stat line returns `Err`.
///
/// A `/proc` that hides other users' processes from the listing (`hidepid=2`)
/// cannot show them, so such a process is not counted, as before this change.
pub(in crate::process) fn process_group_count(pgid: libc::pid_t) -> io::Result<usize> {
    let own_pid = libc::pid_t::try_from(std::process::id())
        .map_err(|_| io::Error::other("this process ID does not fit pid_t"))?;
    count(&KernelProcessTable, own_pid, pgid)
}

fn count(table: &impl ProcessTable, own_pid: libc::pid_t, pgid: libc::pid_t) -> io::Result<usize> {
    let mut count = 0;
    walk_group(table, own_pid, pgid, true, |_, _| {
        count += 1;
        true
    })?;
    Ok(count)
}

fn scan(table: &impl ProcessTable, own_pid: libc::pid_t, pgid: libc::pid_t) -> bool {
    let mut leader_seen = false;
    // A refused stat stays unknown here: the leader's zombie state needs it.
    let complete = walk_group(table, own_pid, pgid, false, |pid, stat| {
        if pid != pgid || stat.is_none_or(|stat| stat.state().ok() != Some(b'Z')) {
            return false;
        }
        leader_seen = true;
        true
    });
    complete.unwrap_or(false) && leader_seen
}

// `own_pid` is the reader's ID in its own pid namespace. The visitor may stop
// early once a group member disproves the cleanup scan's claim. It receives no
// `stat` for a member placed by `getpgid` after a permission-refused read, which
// only happens when `getpgid_on_refusal` is set.
fn walk_group(
    table: &impl ProcessTable,
    own_pid: libc::pid_t,
    pgid: libc::pid_t,
    getpgid_on_refusal: bool,
    mut visit: impl FnMut(libc::pid_t, Option<LinuxStat<'_>>) -> bool,
) -> io::Result<bool> {
    if table.self_pid()? != own_pid {
        return Err(io::Error::other("/proc names another pid namespace"));
    }
    for entry in table.entries()? {
        let name = entry?;
        let Some(pid) = name
            .to_str()
            .and_then(|name| name.parse::<libc::pid_t>().ok())
        else {
            continue;
        };
        let stat = match table.stat(pid) {
            Ok(stat) => stat,
            Err(error) if vanished(&error) => continue,
            Err(error) if getpgid_on_refusal && error.kind() == io::ErrorKind::PermissionDenied => {
                match table.process_group(pid) {
                    Ok(group) if group == pgid && !visit(pid, None) => return Ok(false),
                    Ok(_) => continue,
                    Err(error) if vanished(&error) => continue,
                    Err(error) => return Err(error),
                }
            }
            Err(error) => return Err(error),
        };
        let stat = LinuxStat::parse(pid, &stat)?;
        if stat.pgrp()? == pgid && !visit(pid, Some(stat)) {
            return Ok(false);
        }
    }
    Ok(true)
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
        // What `getpgid` answers per pid; an unscripted pid fails with EIO.
        groups: Vec<(libc::pid_t, Result<libc::pid_t, libc::c_int>)>,
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
                groups: Vec::new(),
            }
        }

        fn with_other_stat(mut self, stat: Result<&'static [u8], libc::c_int>) -> FixedTable {
            self.stats.retain(|(pid, _)| *pid != OTHER);
            self.stats.push((OTHER, stat));
            self
        }

        // `OTHER`'s stat is refused for permission and `getpgid` answers `group`.
        fn with_other_refused(mut self, group: Result<libc::pid_t, libc::c_int>) -> FixedTable {
            self.groups.push((OTHER, group));
            self.with_other_stat(Err(libc::EPERM))
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

        fn process_group(&self, pid: libc::pid_t) -> io::Result<libc::pid_t> {
            match self.groups.iter().find(|(listed, _)| *listed == pid) {
                Some((_, Ok(group))) => Ok(*group),
                Some((_, Err(errno))) => Err(io::Error::from_raw_os_error(*errno)),
                None => Err(io::Error::from_raw_os_error(libc::EIO)),
            }
        }
    }

    #[test]
    fn counting_includes_group_members_and_ignores_other_groups() {
        assert_eq!(count(&FixedTable::proving(), OWN_PID, LEADER).unwrap(), 1);
        let table = FixedTable::proving().with_other_stat(Ok(b"200 (sleep) S 100 100 100 0"));
        assert_eq!(count(&table, OWN_PID, LEADER).unwrap(), 2);
        assert_eq!(count(&table, OWN_PID, OTHER).unwrap(), 0);
    }

    #[test]
    fn counting_skips_processes_that_vanished() {
        for errno in [libc::ENOENT, libc::ESRCH] {
            let table = FixedTable::proving().with_other_stat(Err(errno));
            assert_eq!(count(&table, OWN_PID, LEADER).unwrap(), 1);
        }
    }

    #[test]
    fn counting_rejects_a_namespace_mismatch() {
        let mut table = FixedTable::proving();
        table.self_pid = Ok(OWN_PID + 1);
        assert!(count(&table, OWN_PID, LEADER).is_err());
    }

    #[test]
    fn counting_rejects_unparseable_stat_lines() {
        for stat in [&b"200 (init"[..], b"200 (init) S 0", b"200 (init) S 0 x"] {
            let table = FixedTable::proving().with_other_stat(Ok(stat));
            assert!(count(&table, OWN_PID, LEADER).is_err());
        }
    }

    #[test]
    fn counting_rejects_a_failed_listing() {
        let mut table = FixedTable::proving();
        table.listing = None;
        assert!(count(&table, OWN_PID, LEADER).is_err());
    }

    #[test]
    fn counting_rejects_failed_entries_and_non_vanishing_stat_errors() {
        let mut table = FixedTable::proving();
        table.listing.as_mut().unwrap().push(Err(libc::EIO));
        assert!(count(&table, OWN_PID, LEADER).is_err());
        let table = FixedTable::proving().with_other_stat(Err(libc::EIO));
        assert!(count(&table, OWN_PID, LEADER).is_err());
    }

    #[test]
    fn counting_places_a_permission_refused_process_by_getpgid() {
        for refusal in [libc::EPERM, libc::EACCES] {
            let mut table = FixedTable::proving().with_other_refused(Ok(LEADER));
            table.stats.retain(|(pid, _)| *pid != OTHER);
            table.stats.push((OTHER, Err(refusal)));
            assert_eq!(
                count(&table, OWN_PID, LEADER).unwrap(),
                2,
                "errno {refusal}"
            );
        }
    }

    // The `getpgid` fallback is for permission refusals only: an EIO stat stays
    // an error even when `getpgid` could place the process in the counted group.
    #[test]
    fn counting_rejects_a_non_permission_stat_failure_even_when_getpgid_answers() {
        let mut table = FixedTable::proving().with_other_stat(Err(libc::EIO));
        table.groups.push((OTHER, Ok(LEADER)));
        assert!(count(&table, OWN_PID, LEADER).is_err());
    }

    #[test]
    fn counting_ignores_a_refused_process_in_another_group() {
        let table = FixedTable::proving().with_other_refused(Ok(OTHER));
        assert_eq!(count(&table, OWN_PID, LEADER).unwrap(), 1);
    }

    #[test]
    fn counting_skips_a_refused_process_that_vanished_during_getpgid() {
        for errno in [libc::ESRCH, libc::ENOENT] {
            let table = FixedTable::proving().with_other_refused(Err(errno));
            assert_eq!(count(&table, OWN_PID, LEADER).unwrap(), 1, "errno {errno}");
        }
    }

    #[test]
    fn counting_rejects_a_refused_process_whose_getpgid_fails() {
        let table = FixedTable::proving().with_other_refused(Err(libc::EIO));
        assert!(count(&table, OWN_PID, LEADER).is_err());
    }

    #[test]
    fn a_permission_refused_stat_proves_nothing_even_when_getpgid_answers() {
        let table = FixedTable::proving().with_other_refused(Ok(LEADER));
        assert!(!scan(&table, OWN_PID, LEADER));
    }

    // The scan never asks `getpgid`: a refused stat proves nothing even when
    // `getpgid` would place the process outside the leader's group.
    #[test]
    fn a_permission_refused_stat_proves_nothing_even_when_getpgid_names_another_group() {
        let table = FixedTable::proving().with_other_refused(Ok(OTHER));
        assert!(!scan(&table, OWN_PID, LEADER));
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
