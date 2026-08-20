//! Identity-safe ownership of one harness process group.
//!
//! The direct child is also the process-group leader. The leader stays
//! unreaped while this module can send a destructive signal, which prevents
//! its process ID and process-group ID from being reused during cleanup. After the
//! leader is reaped, this module only observes the group until the kernel says
//! it no longer exists.

use std::io;
use std::os::unix::process::CommandExt;
#[cfg(test)]
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
#[cfg(test)]
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::time::{Duration, Instant};

use crate::poll;

const SIGNAL_DELIVERY_WINDOW: Duration = Duration::from_millis(100);
const GROUP_CLEANUP_DEADLINE: Duration = Duration::from_secs(5);

#[derive(Debug)]
pub(super) struct ProcessEnd {
    pub(super) pid: libc::pid_t,
    pub(super) status: libc::c_int,
    pub(super) usage: libc::rusage,
    pub(super) timed_out: bool,
}

pub(super) struct OwnedProcessGroup {
    pgid: libc::pid_t,
    state: OwnershipState,
    cleanup_timing: Option<CleanupTiming>,
    cleanup_armed: bool,
    #[cfg(test)]
    faults: Faults,
}

pub(super) enum WaitResult<T> {
    Finished {
        result: io::Result<ProcessEnd>,
        group_empty: bool,
    },
    Interrupted {
        event: T,
        cleanup: io::Result<ProcessEnd>,
        group_empty: bool,
    },
}

enum OwnershipState {
    Pinned(Child),
    Reaped,
    EmptyProven,
}

#[derive(Clone, Copy)]
struct CleanupTiming {
    delivery_ends: Instant,
    cleanup_ends: Instant,
    deadline: Duration,
}

#[cfg(test)]
#[derive(Default)]
struct Faults {
    leader_observation: Option<libc::c_int>,
    group_signal: Option<libc::c_int>,
    group_observation: Option<libc::c_int>,
    panic_marker: Option<PathBuf>,
    pretend_signal_presence: usize,
    signal_attempts: Arc<AtomicUsize>,
    cleanup_deadline: Option<Duration>,
    group_remains_present: bool,
}

impl OwnedProcessGroup {
    pub(super) fn spawn(command: &mut Command) -> io::Result<Self> {
        command.process_group(0);
        let child = command.spawn()?;
        let pgid = child.id() as libc::pid_t;
        Ok(OwnedProcessGroup {
            pgid,
            state: OwnershipState::Pinned(child),
            cleanup_timing: None,
            cleanup_armed: true,
            #[cfg(test)]
            faults: Faults::default(),
        })
    }

    pub(super) fn wait(self, deadline: Duration) -> io::Result<ProcessEnd> {
        match self.wait_interruptible(deadline, || Ok(None::<()>)) {
            WaitResult::Finished { result, .. } => result,
            WaitResult::Interrupted { .. } => unreachable!("the inert interrupt source fired"),
        }
    }

    pub(super) fn wait_interruptible<F, T>(
        mut self,
        deadline: Duration,
        interrupt: F,
    ) -> WaitResult<T>
    where
        F: FnMut() -> io::Result<Option<T>>,
    {
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.wait_inner(deadline, interrupt)
        })) {
            Ok(result) => {
                self.cleanup_armed = false;
                result
            }
            Err(panic) => {
                let cleanup = self.close(false);
                self.cleanup_armed = false;
                match cleanup {
                    Ok(_) => std::panic::resume_unwind(panic),
                    Err(cleanup_error) => panic!(
                        "process-group cleanup failed during panic `{}`: {cleanup_error}",
                        panic_evidence(&panic)
                    ),
                }
            }
        }
    }

    fn wait_inner<F, T>(&mut self, deadline: Duration, mut interrupt: F) -> WaitResult<T>
    where
        F: FnMut() -> io::Result<Option<T>>,
    {
        let started = Instant::now();
        let mut poll_wait = poll::FIRST_GAP;
        let timed_out = loop {
            match interrupt() {
                Ok(Some(event)) => {
                    let cleanup = self.close(false);
                    let group_empty = self.group_empty();
                    return WaitResult::Interrupted {
                        event,
                        cleanup,
                        group_empty,
                    };
                }
                Ok(None) => {}
                Err(interrupt_error) => {
                    let cleanup = self.close(false);
                    let group_empty = self.group_empty();
                    let result = match cleanup {
                        Ok(ended) => Err(with_leader_status(interrupt_error, ended.status)),
                        Err(cleanup_error) => Err(combined(interrupt_error, cleanup_error)),
                    };
                    return WaitResult::Finished {
                        result,
                        group_empty,
                    };
                }
            }
            match self.observe_leader() {
                Ok(true) => break false,
                Ok(false) => {}
                Err(wait_error) => {
                    let wait_error = io::Error::new(
                        wait_error.kind(),
                        format!(
                            "`waitid` failed while observing process-group leader {}: {wait_error}",
                            self.pgid
                        ),
                    );
                    let cleanup = self.close(false);
                    let group_empty = self.group_empty();
                    let result = match cleanup {
                        Ok(ended) => Err(with_leader_status(wait_error, ended.status)),
                        Err(cleanup_error) => Err(combined(wait_error, cleanup_error)),
                    };
                    return WaitResult::Finished {
                        result,
                        group_empty,
                    };
                }
            }

            let left = deadline.saturating_sub(started.elapsed());
            if left.is_zero() {
                break true;
            }
            poll_wait = poll::sleep_gap(poll_wait, left);
        };

        let result = self.close(timed_out);
        let group_empty = self.group_empty();
        WaitResult::Finished {
            result,
            group_empty,
        }
    }

    pub(super) fn close_now(mut self) -> io::Result<ProcessEnd> {
        let result = self.close(false);
        self.cleanup_armed = false;
        result
    }

    pub(super) fn pgid(&self) -> libc::pid_t {
        self.pgid
    }

    fn group_empty(&self) -> bool {
        matches!(self.state, OwnershipState::EmptyProven)
    }

    fn close(&mut self, timed_out: bool) -> io::Result<ProcessEnd> {
        let timing = self.cleanup_timing();
        let delivery_ends = timing.delivery_ends;
        let cleanup_deadline = timing.deadline;
        let cleanup_ends = timing.cleanup_ends;
        let mut signal_errors = Vec::new();
        let mut delivery_failed = false;
        let mut poll_wait = poll::FIRST_GAP;

        loop {
            let now = Instant::now();
            if now >= delivery_ends || now >= cleanup_ends {
                break;
            }
            match self.send_group_signal(libc::SIGKILL) {
                Ok(GroupPresence::Absent) => break,
                Ok(GroupPresence::Present) => {}
                // macOS returns EPERM when the pinned group contains only
                // zombie members. Reaping plus the ESRCH proof below decides
                // whether a live inaccessible member also remains.
                Err(error) if error.raw_os_error() == Some(libc::EPERM) => {
                    signal_errors.push(error);
                    break;
                }
                Err(error) => {
                    delivery_failed = true;
                    signal_errors.push(error);
                }
            };

            let now = Instant::now();
            poll_wait = poll::sleep_gap(poll_wait, delivery_ends.saturating_duration_since(now));
        }

        let mut poll_wait = poll::FIRST_GAP;
        loop {
            match self.observe_leader() {
                Ok(true) => break,
                Ok(false) => {}
                Err(error) => {
                    return Err(with_signal_errors(
                        io::Error::other(format!(
                            "failed to observe process-group leader {} during cleanup: {error}",
                            self.pgid
                        )),
                        signal_errors,
                    ));
                }
            }
            let left = cleanup_ends.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(with_signal_errors(
                    cleanup_timeout(self.pgid, cleanup_deadline, None),
                    signal_errors,
                ));
            }
            poll_wait = poll::sleep_gap(poll_wait, left);
        }

        let ended = reap_leader(self.pgid).map_err(|error| {
            with_signal_errors(
                io::Error::other(format!(
                    "failed to reap process-group leader {}: {error}",
                    self.pgid
                )),
                std::mem::take(&mut signal_errors),
            )
        })?;
        let OwnershipState::Pinned(child) =
            std::mem::replace(&mut self.state, OwnershipState::Reaped)
        else {
            return Err(io::Error::other(format!(
                "process group {} lost its leader identity before `wait4` reaped it",
                self.pgid
            )));
        };
        drop(child);

        let mut poll_wait = poll::FIRST_GAP;
        loop {
            match self.observe_owned_group() {
                Ok(GroupPresence::Absent) => {
                    self.state = OwnershipState::EmptyProven;
                    if delivery_failed {
                        return Err(io::Error::other(format!(
                            "process group {} is empty and its leader reported raw status {:#x}, but SIGKILL delivery failed: {}",
                            self.pgid,
                            ended.status,
                            error_evidence(&signal_errors)
                        )));
                    }
                    return Ok(ProcessEnd {
                        pid: self.pgid,
                        status: ended.status,
                        usage: ended.usage,
                        timed_out,
                    });
                }
                Ok(GroupPresence::Present) => {}
                Err(error) => {
                    return Err(with_signal_errors(
                        io::Error::new(
                            error.kind(),
                            format!(
                                "failed to prove that process group {} is empty after its leader reported raw status {:#x}: {error}",
                                self.pgid, ended.status
                            ),
                        ),
                        signal_errors,
                    ));
                }
            }
            let left = cleanup_ends.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(with_signal_errors(
                    cleanup_timeout(self.pgid, cleanup_deadline, Some(ended.status)),
                    signal_errors,
                ));
            }
            poll_wait = poll::sleep_gap(poll_wait, left);
        }
    }

    fn observe_leader(&mut self) -> io::Result<bool> {
        #[cfg(test)]
        {
            if self
                .faults
                .panic_marker
                .as_ref()
                .is_some_and(|path| marker_exists(path))
            {
                self.faults.panic_marker = None;
                panic!("injected process wait panic");
            }
            if let Some(errno) = self.faults.leader_observation.take() {
                return Err(io::Error::from_raw_os_error(errno));
            }
        }
        leader_has_ended(self.pgid)
    }

    fn send_group_signal(&mut self, signal: libc::c_int) -> io::Result<GroupPresence> {
        if !matches!(self.state, OwnershipState::Pinned(_)) {
            return Err(io::Error::other(format!(
                "refused signal {signal} for unpinned process group {}",
                self.pgid
            )));
        }
        #[cfg(test)]
        {
            self.faults.signal_attempts.fetch_add(1, Ordering::Relaxed);
            if self.faults.pretend_signal_presence > 0 {
                self.faults.pretend_signal_presence -= 1;
                return Ok(GroupPresence::Present);
            }
            if let Some(errno) = self.faults.group_signal.take() {
                return Err(io::Error::from_raw_os_error(errno));
            }
        }
        signal_group(self.pgid, signal)
    }

    fn observe_owned_group(&mut self) -> io::Result<GroupPresence> {
        #[cfg(test)]
        {
            if let Some(errno) = self.faults.group_observation.take() {
                return Err(io::Error::from_raw_os_error(errno));
            }
            if self.faults.group_remains_present {
                return Ok(GroupPresence::Present);
            }
        }
        observe_group(self.pgid)
    }

    fn cleanup_deadline(&self) -> Duration {
        #[cfg(test)]
        if let Some(deadline) = self.faults.cleanup_deadline {
            return deadline;
        }
        GROUP_CLEANUP_DEADLINE
    }

    fn cleanup_timing(&mut self) -> CleanupTiming {
        if let Some(timing) = self.cleanup_timing {
            return timing;
        }
        let started = Instant::now();
        let deadline = self.cleanup_deadline();
        let timing = CleanupTiming {
            delivery_ends: started + SIGNAL_DELIVERY_WINDOW,
            cleanup_ends: started + deadline,
            deadline,
        };
        self.cleanup_timing = Some(timing);
        timing
    }

    #[cfg(test)]
    fn pgid_for_test(&self) -> libc::pid_t {
        self.pgid
    }

    #[cfg(test)]
    fn fail_next_leader_observation(&mut self, errno: libc::c_int) {
        self.faults.leader_observation = Some(errno);
    }

    #[cfg(test)]
    fn fail_next_group_signal(&mut self, errno: libc::c_int) {
        self.faults.group_signal = Some(errno);
    }

    #[cfg(test)]
    fn fail_next_group_observation(&mut self, errno: libc::c_int) {
        self.faults.group_observation = Some(errno);
    }

    #[cfg(test)]
    fn panic_when_path_exists(&mut self, path: PathBuf) {
        self.faults.panic_marker = Some(path);
    }

    #[cfg(test)]
    fn count_group_signals_for_test(&mut self) -> Arc<AtomicUsize> {
        Arc::clone(&self.faults.signal_attempts)
    }

    #[cfg(test)]
    fn pretend_group_present_for_signals(&mut self, attempts: usize) {
        self.faults.pretend_signal_presence = attempts;
    }

    #[cfg(test)]
    fn set_cleanup_deadline_for_test(&mut self, deadline: Duration) {
        self.faults.cleanup_deadline = Some(deadline);
    }

    #[cfg(test)]
    fn pretend_group_remains_present(&mut self) {
        self.faults.group_remains_present = true;
    }
}

impl Drop for OwnedProcessGroup {
    fn drop(&mut self) {
        if self.cleanup_armed && matches!(self.state, OwnershipState::Pinned(_)) {
            let _ = self.close(false);
        }
    }
}

struct Ended {
    status: libc::c_int,
    usage: libc::rusage,
}

#[derive(Clone, Copy)]
enum GroupPresence {
    Present,
    Absent,
}

fn leader_has_ended(pid: libc::pid_t) -> io::Result<bool> {
    loop {
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        let result = unsafe {
            libc::waitid(
                libc::P_PID,
                pid as libc::id_t,
                &mut info,
                libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
            )
        };
        if result == 0 {
            return Ok(unsafe { info.si_pid() } == pid);
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
}

fn reap_leader(pid: libc::pid_t) -> io::Result<Ended> {
    loop {
        let mut status = 0;
        let mut usage = unsafe { std::mem::zeroed() };
        let waited = unsafe { libc::wait4(pid, &mut status, 0, &mut usage) };
        if waited == pid {
            return Ok(Ended { status, usage });
        }
        if waited != -1 {
            return Err(io::Error::other(format!(
                "`wait4` on pid {pid} returned {waited}, which is neither the child nor a failure"
            )));
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
}

fn signal_group(pgid: libc::pid_t, signal: libc::c_int) -> io::Result<GroupPresence> {
    if unsafe { libc::killpg(pgid, signal) } == 0 {
        return Ok(GroupPresence::Present);
    }
    let error = io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::ESRCH) {
        Ok(GroupPresence::Absent)
    } else {
        Err(error)
    }
}

fn observe_group(pgid: libc::pid_t) -> io::Result<GroupPresence> {
    signal_group(pgid, 0).map_err(|error| {
        io::Error::other(format!(
            "failed to prove that process group {pgid} is empty: {error}"
        ))
    })
}

fn cleanup_timeout(
    pgid: libc::pid_t,
    deadline: Duration,
    leader_status: Option<libc::c_int>,
) -> io::Error {
    let status = leader_status
        .map(|status| format!("; the leader reported raw status {status:#x}"))
        .unwrap_or_default();
    io::Error::new(
        io::ErrorKind::TimedOut,
        format!(
            "process group {pgid} still exists after the {deadline:?} cleanup deadline{status}"
        ),
    )
}

fn with_signal_errors(error: io::Error, signal_errors: Vec<io::Error>) -> io::Error {
    if signal_errors.is_empty() {
        error
    } else {
        io::Error::new(
            error.kind(),
            format!(
                "{error}. SIGKILL delivery also reported: {}",
                error_evidence(&signal_errors)
            ),
        )
    }
}

fn error_evidence(errors: &[io::Error]) -> String {
    errors
        .iter()
        .enumerate()
        .map(|(index, error)| format!("{}: {error}", index + 1))
        .collect::<Vec<_>>()
        .join(", ")
}

fn combined(primary: io::Error, cleanup: io::Error) -> io::Error {
    io::Error::new(
        primary.kind(),
        format!("process wait failed: {primary}. Process-group cleanup also failed: {cleanup}"),
    )
}

fn with_leader_status(error: io::Error, status: libc::c_int) -> io::Error {
    io::Error::new(
        error.kind(),
        format!("{error}. Cleanup reaped the leader with raw status {status:#x}"),
    )
}

fn panic_evidence(panic: &Box<dyn std::any::Any + Send>) -> &str {
    panic
        .downcast_ref::<String>()
        .map(String::as_str)
        .or_else(|| panic.downcast_ref::<&'static str>().copied())
        .unwrap_or("non-string panic payload")
}

#[cfg(test)]
#[allow(clippy::disallowed_methods)] // Harness fault seam: observing its own marker.
fn marker_exists(path: &Path) -> bool {
    path.exists()
}

#[cfg(test)]
mod tests {
    use std::panic::{AssertUnwindSafe, catch_unwind};
    use std::process::Stdio;
    use std::sync::atomic::Ordering;

    use crate::scratch::Scratch;

    use super::*;

    fn sleeping_group() -> OwnedProcessGroup {
        let mut command = Command::new("/bin/sh");
        command
            .args(["-c", "exec sleep 3600"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        OwnedProcessGroup::spawn(&mut command).expect("a sleeping process group")
    }

    fn completed_group() -> OwnedProcessGroup {
        let mut command = Command::new("/bin/sh");
        command
            .args(["-c", "exit 0"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        OwnedProcessGroup::spawn(&mut command).expect("a completed process group")
    }

    fn wait_for_file(path: &Path) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !marker_exists(path) {
            assert!(
                Instant::now() < deadline,
                "{} did not appear before the test deadline",
                path.display()
            );
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    fn panic_text(payload: Box<dyn std::any::Any + Send>) -> String {
        match payload.downcast::<String>() {
            Ok(message) => *message,
            Err(payload) => payload
                .downcast::<&'static str>()
                .map(|message| (*message).to_string())
                .unwrap_or_else(|_| "non-string panic".to_string()),
        }
    }

    #[test]
    fn a_wait_error_still_empties_the_owned_group() {
        let mut group = sleeping_group();
        let pgid = group.pgid_for_test();
        group.fail_next_leader_observation(libc::EIO);

        let error = group
            .wait(Duration::from_secs(5))
            .expect_err("the injected wait error");

        assert!(error.to_string().contains("waitid"), "{error}");
        assert!(error.to_string().contains("raw status"), "{error}");
        assert!(matches!(
            observe_group(pgid).expect("observing the cleaned group"),
            GroupPresence::Absent
        ));
    }

    #[test]
    fn wait_and_cleanup_errors_are_both_reported() {
        let mut group = sleeping_group();
        group.fail_next_leader_observation(libc::EIO);
        group.fail_next_group_signal(libc::EIO);

        let (result, group_empty) =
            match group.wait_interruptible(Duration::from_secs(5), || Ok(None::<()>)) {
                WaitResult::Finished {
                    result,
                    group_empty,
                } => (result, group_empty),
                WaitResult::Interrupted { .. } => unreachable!("the inert interrupt source fired"),
            };
        let error = result.expect_err("the combined lifecycle error");
        let message = error.to_string();

        assert!(
            group_empty,
            "the injected delivery error hid containment proof"
        );
        assert!(message.contains("waitid"), "{message}");
        assert!(message.contains("SIGKILL"), "{message}");
        assert!(message.contains("Input/output error"), "{message}");
    }

    #[test]
    fn an_unwinding_wait_empties_its_group_before_resuming_the_panic() {
        let scratch = Scratch::new("process-group-panic");
        let marker = scratch.join("descendant.pid");
        let mut command = Command::new("/bin/sh");
        command
            .args(["-c", "sleep 3600 & echo $! > \"$MARKER\"; wait"])
            .env("MARKER", &marker)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let mut group = OwnedProcessGroup::spawn(&mut command).expect("a process group");
        let pgid = group.pgid_for_test();
        group.panic_when_path_exists(marker.clone());

        let panic = catch_unwind(AssertUnwindSafe(|| {
            let _ = group.wait(Duration::from_secs(5));
        }))
        .expect_err("the injected wait panic");

        assert_eq!(panic_text(panic), "injected process wait panic");
        wait_for_file(&marker);
        assert!(matches!(
            observe_group(pgid).expect("observing the cleaned group"),
            GroupPresence::Absent
        ));
    }

    #[test]
    fn a_cleanup_failure_during_unwind_replaces_the_panic_with_combined_evidence() {
        let scratch = Scratch::new("process-group-combined-panic");
        let marker = scratch.join("started");
        let mut command = Command::new("/bin/sh");
        command
            .args(["-c", "touch \"$MARKER\"; exec sleep 3600"])
            .env("MARKER", &marker)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let mut group = OwnedProcessGroup::spawn(&mut command).expect("a process group");
        group.panic_when_path_exists(marker);
        group.fail_next_group_signal(libc::EIO);

        let panic = catch_unwind(AssertUnwindSafe(|| {
            let _ = group.wait(Duration::from_secs(5));
        }))
        .expect_err("the combined containment panic");
        let message = panic_text(panic);

        assert!(message.contains("injected process wait panic"), "{message}");
        assert!(message.contains("SIGKILL"), "{message}");
        assert!(message.contains("Input/output error"), "{message}");
    }

    #[test]
    fn cleanup_repeats_sigkill_during_the_pinned_delivery_window() {
        assert_eq!(SIGNAL_DELIVERY_WINDOW, Duration::from_millis(100));
        let mut group = sleeping_group();
        let attempts = group.count_group_signals_for_test();
        group.pretend_group_present_for_signals(2);

        let ended = group
            .wait(Duration::from_millis(1))
            .expect("cleaning the process group");

        assert!(ended.timed_out);
        assert!(
            attempts.load(Ordering::Relaxed) >= 3,
            "cleanup sent SIGKILL fewer than three times"
        );
    }

    #[test]
    fn pinned_eperm_stops_destructive_signals_and_defers_to_the_final_proof() {
        let mut group = completed_group();
        let attempts = group.count_group_signals_for_test();
        group.fail_next_group_signal(libc::EPERM);

        let ended = group
            .wait(Duration::from_secs(5))
            .expect("the final ESRCH proof settles pinned EPERM");

        assert!(!ended.timed_out);
        assert_eq!(attempts.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn group_verification_is_bounded_and_reports_the_known_leader_status() {
        let mut group = completed_group();
        group.set_cleanup_deadline_for_test(Duration::from_millis(20));
        group.pretend_group_remains_present();

        let error = group
            .wait(Duration::from_secs(5))
            .expect_err("the bounded containment error");
        let message = error.to_string();

        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert!(message.contains("20ms"), "{message}");
        assert!(message.contains("raw status"), "{message}");
    }

    #[test]
    fn final_eperm_and_delivery_errors_are_both_reported_with_leader_status() {
        let mut group = sleeping_group();
        group.fail_next_group_signal(libc::EIO);
        group.fail_next_group_observation(libc::EPERM);

        let error = group
            .wait(Duration::from_millis(1))
            .expect_err("the combined cleanup error");
        let message = error.to_string();

        assert!(message.contains("raw status"), "{message}");
        assert!(message.contains("SIGKILL"), "{message}");
        assert!(message.contains("Operation not permitted"), "{message}");
    }
}
