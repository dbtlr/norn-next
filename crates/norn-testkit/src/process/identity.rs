//! Kernel process identities used by the development process registry.

use std::io;

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(super) struct ProcessIdentity {
    pub(super) pid: libc::pid_t,
    pub(super) start: StartIdentity,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(super) struct StartIdentity {
    pub(super) source: StartIdentitySource,
    pub(super) value: u64,
    pub(super) boot_id: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(super) enum StartIdentitySource {
    LinuxClockTicks,
    DarwinMicroseconds,
}

pub(super) enum Observation {
    Absent,
    Matching,
    Mismatched,
}

pub(super) fn observe(expected: &ProcessIdentity) -> io::Result<Observation> {
    match process(expected.pid) {
        Ok(observed) if observed == *expected => Ok(Observation::Matching),
        Ok(_) => Ok(Observation::Mismatched),
        Err(error)
            if error.raw_os_error() == Some(libc::ESRCH)
                || error.kind() == io::ErrorKind::NotFound =>
        {
            Ok(Observation::Absent)
        }
        Err(error) => Err(error),
    }
}

pub(super) fn process(pid: libc::pid_t) -> io::Result<ProcessIdentity> {
    Ok(ProcessIdentity {
        pid,
        start: start_identity(pid)?,
    })
}

pub(super) fn process_group(pgid: libc::pid_t) -> io::Result<ProcessIdentity> {
    let observed = unsafe { libc::getpgid(pgid) };
    if observed == -1 {
        return Err(io::Error::last_os_error());
    }
    if observed != pgid {
        return Err(io::Error::other(format!(
            "pid {pgid} belongs to process group {observed}, not the group it must lead"
        )));
    }
    process(pgid)
}

#[cfg(target_os = "linux")]
#[allow(clippy::disallowed_methods)] // The process registry reads the kernel identity it records.
fn start_identity(pid: libc::pid_t) -> io::Result<StartIdentity> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat"))?;
    let value = linux_start_ticks(pid, &stat)?;
    let boot_id = std::fs::read_to_string("/proc/sys/kernel/random/boot_id")?;
    let boot_id = boot_id.trim();
    if boot_id.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "the Linux boot identity is empty",
        ));
    }
    Ok(StartIdentity {
        source: StartIdentitySource::LinuxClockTicks,
        value,
        boot_id: boot_id.to_owned(),
    })
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn linux_start_ticks(pid: libc::pid_t, stat: &str) -> io::Result<u64> {
    LinuxStat::parse(pid, stat.as_bytes())?.start_ticks()
}

/// One Linux `/proc/<pid>/stat` line, read as the fields after the command
/// name. The process registry reads the start time from it, and cleanup's
/// group-member scan reads the state and process group. Each field is checked
/// only when it is asked for, so a reader never rejects a line over a field it
/// does not use.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(super) struct LinuxStat<'a> {
    pid: libc::pid_t,
    after_name: &'a str,
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
impl<'a> LinuxStat<'a> {
    // Field 2 is the command name in parentheses, and the name may itself hold
    // spaces, parentheses and bytes that are not UTF-8. The kernel writes
    // nothing after it but the state letter and numbers, so the fields resume
    // after the last `)`.
    pub(super) fn parse(pid: libc::pid_t, stat: &'a [u8]) -> io::Result<Self> {
        let after_name = stat
            .iter()
            .rposition(|&byte| byte == b')')
            .map(|close| &stat[close + 1..])
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("/proc/{pid}/stat has no closing process-name delimiter"),
                )
            })?;
        let after_name = std::str::from_utf8(after_name).map_err(|error| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "/proc/{pid}/stat has fields after the process name that are not text: {error}"
                ),
            )
        })?;
        Ok(LinuxStat { pid, after_name })
    }

    /// Field 3, one letter: `Z` is a zombie.
    pub(super) fn state(&self) -> io::Result<u8> {
        match self.field(3, "process state")?.as_bytes() {
            [state] => Ok(*state),
            _ => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("/proc/{}/stat has an invalid process state field", self.pid),
            )),
        }
    }

    /// Field 5, the process-group ID.
    pub(super) fn pgrp(&self) -> io::Result<libc::pid_t> {
        self.number(5, "process group")
    }

    /// Field 22, the start time in clock ticks since boot.
    pub(super) fn start_ticks(&self) -> io::Result<u64> {
        self.number(22, "process start")
    }

    fn number<T>(&self, field: usize, name: &str) -> io::Result<T>
    where
        T: std::str::FromStr,
        T::Err: std::fmt::Display,
    {
        self.field(field, name)?.parse().map_err(|error| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "/proc/{}/stat has an invalid {name} field: {error}",
                    self.pid
                ),
            )
        })
    }

    // `field` counts from 1 as proc(5) does; field 3 is the first after the name.
    fn field(&self, field: usize, name: &str) -> io::Result<&'a str> {
        self.after_name
            .split_whitespace()
            .nth(field - 3)
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("/proc/{}/stat has no {name} field", self.pid),
                )
            })
    }
}

#[cfg(target_os = "macos")]
fn start_identity(pid: libc::pid_t) -> io::Result<StartIdentity> {
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let expected = std::mem::size_of::<libc::proc_bsdinfo>();
    let read = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDTBSDINFO,
            0,
            (&raw mut info).cast(),
            expected as libc::c_int,
        )
    };
    if read != expected as libc::c_int {
        return Err(if read <= 0 {
            io::Error::last_os_error()
        } else {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("proc_pidinfo returned {read} bytes for pid {pid}, expected {expected}"),
            )
        });
    }
    let value = info
        .pbi_start_tvsec
        .checked_mul(1_000_000)
        .and_then(|seconds| seconds.checked_add(info.pbi_start_tvusec))
        .ok_or_else(|| io::Error::other(format!("pid {pid} has an overflowing start time")))?;
    Ok(StartIdentity {
        source: StartIdentitySource::DarwinMicroseconds,
        value,
        boot_id: darwin_boot_id()?,
    })
}

#[cfg(target_os = "macos")]
fn darwin_boot_id() -> io::Result<String> {
    let name = c"kern.bootsessionuuid";
    let mut length = 0;
    if unsafe {
        libc::sysctlbyname(
            name.as_ptr(),
            std::ptr::null_mut(),
            &raw mut length,
            std::ptr::null_mut(),
            0,
        )
    } == -1
    {
        return Err(io::Error::last_os_error());
    }
    let mut bytes = vec![0_u8; length];
    if unsafe {
        libc::sysctlbyname(
            name.as_ptr(),
            bytes.as_mut_ptr().cast(),
            &raw mut length,
            std::ptr::null_mut(),
            0,
        )
    } == -1
    {
        return Err(io::Error::last_os_error());
    }
    bytes.truncate(length);
    if bytes.last() == Some(&0) {
        bytes.pop();
    }
    let boot_id = String::from_utf8(bytes)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    if boot_id.is_empty() {
        Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "the Darwin boot-session identity is empty",
        ))
    } else {
        Ok(boot_id)
    }
}

#[cfg(test)]
mod tests {
    #[cfg(target_os = "linux")]
    use super::{LinuxStat, linux_start_ticks};
    use super::{Observation, ProcessIdentity, StartIdentity, StartIdentitySource, observe};

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn an_absent_process_is_observed_as_absent() {
        let expected = ProcessIdentity {
            pid: libc::pid_t::MAX,
            start: StartIdentity {
                source: if cfg!(target_os = "linux") {
                    StartIdentitySource::LinuxClockTicks
                } else {
                    StartIdentitySource::DarwinMicroseconds
                },
                value: 1,
                boot_id: "not-a-live-process".to_string(),
            },
        };
        assert!(matches!(observe(&expected).unwrap(), Observation::Absent));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_stat_parser_uses_the_start_field_after_a_name_with_a_closing_parenthesis() {
        let stat = "41 (worker) name) S 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 987 20";
        assert_eq!(linux_start_ticks(41, stat).unwrap(), 987);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_stat_reads_state_and_group_after_a_name_holding_parentheses() {
        let stat = b"4242 (a) (b) c)) Z 1 4200 4200 0 -1 4194560 0 0\n";
        let stat = LinuxStat::parse(4242, stat).unwrap();

        assert_eq!(stat.state().unwrap(), b'Z');
        assert_eq!(stat.pgrp().unwrap(), 4200);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_stat_reads_fields_after_a_name_that_is_not_text() {
        let stat = b"4242 (\xff\xfe) S 1 4200 4200";
        let stat = LinuxStat::parse(4242, stat).unwrap();

        assert_eq!(stat.pgrp().unwrap(), 4200);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_truncated_or_malformed_linux_stat_line_yields_no_state_and_group() {
        assert!(LinuxStat::parse(4242, b"4242 (sleep").is_err());
        for stat in [
            &b"4242 (sleep) S 1"[..],
            b"4242 (sleep) S 1 x",
            b"4242 (sleep) SS 1 4200",
        ] {
            let stat = LinuxStat::parse(4242, stat).unwrap();
            assert!(
                stat.state().is_err() || stat.pgrp().is_err(),
                "{:?}",
                stat.after_name
            );
        }
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn start_identity(pid: libc::pid_t) -> io::Result<StartIdentity> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        format!("kernel process identity is unsupported for pid {pid} on this platform"),
    ))
}
