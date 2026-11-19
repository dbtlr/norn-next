#![deny(unsafe_code)]
//! The HTTP surface and the host process it runs in.
//!
//! `norn serve` is the one way a host process starts (ADR 0053), and [`run`]
//! is its entry: the composition root hands it the `serve` arguments to parse
//! and runs it in its own process — a process entry, not a request.
//!
//! **The process, in order.** [`run`] blocks the stop signals in the main
//! thread before anything else (`signals.rs`), so every thread spawned after
//! inherits the mask; starts the log thread (`log.rs`), the hard-exit timer
//! (`exit.rs`) and the signal thread; parses the arguments and reads the
//! serving settings; then runs the start sequence (`start.rs`): the channel
//! lock, waited for and taken over where an auto-launched host holds it, the
//! start-bound watchdog, the token minted where the file holds none (the
//! private `mint` module), the registry read and the host built over it, the
//! port probe, the bind and `listening`. It serves the hello and the bearer
//! middleware in front of every route (`http.rs`, over the credential state in
//! `credentials.rs`), the wire route (`wire.rs`, run by `request.rs`), the
//! drain request and, under the `counters` feature, `GET /v1/debug/counters`.
//! While it serves it rechecks its lock's name every 5 s (`lock_watch.rs`),
//! and an auto-launched host exits after `auto_exit_after` idle.
//!
//! **Every stop is one bounded drain** (`lifecycle.rs`, `drain.rs`): the
//! listener closes, new requests are refused, in-flight ones get
//! `drain_bound` to finish, the ones still running are answered by kind, every
//! answer is flushed and its connection half-closed, the host is dropped on a
//! helper thread, and the process exits when that drop finishes or at
//! `drain_bound` + the shutdown margin at the latest, by the timer thread's
//! `_exit`. The kernel frees `serve.lock` and every maintainer lock with the
//! process, and a store left mid-write recovers through the heal path.
//!
//! **Exit statuses.** 0 after a drain a signal, a drain request or the idle
//! exit began; 1 for a refused start or an abnormal stop; 2 for a usage error
//! (clap's); and, for a launched start only, 10 when a verified host serves
//! this channel and 11 when the channel lock is held and no verified host
//! answers at this endpoint.
//!
//! **No async type reaches `norn-host`** (ruling R2): the host is built,
//! called and dropped on ordinary threads, and nothing it is handed is a
//! tokio or hyper type. tokio and hyper run this crate's server and its
//! takeover dial; `norn-client`'s hello dialer runs on them too, and the rule
//! holds there the same way.
//!
//! **The server log is stderr**, one plain-text line per lifecycle event
//! (the private `log` module). It is a diagnostic stream, not rendered output.
//!
//! **`unsafe` lives in one module**, `sys.rs`: the signal mask, `sigwait`, a
//! signal's inherited disposition and `_exit`, which the standard library
//! does not name. Every other module is denied it.

use std::fmt;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use norn_config::machine::settings::{ServingSettings, SettingsSource};
use norn_config::machine::tokens::WatchedTokens;
use norn_config::{Channel, ConfigDirs};
use norn_host::{Host, ProductionEntryOps, RegistryRead};
use norn_wire::{BuildIdentity, HelloChannel, HelloLaunch};
use tokio::net::{TcpListener, TcpSocket};

mod admission;
mod bounds;
mod connection;
mod credentials;
pub mod dial;
mod drain;
mod exit;
mod http;
mod lifecycle;
mod lock_watch;
mod log;
mod mint;
mod policy;
mod pool;
mod request;
mod signals;
mod start;
mod stop;
mod sys;
mod wire;

use exit::HardExit;
use lifecycle::{Lifecycle, REFUSED, StopReason};
use log::log;

/// What `norn serve` was asked to do.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Serve {
    /// The `--port` override; the settings file's `[endpoint] port` and the
    /// channel default stand behind it.
    pub port: Option<std::num::NonZeroU16>,
    /// The `--config` file, read instead of the channel's `config.toml`.
    pub config: Option<PathBuf>,
    pub launch: Launch,
}

/// Who started this host.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Launch {
    /// `norn serve` by hand, or the service manager.
    Explicit,
    /// A client launched it with the auto-launch marker.
    Auto,
}

impl From<Launch> for HelloLaunch {
    fn from(launch: Launch) -> Self {
        match launch {
            Launch::Explicit => HelloLaunch::Explicit,
            Launch::Auto => HelloLaunch::Auto,
        }
    }
}

impl fmt::Display for Launch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Launch::Explicit => "explicit",
            Launch::Auto => "auto",
        })
    }
}

/// How long the accept loop rests after a failed accept, so a condition that
/// fails every accept (descriptor exhaustion) is not a busy loop.
const ACCEPT_RETRY_PAUSE: Duration = Duration::from_millis(100);

/// The longest the process waits for its last log lines to be written before
/// it exits: best effort, and never a hold on the exit.
const LAST_LINES_WAIT: Duration = Duration::from_millis(200);

/// A start that cannot go on: the status it exits with, and what refused it
/// and how to resolve it, as the one log line the refusal is.
#[derive(Debug)]
struct Refusal {
    status: u8,
    reason: String,
}

impl Refusal {
    /// A refusal exiting 1.
    fn new(reason: String) -> Refusal {
        Refusal {
            status: REFUSED,
            reason,
        }
    }
}

impl From<start::Ended> for Refusal {
    fn from(ended: start::Ended) -> Self {
        Refusal {
            status: ended.status,
            reason: ended.line,
        }
    }
}

/// Open connections at once; past it accept pauses, and further connects wait
/// in the listen backlog, then the kernel refuses them.
const CONNECTION_BOUND: usize = 512;

/// The listen backlog connects wait in while accept is paused.
const LISTEN_BACKLOG: u32 = 64;

/// Run the host process for this channel until a drain stops it, and answer
/// the status it exits with (the module doc's exit statuses).
///
/// The build identity is captured by the bin. The arguments are parsed only
/// once the stop signals are blocked and the signal thread waits for them.
pub fn run(build: &'static str, arguments: impl FnOnce() -> Serve) -> ExitCode {
    // First, before any thread exists: every thread spawned from here on
    // inherits the mask.
    let mask = match signals::block() {
        Ok(mask) => mask,
        Err(error) => return refused(&Refusal::new(format!("blocking signals failed: {error}"))),
    };
    if let Err(error) = log::start() {
        return refused(&Refusal::new(format!(
            "starting the log thread failed: {error}"
        )));
    }
    let exits = match HardExit::start() {
        Ok(exits) => exits,
        Err(error) => {
            return refused(&Refusal::new(format!(
                "starting the exit timer failed: {error}"
            )));
        }
    };
    let target = signals::Target::default();
    if let Err(error) = signals::start(mask, Arc::clone(&target)) {
        return refused(&Refusal::new(format!(
            "starting the signal thread failed: {error}"
        )));
    }
    match serve_channel(BuildIdentity::new(build), arguments, exits, &target) {
        Ok(reason) => {
            log(format_args!("stopped: {reason}"));
            log::flush_within(LAST_LINES_WAIT);
            ExitCode::from(reason.status())
        }
        Err(refusal) => refused(&refusal),
    }
}

fn refused(refusal: &Refusal) -> ExitCode {
    log(format_args!("refused: {}", refusal.reason));
    log::flush_within(LAST_LINES_WAIT);
    ExitCode::from(refusal.status)
}

fn serve_channel(
    build: BuildIdentity,
    arguments: impl FnOnce() -> Serve,
    exits: Arc<HardExit>,
    target: &signals::Target,
) -> Result<StopReason, Refusal> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .enable_time()
        .build()
        .map_err(|error| Refusal::new(format!("starting the I/O runtime failed: {error}")))?;
    let stopped = runtime.block_on(async {
        let serve = arguments();
        let settings_args = serve.clone();
        let (dirs, settings) = tokio::task::spawn_blocking(move || {
            let dirs = ConfigDirs::from_environment().map_err(|error| {
                Refusal::new(format!(
                    "{error}; set HOME, or XDG_CONFIG_HOME and XDG_DATA_HOME, to absolute paths"
                ))
            })?;
            let settings = settings(&dirs, &settings_args)?;
            Ok::<_, Refusal>((dirs, settings))
        })
        .await
        .map_err(|error| Refusal::new(format!("reading settings failed: {error}")))??;
        let lifecycle = Arc::new(Lifecycle::new(
            Arc::new(drain::Drain::new(bounds::drain_bound(settings.drain_bound))),
            Arc::default(),
            exits,
        ));
        // From here a signal reaches the start through the lifecycle, which
        // still ends it at once until the host listens.
        let _ = target.set(Arc::clone(&lifecycle));
        serve_on_runtime(build, &serve, dirs, settings, lifecycle).await
    });
    // A host construction the watchdog or a signal cut off ends the process
    // from its own thread; nothing here waits for it.
    runtime.shutdown_background();
    stopped
}

/// Step 1 of the start: the settings file with the `--port` override resolved
/// inside the read, each warning logged as its own line.
fn settings(dirs: &ConfigDirs, serve: &Serve) -> Result<ServingSettings, Refusal> {
    let source = serve
        .config
        .as_deref()
        .map_or(SettingsSource::ChannelDefault, SettingsSource::Explicit);
    let (settings, warnings) = ServingSettings::read(dirs, source, serve.port)
        .map_err(|error| Refusal::new(error.to_string()))?;
    for warning in warnings {
        log(format_args!(
            "warning: {}: {}",
            warning.key, warning.message
        ));
    }
    Ok(settings)
}

async fn serve_on_runtime(
    build: BuildIdentity,
    serve: &Serve,
    dirs: ConfigDirs,
    settings: ServingSettings,
    lifecycle: Arc<Lifecycle>,
) -> Result<StopReason, Refusal> {
    let endpoint = settings.endpoint;
    let channel: HelloChannel = Channel::COMPILED.into();
    // Steps 2 to 4.
    let lock = start::take_channel(&start::Start {
        dirs: &dirs,
        endpoint: endpoint.address(),
        channel,
        launch: serve.launch,
    })
    .await
    .map_err(Refusal::from)?;

    // Step 5: the start bound runs from the lock to listening. Its end is the
    // timer thread's `_exit(1)`; what the start was waiting on was logged
    // when it was entered.
    lifecycle
        .exits()
        .arm(Instant::now() + bounds::START_BOUND, REFUSED);
    let wire_settings = WireSettings {
        read_connections: settings.read_connections,
        ready_wait: settings.ready_wait,
    };
    let auto_exit_after = settings.auto_exit_after;
    let request_log = settings.request_log;
    let (host, watched) = {
        let dirs = dirs.clone();
        tokio::task::spawn_blocking(move || construct(dirs, &settings))
            .await
            .map_err(|error| Refusal::new(format!("constructing the host failed: {error}")))??
    };

    let mcp_tools = norn_mcp::ToolList::derive(
        norn_wire::Verb::ALL
            .into_iter()
            .filter(|verb| !wire::NOT_SERVED.contains(verb)),
    )
    .map_err(|error| Refusal::new(format!("preparing MCP tools failed: {error}")))?
    .prepare(build.as_str());

    // Step 5.3: a process holding the port, unless it is a verified host of
    // this channel whose lock file was removed.
    entering(format_args!("checking port {}", endpoint.port()));
    if let Err(error) = start::probe(endpoint.address()) {
        if error.kind() != std::io::ErrorKind::AddrInUse {
            return Err(Refusal::new(format!(
                "checking port {} failed: {error}",
                endpoint.port()
            )));
        }
        if start::verified_host_at(endpoint.address(), channel, &watched).await {
            return Err(Refusal {
                status: match serve.launch {
                    Launch::Auto => start::HOST_SERVES,
                    Launch::Explicit => REFUSED,
                },
                reason: format!(
                    "a norn host already serves this channel at {endpoint}, and its lock file in \
                     {} was removed; stop that host before starting another",
                    dirs.config_dir().display()
                ),
            });
        }
        return Err(port_held(serve, &dirs, endpoint));
    }
    // Step 5.4.
    entering(format_args!("binding {endpoint}"));
    let listener = bind(endpoint.address()).map_err(|error| {
        if error.kind() == std::io::ErrorKind::AddrInUse {
            port_held(serve, &dirs, endpoint)
        } else {
            Refusal::new(format!("binding {endpoint} failed: {error}"))
        }
    })?;

    let route = Arc::new(wire::HostRoute {
        host,
        drain: Arc::clone(&lifecycle.drain),
    });
    let stopper = Arc::new(stop::Stopper::default());
    let wire_state = wire_state(
        &wire_settings,
        Arc::clone(&route) as Arc<dyn request::Dispatch>,
        &lifecycle,
    )
    .map_err(|error| {
        Refusal::new(format!(
            "starting the host's request threads failed: {error}"
        ))
    })?;
    let context = Arc::new(http::Context {
        credentials: Mutex::new(credentials::Credentials::new(&dirs, watched)),
        mcp_tools,
        channel,
        build,
        launch: serve.launch.into(),
        wire: Arc::new(wire_state),
        stopper: Arc::clone(&stopper),
        lifecycle: Arc::clone(&lifecycle),
        bounds: connection::Bounds::SERVING,
        request_log,
        #[cfg(feature = "counters")]
        readings: {
            let route = Arc::clone(&route);
            Box::new(move || host_readings(route.host.evidence()))
        },
    });
    drop(route);

    // Step 5.5: disarm the watchdog, then listen. A signal that ended the
    // start first has exited the process already.
    lifecycle.exits().disarm();
    if !lifecycle.listening() {
        return Err(Refusal::new(
            "a stop arrived before the host was listening".into(),
        ));
    }
    log(format_args!(
        "listening: channel {}, port {}, {} start, build {build}",
        Channel::COMPILED.app_directory(),
        endpoint.port(),
        serve.launch,
    ));
    let lock = Arc::new(Mutex::new(lock));
    lock_watch::start(dirs.clone(), Arc::clone(&lock), Arc::clone(&lifecycle))
        .map_err(|error| Refusal::new(format!("starting the lock recheck failed: {error}")))?;

    let open = Arc::new(tokio::sync::Semaphore::new(CONNECTION_BOUND));
    let mut connections = tokio::task::JoinSet::new();
    let idle_exit = serve.launch == Launch::Auto;
    loop {
        // Reap every finished connection before the next accept. The select
        // is biased toward accepting, so under a steady stream of
        // short-lived connections its `join_next` arm may never be reached,
        // and the finished tasks' entries would pile up in the set past the
        // connection bound, which counts only open connections. No test
        // drives this: the set's length is not observable from outside the
        // loop, and a load test that showed the growth would be neither
        // cheap nor deterministic.
        while connections.try_join_next().is_some() {}
        tokio::select! {
            biased;
            () = lifecycle.stopping() => break,
            () = stopper.requested() => {
                lifecycle.begin_drain(StopReason::WorkerStopped);
            }
            () = idle_for(&lifecycle.drain, auto_exit_after), if idle_exit => {
                lifecycle.begin_drain(StopReason::Idle);
            }
            (slot, accepted) = accept_under(&listener, &open) => match accepted {
                Ok((stream, _)) => {
                    lifecycle.drain.touch();
                    let context = Arc::clone(&context);
                    connections.spawn(async move {
                        http::answer(stream, context).await;
                        drop(slot);
                    });
                }
                Err(error) => {
                    log(format_args!("warning: accepting a connection failed: {error}"));
                    tokio::time::sleep(ACCEPT_RETRY_PAUSE).await;
                }
            },
            Some(_) = connections.join_next(), if !connections.is_empty() => {}
        }
    }

    // The drain. Step 1: the stage is `draining` already; close the listener.
    drop(listener);
    lifecycle.listener_closed();
    let reason = lifecycle
        .reason()
        .expect("the serving loop ends only once a drain has begun");
    let deadline = lifecycle
        .drain
        .deadline()
        .expect("a drain that has begun has a deadline");
    // Step 3: every in-flight request gets the drain bound to finish and
    // flush. Step 4 happens in each request's own task at the bound; the
    // margin's end is the timer thread's exit, whatever is left.
    if !lifecycle.drain.settle_by(deadline).await {
        lifecycle
            .drain
            .settle_by(deadline + bounds::SHUTDOWN_MARGIN)
            .await;
    }
    // Step 5: every answer is flushed, and each that told its client to
    // close is half-closed. The rest close now, and the host is dropped on a
    // helper thread; the lock is held until it has gone. A kept-alive
    // connection with no request in flight closes here and no sooner, so a
    // request its client sends during the drain is answered
    // `serve/draining`. One the client sent that the host has not yet read
    // when the drain settles meets the close instead: the race every
    // kept-alive connection has with its server's close, which no answer
    // can cover, and which nothing in flight means no outcome is lost to.
    connections.abort_all();
    while connections.join_next().await.is_some() {}
    let (dropped, gone) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name("norn-host-drop".into())
        .spawn(move || {
            drop(context);
            let _ = dropped.send(());
        })
        .map_err(|error| Refusal::new(format!("stopping the host failed: {error}")))?;
    let _ = gone.recv();
    drop(lock);
    Ok(reason)
}

/// Completes once the host has seen no activity for `after`: no connection
/// accepted, no hello answered and no request in flight.
async fn idle_for(drain: &drain::Drain, after: Duration) {
    loop {
        match drain.idle_since() {
            Some(since) if since.elapsed() >= after => return,
            Some(since) => {
                tokio::time::sleep_until(tokio::time::Instant::from_std(since + after)).await;
            }
            // A request in flight: idle no sooner than `after` past its end.
            None => tokio::time::sleep(after).await,
        }
    }
}

/// Log what the start is waiting on as it enters it, best effort: the
/// watchdog's `_exit` writes nothing, so this line is what names the wait.
fn entering(what: fmt::Arguments<'_>) {
    log(format_args!("starting: {what}"));
}

/// A port another process holds, as a start refuses over it.
fn port_held(
    serve: &Serve,
    dirs: &ConfigDirs,
    endpoint: norn_config::machine::Endpoint,
) -> Refusal {
    Refusal::new(format!(
        "port {} on {} is held by another process; choose another with `--port N`, or with \
         `[endpoint] port` in {}",
        endpoint.port(),
        endpoint.address().ip(),
        settings_file_named(serve, dirs),
    ))
}

/// Steps 5.1 and 5.2: the token minted where the file holds none and the set
/// read, then the registry read and the host built over it. The host's
/// startup sweep runs inside `Host::new`.
fn construct(
    dirs: ConfigDirs,
    settings: &ServingSettings,
) -> Result<(Host<ProductionEntryOps>, WatchedTokens), Refusal> {
    entering(format_args!(
        "taking the token file's lock to mint or read the token"
    ));
    let watched = mint_and_read(&dirs)?;
    entering(format_args!("reading the registry"));
    let registry = RegistryRead::load(&dirs).map_err(|error| {
        // An error that already names the file is not prefixed with it again.
        let path = dirs.config_dir().join("registry.toml");
        let error = error.to_string();
        let named = if error.contains(&path.display().to_string()) {
            error
        } else {
            format!("{}: {error}", path.display())
        };
        Refusal::new(format!(
            "{named}; repair the registry file, or move it aside to start with no vaults"
        ))
    })?;
    let production = policy::production(settings)
        .map_err(|error| Refusal::new(format!("the serving settings are not usable: {error}")))?;
    entering(format_args!("constructing the host"));
    let host = Host::new(
        registry,
        ProductionEntryOps::new(dirs, production),
        policy::lifecycle(settings),
    )
    .map_err(|error| Refusal::new(format!("the serving settings are not usable: {error}")))?;
    Ok((host, watched))
}

/// Step 5.1: mint a `default` token if the file holds none, then read the set
/// the hello and the bearer middleware serve from.
///
/// A set that does not read on that second read — the file changed in the
/// moment between — refuses the start, as an unreadable token file does at
/// the mint: a host serves from a set it has read, or does not start.
fn mint_and_read(dirs: &ConfigDirs) -> Result<WatchedTokens, Refusal> {
    let file = credentials::tokens_file(dirs);
    let secret = mint::random_secret().map_err(|error| {
        Refusal::new(format!(
            "drawing a token from the operating system's random source failed: {error}"
        ))
    })?;
    match mint::mint(dirs, &secret) {
        Ok(mint::Mint::Held) => {}
        Ok(mint::Mint::Minted) => log(format_args!("minted the `default` token into {file}")),
        Ok(mint::Mint::MintedUnconfirmed(error)) => log(format_args!(
            "minted the `default` token into {file}; warning: {error}"
        )),
        Err(error) => return Err(Refusal::new(credentials::unreadable(&file, &error))),
    }
    readable(dirs, WatchedTokens::read(dirs))
}

/// `watched`, or the start's refusal when the read it holds refused: the file
/// and the fix, as the mint's own refusal names them.
fn readable(dirs: &ConfigDirs, watched: WatchedTokens) -> Result<WatchedTokens, Refusal> {
    if let Err(error) = watched.current() {
        let file = credentials::tokens_file(dirs);
        return Err(Refusal::new(credentials::unreadable(&file, error)));
    }
    Ok(watched)
}

/// Bind and listen on exactly `address`, with `SO_REUSEADDR` so a socket this
/// channel's previous host left in TIME_WAIT does not refuse the start, and
/// without `SO_REUSEPORT`, so a second listener cannot share the port.
fn bind(address: std::net::SocketAddr) -> std::io::Result<TcpListener> {
    let socket = TcpSocket::new_v4()?;
    socket.set_reuseaddr(true)?;
    socket.bind(address)?;
    socket.listen(LISTEN_BACKLOG)
}

/// The next connection, once an open slot is free: accept pauses at the
/// connection bound. Cancel-safe, as a select arm must be: a slot taken and
/// not used goes back where the future is dropped.
async fn accept_under(
    listener: &TcpListener,
    open: &Arc<tokio::sync::Semaphore>,
) -> (
    Option<tokio::sync::OwnedSemaphorePermit>,
    std::io::Result<(tokio::net::TcpStream, std::net::SocketAddr)>,
) {
    let slot = Arc::clone(open).acquire_owned().await.ok();
    (slot, listener.accept().await)
}

/// The serving settings the wire route reads.
struct WireSettings {
    read_connections: usize,
    ready_wait: Duration,
}

/// The wire route's state over `dispatcher`: its two fixed pools, its
/// admission and its counters.
fn wire_state(
    settings: &WireSettings,
    dispatcher: Arc<dyn request::Dispatch>,
    lifecycle: &Lifecycle,
) -> std::io::Result<request::Wire> {
    Ok(request::Wire {
        admission: lifecycle.admission(),
        semaphores: Arc::default(),
        reads: pool::Pool::new(
            "norn-read",
            settings.read_connections + pool::READ_POOL_MARGIN,
        )?,
        mutations: pool::Pool::new("norn-mutation", pool::MUTATION_POOL_THREADS)?,
        dispatcher,
        ready_wait: settings.ready_wait,
        counters: Arc::default(),
        drain: Arc::clone(&lifecycle.drain),
    })
}

/// The host's readings, named as the counters route answers them: the
/// per-request tallies under `host.request.*`, and the job account of the
/// work no one request owns — attach, re-attach, heal, background turns —
/// under `host.jobs.*`.
#[cfg(feature = "counters")]
fn host_readings(evidence: norn_host::HostEvidence) -> Vec<(String, u64)> {
    let requests = evidence.requests;
    let jobs = evidence.jobs;
    [
        ("host.request.requests", requests.requests),
        ("host.request.statements", requests.tally.statements),
        ("host.request.vm_steps", requests.tally.vm_steps),
        ("host.request.fs_reads", requests.tally.fs_reads),
        (
            "host.request.registrations_visited",
            requests.tally.registrations_visited,
        ),
        (
            "host.request.entries_visited",
            requests.tally.entries_visited,
        ),
        ("host.jobs.document_opens", jobs.document_opens),
        (
            "host.jobs.documents_read_by_build",
            jobs.documents_read_by_build,
        ),
        ("host.jobs.stats", jobs.stats),
        ("host.jobs.walk_dirents", jobs.walk_dirents),
        ("host.jobs.documents_derived", jobs.documents_derived),
        ("host.jobs.changesets_applied", jobs.changesets_applied),
        ("host.jobs.documents_upserted", jobs.documents_upserted),
        ("host.jobs.documents_deleted", jobs.documents_deleted),
        ("host.jobs.content_rows_written", jobs.content_rows_written),
        (
            "host.jobs.structure_rows_written",
            jobs.structure_rows_written,
        ),
        ("host.jobs.findings_written", jobs.findings_written),
        (
            "host.jobs.full_text_rows_indexed",
            jobs.full_text_rows_indexed,
        ),
        ("host.jobs.recoveries_run", jobs.recoveries_run),
        ("host.jobs.rebuilds_run", jobs.rebuilds_run),
        ("host.jobs.from_zero_builds", jobs.from_zero_builds),
    ]
    .into_iter()
    .map(|(name, value)| (name.to_string(), value))
    .collect()
}

/// The settings file a port refusal points at, as a person would find it.
fn settings_file_named(serve: &Serve, dirs: &ConfigDirs) -> String {
    match &serve.config {
        Some(path) => path.display().to_string(),
        None => format!("config.toml in {}", dirs.config_dir().display()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A start whose read of the token file refuses, after a mint that read
    /// it, refuses the start rather than serving fail-closed: an unreadable
    /// token file refuses a start, however the moment between came to change
    /// it. The refusal names the file and the fix.
    #[test]
    #[allow(clippy::disallowed_methods)] // Harness scaffolding: a token file whose mode the API never writes.
    fn a_start_whose_read_of_the_token_file_refuses_is_refused() {
        use std::os::unix::fs::PermissionsExt;

        let scratch = norn_testkit::scratch::Scratch::new("norn-serve-start-read");
        std::fs::create_dir_all(scratch.join("config")).expect("a config base");
        let dirs = ConfigDirs::new(scratch.join("config"), scratch.join("data")).expect("bases");
        norn_config::machine::tokens::mutate(&dirs, |tokens| {
            tokens.add(
                &norn_config::machine::tokens::TokenLabel::new("default").unwrap(),
                b"secret",
            )
        })
        .expect("a token");
        assert!(readable(&dirs, WatchedTokens::read(&dirs)).is_ok());

        let file = credentials::tokens_file(&dirs);
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o640)).unwrap();
        let Err(Refusal { reason, .. }) = readable(&dirs, WatchedTokens::read(&dirs)) else {
            panic!("a token file that does not read refused no start");
        };
        assert!(
            reason.contains(&file) && reason.contains("chmod 600"),
            "{reason}"
        );
    }
}
