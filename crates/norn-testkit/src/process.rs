//! Spawning a process under harness isolation, and measuring what it cost.
//!
//! Every run gets a [`Sandbox`]: a fresh private directory, and an
//! environment built from an allowlist rather than inherited wholesale.
//! `HOME`, `TMPDIR` and the XDG variables point inside that directory, so a
//! run reads and writes machine-local state that belongs to it alone. `PATH`
//! is the one variable carried over from the parent, because a child that
//! cannot find a program cannot run at all.
//!
//! Four properties are the reason this module exists rather than a bare
//! [`std::process::Command`] at each call site:
//!
//! - **Nothing is shared between concurrent runs.** Each sandbox is named for
//!   the process that made it and a counter, so two runs in the same suite
//!   never meet in a cache directory.
//! - **A build artifact is never executed where it lies.** [`Sandbox::install_binary`]
//!   copies it to a private path first. A binary that a concurrent build may
//!   rewrite is not a stable thing to exec, and the failure is a spurious one
//!   in whichever suite happens to be running.
//! - **The child's peak resident set is measured**, by waiting on it through
//!   `wait4` and reading the kernel's own accounting rather than sampling.
//!   The measurement covers the child this harness spawned and every
//!   descendant *that child waited on*, which is what `wait4` accounts for. A
//!   grandchild the child forked and left running is invisible to it, so a
//!   subject that measures itself by detaching a worker measures nothing.
//! - **The measured wait is bounded.** Every run carries a deadline; the wait
//!   polls for the child's end, and at the deadline it kills the child, reaps
//!   it, and reports [`RunStatus::TimedOut`] naming the bound it passed. A
//!   subject that hangs costs its run's deadline rather than the runner's own
//!   timeout. What the deadline bounds is the polling wait: the reap that
//!   follows a kill blocks, because `SIGKILL` is not the child's to catch and
//!   the kernel is what ends it, and a kill the kernel refused leaves the reap
//!   after it with nothing to bound it. The kill goes to the child this
//!   harness spawned, so a grandchild that child left running outlives the run
//!   and finishes under pid 1 — the accepted cost of signalling one process
//!   rather than a group, and the same reason that grandchild is absent from
//!   the accounting.
//!
//! The sandbox is removed when it drops, so a suite leaves nothing behind;
//! what a run said is in its [`Outcome`], not in a file to go looking for.
//!
//! **This module is unix-only.** The wait-and-account call is `wait4` and the
//! status decoding is the unix one; there is no Windows path and none is
//! pretended.

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// The environment variables a run is given, beyond `PATH`. Each points into
/// the sandbox, so a run's machine-local state is its own.
pub const ISOLATED_VARIABLES: &[&str] = &[
    "HOME",
    "TMPDIR",
    "XDG_CACHE_HOME",
    "XDG_CONFIG_HOME",
    "XDG_DATA_HOME",
    "XDG_STATE_HOME",
];

/// On macOS the kernel reports a maximum resident set in bytes; elsewhere in
/// kilobytes.
const MAX_RSS_IN_BYTES: bool = cfg!(any(target_os = "macos", target_os = "ios"));

static NEXT_SANDBOX: AtomicU64 = AtomicU64::new(0);

/// A private directory tree that one run owns and that is removed with it.
pub struct Sandbox {
    root: PathBuf,
    runs: AtomicU64,
}

impl Sandbox {
    /// A fresh sandbox under `base`, named for `label`.
    ///
    /// The name carries the process id and a counter, so sandboxes made by
    /// concurrent tests — in one process or several — never collide.
    #[allow(clippy::disallowed_methods)] // Harness scaffolding: the sandbox is this crate's to build.
    pub fn new(base: &Path, label: &str) -> io::Result<Sandbox> {
        let serial = NEXT_SANDBOX.fetch_add(1, Ordering::Relaxed);
        let root = base.join(format!("{label}-{}-{serial}", std::process::id()));
        std::fs::remove_dir_all(&root).or_else(ignore_missing)?;
        for directory in [
            "home", "tmp", "config", "data", "cache", "state", "bin", "work",
        ] {
            std::fs::create_dir_all(root.join(directory))?;
        }
        Ok(Sandbox {
            root,
            runs: AtomicU64::new(0),
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The directory a run starts in.
    pub fn work_dir(&self) -> PathBuf {
        self.root.join("work")
    }

    /// The environment a run is given: the allowlist, pointed inside this
    /// sandbox, plus the parent's `PATH`.
    pub fn environment(&self) -> BTreeMap<String, OsString> {
        let mut environment: BTreeMap<String, OsString> = BTreeMap::new();
        environment.insert("HOME".to_string(), self.root.join("home").into());
        environment.insert("TMPDIR".to_string(), self.root.join("tmp").into());
        environment.insert("XDG_CACHE_HOME".to_string(), self.root.join("cache").into());
        environment.insert(
            "XDG_CONFIG_HOME".to_string(),
            self.root.join("config").into(),
        );
        environment.insert("XDG_DATA_HOME".to_string(), self.root.join("data").into());
        environment.insert("XDG_STATE_HOME".to_string(), self.root.join("state").into());
        if let Some(path) = std::env::var_os("PATH") {
            environment.insert("PATH".to_string(), path);
        }
        environment
    }

    /// Copy an executable into this sandbox and hand back the copy's path.
    ///
    /// **A build artifact is executed from here, never from where cargo put
    /// it.** The artifact is a file a concurrent build is entitled to
    /// rewrite, and executing a file while it is being written is a failure
    /// of the harness rather than of the program.
    #[allow(clippy::disallowed_methods)] // Harness scaffolding: installing the artifact under test.
    pub fn install_binary(&self, source: &Path) -> io::Result<PathBuf> {
        let name = source.file_name().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{} names no file", source.display()),
            )
        })?;
        let installed = self.root.join("bin").join(name);
        std::fs::copy(source, &installed)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&installed, std::fs::Permissions::from_mode(0o755))?;
        }
        Ok(installed)
    }

    fn next_run(&self) -> u64 {
        self.runs.fetch_add(1, Ordering::Relaxed)
    }
}

impl Drop for Sandbox {
    #[allow(clippy::disallowed_methods)] // Harness scaffolding: a sandbox outlives nothing.
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// How much of a run's output is read into memory, by default.
///
/// A run that prints without bound is a run under test, not a reason for the
/// process measuring it to die. Output past this is left in the sandbox file
/// and reported as truncated.
pub const DEFAULT_CAPTURE_LIMIT: u64 = 8 * 1024 * 1024;

/// How long a run may take, by default, before the harness ends it.
///
/// A child that never finishes is a subject under test, not a reason for the
/// suite measuring it to wait on the runner's own timeout to decide. At this
/// bound the harness kills the child it spawned and reports
/// [`RunStatus::TimedOut`]. A run with more work than this to do names its own
/// bound with [`Run::deadline`]. Every run has one: this is what a run that
/// says nothing gets, and there is no constructor that leaves it unset.
pub const DEFAULT_WAIT_DEADLINE: Duration = Duration::from_secs(60);

/// How long the harness waits before asking about the child again: the first
/// wait, and the longest one.
///
/// The first question is asked before any wait, so a child that is already
/// gone is answered immediately; the wait doubles from the first bound to the
/// longest, so a run that lasts is asked about a bounded number of times
/// rather than once per millisecond, and the answer for one that ends is late
/// by at most the longest wait.
const FIRST_POLL_WAIT: Duration = Duration::from_millis(1);
const LONGEST_POLL_WAIT: Duration = Duration::from_millis(50);

/// One process to run under a sandbox.
pub struct Run<'a> {
    sandbox: &'a Sandbox,
    program: PathBuf,
    args: Vec<OsString>,
    environment: BTreeMap<String, OsString>,
    capture_limit: u64,
    deadline: Duration,
}

impl<'a> Run<'a> {
    /// A run of `program`, isolated by `sandbox`.
    pub fn new(sandbox: &'a Sandbox, program: impl Into<PathBuf>) -> Self {
        Run {
            sandbox,
            program: program.into(),
            args: Vec::new(),
            environment: sandbox.environment(),
            capture_limit: DEFAULT_CAPTURE_LIMIT,
            deadline: DEFAULT_WAIT_DEADLINE,
        }
    }

    /// Read at most `bytes` of each stream into the [`Outcome`].
    pub fn capture_limit(mut self, bytes: u64) -> Self {
        self.capture_limit = bytes;
        self
    }

    /// End this run after `bound` rather than after
    /// [`DEFAULT_WAIT_DEADLINE`], and report [`RunStatus::TimedOut`] when it
    /// comes to that.
    pub fn deadline(mut self, bound: Duration) -> Self {
        self.deadline = bound;
        self
    }

    pub fn arg(mut self, arg: impl AsRef<OsStr>) -> Self {
        self.args.push(arg.as_ref().to_os_string());
        self
    }

    pub fn args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        self.args
            .extend(args.into_iter().map(|arg| arg.as_ref().to_os_string()));
        self
    }

    /// Add one variable to the run's environment. The allowlist is what the
    /// run starts with; this is how a suite adds what its subject needs.
    pub fn env(mut self, name: impl Into<String>, value: impl AsRef<OsStr>) -> Self {
        self.environment
            .insert(name.into(), value.as_ref().to_os_string());
        self
    }

    /// Run to completion and report what happened.
    #[allow(clippy::disallowed_methods, clippy::disallowed_types)] // Harness scaffolding: the run's own output files.
    pub fn wait(self) -> io::Result<Outcome> {
        let serial = self.sandbox.next_run();
        let out_path = self.sandbox.root.join(format!("run-{serial}.stdout"));
        let err_path = self.sandbox.root.join(format!("run-{serial}.stderr"));

        // Output goes to files rather than pipes: a pipe nobody is reading
        // fills and stops the child, and the harness has to wait on the child
        // to measure it.
        let child = Command::new(&self.program)
            .args(&self.args)
            .current_dir(self.sandbox.work_dir())
            .env_clear()
            .envs(&self.environment)
            .stdin(Stdio::null())
            .stdout(Stdio::from(std::fs::File::create(&out_path)?))
            .stderr(Stdio::from(std::fs::File::create(&err_path)?))
            .spawn()?;

        let (status, peak_rss_bytes) = wait_for(child, self.deadline)?;
        let (stdout, stdout_truncated) = capture(&out_path, self.capture_limit)?;
        let (stderr, stderr_truncated) = capture(&err_path, self.capture_limit)?;
        Ok(Outcome {
            status,
            peak_rss_bytes,
            stdout,
            stdout_truncated,
            stderr,
            stderr_truncated,
        })
    }
}

/// How a run ended.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RunStatus {
    Exited(i32),
    Signaled(i32),
    /// The harness ended the run: the child was still going at the deadline
    /// it names, and was killed and reaped there.
    ///
    /// This is reported for an end the harness caused and for no other. A
    /// child that exited or took a signal of its own reports that, however
    /// close to the bound it was. The one end that cannot be told apart is a
    /// `SIGKILL` from somewhere else landing on the child in the moment the
    /// harness sent its own, which reads as the timeout.
    TimedOut {
        /// The deadline the run was given, which is the bound it passed.
        after: Duration,
    },
}

/// What a run did, and what it cost.
pub struct Outcome {
    pub status: RunStatus,
    /// The child's peak resident set, in bytes, as the kernel accounted it:
    /// the spawned process and every descendant it waited on, and nothing it
    /// left running. For a run that reports [`RunStatus::TimedOut`] this is
    /// the peak as of the kill, which is as far as the run got.
    pub peak_rss_bytes: u64,
    pub stdout: Vec<u8>,
    /// Whether the child wrote more to stdout than the capture limit, so what
    /// is here is a prefix.
    pub stdout_truncated: bool,
    pub stderr: Vec<u8>,
    /// Whether the child wrote more to stderr than the capture limit.
    pub stderr_truncated: bool,
}

impl Outcome {
    pub fn stdout_text(&self) -> String {
        String::from_utf8_lossy(&self.stdout).to_string()
    }

    pub fn stderr_text(&self) -> String {
        String::from_utf8_lossy(&self.stderr).to_string()
    }

    pub fn assert_success(&self) {
        assert_eq!(
            self.status,
            RunStatus::Exited(0),
            "the run failed: {}",
            self.stderr_text()
        );
    }
}

/// What one `wait4` reported about a child that has ended: the raw status and
/// the accounting, read in the one call.
struct Ended {
    status: libc::c_int,
    usage: libc::rusage,
}

/// Wait on `child` through `wait4` until it ends or `deadline` is up, so its
/// exit and its resource accounting are read in one call.
///
/// The wait polls rather than blocks, because a blocking wait on a child that
/// never ends is the whole thing the deadline exists to stop. At the deadline
/// the child this harness spawned takes a `SIGKILL` and is reaped through the
/// same call; that reap blocks, because `SIGKILL` is not the child's to catch
/// and the kernel is what ends the wait.
///
/// **The deadline observes; it does not preempt.** A status the child produced
/// is the status reported, including one produced between the last poll and
/// the kill: [`RunStatus::TimedOut`] is reported for the harness's own
/// `SIGKILL` and for nothing else.
///
/// **On the success path the child is reaped here and nowhere else**: nothing
/// calls `wait` on it afterwards, because the process is already gone and its
/// identifier is free for the kernel to hand to somebody else. Where `wait4`
/// did not reap — it failed, or the kill before it failed — the child is
/// killed and waited on through [`reaping`] before the error goes back, so a
/// run that failed to be measured leaves no process behind. The one failure
/// that comes after a reap is a status this code has no reading for, and it
/// takes that path for the same reason: the child is already gone.
fn wait_for(child: std::process::Child, deadline: Duration) -> io::Result<(RunStatus, u64)> {
    let pid = child.id() as libc::pid_t;
    let started = Instant::now();
    let mut poll_wait = FIRST_POLL_WAIT;
    let ended = loop {
        match waited(pid, libc::WNOHANG) {
            Ok(Some(ended)) => break Some(ended),
            Ok(None) => {}
            Err(error) => return Err(reaping(child, error)),
        }
        // The poll above is the question asked at the boundary too, so a child
        // that ended inside its deadline is reported however late this loop
        // got around to asking.
        let left = deadline.saturating_sub(started.elapsed());
        if left.is_zero() {
            break None;
        }
        std::thread::sleep(poll_wait.min(left));
        poll_wait = (poll_wait * 2).min(LONGEST_POLL_WAIT);
    };

    let (ended, timed_out) = match ended {
        Some(ended) => (ended, false),
        None => {
            // SAFETY: `pid` is this process's own child and nothing has reaped
            // it, so the number is still that child's and not somebody else's.
            if unsafe { libc::kill(pid, libc::SIGKILL) } == -1 {
                return Err(reaping(child, io::Error::last_os_error()));
            }
            match waited(pid, 0) {
                Ok(Some(ended)) => (ended, true),
                // A zero is the answer `WNOHANG` asks for, and this wait does
                // not ask for it, so it is a return this code has no reading
                // for like any other.
                Ok(None) => return Err(reaping(child, unexpected(pid, 0))),
                Err(error) => return Err(reaping(child, error)),
            }
        }
    };

    let status = decoded(pid, ended.status)?;
    let status = match status {
        RunStatus::Signaled(libc::SIGKILL) if timed_out => RunStatus::TimedOut { after: deadline },
        status => status,
    };
    let max_rss = ended.usage.ru_maxrss.max(0) as u64;
    Ok((
        status,
        if MAX_RSS_IN_BYTES {
            max_rss
        } else {
            max_rss * 1024
        },
    ))
}

/// One `wait4` on `pid`, retried through interruption.
///
/// `None` is a zero return: `wait4` reporting the child still running, which
/// is the answer `WNOHANG` asks for and the only one that does not reap. What
/// it means where nothing asked for it is the caller's to say.
fn waited(pid: libc::pid_t, flags: libc::c_int) -> io::Result<Option<Ended>> {
    loop {
        let mut status: libc::c_int = 0;
        let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
        // SAFETY: `pid` is this process's own child, and both out-parameters
        // are live for the call.
        let waited = unsafe { libc::wait4(pid, &mut status, flags, &mut usage) };
        if waited == pid {
            return Ok(Some(Ended { status, usage }));
        }
        if waited == 0 {
            return Ok(None);
        }
        // `errno` says nothing except where the call reported failure, so it
        // is only read at -1. Asked about one pid, `wait4` returns that pid,
        // zero, or -1; anything else is a contract this code does not
        // understand and will not guess at.
        if waited != -1 {
            return Err(unexpected(pid, waited));
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
}

/// The error for a `wait4` return this code has no reading for.
fn unexpected(pid: libc::pid_t, returned: libc::pid_t) -> io::Error {
    io::Error::other(format!(
        "`wait4` on pid {pid} returned {returned}, which is neither the child nor a failure"
    ))
}

/// What a raw wait status says the child did.
///
/// Exit and signal are decided by asking, not by elimination: a status that is
/// neither is a status this code has no reading for.
fn decoded(pid: libc::pid_t, status: libc::c_int) -> io::Result<RunStatus> {
    if libc::WIFEXITED(status) {
        Ok(RunStatus::Exited(libc::WEXITSTATUS(status)))
    } else if libc::WIFSIGNALED(status) {
        Ok(RunStatus::Signaled(libc::WTERMSIG(status)))
    } else {
        Err(io::Error::other(format!(
            "pid {pid} reported wait status {status:#x}, which is neither an exit nor a signal"
        )))
    }
}

/// Kill and reap a child that `wait4` did not, and hand back the error that
/// says why.
///
/// **This is called only where the reap did not happen**, and it holds only
/// there: the wait it makes is `std::process::Child`'s own, which asks about a
/// pid the kernel is free to reuse the moment something else reaps it. Reached
/// after a reap, it would be a question about whatever process holds that
/// number now.
fn reaping(mut child: std::process::Child, error: io::Error) -> io::Error {
    let _ = child.kill();
    let _ = child.wait();
    error
}

/// Read at most `limit` bytes of `path`, and say whether there were more.
#[allow(clippy::disallowed_methods, clippy::disallowed_types)] // Harness scaffolding: the run's own output files.
fn capture(path: &Path, limit: u64) -> io::Result<(Vec<u8>, bool)> {
    let file = std::fs::File::open(path)?;
    let mut bytes = Vec::new();
    file.take(limit.saturating_add(1)).read_to_end(&mut bytes)?;
    let truncated = bytes.len() as u64 > limit;
    bytes.truncate(limit as usize);
    Ok((bytes, truncated))
}

fn ignore_missing(error: io::Error) -> io::Result<()> {
    if error.kind() == io::ErrorKind::NotFound {
        Ok(())
    } else {
        Err(error)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    fn sandbox(label: &str) -> Sandbox {
        Sandbox::new(&std::env::temp_dir(), label).expect("a sandbox")
    }

    fn shell(sandbox: &Sandbox, script: &str) -> Outcome {
        Run::new(sandbox, "/bin/sh")
            .args(["-c", script])
            .wait()
            .expect("running a shell")
    }

    #[test]
    fn a_run_starts_from_the_allowlist_and_inherits_nothing_else() {
        let sandbox = sandbox("environment");
        // `CARGO` is set in this process by the harness that runs the test,
        // so a child that cannot see it is a child that inherited nothing.
        let outcome = shell(&sandbox, "echo \"${CARGO:-absent}\"; echo \"$HOME\"");
        outcome.assert_success();
        let stdout = outcome.stdout_text();
        let mut lines = stdout.lines();
        assert_eq!(lines.next(), Some("absent"));
        assert_eq!(
            lines.next(),
            Some(sandbox.root().join("home").to_string_lossy().as_ref())
        );
    }

    /// The allowlist is the contract, so the environment is exactly it plus
    /// `PATH` — a variable added to one and not the other would make the
    /// constant a description of nothing.
    #[test]
    fn a_runs_environment_is_the_allowlist_and_path_and_nothing_else() {
        let sandbox = sandbox("allowlist");
        let environment = sandbox.environment();
        let names: BTreeSet<&str> = environment.keys().map(String::as_str).collect();
        let mut expected: BTreeSet<&str> = ISOLATED_VARIABLES.iter().copied().collect();
        if std::env::var_os("PATH").is_some() {
            expected.insert("PATH");
        }
        assert_eq!(names, expected);
    }

    #[test]
    fn every_isolated_variable_points_inside_the_sandbox() {
        let sandbox = sandbox("variables");
        let environment = sandbox.environment();
        for name in ISOLATED_VARIABLES {
            let value = environment
                .get(*name)
                .unwrap_or_else(|| panic!("`{name}` is not set"));
            assert!(
                Path::new(value).starts_with(sandbox.root()),
                "`{name}` points outside the sandbox: {value:?}"
            );
        }
    }

    #[test]
    fn two_sandboxes_share_no_directory() {
        let (one, two) = (sandbox("shared"), sandbox("shared"));
        assert_ne!(one.root(), two.root());
    }

    #[test]
    fn a_sandbox_is_gone_when_it_drops() {
        let root = {
            let sandbox = sandbox("dropped");
            sandbox.root().to_path_buf()
        };
        #[allow(clippy::disallowed_methods)] // Asserting on the harness's own scaffolding.
        let leftover = std::fs::metadata(&root).is_ok();
        assert!(!leftover, "{} outlived its sandbox", root.display());
    }

    /// The subject is this suite's own executable — a build artifact, which
    /// is exactly the kind of file the copy exists for. Listing its tests
    /// runs none of them.
    #[test]
    fn an_installed_binary_runs_from_the_sandbox_rather_than_its_source() {
        let sandbox = sandbox("installed");
        let source = std::env::current_exe().expect("this suite's own executable");
        let installed = sandbox
            .install_binary(&source)
            .expect("installing a binary");
        assert!(installed.starts_with(sandbox.root()));
        assert_ne!(installed, source);

        let outcome = Run::new(&sandbox, &installed)
            .arg("--list")
            .wait()
            .expect("running the installed copy");
        outcome.assert_success();
        assert!(
            outcome
                .stdout_text()
                .contains("an_installed_binary_runs_from_the_sandbox_rather_than_its_source: test"),
            "the copy listed: {}",
            outcome.stdout_text()
        );
    }

    #[test]
    fn a_runs_exit_code_comes_back() {
        let sandbox = sandbox("exit");
        assert_eq!(shell(&sandbox, "exit 3").status, RunStatus::Exited(3));
    }

    #[test]
    fn a_signalled_run_is_reported_as_signalled() {
        let sandbox = sandbox("signal");
        let outcome = shell(&sandbox, "kill -TERM $$");
        assert_eq!(outcome.status, RunStatus::Signaled(libc::SIGTERM));
    }

    /// A child that prints without bound is a subject under test. The
    /// process measuring it reads a prefix and says so, rather than growing
    /// to match.
    #[test]
    fn a_runs_output_is_captured_up_to_a_limit() {
        let sandbox = sandbox("capture");
        let outcome = Run::new(&sandbox, "/bin/sh")
            .args(["-c", "head -c 4096 /dev/zero | tr '\\0' 'a'"])
            .capture_limit(64)
            .wait()
            .expect("running a shell");
        outcome.assert_success();
        assert_eq!(outcome.stdout.len(), 64);
        assert!(outcome.stdout_truncated);
        assert!(!outcome.stderr_truncated);

        let short = shell(&sandbox, "printf hello");
        assert_eq!(short.stdout_text(), "hello");
        assert!(!short.stdout_truncated);
    }

    /// A wait that never returns is what this forbids, so the assertion is
    /// made from outside the wait: the run has a thread of its own and the
    /// test holds a bound of its own, generous enough that nothing but a
    /// missing deadline reaches it. A harness that stopped ending runs fails
    /// here by name at that bound, not by hanging until the runner decides.
    ///
    /// The failing path leaves the thread and the sleeping child behind. A
    /// leaked sleeper on a run that is already failing is worth an assertion
    /// that names the defect instead of a suite that stops.
    #[test]
    fn a_run_past_its_deadline_is_killed_and_reported_as_timed_out() {
        const DEADLINE: Duration = Duration::from_millis(500);
        /// What the test itself will wait, which the mechanism has no business
        /// approaching: it is sixty times the deadline it is watching.
        const BOUND: Duration = Duration::from_secs(30);

        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let sandbox = sandbox("deadline");
            let started = Instant::now();
            // The child prints from a program of its own, so what it wrote is
            // flushed and on disk before it hangs, and then becomes the sleep
            // itself, so the process the harness kills is the one that hangs.
            let outcome = Run::new(&sandbox, "/bin/sh")
                .args(["-c", "env printf hang; exec sleep 3600"])
                .deadline(DEADLINE)
                .wait();
            let _ = sender.send((outcome, started.elapsed()));
        });

        // The two ways this channel fails apart: the bound passing is the
        // defect under test, and a sender that is gone is the run's own thread
        // having died, which says nothing about the deadline.
        let (outcome, elapsed) = match receiver.recv_timeout(BOUND) {
            Ok(answer) => answer,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => panic!(
                "the wait did not return within {BOUND:?}: a run given a {DEADLINE:?} deadline is \
                 still waiting on a child that sleeps for an hour, so the deadline is not ending \
                 the run"
            ),
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                panic!("the thread running the child ended without reporting, so it panicked")
            }
        };
        let outcome = outcome.expect("waiting on a run");
        assert_eq!(outcome.status, RunStatus::TimedOut { after: DEADLINE });
        assert!(
            elapsed >= DEADLINE,
            "a run reported the deadline after {elapsed:?}, which is inside it"
        );
        assert!(
            elapsed < BOUND / 6,
            "ending the run cost {elapsed:?}, which is nowhere near the {DEADLINE:?} it was given \
             and within reach of the {BOUND:?} this test holds"
        );
        // What the child wrote before it hung is a file, so the timeout hands
        // it back, and the accounting is the kernel's own as of the kill.
        assert_eq!(outcome.stdout_text(), "hang");
        assert!(
            outcome.peak_rss_bytes > 0,
            "a killed run reported no peak resident set"
        );
    }

    /// The deadline observes; it does not preempt. A child that ends well
    /// inside its bound reports what it did, and a timeout here would mean the
    /// harness ended a run that was ending on its own.
    #[test]
    fn a_run_that_ends_inside_its_deadline_reports_its_own_status() {
        let sandbox = sandbox("inside-deadline");
        let outcome = Run::new(&sandbox, "/bin/sh")
            .args(["-c", "exit 7"])
            .deadline(Duration::from_secs(10))
            .wait()
            .expect("running a shell");
        assert_eq!(outcome.status, RunStatus::Exited(7));
    }

    /// `SIGKILL` is the signal the deadline sends, and a child that takes one
    /// of its own inside its bound still ended itself. Reading the signal
    /// alone — rather than the signal on a run this harness killed — is what
    /// would report this run as timed out.
    #[test]
    fn a_run_that_kills_itself_inside_its_deadline_is_not_a_timeout() {
        let sandbox = sandbox("self-killed");
        let outcome = Run::new(&sandbox, "/bin/sh")
            .args(["-c", "kill -KILL $$"])
            .deadline(Duration::from_secs(10))
            .wait()
            .expect("running a shell");
        assert_eq!(outcome.status, RunStatus::Signaled(libc::SIGKILL));
    }

    #[test]
    fn a_runs_peak_resident_set_is_measured() {
        let sandbox = sandbox("memory");
        let idle = shell(&sandbox, "exit 0");
        idle.assert_success();
        assert!(
            idle.peak_rss_bytes > 64 * 1024,
            "a process that ran reported {} bytes",
            idle.peak_rss_bytes
        );

        // The child holds twenty megabytes in a shell variable, which the
        // idle run does not, so the measurement has to separate them.
        let hungry = shell(
            &sandbox,
            "big=$(head -c 20000000 /dev/zero | tr '\\0' 'a'); echo ${#big}",
        );
        hungry.assert_success();
        assert_eq!(hungry.stdout_text().trim(), "20000000");
        assert!(
            hungry.peak_rss_bytes > idle.peak_rss_bytes + 8 * 1024 * 1024,
            "an idle run reported {} bytes and a hungry one {}",
            idle.peak_rss_bytes,
            hungry.peak_rss_bytes
        );
    }
}
