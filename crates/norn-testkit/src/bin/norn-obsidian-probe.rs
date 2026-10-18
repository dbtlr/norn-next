//! Install the Obsidian link probe into a vault for a manual Obsidian run.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use norn_testkit::obsidian_probe::Probe;

#[derive(Parser)]
#[command(
    name = "norn-obsidian-probe",
    about = "Install the Obsidian link probe, with each name at the code points cases.json states"
)]
struct Cli {
    /// The root of the Obsidian vault to install into. It must not already
    /// hold the probe's folder.
    vault: PathBuf,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let installed = Probe::workspace().and_then(|probe| {
        probe.install(&cli.vault)?;
        Ok(cli.vault.join(probe.probe_root()))
    });
    match installed {
        Ok(root) => {
            println!("installed the probe at {}", root.display());
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("norn-obsidian-probe: {error}");
            ExitCode::FAILURE
        }
    }
}
