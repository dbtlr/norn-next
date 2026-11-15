//! The server log: one plain-text line per lifecycle event, on stderr.
//!
//! Every line is best effort. A stderr nobody reads, or one closed under the
//! process, loses the line and never stops the host. Lines are written from
//! the thread that has them for now; the bounded log queue that keeps a
//! stalled stderr pipe from delaying a hard deadline arrives with those
//! deadlines, in a later task.

use std::fmt;
use std::io::Write;

/// Write one line to the server log.
pub(crate) fn log(line: fmt::Arguments<'_>) {
    let mut stderr = std::io::stderr().lock();
    let _ = stderr.write_fmt(line);
    let _ = stderr.write_all(b"\n");
}
