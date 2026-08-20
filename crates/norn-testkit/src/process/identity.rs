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
    let after_name = stat.rsplit_once(')').map(|(_, rest)| rest).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("/proc/{pid}/stat has no closing process-name delimiter"),
        )
    })?;
    let value = after_name
        .split_whitespace()
        .nth(19)
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("/proc/{pid}/stat has no process start field"),
            )
        })?
        .parse()
        .map_err(|error| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("/proc/{pid}/stat has an invalid process start field: {error}"),
            )
        })?;
    Ok(value)
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
    use super::linux_start_ticks;

    #[test]
    fn linux_stat_parser_uses_the_start_field_after_a_name_with_a_closing_parenthesis() {
        let stat = "41 (worker) name) S 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 987 20";
        assert_eq!(linux_start_ticks(41, stat).unwrap(), 987);
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn start_identity(pid: libc::pid_t) -> io::Result<StartIdentity> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        format!("kernel process identity is unsupported for pid {pid} on this platform"),
    ))
}
