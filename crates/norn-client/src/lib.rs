#![forbid(unsafe_code)]
//! Everything outside the host.
//!
//! The crate map in `docs/architecture.md` gives this crate argv, loopback
//! routing, the hello's dialer half, auto-launch, the projections onto the
//! render seam and the machine-local verbs. **It lands carrying only the
//! `serve` command's argument grammar** — `--port`, `--config` and the hidden
//! auto-launch marker — and the endpoint resolution every dialer will stand
//! on. Every other command arrives with the surfaces (6.3).
//!
//! [`parse`] reads argv and answers an [`Invocation`]; the composition root
//! hands a [`Invocation::Serve`] to `norn-serve`'s entry, which this crate has
//! no edge to (boundary invariant 1). Until `norn-console` exists, a usage
//! error and `norn serve --help` are clap's stock output.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use clap::{Args, Parser, Subcommand};
use norn_config::ConfigDirs;
use norn_config::machine::Endpoint;
use norn_config::machine::settings::{ServingSettings, SettingsError, SettingsSource};

/// What one invocation of the `norn` binary asks for.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Invocation {
    /// Run the host process for this channel.
    Serve(ServeArgs),
}

/// The `norn serve` arguments, as parsed.
#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct ServeArgs {
    /// Listen on this port instead of the configured one, for this invocation.
    #[arg(
        long,
        value_name = "N",
        value_parser = clap::value_parser!(u16).range(1..),
        conflicts_with = "auto_launched"
    )]
    pub port: Option<u16>,
    /// Read serving settings from this file instead of the channel's config.toml.
    #[arg(long, value_name = "PATH")]
    pub config: Option<PathBuf>,
    /// The auto-launch marker. Hidden: only a client launching a host passes
    /// it, and such a host binds only its resolved endpoint, which is why it
    /// conflicts with `--port`.
    #[arg(long, hide = true)]
    pub auto_launched: bool,
}

#[derive(Parser, Debug)]
#[command(name = "norn", about = "Deterministic Markdown vault engine")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Run the host process for this channel.
    Serve(ServeArgs),
}

/// Parse argv (program name first) into an [`Invocation`], or the usage error
/// or help clap answers with.
pub fn try_parse<I, T>(args: I) -> Result<Invocation, clap::Error>
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    let cli = Cli::try_parse_from(args)?;
    Ok(match cli.command {
        Command::Serve(serve) => Invocation::Serve(serve),
    })
}

/// Parse argv, or end the process the way clap does: help on stdout with
/// status 0, a usage error on stderr with status 2.
pub fn parse<I, T>(args: I) -> Invocation
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    try_parse(args).unwrap_or_else(|error| error.exit())
}

/// The endpoint a client dials: the channel default, or `[endpoint]` from the
/// channel's `config.toml` or from the file `config` names.
///
/// **A dormant carrier for the 6.3 dialer.** Loopback routing and auto-launch
/// dial this endpoint, and neither is built, so nothing in the shipped binary
/// calls it yet. It is here now because a host and a client reading one file
/// must agree, and the one way to keep them agreeing is that both resolve
/// through the same `norn-config` read; the `norn` package's process suite
/// holds it to the port `norn serve` listens on from the same file.
pub fn endpoint(dirs: &ConfigDirs, config: Option<&Path>) -> Result<Endpoint, SettingsError> {
    let source = config.map_or(SettingsSource::ChannelDefault, SettingsSource::Explicit);
    ServingSettings::read(dirs, source).map(|(settings, _)| settings.endpoint)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn serve(args: &[&str]) -> Result<ServeArgs, clap::Error> {
        let argv = ["norn", "serve"].iter().chain(args).copied();
        try_parse(argv).map(|Invocation::Serve(serve)| serve)
    }

    #[test]
    fn serve_takes_a_port_and_a_config_path() {
        assert_eq!(
            serve(&["--port", "4100", "--config", "/elsewhere/config.toml"]).unwrap(),
            ServeArgs {
                port: Some(4100),
                config: Some(PathBuf::from("/elsewhere/config.toml")),
                auto_launched: false,
            }
        );
    }

    #[test]
    fn port_zero_is_a_usage_error() {
        let error = serve(&["--port", "0"]).unwrap_err();
        assert_eq!(error.exit_code(), 2);
    }

    #[test]
    fn the_auto_launch_marker_conflicts_with_a_port() {
        let error = serve(&["--auto-launched", "--port", "4100"]).unwrap_err();
        assert_eq!(error.kind(), clap::error::ErrorKind::ArgumentConflict);
        assert_eq!(error.exit_code(), 2);
        assert!(serve(&["--auto-launched"]).unwrap().auto_launched);
    }

    #[test]
    fn the_auto_launch_marker_is_hidden_from_help() {
        let help = serve(&["--help"]).unwrap_err().to_string();
        assert!(help.contains("--port"), "{help}");
        assert!(!help.contains("--auto-launched"), "{help}");
    }

    #[test]
    fn a_command_other_than_serve_is_a_usage_error() {
        let error = try_parse(["norn", "find"]).unwrap_err();
        assert_eq!(error.exit_code(), 2);
    }
}
