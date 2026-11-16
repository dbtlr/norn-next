//! Print the `run:` steps of a workflow's Linux jobs for `scripts/gate.sh`.
//!
//! The output is one NUL-delimited record per step, in file order, so a shell
//! can read a multi-line command back whole: the job key, the job's display
//! name, the step name, the count of environment entries, each entry as
//! `KEY=value`, and the command.

use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use norn_testkit::workflows::{GateStep, linux_run_steps};

#[derive(Parser)]
#[command(
    name = "norn-gate-steps",
    about = "Print the run steps of a workflow's Linux jobs as NUL-delimited records"
)]
struct Cli {
    /// The workflow to read.
    #[arg(default_value = ".github/workflows/ci.yml")]
    workflow: PathBuf,
}

/// The records of `steps`, each field followed by a NUL.
fn records(steps: &[GateStep]) -> Vec<u8> {
    let mut out = Vec::new();
    for step in steps {
        let mut fields = vec![
            step.job.clone(),
            step.job_name.clone(),
            step.name.clone(),
            step.env.len().to_string(),
        ];
        fields.extend(step.env.iter().map(|(key, value)| format!("{key}={value}")));
        fields.push(step.command.clone());
        for field in fields {
            out.extend_from_slice(field.as_bytes());
            out.push(0);
        }
    }
    out
}

#[allow(clippy::disallowed_methods)] // This command line reads the workflow it is handed.
fn main() -> ExitCode {
    let cli = Cli::parse();
    let read = std::fs::read_to_string(&cli.workflow)
        .map_err(|e| format!("reading {}: {e}", cli.workflow.display()))
        .and_then(|text| linux_run_steps(&text));
    match read {
        Ok(steps) => {
            let mut stdout = std::io::stdout().lock();
            match stdout
                .write_all(&records(&steps))
                .and_then(|()| stdout.flush())
            {
                Ok(()) => ExitCode::SUCCESS,
                Err(error) => {
                    eprintln!("norn-gate-steps: {error}");
                    ExitCode::FAILURE
                }
            }
        }
        Err(error) => {
            eprintln!("norn-gate-steps: {error}");
            ExitCode::FAILURE
        }
    }
}
