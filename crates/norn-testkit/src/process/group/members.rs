//! Linux's view of who is still in a process group, read from `/proc`.
//!
//! [`super::OwnedProcessGroup::close`] asks one question here: is the exited,
//! unreaped leader the only process left in the group? `killpg` cannot answer
//! it, because on Linux signalling a group that holds only a zombie succeeds.
//!
//! The answer is "yes" only when it is proved. Every doubt reads as "no", which
//! keeps the full signal-delivery window: `/proc` cannot be listed, a member's
//! `stat` cannot be read for a reason other than the process having gone, a
//! `stat` line does not parse, the leader is not in the listing as a zombie of
//! the group, or any other process is in the group in any state.
//!
//! A zombie that is not the leader counts as a member. Its `stat` state is its
//! main thread's, and a process whose main thread called `pthread_exit` reads as
//! a zombie while its other threads run and can fork. The leader's own zombie
//! state does not carry this doubt only because the caller also has `waitid`
//! report the leader ended, which the kernel does only once every thread of it
//! has exited. A zombie non-leader is reaped by its parent or by `init`, so
//! counting it costs at most a few more poll gaps of the window.

use std::io;

/// Whether `/proc` shows the group `pgid` holding nothing but its own leader,
/// as a zombie. One pass over `/proc`: a directory listing and one `stat` read
/// per process on the machine.
#[allow(clippy::disallowed_methods)] // Harness process cleanup: reading the kernel's process table.
pub(super) fn only_the_exited_leader_remains(pgid: libc::pid_t) -> bool {
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return false;
    };
    let mut leader_seen = false;
    for entry in entries {
        let Ok(entry) = entry else {
            return false;
        };
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<libc::pid_t>().ok())
        else {
            continue;
        };
        let stat = match std::fs::read(entry.path().join("stat")) {
            Ok(stat) => stat,
            Err(error) if vanished(&error) => continue,
            Err(_) => return false,
        };
        let Some(fields) = StatFields::parse(&stat) else {
            return false;
        };
        if fields.pgrp != pgid {
            continue;
        }
        if pid != pgid || fields.state != b'Z' {
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

/// The two `/proc/<pid>/stat` fields the scan reads.
#[derive(Debug, PartialEq, Eq)]
struct StatFields {
    /// Field 3, one letter: `Z` is a zombie.
    state: u8,
    /// Field 5, the process-group ID.
    pgrp: libc::pid_t,
}

impl StatFields {
    // Field 2 is the command name in parentheses, and the name may itself hold
    // spaces and parentheses. The kernel writes nothing after it but numbers and
    // the state letter, so the fields resume after the last `)`.
    fn parse(stat: &[u8]) -> Option<StatFields> {
        let after_name = stat.iter().rposition(|&byte| byte == b')')?;
        let rest = std::str::from_utf8(&stat[after_name + 1..]).ok()?;
        let mut fields = rest.split_ascii_whitespace();
        let state = fields.next()?;
        let _parent = fields.next()?;
        let pgrp = fields.next()?.parse().ok()?;
        let [state] = state.as_bytes() else {
            return None;
        };
        Some(StatFields {
            state: *state,
            pgrp,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stat_line_reads_state_and_group_after_the_last_parenthesis() {
        let stat = b"4242 (a) (b) c)) Z 1 4200 4200 0 -1 4194560 0 0\n";

        assert_eq!(
            StatFields::parse(stat),
            Some(StatFields {
                state: b'Z',
                pgrp: 4200
            })
        );
    }

    #[test]
    fn a_truncated_or_malformed_stat_line_is_not_read() {
        for stat in [
            &b"4242 (sleep"[..],
            b"4242 (sleep) S 1",
            b"4242 (sleep) S 1 x",
            b"4242 (sleep) SS 1 4200",
        ] {
            assert_eq!(StatFields::parse(stat), None, "{stat:?}");
        }
    }

    #[test]
    fn this_live_process_is_a_live_member_of_its_own_group() {
        let pgid = unsafe { libc::getpgrp() };

        assert!(!only_the_exited_leader_remains(pgid));
    }
}
