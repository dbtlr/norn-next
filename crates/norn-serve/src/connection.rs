//! The bounds one connection's reads are held to: how long it may sit idle
//! between requests, and how long a request's head may take to arrive.
//!
//! **A connection is in one of three phases.** It starts in `head`: the first
//! request's head must arrive within the header bound of the accept. Once a
//! route has a request's head it is `busy` and no connection bound runs — the
//! body's arrival is the route's own bound, and the answer takes what it
//! takes, its write included: hyper keeps reading while an answer's write is
//! blocked on a slow client, so the connection stays `busy` until the answer
//! has flushed. Once it has, the connection is `idle`: the next request may
//! begin within the keep-alive bound, and its first byte moves the connection
//! to `head`, where the rest of that head must arrive within the header
//! bound. A byte that arrived while the answer was still being written moves
//! the connection to `head` when the answer flushes. Past either bound the
//! read fails and hyper closes the connection.
//!
//! hyper's own header timeout is not used: it starts when hyper begins
//! reading a head, which on a kept-alive connection is the moment it goes
//! idle, so it would cut every idle connection at the header bound.
//!
//! **A request is in flight until its answer is flushed** (`drain.rs`). The
//! route's head moves the connection to `busy` and counts the request; once
//! the route answers, the next flush that completes is that answer reaching
//! the socket, and takes the request off the count. After an answer that
//! tells its client to close, every answer during a drain among them, that
//! same flush half-closes the connection (`shutdown(SHUT_WR)`) before it
//! does, so the client reads the answer and an end of stream rather than a
//! reset when the process exits. hyper writes a whole `Full` body into its buffer before
//! it flushes, so the first flush after the answer is the whole answer. A
//! connection that ends before its answer flushed takes its request off the
//! count as it goes.
//!
//! **Only an answer that told its client to close is half-closed.** Whether
//! an answer carries `Connection: close` is decided once, when the route
//! returns it, and the flush half-closes on that decision alone. An answer
//! that went out kept-alive leaves the connection readable even when the
//! drain began while it was being written (a large answer under
//! backpressure can take seconds): the client's next request on it is
//! answered `serve/draining`, which itself closes. Each answer the drain
//! gives carries the close, so every connection a drain answered still ends
//! after its answer, and the margin's exit ends the rest.

use std::io;
use std::pin::Pin;
use std::sync::{Arc, Mutex, PoisonError};
use std::task::{Context, Poll};
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::time::{Instant, Sleep};

use crate::drain::Drain;

/// The bounds a connection is held to. Fixed constants in production; this
/// crate's tests use short ones.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Bounds {
    /// Idle time between requests before the connection is closed.
    pub(crate) keep_alive: Duration,
    /// From a request's first byte, or from the accept for the first request,
    /// until its whole head has arrived.
    pub(crate) header_read: Duration,
    /// From a request's head until its whole body has arrived.
    pub(crate) body_arrival: Duration,
}

impl Bounds {
    /// The production bounds.
    pub(crate) const SERVING: Bounds = Bounds {
        keep_alive: Duration::from_secs(30),
        header_read: Duration::from_secs(5),
        body_arrival: Duration::from_secs(30),
    };
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PhaseKind {
    Idle,
    Head,
    Busy,
}

/// Where one connection stands, shared between its reads and its route.
#[derive(Debug)]
pub(crate) struct Phase {
    state: Mutex<State>,
    drain: Arc<Drain>,
}

#[derive(Debug)]
struct State {
    kind: PhaseKind,
    since: Instant,
    /// A request is counted in flight and its answer not yet flushed.
    counted: bool,
    /// The route has answered the counted request, and whether that answer
    /// tells its client to close.
    answered: Option<Closes>,
    /// A byte arrived after the route answered and before the answer
    /// flushed: the next request's head began during the write.
    head_began: bool,
}

/// Whether an answer carries `Connection: close`, so its flush is followed
/// by a half-close.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Closes {
    Yes,
    No,
}

impl Phase {
    fn new(drain: Arc<Drain>) -> Phase {
        Phase {
            state: Mutex::new(State {
                kind: PhaseKind::Head,
                since: Instant::now(),
                counted: false,
                answered: None,
                head_began: false,
            }),
            drain,
        }
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn get(&self) -> (PhaseKind, Instant) {
        let state = self.state();
        (state.kind, state.since)
    }

    /// A request's head has arrived: no connection bound runs until it is
    /// answered, and the request is in flight until its answer is flushed.
    pub(crate) fn busy(&self) {
        let mut state = self.state();
        state.kind = PhaseKind::Busy;
        state.since = Instant::now();
        state.answered = None;
        state.head_began = false;
        if !state.counted {
            state.counted = true;
            self.drain.request_started();
        }
    }

    /// The request is answered, by an answer that `closes` or not: the next
    /// completed flush is its answer's, and the keep-alive bound runs from
    /// that flush. A request that is not counted has no flush to wait for,
    /// so its bound runs from now.
    pub(crate) fn idle(&self, closes: Closes) {
        let mut state = self.state();
        state.head_began = false;
        if state.counted {
            state.answered = Some(closes);
        } else {
            state.kind = PhaseKind::Idle;
            state.since = Instant::now();
            state.answered = None;
        }
    }

    /// A byte arrived: an idle connection's next request has begun, or, while
    /// an answer is still being written, will be timed from its flush.
    fn byte_arrived(&self) {
        let mut state = self.state();
        if state.kind == PhaseKind::Idle {
            state.kind = PhaseKind::Head;
            state.since = Instant::now();
        } else if state.answered.is_some() {
            state.head_began = true;
        }
    }

    /// The answer that waits for its flush, if any: whether it closes.
    fn answer_unflushed(&self) -> Option<Closes> {
        self.state().answered
    }

    /// The answered request's flush completed: it is no longer in flight,
    /// and the connection is idle, or reading the head that began during the
    /// write, from now.
    fn answer_flushed(&self) {
        let mut state = self.state();
        if state.answered.is_some() {
            state.answered = None;
            state.counted = false;
            state.kind = if state.head_began {
                PhaseKind::Head
            } else {
                PhaseKind::Idle
            };
            state.head_began = false;
            state.since = Instant::now();
            drop(state);
            self.drain.request_ended();
        }
    }
}

impl Drop for Phase {
    /// A connection gone before its request's answer flushed: the request is
    /// no longer in flight either way.
    fn drop(&mut self) {
        let counted = self.state().counted;
        if counted {
            self.drain.request_ended();
        }
    }
}

/// A stream whose reads fail past its connection's bounds.
pub(crate) struct Paced<S> {
    inner: S,
    phase: Arc<Phase>,
    bounds: Bounds,
    timer: Pin<Box<Sleep>>,
}

impl<S> Paced<S> {
    /// `inner`, in its first request's `head` phase, and the phase its route
    /// moves, its requests counted in flight on `drain`.
    pub(crate) fn new(inner: S, bounds: Bounds, drain: Arc<Drain>) -> (Paced<S>, Arc<Phase>) {
        let phase = Arc::new(Phase::new(drain));
        let paced = Paced {
            inner,
            phase: Arc::clone(&phase),
            bounds,
            timer: Box::pin(tokio::time::sleep(bounds.header_read)),
        };
        (paced, phase)
    }

    /// The deadline the current phase runs to, or none while busy.
    fn deadline(&self) -> Option<Instant> {
        match self.phase.get() {
            (PhaseKind::Idle, since) => Some(since + self.bounds.keep_alive),
            (PhaseKind::Head, since) => Some(since + self.bounds.header_read),
            (PhaseKind::Busy, _) => None,
        }
    }
}

impl<S: AsyncRead + Unpin> AsyncRead for Paced<S> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let before = buf.filled().len();
        match Pin::new(&mut self.inner).poll_read(cx, buf) {
            Poll::Ready(result) => {
                if buf.filled().len() > before {
                    self.phase.byte_arrived();
                }
                Poll::Ready(result)
            }
            Poll::Pending => {
                let Some(deadline) = self.deadline() else {
                    return Poll::Pending;
                };
                if self.timer.deadline() != deadline {
                    self.timer.as_mut().reset(deadline);
                }
                match self.timer.as_mut().poll(cx) {
                    Poll::Ready(()) => Poll::Ready(Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "the connection sat idle past its keep-alive, or a request's head did \
                         not arrive in time",
                    ))),
                    Poll::Pending => Poll::Pending,
                }
            }
        }
    }
}

impl<S: AsyncWrite + Unpin> AsyncWrite for Paced<S> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let flushed = std::task::ready!(Pin::new(&mut self.inner).poll_flush(cx));
        if flushed.is_ok()
            && let Some(closes) = self.phase.answer_unflushed()
        {
            if closes == Closes::Yes {
                // The half-close before the request leaves the count, so the
                // drain's exit cannot come between the answer and its end of
                // stream. A failed shutdown leaves a connection the client
                // already closed.
                let _ = std::task::ready!(Pin::new(&mut self.inner).poll_shutdown(cx));
            }
            self.phase.answer_flushed();
        }
        Poll::Ready(flushed)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }

    fn poll_write_vectored(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[io::IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write_vectored(cx, bufs)
    }

    fn is_write_vectored(&self) -> bool {
        self.inner.is_write_vectored()
    }
}
