//! The server log: one plain-text line per lifecycle event, on stderr.
//!
//! **Every line is best effort, and logging never waits.** A process that
//! [`start`]s the log hands each line to one log thread through a bounded
//! queue; a full queue drops the line and counts it, and the log thread says
//! how many it lost once it writes again. So a stderr pipe nobody drains can
//! stall the log thread alone: it cannot hold the I/O thread, a drain, or a
//! hard deadline, each of which exits without writing anything.
//!
//! Where no log thread was started — this crate's own tests — a line is
//! written from the thread that has it.

use std::fmt;
use std::io::Write;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

/// Lines the queue holds before a further line is dropped.
const QUEUE_LINES: usize = 1024;

/// The log thread's queue, once its thread has started.
static QUEUE: OnceLock<Queue> = OnceLock::new();

struct Queue {
    lines: SyncSender<String>,
    counts: Arc<Counts>,
}

/// What the queue and its log thread both count.
#[derive(Default)]
struct Counts {
    /// Lines handed to the queue.
    sent: AtomicU64,
    /// Lines the log thread has written, or dropped on a failed write.
    written: AtomicU64,
    /// Lines dropped because the queue was full, not yet reported.
    dropped: AtomicU64,
}

/// Start the log thread. Every later [`log`] line goes through its queue.
///
/// The queue is published only once its thread is running: a spawn that
/// fails leaves no queue, so the refusal it causes is still written straight
/// to stderr rather than into a queue nothing reads.
pub(crate) fn start() -> std::io::Result<()> {
    if QUEUE.get().is_some() {
        return Ok(());
    }
    let (lines, receiver) = sync_channel(QUEUE_LINES);
    let counts = Arc::new(Counts::default());
    {
        let counts = Arc::clone(&counts);
        std::thread::Builder::new()
            .name("norn-log".into())
            .spawn(move || write_from(&receiver, &counts))?;
    }
    // A start that lost a race to another keeps the first queue; this one's
    // sender goes, and its thread ends with it.
    let _ = QUEUE.set(Queue { lines, counts });
    Ok(())
}

fn write_from(receiver: &Receiver<String>, counts: &Counts) {
    while let Ok(line) = receiver.recv() {
        let dropped = counts.dropped.swap(0, Ordering::SeqCst);
        if dropped > 0 {
            write_line(format_args!(
                "warning: {dropped} server log lines were dropped while stderr was not read"
            ));
        }
        write_line(format_args!("{line}"));
        counts.written.fetch_add(1, Ordering::SeqCst);
    }
}

fn write_line(line: fmt::Arguments<'_>) {
    let mut stderr = std::io::stderr().lock();
    let _ = stderr.write_fmt(line);
    let _ = stderr.write_all(b"\n");
}

/// Write one line to the server log.
pub(crate) fn log(line: fmt::Arguments<'_>) {
    #[cfg(test)]
    if capture::offer(&line) {
        return;
    }
    match QUEUE.get() {
        Some(queue) => match queue.lines.try_send(line.to_string()) {
            Ok(()) => {
                queue.counts.sent.fetch_add(1, Ordering::SeqCst);
            }
            Err(TrySendError::Full(_) | TrySendError::Disconnected(_)) => {
                queue.counts.dropped.fetch_add(1, Ordering::SeqCst);
            }
        },
        None => write_line(line),
    }
}

/// Wait at most `bound` for every line queued so far to be written: the last
/// lines of a process that is about to exit, given a bounded chance.
pub(crate) fn flush_within(bound: Duration) {
    let Some(queue) = QUEUE.get() else { return };
    let sent = queue.counts.sent.load(Ordering::SeqCst);
    let deadline = Instant::now() + bound;
    while queue.counts.written.load(Ordering::SeqCst) < sent && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(1));
    }
}

/// A test's view of the lines the current thread logs, so a case can assert
/// what a request wrote without reading stderr.
#[cfg(test)]
pub(crate) mod capture {
    use std::cell::RefCell;
    use std::fmt;

    thread_local! {
        static LINES: RefCell<Option<Vec<String>>> = const { RefCell::new(None) };
    }

    /// Run `body`, returning every line this thread logged meanwhile.
    pub(crate) fn lines_of<R>(body: impl FnOnce() -> R) -> (R, Vec<String>) {
        LINES.with(|lines| *lines.borrow_mut() = Some(Vec::new()));
        let result = body();
        let lines = LINES.with(|lines| lines.borrow_mut().take().unwrap_or_default());
        (result, lines)
    }

    pub(super) fn offer(line: &fmt::Arguments<'_>) -> bool {
        LINES.with(|lines| match lines.borrow_mut().as_mut() {
            Some(held) => {
                held.push(line.to_string());
                true
            }
            None => false,
        })
    }
}
