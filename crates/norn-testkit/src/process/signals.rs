//! Synchronous SIGINT and SIGTERM observation for one supervisor process.

use std::io;

pub(super) struct TerminationSignals {
    previous: libc::sigset_t,
    set: libc::sigset_t,
}

impl TerminationSignals {
    pub(super) fn block_and_listen() -> io::Result<Self> {
        let set = termination_set()?;
        let mut previous = unsafe { std::mem::zeroed() };
        let masked =
            unsafe { libc::pthread_sigmask(libc::SIG_BLOCK, &raw const set, &raw mut previous) };
        if masked != 0 {
            return Err(io::Error::from_raw_os_error(masked));
        }
        Ok(TerminationSignals { previous, set })
    }

    pub(super) fn pending(&self) -> io::Result<Option<libc::c_int>> {
        let mut pending = unsafe { std::mem::zeroed() };
        if unsafe { libc::sigpending(&raw mut pending) } == -1 {
            return Err(io::Error::last_os_error());
        }
        for candidate in [libc::SIGINT, libc::SIGTERM] {
            match unsafe { libc::sigismember(&raw const pending, candidate) } {
                1 => {
                    let mut signal = 0;
                    let waited = unsafe { libc::sigwait(&raw const self.set, &raw mut signal) };
                    return if waited == 0 {
                        Ok(Some(signal))
                    } else {
                        Err(io::Error::from_raw_os_error(waited))
                    };
                }
                0 => {}
                _ => return Err(io::Error::last_os_error()),
            }
        }
        Ok(None)
    }
}

impl Drop for TerminationSignals {
    fn drop(&mut self) {
        let _ = unsafe {
            libc::pthread_sigmask(
                libc::SIG_SETMASK,
                &raw const self.previous,
                std::ptr::null_mut(),
            )
        };
    }
}

pub(super) fn unblock_termination_signals() -> io::Result<()> {
    let set = termination_set()?;
    let result =
        unsafe { libc::pthread_sigmask(libc::SIG_UNBLOCK, &raw const set, std::ptr::null_mut()) };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::from_raw_os_error(result))
    }
}

pub(super) fn terminate(signal: libc::c_int) -> io::Result<()> {
    let set = signal_set(signal)?;
    if unsafe { libc::signal(signal, libc::SIG_DFL) } == libc::SIG_ERR {
        return Err(io::Error::last_os_error());
    }
    let unblocked =
        unsafe { libc::pthread_sigmask(libc::SIG_UNBLOCK, &raw const set, std::ptr::null_mut()) };
    if unblocked != 0 {
        return Err(io::Error::from_raw_os_error(unblocked));
    }
    if unsafe { libc::kill(libc::getpid(), signal) } == -1 {
        return Err(io::Error::last_os_error());
    }
    Err(io::Error::other(format!(
        "signal {signal} did not terminate the process"
    )))
}

fn termination_set() -> io::Result<libc::sigset_t> {
    let mut set = unsafe { std::mem::zeroed() };
    if unsafe { libc::sigemptyset(&raw mut set) } == -1
        || unsafe { libc::sigaddset(&raw mut set, libc::SIGINT) } == -1
        || unsafe { libc::sigaddset(&raw mut set, libc::SIGTERM) } == -1
    {
        return Err(io::Error::last_os_error());
    }
    Ok(set)
}

fn signal_set(signal: libc::c_int) -> io::Result<libc::sigset_t> {
    let mut set = unsafe { std::mem::zeroed() };
    if unsafe { libc::sigemptyset(&raw mut set) } == -1
        || unsafe { libc::sigaddset(&raw mut set, signal) } == -1
    {
        return Err(io::Error::last_os_error());
    }
    Ok(set)
}
