//! Registered supervision for one development workload.
//!
//! A resident launcher leads the owned group. It waits on a release pipe before
//! workload startup, reports the workload status, and stays alive as the
//! registry's process-identity pin until group cleanup.

use std::ffi::OsString;
use std::io::{self, Read};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::process::{Command, ExitCode};
use std::time::{Duration, Instant, SystemTime};

use super::group::{OwnedProcessGroup, ProcessEnd, WaitResult};
use super::{identity, registry, signals};

/// One workload for `norn-process supervise` to own.
pub struct SuperviseRequest {
    /// Why this workload exists.
    pub purpose: String,
    /// How long the workload can run before cleanup starts.
    pub deadline: Duration,
    /// The program followed by its arguments.
    pub command: Vec<OsString>,
}

#[doc(hidden)]
pub struct LaunchRequest {
    pub release_fd: RawFd,
    pub status_fd: RawFd,
    pub command: Vec<OsString>,
}

enum SupervisorEvent {
    Signal(libc::c_int),
    Workload(libc::c_int),
}

/// Register, release, and wait for one owned development process group.
///
/// Controlled completion removes the complete group and its registry record.
/// SIGINT and SIGTERM remove the group before this function re-raises the same
/// signal with its default disposition.
pub fn supervise(request: SuperviseRequest) -> io::Result<ExitCode> {
    let signals = signals::TerminationSignals::block_and_listen()?;
    let deadline_started = Instant::now();
    let registered_at = SystemTime::now();
    let supervisor_identity = identity::process(unsafe { libc::getpid() })?;
    let (release_reader, release_writer) = release_pipe()?;
    let (status_reader, status_writer) = status_pipe()?;
    let executable = std::env::current_exe()?;
    let mut launcher = Command::new(executable);
    launcher
        .arg("__launch")
        .arg("--release-fd")
        .arg(release_reader.as_raw_fd().to_string())
        .arg("--status-fd")
        .arg(status_writer.as_raw_fd().to_string())
        .arg("--")
        .args(&request.command);
    close_unrelated_descriptors(
        &mut launcher,
        &[release_reader.as_raw_fd(), status_writer.as_raw_fd()],
    )?;
    let group = OwnedProcessGroup::spawn(&mut launcher)?;
    drop(release_reader);
    drop(status_writer);

    let registration = match identity::process_group(group.pgid()).and_then(|process_group| {
        registry::Registration::new(
            request.purpose,
            request.deadline,
            registered_at,
            supervisor_identity,
            process_group,
        )
    }) {
        Ok(registration) => registration,
        Err(registration_error) => {
            return match group.close_now() {
                Ok(_) => Err(registration_error),
                Err(cleanup_error) => Err(combined(registration_error, cleanup_error)),
            };
        }
    };
    let published = match registry::Published::create(&registration) {
        Ok(published) => published,
        Err(registration_error) => {
            return match group.close_now() {
                Ok(_) => Err(registration_error),
                Err(cleanup_error) => Err(combined(registration_error, cleanup_error)),
            };
        }
    };
    if deadline_started.elapsed() >= request.deadline {
        return match group.close_now() {
            Ok(_) => {
                published.remove()?;
                Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "the supervised workload reached its deadline before release",
                ))
            }
            Err(cleanup_error) => Err(cleanup_error),
        };
    }
    if let Err(release_error) = release(&release_writer) {
        return match group.close_now() {
            Ok(_) => match published.remove() {
                Ok(()) => Err(release_error),
                Err(removal_error) => Err(combined(release_error, removal_error)),
            },
            Err(cleanup_error) => Err(combined(release_error, cleanup_error)),
        };
    }
    let _launcher_pin = release_writer;

    let mut workload = WorkloadStatus::new(status_reader);
    let remaining = request.deadline.saturating_sub(deadline_started.elapsed());
    match group.wait_interruptible(remaining, || {
        if let Some(signal) = signals.pending()? {
            return Ok(Some(SupervisorEvent::Signal(signal)));
        }
        workload
            .pending()
            .map(|status| status.map(SupervisorEvent::Workload))
    }) {
        WaitResult::Finished {
            result,
            group_empty,
        } => {
            if group_empty {
                published.remove()?;
            }
            let ended = result?;
            preserve_pending_signal(&signals)?;
            finish(ended)
        }
        WaitResult::Interrupted {
            event: SupervisorEvent::Workload(status),
            cleanup,
            group_empty,
        } => {
            if group_empty {
                published.remove()?;
            }
            match cleanup {
                Ok(_) => {
                    preserve_pending_signal(&signals)?;
                    finish_status(status)
                }
                Err(error) => Err(error),
            }
        }
        WaitResult::Interrupted {
            event: SupervisorEvent::Signal(signal),
            cleanup,
            group_empty,
        } => {
            if let Some(Err(error)) = group_empty.then(|| published.remove()) {
                eprintln!(
                    "norn-process: cleaned the process group but could not remove its registry record: {error}"
                );
            }
            match cleanup {
                Ok(_) => {}
                Err(error) => {
                    eprintln!(
                        "norn-process: process-group cleanup failed after signal {signal}: {error}"
                    );
                }
            }
            signals::terminate(signal)?;
            unreachable!("a preserved termination signal returned")
        }
    }
}

#[doc(hidden)]
#[allow(clippy::disallowed_methods, clippy::disallowed_types)] // The launcher adopts its private release-pipe handle.
pub fn launch(request: LaunchRequest) -> io::Result<ExitCode> {
    validate_launcher_descriptors(request.release_fd, request.status_fd)?;
    let mut release = unsafe { std::fs::File::from_raw_fd(request.release_fd) };
    let status = unsafe { std::fs::File::from_raw_fd(request.status_fd) };
    let mut byte = [0_u8; 1];
    release.read_exact(&mut byte)?;
    signals::unblock_termination_signals()?;
    close_on_exec(release.as_raw_fd())?;
    close_on_exec(status.as_raw_fd())?;

    let (program, arguments) = request.command.split_first().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "the workload command is empty")
    })?;
    let workload = Command::new(program).args(arguments).status()?;
    let raw = workload.into_raw().to_ne_bytes();
    let sigpipe_ignored = unsafe { libc::signal(libc::SIGPIPE, libc::SIG_IGN) } != libc::SIG_ERR;
    if !sigpipe_ignored || write_all(status.as_raw_fd(), &raw).is_err() {
        drop(status);
    }
    drop(release);
    loop {
        unsafe { libc::pause() };
    }
}

fn validate_launcher_descriptors(release_fd: RawFd, status_fd: RawFd) -> io::Result<()> {
    if release_fd < 3 || status_fd < 3 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "launcher file descriptors must not be standard or negative descriptors",
        ));
    }
    if release_fd == status_fd {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "launcher release and status descriptors must be different",
        ));
    }
    for (name, fd) in [("release", release_fd), ("status", status_fd)] {
        if unsafe { libc::fcntl(fd, libc::F_GETFD) } == -1 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "launcher {name} descriptor {fd} is not open: {}",
                    io::Error::last_os_error()
                ),
            ));
        }
    }
    Ok(())
}

fn finish(ended: ProcessEnd) -> io::Result<ExitCode> {
    if ended.timed_out {
        return Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "the supervised workload reached its deadline",
        ));
    }
    finish_status(ended.status)
}

fn finish_status(status: libc::c_int) -> io::Result<ExitCode> {
    if libc::WIFEXITED(status) {
        return Ok(ExitCode::from(libc::WEXITSTATUS(status) as u8));
    }
    if libc::WIFSIGNALED(status) {
        signals::terminate(libc::WTERMSIG(status))?;
        unreachable!("a mirrored launcher signal returned")
    }
    Err(io::Error::other(format!(
        "the workload reported an unreadable wait status {status:#x}"
    )))
}

fn preserve_pending_signal(signals: &signals::TerminationSignals) -> io::Result<()> {
    if let Some(signal) = signals.pending()? {
        signals::terminate(signal)?;
        unreachable!("a preserved termination signal returned")
    }
    Ok(())
}

fn release_pipe() -> io::Result<(OwnedFd, OwnedFd)> {
    let (reader, writer) = pipe()?;
    close_on_exec(writer.as_raw_fd())?;
    Ok((reader, writer))
}

fn status_pipe() -> io::Result<(OwnedFd, OwnedFd)> {
    let (reader, writer) = pipe()?;
    close_on_exec(reader.as_raw_fd())?;
    let flags = unsafe { libc::fcntl(reader.as_raw_fd(), libc::F_GETFL) };
    if flags == -1
        || unsafe { libc::fcntl(reader.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) } == -1
    {
        return Err(io::Error::last_os_error());
    }
    Ok((reader, writer))
}

fn pipe() -> io::Result<(OwnedFd, OwnedFd)> {
    let mut descriptors = [-1; 2];
    if unsafe { libc::pipe(descriptors.as_mut_ptr()) } == -1 {
        return Err(io::Error::last_os_error());
    }
    let reader = unsafe { OwnedFd::from_raw_fd(descriptors[0]) };
    let writer = unsafe { OwnedFd::from_raw_fd(descriptors[1]) };
    Ok((reader, writer))
}

fn release(writer: &OwnedFd) -> io::Result<()> {
    let byte = [1_u8];
    let written = unsafe { libc::write(writer.as_raw_fd(), byte.as_ptr().cast(), byte.len()) };
    if written == byte.len() as isize {
        Ok(())
    } else if written == -1 {
        Err(io::Error::last_os_error())
    } else {
        Err(io::Error::new(
            io::ErrorKind::WriteZero,
            format!("the launcher release pipe accepted {written} bytes"),
        ))
    }
}

fn write_all(fd: RawFd, mut bytes: &[u8]) -> io::Result<()> {
    while !bytes.is_empty() {
        let written = unsafe { libc::write(fd, bytes.as_ptr().cast(), bytes.len()) };
        if written > 0 {
            bytes = &bytes[written as usize..];
        } else if written == -1 && io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
        } else if written == -1 {
            return Err(io::Error::last_os_error());
        } else {
            return Err(io::Error::new(
                io::ErrorKind::WriteZero,
                "the workload-status pipe accepted no bytes",
            ));
        }
    }
    Ok(())
}

struct WorkloadStatus {
    reader: OwnedFd,
    bytes: [u8; std::mem::size_of::<libc::c_int>()],
    read: usize,
}

impl WorkloadStatus {
    fn new(reader: OwnedFd) -> Self {
        WorkloadStatus {
            reader,
            bytes: [0; std::mem::size_of::<libc::c_int>()],
            read: 0,
        }
    }

    fn pending(&mut self) -> io::Result<Option<libc::c_int>> {
        let result = unsafe {
            libc::read(
                self.reader.as_raw_fd(),
                self.bytes[self.read..].as_mut_ptr().cast(),
                self.bytes.len() - self.read,
            )
        };
        if result > 0 {
            self.read += result as usize;
            if self.read == self.bytes.len() {
                return Ok(Some(libc::c_int::from_ne_bytes(self.bytes)));
            }
        } else if result == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "the launcher exited before it reported the workload status",
            ));
        } else {
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::WouldBlock
                && error.kind() != io::ErrorKind::Interrupted
            {
                return Err(error);
            }
        }
        Ok(None)
    }
}

fn close_on_exec(fd: RawFd) -> io::Result<()> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    if flags == -1 || unsafe { libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) } == -1 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

fn close_unrelated_descriptors(command: &mut Command, preserved: &[RawFd]) -> io::Result<()> {
    let open_max = unsafe { libc::sysconf(libc::_SC_OPEN_MAX) };
    if open_max == -1 {
        return Err(io::Error::last_os_error());
    }
    let preserved = preserved.to_vec();
    unsafe {
        command.pre_exec(move || {
            #[cfg(target_os = "linux")]
            {
                let result = libc::syscall(
                    libc::SYS_close_range,
                    3_u32,
                    u32::MAX,
                    libc::CLOSE_RANGE_CLOEXEC,
                );
                if result == 0 {
                    for &fd in &preserved {
                        clear_close_on_exec(fd)?;
                    }
                    return Ok(());
                }
                let error = io::Error::last_os_error();
                if error.raw_os_error() != Some(libc::ENOSYS) {
                    return Err(error);
                }
            }
            for fd in 3..open_max as RawFd {
                if preserved.contains(&fd) {
                    clear_close_on_exec(fd)?;
                } else if libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) == -1 {
                    let error = io::Error::last_os_error();
                    if error.raw_os_error() != Some(libc::EBADF) {
                        return Err(error);
                    }
                }
            }
            Ok(())
        });
    }
    Ok(())
}

fn clear_close_on_exec(fd: RawFd) -> io::Result<()> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    if flags == -1 || unsafe { libc::fcntl(fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) } == -1 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

fn combined(primary: io::Error, cleanup: io::Error) -> io::Error {
    io::Error::new(
        primary.kind(),
        format!("{primary}. Process-group cleanup also failed: {cleanup}"),
    )
}
