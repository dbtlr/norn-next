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
//! **No async type reaches `norn-host`** (ruling R2): tokio and hyper live in
//! this crate alone, and the host is built and dropped on ordinary threads.
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

mod credentials;
pub mod dial;
mod http;
mod log;
mod mint;
mod policy;
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
#[derive(Clone, Copy, Debug)]
enum Stop {
    Terminate,
    Interrupt,
}

impl fmt::Display for Stop {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Stop::Terminate => "SIGTERM",
            Stop::Interrupt => "SIGINT",
        })
    }
}

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
            ExitCode::SUCCESS
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

    let context = Arc::new(http::Context {
        credentials: Mutex::new(credentials::Credentials::new(&dirs, watched)),
        channel: Channel::COMPILED.into(),
        build,
        launch: serve.launch.into(),
    });

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

        let stop = loop {
            tokio::select! {
                accepted = listener.accept() => match accepted {
                    Ok((stream, _)) => {
                        tokio::spawn(http::answer(stream, Arc::clone(&context)));
                    }
                    Err(error) => {
                        log(format_args!("warning: accepting a connection failed: {error}"));
                        tokio::time::sleep(ACCEPT_RETRY_PAUSE).await;
                    }
                },
                _ = terminate.recv() => break Stop::Terminate,
                _ = interrupt.recv() => break Stop::Interrupt,
            }
        };
        // A plain stop: the listener closes, and the host's own destruction joins
        // its workers. The bounded drain that answers what is still in flight is
        // a later task of this phase.
        drop(listener);
        Ok(stop)
    }
    .await;
    // Destruction joins workers, including on a bind refusal. Keep the lock
    // until the host has gone, and leave the I/O thread free for signals.
    tokio::task::spawn_blocking(move || {
        drop(host);
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
/// moment between — is held like any later refusal: the host fails closed
/// from its first request and logs it, rather than refusing a start whose
/// mint just read the file.
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
    Ok(WatchedTokens::read(dirs))
}

/// Bind and listen on exactly `address`, with `SO_REUSEADDR` so a socket this
/// channel's previous host left in TIME_WAIT does not refuse the start, and
/// without `SO_REUSEPORT`, so a second listener cannot share the port.
fn bind(address: std::net::SocketAddr) -> std::io::Result<TcpListener> {
    let socket = TcpSocket::new_v4()?;
    socket.set_reuseaddr(true)?;
    socket.bind(address)?;
    socket.listen(1024)
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
