//! Spawning a process under harness isolation, and measuring what it cost.
//!
//! Every run gets a [`Sandbox`]: a fresh private directory, and an
//! environment built from an allowlist rather than inherited wholesale.
//! `HOME`, `TMPDIR` and the XDG variables point inside that directory, so a
//! run reads and writes machine-local state that belongs to it alone. `PATH`
//! is the one variable carried over from the parent, because a child that
//! cannot find a program cannot run at all.
//!
//! Three properties are the reason this module exists rather than a bare
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
//!
//! The sandbox is removed when it drops, so a suite leaves nothing behind;
//! what a run said is in its [`Outcome`], not in a file to go looking for.

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

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

/// One process to run under a sandbox.
pub struct Run<'a> {
    sandbox: &'a Sandbox,
    program: PathBuf,
    args: Vec<OsString>,
    environment: BTreeMap<String, OsString>,
}

impl<'a> Run<'a> {
    /// A run of `program`, isolated by `sandbox`.
    pub fn new(sandbox: &'a Sandbox, program: impl Into<PathBuf>) -> Self {
        Run {
            sandbox,
            program: program.into(),
            args: Vec::new(),
            environment: sandbox.environment(),
        }
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

        let (status, peak_rss_bytes) = wait_for(child)?;
        Ok(Outcome {
            status,
            peak_rss_bytes,
            stdout: std::fs::read(&out_path)?,
            stderr: std::fs::read(&err_path)?,
        })
    }
}

/// How a run ended.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RunStatus {
    Exited(i32),
    Signaled(i32),
}

/// What a run did, and what it cost.
pub struct Outcome {
    pub status: RunStatus,
    /// The child's peak resident set, in bytes, as the kernel accounted it.
    pub peak_rss_bytes: u64,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
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

/// Wait on `child` through `wait4`, so its exit and its resource accounting
/// are read in one call.
///
/// The child is reaped here and nowhere else: nothing calls `wait` on it
/// afterwards, because the process is already gone and its identifier is free
/// for the kernel to hand to somebody else.
fn wait_for(child: std::process::Child) -> io::Result<(RunStatus, u64)> {
    let pid = child.id() as libc::pid_t;
    let mut status: libc::c_int = 0;
    let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
    loop {
        // SAFETY: `pid` is this process's own child, and both out-parameters
        // are live for the call.
        let waited = unsafe { libc::wait4(pid, &mut status, 0, &mut usage) };
        if waited == pid {
            break;
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
    let outcome = if libc::WIFEXITED(status) {
        RunStatus::Exited(libc::WEXITSTATUS(status))
    } else {
        RunStatus::Signaled(libc::WTERMSIG(status))
    };
    let max_rss = usage.ru_maxrss.max(0) as u64;
    Ok((
        outcome,
        if MAX_RSS_IN_BYTES {
            max_rss
        } else {
            max_rss * 1024
        },
    ))
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
