//! Identity-safe recovery for registered development process groups.

use std::io::{self, BufRead, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use super::identity::{self, Observation, ProcessIdentity};
use super::registry::{self, Registration, StoredRegistration};

const AUDIT_MODE: u32 = 0o600;
const CLEANUP_DEADLINE: Duration = Duration::from_secs(5);

#[derive(Debug, Serialize)]
pub struct RecoveryReport {
    pub schema: u32,
    pub operation: RecoveryOperation,
    pub found: usize,
    pub cleaned: usize,
    pub refused: usize,
    pub errors: usize,
    pub groups: Vec<GroupResult>,
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum RecoveryOperation {
    Scan,
    Reap,
}

#[derive(Debug, Serialize)]
pub struct GroupResult {
    pub run_token: Option<String>,
    pub process_group: Option<libc::pid_t>,
    pub age_ms: Option<u64>,
    pub process_count: Option<usize>,
    pub reason: String,
    pub result: GroupDisposition,
    pub detail: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum GroupDisposition {
    Found,
    Cleaned,
    Refused,
    Error,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct AuditEvent {
    pub schema: u32,
    pub recorded_at_unix_ms: u64,
    pub run_token: Option<String>,
    pub process_group: Option<libc::pid_t>,
    pub process_count: Option<usize>,
    pub age_ms: Option<u64>,
    pub reason: String,
    pub result: GroupDisposition,
    pub detail: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct AuditReport {
    pub schema: u32,
    pub events: Vec<AuditEvent>,
}

pub fn scan() -> io::Result<RecoveryReport> {
    recover(RecoveryOperation::Scan)
}

pub fn reap() -> io::Result<RecoveryReport> {
    recover(RecoveryOperation::Reap)
}

#[allow(clippy::disallowed_methods, clippy::disallowed_types)] // Recovery owns its machine-local audit file.
pub fn report() -> io::Result<AuditReport> {
    let path = audit_path()?;
    let file = match std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(AuditReport {
                schema: 1,
                events: Vec::new(),
            });
        }
        Err(error) => return Err(error),
    };
    validate_private_file(&file)?;
    let events = read_audit(&file)?;
    Ok(AuditReport { schema: 1, events })
}

fn recover(operation: RecoveryOperation) -> io::Result<RecoveryReport> {
    let _lock = matches!(operation, RecoveryOperation::Reap)
        .then(registry::lock_reaper)
        .transpose()?;
    let now = SystemTime::now();
    let mut report = RecoveryReport {
        schema: 1,
        operation,
        found: 0,
        cleaned: 0,
        refused: 0,
        errors: 0,
        groups: Vec::new(),
    };
    for stored in registry::registrations()? {
        match stored {
            Ok(stored) => recover_one(operation, &stored, now, &mut report)?,
            Err(detail) => {
                report.errors += 1;
                let result = GroupResult {
                    run_token: None,
                    process_group: None,
                    age_ms: None,
                    process_count: None,
                    reason: "invalid-registration".to_string(),
                    result: GroupDisposition::Error,
                    detail: Some(detail),
                };
                if matches!(operation, RecoveryOperation::Reap) {
                    append_result(&result, now)?;
                }
                report.groups.push(result);
            }
        }
    }
    Ok(report)
}

fn recover_one(
    operation: RecoveryOperation,
    stored: &StoredRegistration,
    now: SystemTime,
    report: &mut RecoveryReport,
) -> io::Result<()> {
    let stale_reason = match identity::observe(&stored.registration.supervisor) {
        Ok(Observation::Matching) => return Ok(()),
        Ok(Observation::Absent) => "supervisor-absent",
        Ok(Observation::Mismatched) => "supervisor-identity-mismatch",
        Err(error) => {
            let result = result_for(
                stored,
                now,
                "supervisor-observation-failed",
                GroupDisposition::Error,
                None,
                Some(error.to_string()),
            );
            report.errors += 1;
            if matches!(operation, RecoveryOperation::Reap) {
                append_result(&result, now)?;
            }
            report.groups.push(result);
            return Ok(());
        }
    };
    report.found += 1;
    if matches!(operation, RecoveryOperation::Scan) {
        report.groups.push(result_for(
            stored,
            now,
            stale_reason,
            GroupDisposition::Found,
            None,
            None,
        ));
        return Ok(());
    }

    let count = match process_group_count(stored.registration.process_group.pid) {
        Ok(count) => Some(count),
        Err(error) => {
            report.errors += 1;
            let result = result_for(
                stored,
                now,
                "process-group-count-failed",
                GroupDisposition::Error,
                None,
                Some(error.to_string()),
            );
            append_result(&result, now)?;
            report.groups.push(result);
            return Ok(());
        }
    };
    let disposition = match observe_group(&stored.registration.process_group) {
        Ok(GroupObservation::Absent) => GroupDisposition::Cleaned,
        Ok(GroupObservation::Matching) => {
            match close_group(stored.registration.process_group.pid) {
                Ok(()) => GroupDisposition::Cleaned,
                Err(error) => {
                    report.errors += 1;
                    let result = result_for(
                        stored,
                        now,
                        "cleanup-failed",
                        GroupDisposition::Error,
                        count,
                        Some(error.to_string()),
                    );
                    append_result(&result, now)?;
                    report.groups.push(result);
                    return Ok(());
                }
            }
        }
        Ok(GroupObservation::Mismatched) => {
            report.refused += 1;
            let result = result_for(
                stored,
                now,
                "process-group-identity-mismatch",
                GroupDisposition::Refused,
                count,
                None,
            );
            append_result(&result, now)?;
            report.groups.push(result);
            return Ok(());
        }
        Err(error) => {
            report.errors += 1;
            let result = result_for(
                stored,
                now,
                "process-group-observation-failed",
                GroupDisposition::Error,
                count,
                Some(error.to_string()),
            );
            append_result(&result, now)?;
            report.groups.push(result);
            return Ok(());
        }
    };
    let result = result_for(stored, now, stale_reason, disposition, count, None);
    append_result(&result, now)?;
    stored.remove()?;
    report.cleaned += 1;
    report.groups.push(result);
    Ok(())
}

enum GroupObservation {
    Absent,
    Matching,
    Mismatched,
}

fn observe_group(expected: &ProcessIdentity) -> io::Result<GroupObservation> {
    if unsafe { libc::killpg(expected.pid, 0) } == -1 {
        let error = io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::ESRCH) {
            return Ok(GroupObservation::Absent);
        }
        if error.raw_os_error() != Some(libc::EPERM) {
            return Err(error);
        }
    }
    match identity::process_group(expected.pid) {
        Ok(observed) if observed == *expected => Ok(GroupObservation::Matching),
        Ok(_) => Ok(GroupObservation::Mismatched),
        Err(error) if error.raw_os_error() == Some(libc::ESRCH) => Ok(GroupObservation::Mismatched),
        Err(error) => Err(error),
    }
}

fn close_group(pgid: libc::pid_t) -> io::Result<()> {
    if unsafe { libc::killpg(pgid, libc::SIGKILL) } == -1 {
        let error = io::Error::last_os_error();
        if error.raw_os_error() != Some(libc::ESRCH) {
            return Err(error);
        }
    }
    let deadline = Instant::now() + CLEANUP_DEADLINE;
    loop {
        if unsafe { libc::killpg(pgid, 0) } == -1 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::ESRCH) {
                return Ok(());
            }
            if error.raw_os_error() != Some(libc::EPERM) {
                return Err(error);
            }
        }
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!("process group {pgid} remained after SIGKILL"),
            ));
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn result_for(
    stored: &StoredRegistration,
    now: SystemTime,
    reason: &str,
    result: GroupDisposition,
    process_count: Option<usize>,
    detail: Option<String>,
) -> GroupResult {
    GroupResult {
        run_token: Some(stored.registration.run_token.clone()),
        process_group: Some(stored.registration.process_group.pid),
        age_ms: age_ms(&stored.registration, now),
        process_count,
        reason: reason.to_string(),
        result,
        detail,
    }
}

fn append_result(result: &GroupResult, now: SystemTime) -> io::Result<()> {
    append_audit(AuditEvent {
        schema: 1,
        recorded_at_unix_ms: unix_ms(now)?,
        run_token: result.run_token.clone(),
        process_group: result.process_group,
        process_count: result.process_count,
        age_ms: result.age_ms,
        reason: result.reason.clone(),
        result: result.result,
        detail: result.detail.clone(),
    })
}

#[allow(clippy::disallowed_methods, clippy::disallowed_types)] // Recovery owns its append-only machine-local audit file.
fn append_audit(event: AuditEvent) -> io::Result<()> {
    let path = audit_path()?;
    let existed = path.exists();
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .create(true)
        .append(true)
        .mode(AUDIT_MODE)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&path)?;
    validate_private_file(&file)?;
    read_audit(&file)?;
    let mut line = serde_json::to_vec(&event).map_err(io::Error::other)?;
    line.push(b'\n');
    file.write_all(&line)?;
    file.sync_all()?;
    if !existed {
        std::fs::File::open(registry::state_root()?)?.sync_all()?;
    }
    Ok(())
}

#[allow(clippy::disallowed_types)] // Recovery validates its machine-local audit file before append or report.
fn read_audit(file: &std::fs::File) -> io::Result<Vec<AuditEvent>> {
    let mut reader = io::BufReader::new(file);
    let mut line = Vec::new();
    let mut events = Vec::new();
    loop {
        line.clear();
        if reader.read_until(b'\n', &mut line)? == 0 {
            return Ok(events);
        }
        if line.last() != Some(&b'\n') {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "the recovery audit ends with a partial event",
            ));
        }
        let event: AuditEvent = serde_json::from_slice(&line[..line.len() - 1])
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        if event.schema != 1 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("the recovery audit contains schema {}", event.schema),
            ));
        }
        events.push(event);
    }
}

fn audit_path() -> io::Result<std::path::PathBuf> {
    Ok(crate::isolation::root().join("process-groups.audit.jsonl"))
}

#[allow(clippy::disallowed_types)] // Recovery validates its machine-local audit and lock files.
fn validate_private_file(file: &std::fs::File) -> io::Result<()> {
    let metadata = file.metadata()?;
    if !metadata.is_file()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.mode() & 0o077 != 0
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "the recovery file is not a private owner-held regular file",
        ));
    }
    Ok(())
}

fn age_ms(registration: &Registration, now: SystemTime) -> Option<u64> {
    unix_ms(now)
        .ok()?
        .checked_sub(registration.registered_at_unix_ms)
}
fn unix_ms(time: SystemTime) -> io::Result<u64> {
    time.duration_since(UNIX_EPOCH)
        .map_err(io::Error::other)?
        .as_millis()
        .try_into()
        .map_err(|_| io::Error::other("the system time does not fit the audit format"))
}

#[cfg(target_os = "linux")]
#[allow(clippy::disallowed_methods)] // Recovery counts members through the kernel process table for audit evidence.
fn process_group_count(pgid: libc::pid_t) -> io::Result<usize> {
    let mut count = 0;
    for entry in std::fs::read_dir("/proc")? {
        let Ok(pid) = entry?.file_name().to_string_lossy().parse::<libc::pid_t>() else {
            continue;
        };
        if unsafe { libc::getpgid(pid) } == pgid {
            count += 1;
        }
    }
    Ok(count)
}

#[cfg(target_os = "macos")]
fn process_group_count(pgid: libc::pid_t) -> io::Result<usize> {
    let population = unsafe { libc::proc_listallpids(std::ptr::null_mut(), 0) };
    if population <= 0 {
        return Err(io::Error::last_os_error());
    }
    let mut pids = vec![0 as libc::pid_t; population as usize + 32];
    let read = unsafe {
        libc::proc_listallpids(
            pids.as_mut_ptr().cast(),
            (pids.len() * std::mem::size_of::<libc::pid_t>()) as libc::c_int,
        )
    };
    if read < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(pids
        .into_iter()
        .take(read as usize)
        .filter(|pid| *pid > 0 && unsafe { libc::getpgid(*pid) } == pgid)
        .count())
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn process_group_count(_: libc::pid_t) -> io::Result<usize> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "process-group counting is unsupported",
    ))
}
