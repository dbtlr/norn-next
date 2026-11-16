#![forbid(unsafe_code)]
//! The HTTP surface and the host process it runs in.
//!
//! `norn serve` is the one way a host process starts (ADR 0053), and [`run`]
//! is its entry: the composition root parses and hands it the `serve` arguments
//! once its signal handlers are installed
//! and runs it in its own process — a process entry, not a request.
//!
//! **What runs is the start, the credential and a plain stop.** [`run`] reads
//! the serving settings, takes the channel lock before any other start-up
//! work, mints the bearer token where the file holds none (the private `mint`
//! module), reads the registry, constructs the host, binds the exact resolved
//! loopback address and logs `listening`; SIGTERM or SIGINT then logs
//! `stopped` and exits 0. In front of every route stand the hello and the
//! bearer middleware (`http.rs`, over the credential state in
//! `credentials.rs`); behind them an authenticated request is answered 404
//! until the wire route lands, its dispatch reserved in `wire.rs`, and under
//! the `counters` feature `GET /v1/debug/counters` answers the `serve.*`
//! counters. The [`dial`] module is the takeover dial's hello. The takeover
//! of a held lock, the port probe, the bounded drain and the start-bound
//! watchdog are later tasks of the same phase, and each is named where its
//! absence shows.
//!
//! **No async type reaches `norn-host`** (ruling R2): the host is built,
//! called and dropped on ordinary threads, and nothing it is handed is a
//! tokio or hyper type. tokio and hyper run this crate's server and its
//! takeover dial; `norn-client`'s hello dialer runs on them too, and the rule
//! holds there the same way.
//!
//! **The server log is stderr**, one plain-text line per lifecycle event
//! (the private `log` module). It is a diagnostic stream, not rendered output.

use std::fmt;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use norn_config::machine::settings::{ServingSettings, SettingsSource};
use norn_config::machine::tokens::WatchedTokens;
use norn_config::{Channel, ChannelLock, ConfigDirs};
use norn_host::{Host, ProductionEntryOps, RegistryRead};
use norn_wire::{BuildIdentity, HelloLaunch};
use tokio::net::{TcpListener, TcpSocket};
use tokio::signal::unix::{Signal, SignalKind, signal};

mod admission;
mod connection;
mod credentials;
pub mod dial;
mod http;
mod log;
mod mint;
mod policy;
mod pool;
mod request;
mod stop;
mod wire;

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

/// The status a start that refused, or a host that stopped abnormally, exits
/// with.
const REFUSED: u8 = 1;

/// How long the accept loop rests after a failed accept, so a condition that
/// fails every accept (descriptor exhaustion) is not a busy loop.
const ACCEPT_RETRY_PAUSE: Duration = Duration::from_millis(100);

/// A start that cannot go on: what refused it and how to resolve it, as the
/// one log line the refusal is.
#[derive(Debug)]
struct Refusal(String);

/// What ended a serving host.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Stop {
    Terminate,
    Interrupt,
    /// The host's worker stopped, so it can no longer serve: the process
    /// exits 1, to be started again.
    WorkerStopped,
}

impl Stop {
    /// The status the process exits with after this stop.
    fn status(self) -> u8 {
        match self {
            Stop::Terminate | Stop::Interrupt => 0,
            Stop::WorkerStopped => REFUSED,
        }
    }
}

impl fmt::Display for Stop {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Stop::Terminate => "SIGTERM",
            Stop::Interrupt => "SIGINT",
            Stop::WorkerStopped => "the host's worker stopped",
        })
    }
}

/// Open connections at once; past it accept pauses, and further connects wait
/// in the listen backlog, then the kernel refuses them.
const CONNECTION_BOUND: usize = 512;

/// The listen backlog connects wait in while accept is paused.
const LISTEN_BACKLOG: u32 = 64;

/// Run the host process for this channel until a signal stops it.
///
/// Exits 0 after a signal stopped a listening host, and 1 for every refused
/// start: a bad settings file, a held or refused channel lock, an unreadable
/// token file, an unreadable registry, a held port, or a signal before
/// listening.
///
/// The build identity is captured by the bin. Argument parsing is deferred
/// until signal handlers are installed.
pub fn run(build: &'static str, arguments: impl FnOnce() -> Serve) -> ExitCode {
    match serve_channel(BuildIdentity::new(build), arguments) {
        Ok(stop) => {
            log(format_args!("stopped: {stop}"));
            ExitCode::from(stop.status())
        }
        Err(Refusal(reason)) => {
            log(format_args!("refused: {reason}"));
            ExitCode::from(REFUSED)
        }
    }
}

fn serve_channel(build: BuildIdentity, arguments: impl FnOnce() -> Serve) -> Result<Stop, Refusal> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .enable_time()
        .build()
        .map_err(|error| Refusal(format!("starting the I/O runtime failed: {error}")))?;
    let stopped = runtime.block_on(async {
        // Install before argv parsing or any filesystem startup work.
        let mut terminate = stop_signal(SignalKind::terminate())?;
        let mut interrupt = stop_signal(SignalKind::interrupt())?;
        let serve = arguments();
        let settings_args = serve.clone();
        let preparation = tokio::task::spawn_blocking(move || {
            let dirs = ConfigDirs::from_environment().map_err(|error| {
                Refusal(format!(
                    "{error}; set HOME, or XDG_CONFIG_HOME and XDG_DATA_HOME, to absolute paths"
                ))
            })?;
            let settings = settings(&dirs, &settings_args)?;
            Ok::<_, Refusal>((dirs, settings))
        });
        let (dirs, settings) = tokio::select! {
            biased;
            _ = terminate.recv() => return Err(before_listening(Stop::Terminate)),
            _ = interrupt.recv() => return Err(before_listening(Stop::Interrupt)),
            prepared = preparation => prepared
                .map_err(|error| Refusal(format!("reading settings failed: {error}")))??,
        };
        serve_on_runtime(build, &serve, dirs, settings, terminate, interrupt).await
    });
    // A host construction a signal cut off is still running on its blocking
    // thread, holding the channel lock; it is not waited for. The process
    // exits next, and the kernel frees the lock.
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
        .map_err(|error| Refusal(error.to_string()))?;
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
    mut terminate: Signal,
    mut interrupt: Signal,
) -> Result<Stop, Refusal> {
    let endpoint = settings.endpoint;
    let settings_for_wire = WireSettings {
        read_connections: settings.read_connections,
        ready_wait: settings.ready_wait,
    };
    let construction = {
        let dirs = dirs.clone();
        tokio::task::spawn_blocking(move || construct(dirs, &settings))
    };
    let (lock, host, watched) = tokio::select! {
        biased;
        _ = terminate.recv() => return Err(before_listening(Stop::Terminate)),
        _ = interrupt.recv() => return Err(before_listening(Stop::Interrupt)),
        built = construction => built
            .map_err(|error| Refusal(format!("constructing the host failed: {error}")))??,
    };

    let route = Arc::new(wire::HostRoute { host });
    let stopper = Arc::new(stop::Stopper::default());
    let wire_state = wire_state(
        &settings_for_wire,
        Arc::clone(&route) as Arc<dyn request::Dispatch>,
    )
    .map_err(|error| {
        Refusal(format!(
            "starting the host's request threads failed: {error}"
        ))
    })?;
    let context = Arc::new(http::Context {
        credentials: Mutex::new(credentials::Credentials::new(&dirs, watched)),
        channel: Channel::COMPILED.into(),
        build,
        launch: serve.launch.into(),
        wire: Arc::new(wire_state),
        stopper: Arc::clone(&stopper),
        bounds: connection::Bounds::SERVING,
        #[cfg(feature = "counters")]
        readings: {
            let route = Arc::clone(&route);
            Box::new(move || host_readings(route.host.evidence()))
        },
    });
    drop(route);

    let result = async {
        let listener = bind(endpoint.address()).map_err(|error| {
            if error.kind() == std::io::ErrorKind::AddrInUse {
                Refusal(format!(
                    "port {} on {} is held by another process; choose another with `--port N`, or \
                 with `[endpoint] port` in {}",
                    endpoint.port(),
                    endpoint.address().ip(),
                    settings_file_named(serve, &dirs),
                ))
            } else {
                Refusal(format!("binding {endpoint} failed: {error}"))
            }
        })?;
        log(format_args!(
            "listening: channel {}, port {}, {} start, build {build}",
            Channel::COMPILED.app_directory(),
            endpoint.port(),
            serve.launch,
        ));

        let open = Arc::new(tokio::sync::Semaphore::new(CONNECTION_BOUND));
        let mut connections = tokio::task::JoinSet::new();
        let stop = loop {
            tokio::select! {
                (slot, accepted) = accept_under(&listener, &open) => match accepted {
                    Ok((stream, _)) => {
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
                _ = terminate.recv() => break Stop::Terminate,
                _ = interrupt.recv() => break Stop::Interrupt,
                // The stopped-worker trigger takes the plain stop a signal
                // takes; the bounded drain routes it through itself.
                () = stopper.requested() => break Stop::WorkerStopped,
            }
        };
        // A plain stop: the listener closes and every connection with it, and
        // the host's own destruction joins its workers. The bounded drain that
        // answers what is still in flight is a later task of this phase.
        drop(listener);
        connections.shutdown().await;
        Ok(stop)
    }
    .await;
    // Destruction joins the request threads and the host's workers, including
    // on a bind refusal. Keep the lock until the host has gone, and leave the
    // I/O thread free for signals.
    tokio::task::spawn_blocking(move || {
        drop(context);
        drop(lock);
    })
    .await
    .map_err(|error| Refusal(format!("stopping the host failed: {error}")))?;
    result
}

/// Step 2 and the first half of step 5: the channel lock, the token minted
/// where the file holds none and the set read, then the registry read and the
/// host built over it. The host's startup sweep runs inside `Host::new`.
fn construct(
    dirs: ConfigDirs,
    settings: &ServingSettings,
) -> Result<(ChannelLock, Host<ProductionEntryOps>, WatchedTokens), Refusal> {
    let lock = match ChannelLock::try_take(&dirs) {
        Ok(Some(lock)) => lock,
        // Waiting out the holder and taking over an auto-launched one is a
        // later task; until it lands a held lock refuses.
        Ok(None) => {
            return Err(Refusal(format!(
                "a norn host already holds this channel's lock in {}; stop that host before \
                 starting another (deleting the lock file does not stop it)",
                dirs.config_dir().display()
            )));
        }
        Err(error) => {
            return Err(Refusal(format!(
                "the channel lock could not be taken: {error}"
            )));
        }
    };
    let watched = mint_and_read(&dirs)?;
    let registry = RegistryRead::load(&dirs).map_err(|error| {
        // An error that already names the file is not prefixed with it again.
        let path = dirs.config_dir().join("registry.toml");
        let error = error.to_string();
        let named = if error.contains(&path.display().to_string()) {
            error
        } else {
            format!("{}: {error}", path.display())
        };
        Refusal(format!(
            "{named}; repair the registry file, or move it aside to start with no vaults"
        ))
    })?;
    let production = policy::production(settings)
        .map_err(|error| Refusal(format!("the serving settings are not usable: {error}")))?;
    let host = Host::new(
        registry,
        ProductionEntryOps::new(dirs, production),
        policy::lifecycle(settings),
    )
    .map_err(|error| Refusal(format!("the serving settings are not usable: {error}")))?;
    Ok((lock, host, watched))
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
        Refusal(format!(
            "drawing a token from the operating system's random source failed: {error}"
        ))
    })?;
    match mint::mint(dirs, &secret) {
        Ok(mint::Mint::Held) => {}
        Ok(mint::Mint::Minted) => log(format_args!("minted the `default` token into {file}")),
        Ok(mint::Mint::MintedUnconfirmed(error)) => log(format_args!(
            "minted the `default` token into {file}; warning: {error}"
        )),
        Err(error) => return Err(Refusal(credentials::unreadable(&file, &error))),
    }
    readable(dirs, WatchedTokens::read(dirs))
}

/// `watched`, or the start's refusal when the read it holds refused: the file
/// and the fix, as the mint's own refusal names them.
fn readable(dirs: &ConfigDirs, watched: WatchedTokens) -> Result<WatchedTokens, Refusal> {
    if let Err(error) = watched.current() {
        let file = credentials::tokens_file(dirs);
        return Err(Refusal(credentials::unreadable(&file, error)));
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
) -> std::io::Result<request::Wire> {
    Ok(request::Wire {
        admission: Arc::default(),
        semaphores: Arc::default(),
        reads: pool::Pool::new(
            "norn-read",
            settings.read_connections + pool::READ_POOL_MARGIN,
        )?,
        mutations: pool::Pool::new("norn-mutation", pool::MUTATION_POOL_THREADS)?,
        dispatcher,
        ready_wait: settings.ready_wait,
        counters: Arc::default(),
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

fn stop_signal(kind: SignalKind) -> Result<Signal, Refusal> {
    signal(kind).map_err(|error| Refusal(format!("installing a signal handler failed: {error}")))
}

fn before_listening(stop: Stop) -> Refusal {
    Refusal(format!("{stop} arrived before the host was listening"))
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

    /// A stopped worker ends the process 1, to be started again; a signal's
    /// stop ends it 0.
    #[test]
    fn a_stopped_worker_exits_1_and_a_signal_exits_0() {
        assert_eq!(Stop::WorkerStopped.status(), 1);
        assert_eq!(Stop::Terminate.status(), 0);
        assert_eq!(Stop::Interrupt.status(), 0);
    }

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
        let Err(Refusal(reason)) = readable(&dirs, WatchedTokens::read(&dirs)) else {
            panic!("a token file that does not read refused no start");
        };
        assert!(
            reason.contains(&file) && reason.contains("chmod 600"),
            "{reason}"
        );
    }
}
