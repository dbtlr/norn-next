//! Private, bounded observer placement. No heal overlaps these queries.
use super::*;
use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError};

// Shorter than Current::close's graceful wait, so a lost acknowledgement can
// unwind the host and retire projections before process-group cleanup is needed.
pub const PARENT_ACK_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Cell {
    N,
    G,
    E,
    Q,
    M,
}

impl Cell {
    pub fn parse(value: &str) -> Result<Self, String> {
        match value {
            "N" => Ok(Self::N),
            "G" => Ok(Self::G),
            "E" => Ok(Self::E),
            "Q" => Ok(Self::Q),
            "M" => Ok(Self::M),
            _ => Err(format!("unknown residency history {value}")),
        }
    }
}

pub type Work = Box<dyn FnOnce() -> Result<(), String> + Send>;
enum Action {
    Pause(&'static str),
    Work(Work),
    Local(Result<(), String>),
}
struct Request {
    action: Action,
    attachment_tid: u32,
    done: SyncSender<Result<(), String>>,
    claimed_or_cancelled: Arc<AtomicBool>,
}

pub struct Diagnostic {
    reporter: Arc<Reporter>,
    cell: Cell,
    requests: SyncSender<Request>,
    timeout: Duration,
}

/// The dispatcher carries closures of owned handles, never answers or snapshots.
/// Dropping it disconnects the observer; pending handles then follow normal RAII.
pub struct Dispatcher {
    requests: Receiver<Request>,
    began: Instant,
    sequence: u64,
    worker_tid: u32,
    attachment_tid: u32,
}

// Linux diagnostics deliberately use the kernel identity the parent sees in /proc.
pub fn thread_tid() -> u32 {
    std::fs::read_link("/proc/thread-self")
        .expect("Linux residency diagnostics require /proc/thread-self")
        .file_name()
        .unwrap()
        .to_str()
        .unwrap()
        .parse()
        .unwrap()
}

impl Diagnostic {
    pub fn new(
        began: Instant,
        name: VaultName,
        cell: Cell,
        timeout: Duration,
    ) -> (Arc<Self>, Dispatcher) {
        let (requests, receiver) = mpsc::sync_channel(1);
        let mut reporter = Reporter::new(began, name, false);
        reporter.diagnostic_threads = true;
        (
            Arc::new(Self {
                reporter: Arc::new(reporter),
                cell,
                requests,
                timeout,
            }),
            Dispatcher {
                requests: receiver,
                began,
                sequence: 0,
                worker_tid: thread_tid(),
                attachment_tid: 0,
            },
        )
    }

    fn rendezvous(&self, action: Action) -> Result<(), String> {
        let (done, completion) = mpsc::sync_channel(1);
        let claimed_or_cancelled = Arc::new(AtomicBool::new(false));
        self.requests
            .try_send(Request {
                action,
                done,
                claimed_or_cancelled: claimed_or_cancelled.clone(),
                attachment_tid: thread_tid(),
            })
            .map_err(|error| format!("residency dispatcher unavailable: {error}"))?;
        let result = completion
            .recv_timeout(self.timeout)
            .map_err(|error| format!("residency acknowledgement failed: {error}"));
        // Cancellation and dispatch atomically compete for an unstarted job.
        // An already running query keeps its own handles on the error path.
        claimed_or_cancelled.store(true, Ordering::Release);
        result?
    }

    pub fn execute(&self, work: Work) -> Result<(), String> {
        // Every arm has the same dispatcher handshake, including empty histories.
        // Local work and all of its captures drop before that handshake completes.
        let action = if self.cell == Cell::M {
            Action::Work(work)
        } else {
            Action::Local(work())
        };
        self.rendezvous(action)
    }

    fn pause(&self, name: &'static str) -> Result<(), String> {
        self.rendezvous(Action::Pause(name))
    }
}

impl Dispatcher {
    pub fn checkpoint(
        &mut self,
        name: &'static str,
        mut sampled: impl FnMut(&Value) -> Result<(), String>,
    ) -> Result<(), String> {
        assert_eq!(
            thread_tid(),
            self.worker_tid,
            "dispatcher left the worker main thread"
        );
        self.sequence += 1;
        let event = json!({"residency_checkpoint":name,"sequence":self.sequence,
            "paused_ns":self.began.elapsed().as_nanos(),"worker_tid":self.worker_tid,
            "attachment_tid":self.attachment_tid,"thread_tid":thread_tid(),
            "construction_eligibility_gets":1,
            "reader_budget_census":null,
            "reader_budget_unavailable":"observer has no access to the host-owned budget",
            "purpose":"residency attribution only; no live serving or performance acceptance"});
        sampled(&event)?;
        super::super::emit(json!({"residency_acknowledged":self.sequence,
            "acknowledged_ns":self.began.elapsed().as_nanos()}));
        Ok(())
    }

    pub fn pump(
        &mut self,
        sampled: impl FnMut(&Value) -> Result<(), String>,
    ) -> Result<(), String> {
        let request = match self.requests.try_recv() {
            Ok(request) => request,
            Err(TryRecvError::Empty) => return Ok(()),
            Err(TryRecvError::Disconnected) => return Err("residency observer disconnected".into()),
        };
        self.attachment_tid = request.attachment_tid;
        if request.claimed_or_cancelled.swap(true, Ordering::AcqRel) {
            return Err("residency observer cancelled before dispatch".into());
        }
        let result = match request.action {
            Action::Pause(name) => self.checkpoint(name, sampled),
            // FnOnce consumes and drops every reader/model capture before ack.
            Action::Work(work) => work(),
            Action::Local(result) => result,
        };
        request
            .done
            .try_send(result.clone())
            .map_err(|_| "residency observer cancelled before acknowledgement")?;
        result
    }
}

/// Parent sampling owns the ack file. No stdin reader or additional worker thread
/// changes the histories. Wrong acks, lost parents and cancellation fail closed.
pub fn sample_ack(root: &std::path::Path, event: &Value, timeout: Duration) -> Result<(), String> {
    super::super::emit(event.clone());
    let sequence = event["sequence"].as_u64().unwrap();
    let path = root.join("residency-acks").join(format!("{sequence}.json"));
    let deadline = Instant::now() + timeout.min(PARENT_ACK_TIMEOUT);
    loop {
        match std::fs::read(&path) {
            Ok(bytes) => {
                let ack: Value = serde_json::from_slice(&bytes)
                    .map_err(|e| format!("invalid residency ack: {e}"))?;
                if ack["sequence"].as_u64() != Some(sequence)
                    || ack["checkpoint"] != event["residency_checkpoint"]
                {
                    return Err("invalid residency acknowledgement identity".into());
                }
                std::fs::remove_file(&path).map_err(|e| e.to_string())?;
                return Ok(());
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
            Err(e) => return Err(format!("reading residency ack: {e}")),
        }
        if Instant::now() >= deadline {
            return Err("residency parent acknowledgement timed out".into());
        }
        std::thread::sleep(Duration::from_millis(1));
    }
}

impl Observer for Diagnostic {
    fn catalogue(&self, catalogue: &Catalogue, model: &ContentModel) -> Result<(), String> {
        self.reporter.catalogue(catalogue, model)
    }
    fn declined(&self, reason: &str) -> Result<(), String> {
        Err(format!(
            "residency diagnostic requires the frozen eligible fixture: {reason}"
        ))
    }
    fn canonical_work(
        &self,
        work: &std::collections::BTreeMap<&'static str, u64>,
    ) -> Result<(), String> {
        self.reporter.canonical_work(work)?;
        super::super::emit(json!({"residency_canonical_work":work}));
        Ok(())
    }
    fn indexed(
        &self,
        stage: Stage,
        reader: Arc<SnapshotReader>,
        model: Arc<ContentModel>,
    ) -> Result<(), String> {
        let (before, after, enabled) = match stage {
            Stage::CanonicalDemand => ("before-early-get", "after-early-get", self.cell != Cell::N),
            // canonical::run has already retired intake here, before minting this pool.
            Stage::CanonicalHealth => (
                "intake-retired-before-final",
                "after-final",
                self.cell == Cell::Q,
            ),
            _ => return Err("unexpected diagnostic stage".into()),
        };
        self.pause(before)?;
        let reporter = self.reporter.clone();
        self.execute(Box::new(move || {
            if enabled {
                reporter.indexed(stage, reader, model)
            } else {
                Ok(())
            }
        }))?;
        self.pause(after)
    }
    fn intake(
        &self,
        reader: Arc<norn_host::startup_experiment::IntakeReader>,
    ) -> Result<(), String> {
        self.reporter.intake.store(true, Ordering::Relaxed);
        self.pause("before-metadata")?;
        let reporter = self.reporter.clone();
        let enabled = matches!(self.cell, Cell::E | Cell::Q | Cell::M);
        self.execute(Box::new(move || {
            if enabled {
                reporter.intake_queries(&reader, reporter.began.elapsed().as_nanos())
            } else {
                Ok(())
            }
        }))?;
        self.pause("after-metadata")
    }
}
