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
//! the launcher — and the launcher spawns the child. The launcher's own
//! reading carries the floor, and nothing reads it. The child is spawned from
//! the launcher's address space, which is a few megabytes of freshly loaded
//! image, so the child's floor is that. The launcher reaps the child with
//! `wait4` and writes the child's wait status and peak to a report file, and
//! the run reads its outcome from there.
//!
//! macOS's `posix_spawn` is a system call that gives the child an address
//! space of its own, so the floor is not expected there; the launcher runs
//! there too, so one path measures on both platforms.
//!
//! # How a binary becomes the launcher
//!
//! The binary that starts a run is whatever links this crate — usually a
//! test binary, whose `main` belongs to the test runner. The launcher is
//! therefore an initializer the loader runs before `main`: [`DIVERT`] is
//! placed in the platform's initializer section, checks for
//! [`REPORT_VARIABLE`] in the environment it was started with, and returns at
//! once when it is absent. When it is present, the process is a launcher: it
//! spawns its arguments as the child, with that variable removed from the
//! child's environment, and exits without returning to `main`. Only libc is
//! called on that path, because the standard library's runtime has not been
//! set up yet.
//!
//! The launcher leads the run's process group and the child joins it, so the
//! group-ownership contract — cleanup of the complete group, the bounded
//! wait — is unchanged. The launcher passes no descriptor of its own to the
//! child: its report is a file it opens only after the child has ended.

use std::ffi::{CStr, c_char, c_int, c_long};
use std::io;
use std::path::{Path, PathBuf};

/// The variable that makes a binary linking this crate start as the
/// launcher. Its value is the path the launcher writes its report to.
pub(super) const REPORT_VARIABLE: &str = "NORN_TESTKIT_MEASURED_REPORT";

/// The same name as a prefix of an environment entry, for the launcher,
/// which reads the raw environment rather than the standard library's view.
const REPORT_ENTRY_PREFIX: &[u8] = b"NORN_TESTKIT_MEASURED_REPORT=";

/// The initializer the loader runs before `main`. glibc and dyld both call
/// one with `argc`, `argv` and `envp`.
#[used]
#[cfg_attr(
    target_vendor = "apple",
    unsafe(link_section = "__DATA,__mod_init_func")
)]
#[cfg_attr(not(target_vendor = "apple"), unsafe(link_section = ".init_array"))]
static DIVERT: extern "C" fn(c_int, *const *const c_char, *const *const c_char) = divert;

/// What the launcher learned about the child.
#[derive(Debug, Eq, PartialEq)]
pub(super) enum Report {
    /// The child ended with this raw wait status and peak, in the platform's
    /// `ru_maxrss` unit.
    Ended { status: c_int, max_rss: c_long },
    /// The child could not be started; the value is the `errno`.
    NotStarted(c_int),
}

/// The executable a run starts as its launcher: the image this process is
/// running, which links [`DIVERT`].
///
/// On Linux that is `/proc/self/exe`, the very file this process executes,
/// so a concurrent build replacing the artifact at its path does not change
/// which image starts. macOS has no such link, so it is the path the loader
/// reports.
pub(super) fn image() -> io::Result<PathBuf> {
    // Naming the initializer here is what keeps it in every binary that
    // starts a run: a linker may leave out an unreferenced part of a library.
    std::hint::black_box(&DIVERT);
    if cfg!(target_os = "linux") {
        Ok(PathBuf::from("/proc/self/exe"))
    } else {
        std::env::current_exe()
    }
}

/// Read the launcher's report at `path`, or `None` when it wrote none —
/// the launcher was killed before the child ended, or failed before it could
/// report.
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

/// Spawn `argv[1..]` with the environment minus the report variable, reap
/// it, write the report, and return the launcher's exit code.
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
    if argc < 2 {
        return finish(report, &Report::NotStarted(libc::EINVAL));
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

    // SAFETY: `argc >= 2`, so `argv[1]` is the program and `argv + 1` is a
    // null-terminated argument vector beginning with it.
    let (program, arguments) = unsafe { (*argv.add(1), argv.add(1)) };
    let mut pid: libc::pid_t = 0;
    // SAFETY: every pointer is valid and null-terminated as `posix_spawnp`
    // requires; null file actions and attributes ask for the defaults, so the
    // child inherits this process's descriptors, directory, group and signal
    // state, as it would have inherited the run's.
    let spawned = unsafe {
        libc::posix_spawnp(
            &mut pid,
            program,
            std::ptr::null(),
            std::ptr::null(),
            arguments.cast(),
            environment.as_ptr().cast(),
        )
    };
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

/// The launcher's exit code when it could not produce a report. The run
/// reads the absence of the report, not this code; the code is for a person
/// reading the launcher's status by hand.
const LAUNCHER_FAILED: c_int = 125;

fn finish(path: &CStr, report: &Report) -> c_int {
    let bytes = render_report(report);
    // SAFETY: `path` is null-terminated; the descriptor is this function's.
    let fd = unsafe {
        libc::open(
            path.as_ptr(),
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_CLOEXEC,
            0o600 as libc::c_uint,
        )
    };
    if fd < 0 {
        return LAUNCHER_FAILED;
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
            // SAFETY: `fd` is this function's.
            unsafe { libc::close(fd) };
            return LAUNCHER_FAILED;
        }
    }
    // SAFETY: `fd` is this function's.
    if unsafe { libc::close(fd) } != 0 {
        return LAUNCHER_FAILED;
    }
    0
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

    #[test]
    fn a_partial_or_foreign_report_is_unreadable() {
        for text in ["", "ended 0", "ended 0 12 extra", "started 1", "ended x 1"] {
            assert_eq!(parse_report(text), None, "{text:?}");
        }
    }
}
