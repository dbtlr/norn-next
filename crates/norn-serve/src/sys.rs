//! The four operating-system calls the process lifecycle needs and the
//! standard library does not name: the thread signal mask, `sigwait`, reading
//! a signal's inherited disposition, and `_exit`.
//!
//! **This is the crate's one module allowed `unsafe`**, each call a plain
//! libc call over values this module owns, with its safety argument beside
//! it. Nothing here allocates on the exit path or takes a lock: `_exit`
//! writes nothing and runs no destructor or exit handler, which is what lets a
//! hard deadline fire while another thread holds stderr or a host lock.
#![allow(unsafe_code)]

use std::io;
use std::mem::MaybeUninit;

pub(crate) use libc::c_int;

/// A set of signals, as the mask and `sigwait` take it.
#[derive(Clone, Copy)]
pub(crate) struct SignalSet(libc::sigset_t);

impl SignalSet {
    /// The set holding exactly `signals`.
    pub(crate) fn of(signals: &[c_int]) -> SignalSet {
        let mut set = MaybeUninit::<libc::sigset_t>::uninit();
        // SAFETY: `sigemptyset` initialises the set it is pointed at, and
        // `sigaddset` takes a set so initialised and a valid signal number.
        unsafe {
            libc::sigemptyset(set.as_mut_ptr());
            for &signal in signals {
                libc::sigaddset(set.as_mut_ptr(), signal);
            }
            SignalSet(set.assume_init())
        }
    }

    /// Whether `signal` is in the set.
    #[cfg(test)]
    pub(crate) fn contains(&self, signal: c_int) -> bool {
        // SAFETY: the set was initialised by `of`.
        unsafe { libc::sigismember(&self.0, signal) == 1 }
    }
}

/// Block `set` in the calling thread, so it and every thread it spawns from
/// now on leave those signals pending for `sigwait` rather than running their
/// default action.
pub(crate) fn block(set: &SignalSet) -> io::Result<()> {
    // SAFETY: the set is initialised, and a null old-mask pointer is allowed.
    let status = unsafe { libc::pthread_sigmask(libc::SIG_BLOCK, &set.0, std::ptr::null_mut()) };
    if status == 0 {
        Ok(())
    } else {
        Err(io::Error::from_raw_os_error(status))
    }
}

/// Wait for one signal of `set`, which must be blocked in every thread, and
/// answer which. An interrupted wait is waited again.
pub(crate) fn wait(set: &SignalSet) -> io::Result<c_int> {
    loop {
        let mut signal: c_int = 0;
        // SAFETY: the set is initialised and `signal` is a valid out-pointer.
        let status = unsafe { libc::sigwait(&set.0, &mut signal) };
        match status {
            0 => return Ok(signal),
            libc::EINTR => {}
            error => return Err(io::Error::from_raw_os_error(error)),
        }
    }
}

/// Whether this process inherited `signal` as ignored: its disposition at
/// start is `SIG_IGN`, as `nohup` leaves SIGHUP.
pub(crate) fn inherited_as_ignored(signal: c_int) -> bool {
    let mut old = MaybeUninit::<libc::sigaction>::zeroed();
    // SAFETY: a null new action only reads the current one into `old`, which
    // is a valid, zeroed out-pointer.
    let status = unsafe { libc::sigaction(signal, std::ptr::null(), old.as_mut_ptr()) };
    if status != 0 {
        return false;
    }
    // SAFETY: `sigaction` succeeded, so it wrote the action.
    let old = unsafe { old.assume_init() };
    old.sa_sigaction == libc::SIG_IGN
}

/// End the process at once with `status`: no destructor, no exit handler, no
/// buffered write flushed, no lock waited for.
pub(crate) fn exit_now(status: u8) -> ! {
    // SAFETY: `_exit` is async-signal-safe and touches no Rust state.
    unsafe { libc::_exit(c_int::from(status)) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_set_holds_exactly_the_signals_it_was_made_of() {
        let set = SignalSet::of(&[libc::SIGTERM, libc::SIGINT]);
        assert!(set.contains(libc::SIGTERM));
        assert!(set.contains(libc::SIGINT));
        assert!(!set.contains(libc::SIGHUP));
    }

    /// A test binary does not inherit SIGTERM as ignored; the case that it
    /// keeps an inherited ignore is the process suite's, under `nohup`'s
    /// arrangement.
    #[test]
    fn a_default_disposition_is_not_read_as_ignored() {
        assert!(!inherited_as_ignored(libc::SIGTERM));
    }
}
