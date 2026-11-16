//! The two fixed-size blocking pools host calls run on.
//!
//! **Reads and mutations never share threads.** Writes queued behind one
//! vault's serial apply queue, or an `init` waiting for its vault to reach
//! Ready, hold mutation-pool threads only, so they cannot hold the threads
//! reads need on any vault. A vault's mutations take at most one mutation
//! thread at a time: the rest wait on that vault's async semaphore, holding no
//! thread (`semaphore.rs`).
//!
//! **The size is fixed and every thread lives as long as the pool**, spawned
//! when the pool is made: the read pool at the read connections plus four,
//! the mutation pool at eight. A thread count is then a bar rather than noise,
//! and no thread is spawned on a request's path.
//!
//! **The queue needs no bound of its own.** Every job is a wire request the
//! route admitted under its in-flight bound, and the job holds that request's
//! slot, and its body, until it has run: a request whose client disconnected
//! keeps its slot while its job is queued, so queued jobs never outnumber the
//! bound. Each one's claim decides whether the job runs at all once a thread
//! takes it up: a request that waited out its deadline, or that the drain
//! refused, has already been answered, and one whose client disconnected has
//! no one to answer; its job runs nothing.

use std::sync::mpsc;
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;

/// One host call, with the claim check that decides whether it runs inside it.
pub(crate) type Job = Box<dyn FnOnce() + Send + 'static>;

/// The threads the read pool holds beyond the read connections.
pub(crate) const READ_POOL_MARGIN: usize = 4;

/// The mutation pool's threads: at most one per vault at a time.
pub(crate) const MUTATION_POOL_THREADS: usize = 8;

/// A fixed set of threads taking jobs off one queue, first in, first out.
pub(crate) struct Pool {
    sender: Option<mpsc::Sender<Job>>,
    threads: Vec<thread::JoinHandle<()>>,
}

impl Pool {
    /// A pool of `size` threads, named `name` for a process listing.
    pub(crate) fn new(name: &str, size: usize) -> std::io::Result<Pool> {
        let (sender, receiver) = mpsc::channel::<Job>();
        let receiver = Arc::new(Mutex::new(receiver));
        let threads = (0..size.max(1))
            .map(|n| {
                let receiver = Arc::clone(&receiver);
                thread::Builder::new()
                    .name(format!("{name}-{n}"))
                    .spawn(move || run(&receiver))
            })
            .collect::<std::io::Result<Vec<_>>>()?;
        Ok(Pool {
            sender: Some(sender),
            threads,
        })
    }

    /// Queue `job` for the next free thread.
    pub(crate) fn submit(&self, job: Job) {
        // The sender goes only in the drop, so a pool being used still has
        // one; a send fails only when every thread has gone, and then the job
        // is dropped unrun and its request sees its answer channel close.
        if let Some(sender) = &self.sender {
            let _ = sender.send(job);
        }
    }

    /// How many threads the pool holds.
    #[cfg(test)]
    pub(crate) fn size(&self) -> usize {
        self.threads.len()
    }
}

impl Drop for Pool {
    /// Close the queue and wait for every thread: each finishes the job it is
    /// running, and the ones queued behind it run their claim check. The plain
    /// stop drops every connection's request before the pools, and a request
    /// dropped with its claim still `waiting` refused it, so a queued job calls
    /// nothing.
    fn drop(&mut self) {
        drop(self.sender.take());
        for thread in self.threads.drain(..) {
            let _ = thread.join();
        }
    }
}

fn run(receiver: &Mutex<mpsc::Receiver<Job>>) {
    loop {
        // The lock is held only for the receive, never across a job, so the
        // other threads take the next job while this one runs.
        let job = receiver
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .recv();
        let Ok(job) = job else { return };
        // A job that unwinds takes its request's answer channel down with it,
        // which the request reads as an answer it will not get; the thread
        // goes on serving.
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(job));
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use super::*;

    /// A pool runs as many jobs at once as it has threads, and no more.
    #[test]
    fn a_pool_runs_at_most_its_size_at_once() {
        let pool = Pool::new("test-pool", 3).unwrap();
        assert_eq!(pool.size(), 3);
        let running = Arc::new(AtomicUsize::new(0));
        let widest = Arc::new(AtomicUsize::new(0));
        let (done, finished) = mpsc::channel();
        for _ in 0..12 {
            let (running, widest, done) = (Arc::clone(&running), Arc::clone(&widest), done.clone());
            pool.submit(Box::new(move || {
                let now = running.fetch_add(1, Ordering::SeqCst) + 1;
                widest.fetch_max(now, Ordering::SeqCst);
                thread::sleep(Duration::from_millis(10));
                running.fetch_sub(1, Ordering::SeqCst);
                done.send(()).unwrap();
            }));
        }
        for _ in 0..12 {
            finished.recv_timeout(Duration::from_secs(10)).unwrap();
        }
        assert_eq!(widest.load(Ordering::SeqCst), 3);
    }

    /// A job that panics leaves its thread serving the next one.
    #[test]
    fn a_panicking_job_does_not_take_its_thread_down() {
        let pool = Pool::new("test-pool", 1).unwrap();
        pool.submit(Box::new(|| panic!("a job that unwinds")));
        let (done, finished) = mpsc::channel();
        pool.submit(Box::new(move || done.send(()).unwrap()));
        finished.recv_timeout(Duration::from_secs(10)).unwrap();
    }
}
