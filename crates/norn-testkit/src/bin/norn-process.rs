#[cfg(unix)]
use std::ffi::OsString;
#[cfg(unix)]
use std::num::NonZeroU64;
#[cfg(unix)]
use std::os::fd::RawFd;
use std::process::ExitCode;
#[cfg(unix)]
use std::time::Duration;

#[cfg(unix)]
use clap::{Parser, Subcommand};
#[cfg(unix)]
use norn_testkit::process::{LaunchRequest, SuperviseRequest, launch, supervise};

#[cfg(unix)]
#[derive(Parser)]
#[command(name = "norn-process", about = "Own development process groups")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[cfg(unix)]
#[derive(Subcommand)]
enum Command {
    /// Run a development workload in a registered process group.
    Supervise {
        /// Short reason for this workload.
        #[arg(long, value_parser = non_empty_purpose)]
        purpose: String,
        /// Maximum workload time in seconds.
        #[arg(long)]
        deadline_seconds: NonZeroU64,
        /// Program and arguments to run.
        #[arg(required = true, trailing_var_arg = true, allow_hyphen_values = true)]
        command: Vec<OsString>,
    },
    #[command(name = "__launch", hide = true)]
    Launch {
        #[arg(long)]
        release_fd: RawFd,
        #[arg(long)]
        status_fd: RawFd,
        #[arg(required = true, trailing_var_arg = true, allow_hyphen_values = true)]
        command: Vec<OsString>,
    },
}

#[cfg(unix)]
fn non_empty_purpose(value: &str) -> Result<String, String> {
    if value.trim().is_empty() {
        Err("the purpose must contain a non-space character".to_string())
    } else {
        Ok(value.to_string())
    }
}

#[cfg(unix)]
fn main() -> ExitCode {
    let result = match Cli::parse().command {
        Command::Supervise {
            purpose,
            deadline_seconds,
            command,
        } => supervise(SuperviseRequest {
            purpose,
            deadline: Duration::from_secs(deadline_seconds.get()),
            command,
        }),
        Command::Launch {
            release_fd,
            status_fd,
            command,
        } => launch(LaunchRequest {
            release_fd,
            status_fd,
            command,
        }),
    };

    match result {
        Ok(code) => code,
        Err(error) => {
            eprintln!("norn-process: {error}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(not(unix))]
fn main() -> ExitCode {
    eprintln!("norn-process: process-group supervision requires Unix");
    ExitCode::FAILURE
}
