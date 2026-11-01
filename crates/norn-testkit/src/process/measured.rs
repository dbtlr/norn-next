//! The measuring launcher: a fresh address space between the process that
//! starts a run and the child the run measures.
//!
//! # Why a run does not spawn its child directly
//!
//! A peak read through `wait4` is the kernel's per-process high-water mark,
//! and on Linux that mark is not only the child's own. When a process calls
//! `exec`, the kernel folds the high-water resident set of the address space
//! it is leaving into the mark it keeps for the process, and that mark
//! survives the `exec`. Rust's standard library starts a child through
//! `posix_spawn` whenever it can, and glibc implements that with
//! `CLONE_VM | CLONE_VFORK`: the address space the child leaves at its `exec`
//! is the spawning process's own. A child spawned directly from a test process
//! therefore reports at least that test process's peak to date, including
//! memory it has since released. Through the `fork` path the floor is the
//! spawning process's resident set at the fork instead, which is no better
//! for a test process holding its fixtures.
//!
//! That floor made two children of one test process read the same: after a
//! test process peaked at 264 MB, an idle shell and one holding a 64 MB
//! string both reported 263.9 MB.
//!
//! So [`super::Run`] starts a fresh image of the binary it is running in —
//! the measuring launcher — and the measuring launcher spawns the child. The
//! measuring launcher's own reading carries the parent's floor, and nothing
//! reads it. The child is spawned from the measuring launcher's address
//! space, which is the spawning binary's freshly loaded image, so the child's
//! floor is that image's resident size. It varies with the binary — 3.4 MiB
//! for this crate's unit-test binary and 6.9 MiB for the debug norn-host
//! `memory` test binary, measured — and is independent of how high the
//! spawning process has peaked. The measuring launcher reaps the child with
//! `wait4` and writes the child's wait status and peak to a report file, and
//! the run reads its outcome from there.
//!
//! macOS's `posix_spawn` is a system call that gives the child an address
//! space of its own, so the floor is not expected there; the measuring
//! launcher runs there too, so one path measures on both platforms.
//!
//! # How a binary becomes the measuring launcher
//!
//! The binary that starts a run is whatever links this crate — usually a
//! test binary, whose `main` belongs to the test runner. The measuring
//! launcher is therefore an initializer the loader runs before `main`:
//! [`DIVERT`] is placed in the platform's initializer section, checks for
//! [`REPORT_VARIABLE`] in the environment it was started with, and returns at
//! once when it is absent. When it is present, the process is a measuring
//! launcher and exits without returning to `main`. A run starts it with
//! [`LAUNCHER_ARGUMENT`] first and the program after it; the measuring
//! launcher spawns the program and its arguments as the child, with the
//! variable removed from the child's environment. That path calls only libc
//! and the system allocator, and touches no state of the standard library's
//! runtime, which has not been set up yet.
//!
//! A binary started with the variable set exits [`NOT_STARTED`], running
//! nothing and writing no report, whenever its first argument is not
//! [`LAUNCHER_ARGUMENT`] or no program follows it. A test binary that
//! inherited a stray copy of the variable therefore fails whatever filter it
//! was given, even one that names a program on `PATH`, rather than exiting 0
//! having run no tests. Started by a run, it also exits [`NOT_STARTED`] when
//! the program cannot be spawned, having reported why.
//!
//! # What the child sees
//!
//! The measuring launcher leads the run's process group and the child joins
//! it, so the group-ownership contract — cleanup of the complete group, the
//! bounded wait — is unchanged. The measuring launcher passes no descriptor
//! of its own to the child: its report is a file it creates only after the
//! child has ended.
//!
//! A signal the child sends to its own group reaches the measuring launcher
//! too. So that such a signal ends what it would have ended had the child been
//! spawned directly — the child, unless it handles or ignores it — the
//! measuring launcher ignores [`SHIELDED_SIGNALS`] and restores, for the
//! child, the default disposition of each one it found at the default, and an
//! empty signal mask. `SIGKILL` cannot be ignored, which is what keeps the
//! harness's deadline kill of the complete group working.
//!
//! # Declared limitations
//!
//! - **musl targets are unsupported.** musl calls `.init_array` functions
//!   without `argc`, `argv` and `envp`, so [`DIVERT`] would read registers
//!   that hold no environment.
//! - **A script without a shebang line no longer runs through `/bin/sh`.**
//!   The child is started by libc `posix_spawnp`, which reports `ENOEXEC` for
//!   such a file, where the standard library's spawn on glibc fell back to
//!   running it through the shell.

use std::ffi::{CStr, c_char, c_int, c_long};
use std::io;
use std::path::{Path, PathBuf};

/// The variable that makes a binary linking this crate start as the
/// measuring launcher. Its value is the path the measuring launcher writes
/// its report to.
pub(super) const REPORT_VARIABLE: &str = "NORN_TESTKIT_MEASURED_REPORT";

/// The same name as a prefix of an environment entry, for the measuring
/// launcher, which reads the raw environment rather than the standard
/// library's view.
const REPORT_ENTRY_PREFIX: &[u8] = b"NORN_TESTKIT_MEASURED_REPORT=";

/// The initializer the loader runs before `main`. glibc and dyld both call
/// one with `argc`, `argv` and `envp`. The Mach-O section, with its
/// `mod_init_funcs` type, is the one the `ctor` crate uses.
#[used]
#[cfg_attr(
    target_vendor = "apple",
    unsafe(link_section = "__DATA,__mod_init_func,mod_init_funcs")
)]
#[cfg_attr(not(target_vendor = "apple"), unsafe(link_section = ".init_array"))]
static DIVERT: extern "C" fn(c_int, *const *const c_char, *const *const c_char) = divert;

/// The first argument of every measuring launcher a run starts, ahead of the
/// program. A binary started with [`REPORT_VARIABLE`] set and any other first
/// argument was not started by a run, and does nothing.
pub(super) const LAUNCHER_ARGUMENT: &str = "--norn-testkit-measured-launcher";

/// The measuring launcher's exit code when it reported [`Report::Ended`].
pub(super) const REPORTED: c_int = 0;

/// The measuring launcher's exit code when it reported
/// [`Report::NotStarted`], or when it was not started by a run at all: the
/// shell's code for a command that was not found, and never 0, so a binary
/// that became the measuring launcher by accident fails loudly.
pub(super) const NOT_STARTED: c_int = 127;

/// The measuring launcher's exit code when it could not produce a report.
/// The run reads the absence of the report, not this code; the code is for a
/// person reading the status by hand.
const LAUNCHER_FAILED: c_int = 125;

/// The signals a child commonly sends to its own process group, which the
/// measuring launcher ignores so that only the child answers them.
/// `SIGCHLD` is absent on purpose — ignoring it would reap the child before
/// `wait4` could — and so are the signals that report a fault of the
/// receiving process.
const SHIELDED_SIGNALS: [c_int; 11] = [
    libc::SIGHUP,
    libc::SIGINT,
    libc::SIGQUIT,
    libc::SIGTERM,
    libc::SIGPIPE,
    libc::SIGUSR1,
    libc::SIGUSR2,
    libc::SIGALRM,
    libc::SIGTSTP,
    libc::SIGTTIN,
    libc::SIGTTOU,
];

/// What the measuring launcher learned about the child.
#[derive(Debug, Eq, PartialEq)]
pub(super) enum Report {
    /// The child ended with this raw wait status and peak, in the platform's
    /// `ru_maxrss` unit.
    Ended { status: c_int, max_rss: c_long },
    /// The child could not be started; the value is the `errno`.
    NotStarted(c_int),
}

/// The executable a run starts as its measuring launcher: the image this
/// process is running, which links [`DIVERT`].
///
/// On Linux that is `/proc/self/exe`, the very file this process executes,
/// so a concurrent build replacing the artifact at its path does not change
/// which image starts. macOS has no such link, so it is the path the loader
/// reports, made absolute once: the loader may report it relative to the
/// directory this process started in, and a run starts its child in the
/// sandbox's.
#[allow(clippy::disallowed_methods)] // Harness scaffolding: resolving this process's own executable.
pub(super) fn image() -> io::Result<PathBuf> {
    // Naming the initializer here is what keeps it in every binary that
    // starts a run: a linker may leave out an unreferenced part of a library.
    // No test can tell this from a linker that happened to keep it anyway.
    std::hint::black_box(&DIVERT);
    if cfg!(target_os = "linux") {
        return Ok(PathBuf::from("/proc/self/exe"));
    }
    static CANONICAL: std::sync::OnceLock<Result<PathBuf, (io::ErrorKind, String)>> =
        std::sync::OnceLock::new();
    CANONICAL
        .get_or_init(|| {
            std::env::current_exe()
                .and_then(|path| path.canonicalize())
                .map_err(|error| (error.kind(), error.to_string()))
        })
        .clone()
        .map_err(|(kind, message)| {
            io::Error::new(kind, format!("locating this executable: {message}"))
        })
}

/// Read the measuring launcher's report at `path`, or `None` when it wrote
/// none — it was killed before it published one, or failed before it could.
/// A report is published whole or not at all, so one that is present and
/// unreadable was not written by a measuring launcher.
#[allow(clippy::disallowed_methods)] // Harness scaffolding: the run's own report file.
pub(super) fn read_report(path: &Path) -> io::Result<Option<Report>> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    parse_report(&text).map(Some).ok_or_else(|| {
        io::Error::other(format!(
            "the measuring launcher wrote an unreadable report to {}: {text:?}",
            path.display()
        ))
    })
}

fn parse_report(text: &str) -> Option<Report> {
    let mut words = text.split_whitespace();
    let report = match words.next()? {
        "ended" => Report::Ended {
            status: words.next()?.parse().ok()?,
            max_rss: words.next()?.parse().ok()?,
        },
        "not-started" => Report::NotStarted(words.next()?.parse().ok()?),
        _ => return None,
    };
    words.next().is_none().then_some(report)
}

fn render_report(report: &Report) -> String {
    match report {
        Report::Ended { status, max_rss } => format!("ended {status} {max_rss}\n"),
        Report::NotStarted(errno) => format!("not-started {errno}\n"),
    }
}

extern "C" fn divert(argc: c_int, argv: *const *const c_char, envp: *const *const c_char) {
    // SAFETY: the loader passes the process's own null-terminated
    // environment, which is valid for the life of the process.
    let Some(report) = (unsafe { report_path(envp) }) else {
        return;
    };
    // SAFETY: as above, and `argv` holds `argc` valid strings followed by a
    // null.
    let code = unsafe { launch(argc, argv, envp, report) };
    // SAFETY: ending the process without returning to the loader is the
    // point; nothing of `main`'s runtime exists to unwind.
    unsafe { libc::_exit(code) }
}

/// The report path, when `envp` names one.
///
/// # Safety
///
/// `envp` is null or a null-terminated array of null-terminated strings.
unsafe fn report_path(envp: *const *const c_char) -> Option<&'static CStr> {
    if envp.is_null() {
        return None;
    }
    let mut entry = envp;
    loop {
        // SAFETY: the array is null-terminated, so every index up to the
        // terminator is in bounds.
        let pointer = unsafe { *entry };
        if pointer.is_null() {
            return None;
        }
        // SAFETY: each entry is a null-terminated string that lives as long
        // as the process.
        let text: &'static CStr = unsafe { CStr::from_ptr(pointer) };
        if let Some(path) = text.to_bytes_with_nul().strip_prefix(REPORT_ENTRY_PREFIX) {
            return CStr::from_bytes_with_nul(path).ok();
        }
        // SAFETY: not yet at the terminator, so the next index is in bounds.
        entry = unsafe { entry.add(1) };
    }
}

/// Spawn `argv[2..]` — what follows [`LAUNCHER_ARGUMENT`] — with the environment minus the report variable, reap
/// it, publish the report, and return the measuring launcher's exit code.
///
/// # Safety
///
/// `argv` holds `argc` valid strings followed by a null, and `envp` is a
/// null-terminated array of null-terminated strings.
unsafe fn launch(
    argc: c_int,
    argv: *const *const c_char,
    envp: *const *const c_char,
    report: &CStr,
) -> c_int {
    // SAFETY: `argv` holds `argc` valid strings, so index 1 is in bounds
    // when `argc >= 2`.
    if argc < 3
        || unsafe { CStr::from_ptr(*argv.add(1)) }.to_bytes() != LAUNCHER_ARGUMENT.as_bytes()
    {
        // Not started by a run: a stray copy of the variable. Write nothing
        // to the path it names and run nothing.
        return NOT_STARTED;
    }
    let mut environment = Vec::new();
    let mut entry = envp;
    loop {
        // SAFETY: the caller's contract; see `report_path`.
        let pointer = unsafe { *entry };
        if pointer.is_null() {
            break;
        }
        // SAFETY: as above.
        if !unsafe { CStr::from_ptr(pointer) }
            .to_bytes()
            .starts_with(REPORT_ENTRY_PREFIX)
        {
            environment.push(pointer.cast_mut());
        }
        // SAFETY: as above.
        entry = unsafe { entry.add(1) };
    }
    environment.push(std::ptr::null_mut());

    let Some(attributes) = SpawnAttributes::shielding() else {
        return LAUNCHER_FAILED;
    };
    // SAFETY: `argc >= 3`, so `argv[2]` is the program and `argv + 2` is a
    // null-terminated argument vector beginning with it.
    let (program, arguments) = unsafe { (*argv.add(2), argv.add(2)) };
    let mut pid: libc::pid_t = 0;
    // SAFETY: every pointer is valid and null-terminated as `posix_spawnp`
    // requires. Null file actions ask for the defaults, so the child inherits
    // this process's descriptors, directory and group, as it would have
    // inherited the run's; the attributes give back the signal state this
    // process had before it shielded itself.
    let spawned = unsafe {
        libc::posix_spawnp(
            &mut pid,
            program,
            std::ptr::null(),
            attributes.as_ptr(),
            arguments.cast(),
            environment.as_ptr().cast(),
        )
    };
    drop(attributes);
    if spawned != 0 {
        return finish(report, &Report::NotStarted(spawned));
    }

    let mut status: c_int = 0;
    // SAFETY: zero is a valid `rusage`, which `wait4` overwrites.
    let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
    loop {
        // SAFETY: `pid` is this process's unreaped child and both out-pointers
        // are valid.
        if unsafe { libc::wait4(pid, &mut status, 0, &mut usage) } == pid {
            break;
        }
        // EINTR is retried defensively; with no handler installed no test can provoke it.
        if io::Error::last_os_error().raw_os_error() != Some(libc::EINTR) {
            return LAUNCHER_FAILED;
        }
    }
    finish(
        report,
        &Report::Ended {
            status,
            max_rss: usage.ru_maxrss,
        },
    )
}

/// The `posix_spawn` attributes that undo the measuring launcher's signal
/// shield for the child.
struct SpawnAttributes(libc::posix_spawnattr_t);

impl SpawnAttributes {
    /// Ignore [`SHIELDED_SIGNALS`] in this process, and describe a child that
    /// gets back the default disposition of each one this process had at the
    /// default — one this process inherited as ignored stays ignored, as it
    /// would have for a child spawned directly — and an empty signal mask.
    fn shielding() -> Option<SpawnAttributes> {
        // SAFETY: zero is a valid out-value for `sigemptyset`.
        let mut defaults: libc::sigset_t = unsafe { std::mem::zeroed() };
        let mut empty: libc::sigset_t = unsafe { std::mem::zeroed() };
        // SAFETY: both sets are valid and this process's own.
        if unsafe { libc::sigemptyset(&mut defaults) } != 0
            || unsafe { libc::sigemptyset(&mut empty) } != 0
        {
            return None;
        }
        for signal in SHIELDED_SIGNALS {
            // SAFETY: ignoring a catchable signal installs no code to run.
            let previous = unsafe { libc::signal(signal, libc::SIG_IGN) };
            if previous == libc::SIG_ERR {
                return None;
            }
            // SAFETY: `defaults` is a valid set and `signal` a valid number.
            if previous == libc::SIG_DFL && unsafe { libc::sigaddset(&mut defaults, signal) } != 0 {
                return None;
            }
        }

        // SAFETY: zero is a valid out-value for `posix_spawnattr_init`.
        let mut raw: libc::posix_spawnattr_t = unsafe { std::mem::zeroed() };
        // SAFETY: `raw` is valid storage for an attributes object.
        if unsafe { libc::posix_spawnattr_init(&mut raw) } != 0 {
            return None;
        }
        let mut attributes = SpawnAttributes(raw);
        let flags = (libc::POSIX_SPAWN_SETSIGDEF | libc::POSIX_SPAWN_SETSIGMASK) as libc::c_short;
        // SAFETY: the attributes object is initialized and both sets are valid.
        let configured = unsafe {
            libc::posix_spawnattr_setsigdefault(&mut attributes.0, &defaults) == 0
                && libc::posix_spawnattr_setsigmask(&mut attributes.0, &empty) == 0
                && libc::posix_spawnattr_setflags(&mut attributes.0, flags) == 0
        };
        configured.then_some(attributes)
    }

    fn as_ptr(&self) -> *const libc::posix_spawnattr_t {
        &raw const self.0
    }
}

impl Drop for SpawnAttributes {
    fn drop(&mut self) {
        // SAFETY: the object was initialized by `shielding` and is destroyed
        // once.
        unsafe { libc::posix_spawnattr_destroy(&mut self.0) };
    }
}

/// Publish `report` at `path` and return the exit code that goes with it.
///
/// **A report is complete or absent at `path`, never partial.** It is
/// written to a sibling with [`PARTIAL_SUFFIX`] and linked into place, so a
/// deadline kill landing mid-write leaves no file at `path` and the run reads
/// a killed measuring launcher rather than an unreadable report. Linking
/// fails when `path` already exists, so a report path some other process
/// wrote is never overwritten and the run sees its contents disagree with
/// this exit code.
fn finish(path: &CStr, report: &Report) -> c_int {
    let mut partial = path.to_bytes().to_vec();
    partial.extend_from_slice(PARTIAL_SUFFIX);
    partial.push(0);
    let Ok(partial) = CStr::from_bytes_with_nul(&partial) else {
        return LAUNCHER_FAILED;
    };
    let written = write_new(partial, render_report(report).as_bytes());
    // SAFETY: both paths are null-terminated.
    let published = written && unsafe { libc::link(partial.as_ptr(), path.as_ptr()) } == 0;
    // SAFETY: `partial` is null-terminated; a partial file left behind by a
    // failed write is removed with it.
    unsafe { libc::unlink(partial.as_ptr()) };
    if !published {
        return LAUNCHER_FAILED;
    }
    match report {
        Report::Ended { .. } => REPORTED,
        Report::NotStarted(_) => NOT_STARTED,
    }
}

/// The suffix of the file a report is written to before it is published.
pub(super) const PARTIAL_SUFFIX: &[u8] = b".partial";

/// Create `path`, which must not exist, holding exactly `bytes`.
fn write_new(path: &CStr, bytes: &[u8]) -> bool {
    // SAFETY: `path` is null-terminated; the descriptor is this function's.
    let fd = unsafe {
        libc::open(
            path.as_ptr(),
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_CLOEXEC,
            0o600 as libc::c_uint,
        )
    };
    if fd < 0 {
        return false;
    }
    let mut written = 0;
    while written < bytes.len() {
        // SAFETY: the range is inside `bytes`, and `fd` is open for writing.
        let count = unsafe {
            libc::write(
                fd,
                bytes.as_ptr().add(written).cast(),
                bytes.len() - written,
            )
        };
        if count > 0 {
            written += count as usize;
        } else if count < 0 && io::Error::last_os_error().raw_os_error() == Some(libc::EINTR) {
            continue;
        } else {
            break;
        }
    }
    // SAFETY: `fd` is this function's.
    let closed = unsafe { libc::close(fd) } == 0;
    closed && written == bytes.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_report_reads_back_as_written() {
        for report in [
            Report::Ended {
                status: 0x0900,
                max_rss: 123_456,
            },
            Report::NotStarted(libc::ENOENT),
        ] {
            assert_eq!(parse_report(&render_report(&report)), Some(report));
        }
    }

    fn report_path_in(directory: &Path) -> (PathBuf, std::ffi::CString) {
        use std::os::unix::ffi::OsStrExt;
        let path = directory.join("run.usage");
        let raw = std::ffi::CString::new(path.as_os_str().as_bytes()).expect("a path");
        (path, raw)
    }

    fn partial_of(path: &Path) -> PathBuf {
        use std::os::unix::ffi::OsStrExt;
        let mut partial = path.as_os_str().to_owned();
        partial.push(std::ffi::OsStr::from_bytes(PARTIAL_SUFFIX));
        partial.into()
    }

    /// A published report is whole at its path, and the file it was
    /// written to first is gone, so nothing in the directory is ever a
    /// report half-written.
    #[test]
    #[allow(clippy::disallowed_methods)] // Harness scaffolding: inspecting the report's directory.
    fn a_published_report_is_whole_and_leaves_no_partial_behind() {
        let scratch = crate::scratch::Scratch::under(&std::env::temp_dir(), "report-publish")
            .expect("a scratch directory");
        let (path, raw) = report_path_in(scratch.root());
        let report = Report::Ended {
            status: 0,
            max_rss: 4_096,
        };
        assert_eq!(finish(&raw, &report), REPORTED);
        assert_eq!(read_report(&path).expect("the report"), Some(report));
        assert!(!partial_of(&path).exists(), "the partial report was left");
    }

    /// **Publishing never replaces a report that is already there.** A
    /// measuring launcher that finds its path taken fails and leaves the
    /// existing file, and no partial, behind.
    #[test]
    #[allow(clippy::disallowed_methods)] // Harness scaffolding: planting a stale report and inspecting its directory.
    fn publishing_onto_an_existing_report_fails_and_leaves_it() {
        let scratch = crate::scratch::Scratch::under(&std::env::temp_dir(), "report-taken")
            .expect("a scratch directory");
        let (path, raw) = report_path_in(scratch.root());
        std::fs::write(&path, "ended 0 1\n").expect("planting a report");
        assert_eq!(
            finish(&raw, &Report::NotStarted(libc::ENOENT)),
            LAUNCHER_FAILED
        );
        assert_eq!(
            std::fs::read_to_string(&path).expect("the planted report"),
            "ended 0 1\n"
        );
        assert!(!partial_of(&path).exists(), "the partial report was left");
    }

    #[test]
    fn a_partial_or_foreign_report_is_unreadable() {
        for text in ["", "ended 0", "ended 0 12 extra", "started 1", "ended x 1"] {
            assert_eq!(parse_report(text), None, "{text:?}");
        }
    }
}
