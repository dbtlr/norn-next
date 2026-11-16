//! The bounds one connection's reads are held to: how long it may sit idle
//! between requests, and how long a request's head may take to arrive.
//!
//! **A connection is in one of three phases.** It starts in `head`: the first
//! request's head must arrive within the header bound of the accept. Once a
//! route has a request's head it is `busy` and no connection bound runs — the
//! body's arrival is the route's own bound, and the answer takes what it
//! takes. When the route answers, the connection is `idle`: the next request
//! may begin within the keep-alive bound, and its first byte moves the
//! connection to `head`, where the rest of that head must arrive within the
//! header bound. Past either bound the read fails and hyper closes the
//! connection.
//!
//! hyper's own header timeout is not used: it starts when hyper begins
//! reading a head, which on a kept-alive connection is the moment it goes
//! idle, so it would cut every idle connection at the header bound.

use std::io;
use std::pin::Pin;
use std::sync::{Arc, Mutex, PoisonError};
use std::task::{Context, Poll};
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::time::{Instant, Sleep};

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
    state: Mutex<(PhaseKind, Instant)>,
}

impl Phase {
    fn new() -> Phase {
        Phase {
            state: Mutex::new((PhaseKind::Head, Instant::now())),
        }
    }

    fn get(&self) -> (PhaseKind, Instant) {
        *self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn set(&self, kind: PhaseKind) {
        *self.state.lock().unwrap_or_else(PoisonError::into_inner) = (kind, Instant::now());
    }

    /// A request's head has arrived: no connection bound runs until it is
    /// answered.
    pub(crate) fn busy(&self) {
        self.set(PhaseKind::Busy);
    }

    /// The request is answered: the keep-alive bound runs from now.
    pub(crate) fn idle(&self) {
        self.set(PhaseKind::Idle);
    }

    /// A byte arrived: an idle connection's next request has begun.
    fn byte_arrived(&self) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if state.0 == PhaseKind::Idle {
            *state = (PhaseKind::Head, Instant::now());
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
    /// moves.
    pub(crate) fn new(inner: S, bounds: Bounds) -> (Paced<S>, Arc<Phase>) {
        let phase = Arc::new(Phase::new());
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
        Pin::new(&mut self.inner).poll_flush(cx)
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
