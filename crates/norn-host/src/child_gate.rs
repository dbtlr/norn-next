//! The children this crate's test binary spawns, kept apart from its
//! maintainer-lock acquisitions.
//!
//! A child holds a copy of every descriptor its parent had open at the fork
//! until it reaches its exec, so a maintainer lock one case releases while
//! another case's child sits between those two points still reads as held, by
//! this very process, until that child execs. `norn_fs::lock` leaves any wait
//! to its caller, and production spawns nothing while it holds a
//! maintainership, so the wait belongs to this binary: every spawn holds the
//! gate until its child is past its exec, and every acquisition of a
//! maintainer lock first waits for the spawns holding it. A lock released
//! before that wait is in no child's hands after it.

use std::io::{self, Read};
use std::process::{Command, ExitStatus, Output, Stdio};
use std::sync::{PoisonError, RwLock, RwLockReadGuard};

/// Shared by the spawns in flight, taken whole by an acquisition's wait.
static GATE: RwLock<()> = RwLock::new(());

/// Hold the gate as a spawn in flight does, until the guard drops.
pub(crate) fn spawning() -> RwLockReadGuard<'static, ()> {
    GATE.read().unwrap_or_else(PoisonError::into_inner)
}

/// Return once no spawn holds the gate.
pub(crate) fn await_spawns_in_flight() {
    drop(GATE.write().unwrap_or_else(PoisonError::into_inner));
}

/// Run a short-lived tool to its exit, holding the gate throughout: a child
/// that has exited holds no descriptor.
pub(crate) fn status(command: &mut Command) -> io::Result<ExitStatus> {
    let _spawning = spawning();
    command.status()
}

/// Run a child of this test binary and collect what it writes, as
/// [`Command::output`] does, holding the gate until the child's first byte on
/// stdout: libtest writes one as it starts, and nothing writes there before
/// the exec. A child that ends without writing has closed every descriptor.
pub(crate) fn output_of_test_binary(command: &mut Command) -> io::Result<Output> {
    let spawning = spawning();
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let mut first = [0_u8; 1];
    let stdout = child.stdout.as_mut().expect("the child's stdout is piped");
    let read = loop {
        match stdout.read(&mut first) {
            Err(interrupted) if interrupted.kind() == io::ErrorKind::Interrupted => {}
            read => break read?,
        }
    };
    drop(spawning);
    let mut output = child.wait_with_output()?;
    output.stdout.splice(0..0, first[..read].iter().copied());
    Ok(output)
}
