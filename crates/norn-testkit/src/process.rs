//! Spawning a process under harness isolation, and measuring what it cost.
//!
//! Every run gets a [`Sandbox`]: a fresh private directory, and an
//! environment built from an allowlist rather than inherited wholesale.
//! `HOME`, `TMPDIR` and the XDG variables point inside that directory, so a
//! run reads and writes machine-local state that belongs to it alone. `PATH`
//! is the one variable carried over from the parent, because a child that
//! cannot find a program cannot run at all.
//!
//! # Partitioning is this module's isolation; exclusion is the other door
//!
//! What a sandbox gives a run is state of its own. The other isolation family
//! is exclusion — one machine-wide thing used by one holder at a time — and
//! that is [`crate::isolation`]'s. The two meet in exactly one place, and it is
//! [`Sandbox::environment`]: the resolved lease root is forwarded to the child
//! rather than repointed inside the sandbox, because a child that computed a
//! lease root of its own would queue against nobody and exclude nothing. A
//! private lease root is isolation in name and an unheld watcher in fact, so
//! the one variable a run does *not* get a private copy of is that one.
//!
//! Four properties are the reason this module exists rather than a bare
//! [`std::process::Command`] at each call site:
//!
//! - **Nothing is shared between concurrent runs.** Each sandbox is named for
//!   the process that made it and a counter, so two runs in the same suite
//!   never meet in a cache directory. The lease root above is the stated
//!   exception, and it is shared on purpose.
//! - **A build artifact is never executed where it lies.** [`Sandbox::install_binary`]
//!   copies it to a private path first. A binary that a concurrent build may
//!   rewrite is not a stable thing to exec, and the failure is a spurious one
//!   in whichever suite happens to be running.
//! - **The direct child's peak resident set is measured**, by reaping it through
//!   `wait4` and reading the kernel's accounting rather than sampling. This
//!   measurement includes descendants that the child waited on. It does not
//!   include a grandchild that the child left running.
//! - **Every run owns one process group.** The direct child leads it. The
//!   harness sends `SIGKILL` to the complete group on every end, reaps the
//!   leader, and returns an [`Outcome`] only after the group no longer exists.
//!   A workload that calls `setsid` or changes its process group has left this
//!   contract.
//! - **The measured wait and cleanup are bounded.** Every run carries a
//!   workload deadline. Cleanup has a separate private five-second deadline.
//!   A run that reaches its workload deadline reports [`RunStatus::TimedOut`]
//!   only after the complete process group is gone.
//!
//! The sandbox is removed when it drops, so a suite leaves nothing behind;
//! what a run said is in its [`Outcome`], not in a file to go looking for.
//!
//! **This module is unix-only.** The wait-and-account call is `wait4` and the
//! status decoding is the unix one; there is no Windows path and none is
//! pretended.
//!
//! # Registered development workloads
//!
//! The non-shipping `norn-process supervise` command owns commands started by
//! development scripts and agents. Its resident launcher leads the workload's
//! process group, waits behind a private release pipe, and stays alive after the
//! direct workload exits to pin the registered identity. The supervisor writes
//! and syncs one registry record before it releases the workload. SIGINT and
//! SIGTERM first close the owned group through the same mechanism as [`Run`],
//! then terminate the supervisor with the original signal.
//!
//! The same command scans the durable registry and recovers groups after
//! supervisor loss. A matching supervisor identity remains authoritative even
//! after its deadline. Recovery sends a destructive signal only after it
//! revalidates the registered group identity. It syncs an append-only audit
//! event before it removes the registry record.

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use crate::scratch::Scratch;

mod group;
mod identity;
mod recovery;
mod registry;
mod signals;
mod supervisor;

pub use recovery::{AuditReport, RecoveryReport, reap, report, scan};
pub use supervisor::{LaunchRequest, SuperviseRequest, launch, supervise};

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

/// A private directory tree that one run owns and that is removed with it.
///
/// The naming and the lifecycle are [`crate::scratch::Scratch`]'s; what this
/// adds is the arrangement a child run reads — one directory per isolated
/// environment variable.
pub struct Sandbox {
    tree: Scratch,
    runs: AtomicU64,
}

impl Sandbox {
    /// A fresh sandbox under `base`, named for `label`.
    ///
    /// The name carries the process id and a counter, so sandboxes made by
    /// concurrent tests — in one process or several — never collide.
    #[allow(clippy::disallowed_methods)] // Harness scaffolding: the sandbox is this crate's to build.
    pub fn new(base: &Path, label: &str) -> io::Result<Sandbox> {
        let tree = Scratch::under(base, label)?;
        for directory in [
            "home", "tmp", "config", "data", "cache", "state", "bin", "work",
        ] {
            std::fs::create_dir_all(tree.join(directory))?;
        }
        Ok(Sandbox {
            tree,
            runs: AtomicU64::new(0),
        })
    }

    pub fn root(&self) -> &Path {
        self.tree.root()
    }

    /// The directory a run starts in.
    pub fn work_dir(&self) -> PathBuf {
        self.tree.join("work")
    }

    /// The environment a run is given: the allowlist, pointed inside this
    /// sandbox, plus the parent's `PATH` and this run's resolved lease root.
    ///
    /// **The lease root is resolved here and passed as a value, not left to be
    /// derived again in the child.** [`crate::isolation::root`] falls back to
    /// the system temporary directory when nothing names one, and `TMPDIR`
    /// above points inside this sandbox — so a child left to derive it would
    /// compute a lease root nobody else has, take every lease uncontended, and
    /// report nothing about having done so. Forwarding the parent's resolved
    /// root is what makes a child's real watcher queue behind its parent's.
    pub fn environment(&self) -> BTreeMap<String, OsString> {
        let mut environment: BTreeMap<String, OsString> = BTreeMap::new();
        environment.insert("HOME".to_string(), self.tree.join("home").into());
        environment.insert("TMPDIR".to_string(), self.tree.join("tmp").into());
        environment.insert("XDG_CACHE_HOME".to_string(), self.tree.join("cache").into());
        environment.insert(
            "XDG_CONFIG_HOME".to_string(),
            self.tree.join("config").into(),
        );
        environment.insert("XDG_DATA_HOME".to_string(), self.tree.join("data").into());
        environment.insert("XDG_STATE_HOME".to_string(), self.tree.join("state").into());
        environment.insert(
            crate::isolation::ISOLATION_ROOT.to_string(),
            crate::isolation::root().into(),
        );
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
        let installed = self.tree.join("bin").join(name);
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
/// bound the harness removes the complete owned process group and reports
/// [`RunStatus::TimedOut`]. A run with more work than this to do names its own
/// bound with [`Run::deadline`]. Every run has one: this is what a run that
/// says nothing gets, and there is no constructor that leaves it unset.
pub const DEFAULT_WAIT_DEADLINE: Duration = Duration::from_secs(60);

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
        let out_path = self.sandbox.tree.join(format!("run-{serial}.stdout"));
        let err_path = self.sandbox.tree.join(format!("run-{serial}.stderr"));

        // Output goes to files rather than pipes: a pipe nobody is reading
        // fills and stops the child, and the harness has to wait on the child
        // to measure it.
        let mut command = Command::new(&self.program);
        command
            .args(&self.args)
            .current_dir(self.sandbox.work_dir())
            .env_clear()
            .envs(&self.environment)
            .stdin(Stdio::null())
            .stdout(Stdio::from(std::fs::File::create(&out_path)?))
            .stderr(Stdio::from(std::fs::File::create(&err_path)?));

        let ended = group::OwnedProcessGroup::spawn(&mut command)?.wait(self.deadline)?;
        let status = decoded(ended.pid, ended.status)?;
        let status = match status {
            RunStatus::Signaled(libc::SIGKILL) if ended.timed_out => RunStatus::TimedOut {
                after: self.deadline,
            },
            status => status,
        };
        let max_rss = ended.usage.ru_maxrss.max(0) as u64;
        let peak_rss_bytes = if MAX_RSS_IN_BYTES {
            max_rss
        } else {
            max_rss * 1024
        };
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
    /// The harness ended the run: the leader was still going at the deadline
    /// it names, and the complete owned process group was removed there.
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
    /// The direct child's peak resident set, in bytes, as the kernel accounted
    /// it: the spawned process and every descendant it waited on. For a run
    /// that reports [`RunStatus::TimedOut`], this is the peak at cleanup.
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

/// How many descriptors this process holds open.
///
/// **A count of this process, taken from the kernel's own listing**: Linux
/// publishes it at `/proc/self/fd` and macOS at `/dev/fd`, and both list one
/// entry per open descriptor. Reading the listing holds a descriptor for the
/// iterator itself and that descriptor is in the listing, so it is subtracted
/// — the answer is what was open before the question was asked.
///
/// A subject whose descriptor cost is the question runs in a child of its own,
/// because a count taken in a test process includes the harness and every case
/// running beside it — and, worse, moves while it is being taken, since a case
/// on another thread opening a file changes the answer. That is also why this
/// function carries no assertion of its own here: a case stating what it counts
/// would be stating it about a process where the number is nobody's to hold
/// still. What holds it is a caller running alone in a process it spawned.
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[allow(clippy::disallowed_methods)] // Harness scaffolding: reads this process's own descriptor listing.
pub fn open_fd_count() -> io::Result<usize> {
    #[cfg(target_os = "linux")]
    const FD_DIRECTORY: &str = "/proc/self/fd";
    #[cfg(target_os = "macos")]
    const FD_DIRECTORY: &str = "/dev/fd";

    let listed = std::fs::read_dir(FD_DIRECTORY)?.count();
    listed.checked_sub(1).ok_or_else(|| {
        io::Error::other(format!(
            "{FD_DIRECTORY} listed nothing, so the iterator's own descriptor is not in its listing"
        ))
    })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::time::Instant;

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

    struct Descendant(libc::pid_t);

    impl Descendant {
        fn from(outcome: &Outcome) -> Self {
            Descendant(
                outcome
                    .stdout_text()
                    .trim()
                    .parse()
                    .expect("the background process pid"),
            )
        }

        fn is_alive(&self) -> bool {
            if unsafe { libc::kill(self.0, 0) } == 0 {
                return true;
            }
            io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
        }
    }

    fn assert_descendant_gone(outcome: &Outcome) {
        let descendant = Descendant::from(outcome);
        assert!(
            !descendant.is_alive(),
            "background pid {} survived its run",
            descendant.0
        );
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

    /// The allowlist is the contract, so the environment is exactly it, `PATH`,
    /// and the lease root — a variable added to one and not the other would
    /// make the constant a description of nothing.
    #[test]
    fn a_runs_environment_is_the_allowlist_and_path_and_the_lease_root() {
        let sandbox = sandbox("allowlist");
        let environment = sandbox.environment();
        let names: BTreeSet<&str> = environment.keys().map(String::as_str).collect();
        let mut expected: BTreeSet<&str> = ISOLATED_VARIABLES.iter().copied().collect();
        expected.insert(crate::isolation::ISOLATION_ROOT);
        if std::env::var_os("PATH").is_some() {
            expected.insert("PATH");
        }
        assert_eq!(names, expected);
    }

    /// **The bar on the one thing a run does not get a private copy of.** A
    /// child is handed the parent's resolved lease root, and a root inside the
    /// sandbox is exactly what that forbids.
    ///
    /// The forbidden shape is the silent one: `TMPDIR` points inside the
    /// sandbox and the lease root is derived from it when nothing names one, so
    /// a child left to derive its own computes a path nobody else has. Every
    /// lease it takes is then uncontended, its real watcher runs beside every
    /// sibling's, and nothing anywhere reports that the exclusion stopped
    /// excluding.
    #[test]
    fn a_run_is_handed_the_parents_lease_root_rather_than_one_inside_its_sandbox() {
        let sandbox = sandbox("lease-root");
        let forwarded = sandbox
            .environment()
            .get(crate::isolation::ISOLATION_ROOT)
            .cloned()
            .expect("the lease root is forwarded");
        assert_eq!(PathBuf::from(&forwarded), crate::isolation::root());
        assert!(
            !Path::new(&forwarded).starts_with(sandbox.root()),
            "the lease root {forwarded:?} is inside the sandbox, so a child queues against nobody"
        );
    }

    /// The child derives the same lease root the parent resolved, which is what
    /// the forwarding is for: the derivation runs in a process whose `TMPDIR`
    /// is the sandbox's, and it still answers with the parent's root.
    #[test]
    fn a_child_of_a_sandbox_resolves_the_parents_lease_root() {
        let sandbox = sandbox("lease-root-child");
        let outcome = shell(
            &sandbox,
            &format!("echo \"${}\"", crate::isolation::ISOLATION_ROOT),
        );
        outcome.assert_success();
        assert_eq!(
            PathBuf::from(outcome.stdout_text().trim()),
            crate::isolation::root(),
            "a child computed a lease root of its own"
        );
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
    fn a_completed_run_leaves_no_background_grandchild_in_its_owned_group() {
        let sandbox = sandbox("background-grandchild");
        let outcome = shell(&sandbox, "sleep 5 & echo $!");

        outcome.assert_success();
        assert_descendant_gone(&outcome);
    }

    #[test]
    fn a_failed_run_leaves_no_background_grandchild_in_its_owned_group() {
        let sandbox = sandbox("failed-background-grandchild");
        let outcome = shell(&sandbox, "sleep 5 & echo $!; exit 7");

        assert_eq!(outcome.status, RunStatus::Exited(7));
        assert_descendant_gone(&outcome);
    }

    #[test]
    fn a_timed_out_run_leaves_no_child_in_its_owned_group() {
        let sandbox = sandbox("timed-out-child");
        let outcome = Run::new(&sandbox, "/bin/sh")
            .args(["-c", "sleep 5 & echo $!; wait"])
            .deadline(Duration::from_millis(100))
            .wait()
            .expect("running a shell");

        assert_eq!(
            outcome.status,
            RunStatus::TimedOut {
                after: Duration::from_millis(100)
            }
        );
        assert_descendant_gone(&outcome);
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
