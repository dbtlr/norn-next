#![forbid(unsafe_code)]
//! The generator's command line: generate a tree, measure one, or list the
//! named profiles. Arguments are parsed by hand because the crate is a
//! zero-dependency leaf.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use norn_fixtures::probe::{self, CALIBRATION, Stat};
use norn_fixtures::{Profile, SENTINEL_CONTENT, SENTINEL_FILE, digest, generate};

fn usage() -> String {
    let mut out = String::from(
        "usage:\n  \
         norn-fixtures generate <out-dir> --profile <name> [--seed N]\n  \
         norn-fixtures calibrate <dir>\n  \
         norn-fixtures profiles\n\nprofiles:\n",
    );
    for profile in Profile::all() {
        out.push_str(&format!(
            "  {:<10} {:>5} documents — {}\n",
            profile.name, profile.docs, profile.purpose
        ));
    }
    out
}

/// The generated-tree guard: an absent or empty target generates fresh; a
/// populated target is cleared only when it carries this generator's sentinel,
/// as a regular file with exactly the sentinel bytes. There is no flag that
/// bypasses the check, because the operation it authorises is a recursive
/// delete of a directory somebody named on a command line.
///
/// This is only the sentinel guard: whether the target is a directory and
/// whether it ends up empty are `generate`'s to enforce, and duplicating them
/// here would put the precondition in two places.
fn prepare_target(out_dir: &Path) -> Result<(), String> {
    let meta = match fs::metadata(out_dir) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(format!("cannot stat {}: {e}", out_dir.display())),
    };
    if !meta.is_dir() {
        return Ok(());
    }
    let mut entries =
        fs::read_dir(out_dir).map_err(|e| format!("cannot read {}: {e}", out_dir.display()))?;
    if entries.next().is_none() {
        return Ok(());
    }
    let sentinel = out_dir.join(SENTINEL_FILE);
    let owned = fs::symlink_metadata(&sentinel)
        .map(|m| m.is_file())
        .unwrap_or(false)
        && fs::read(&sentinel)
            .map(|bytes| bytes == SENTINEL_CONTENT.as_bytes())
            .unwrap_or(false);
    if !owned {
        return Err(format!(
            "{} exists, is non-empty, and carries no valid {SENTINEL_FILE} sentinel \
             — refusing to touch it",
            out_dir.display()
        ));
    }
    for entry in
        fs::read_dir(out_dir).map_err(|e| format!("cannot read {}: {e}", out_dir.display()))?
    {
        let entry =
            entry.map_err(|e| format!("cannot read entry in {}: {e}", out_dir.display()))?;
        let path = entry.path();
        let removed = if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            fs::remove_dir_all(&path)
        } else {
            fs::remove_file(&path)
        };
        removed.map_err(|e| format!("cannot remove {}: {e}", path.display()))?;
    }
    Ok(())
}

struct GenerateArgs {
    out_dir: PathBuf,
    profile: String,
    seed: u64,
}

fn parse_generate(argv: &[String]) -> Result<GenerateArgs, String> {
    let mut out_dir: Option<PathBuf> = None;
    let mut profile: Option<String> = None;
    let mut seed: u64 = 0;

    let mut i = 0;
    while i < argv.len() {
        match argv[i].as_str() {
            "--profile" => {
                i += 1;
                profile = Some(argv.get(i).ok_or("--profile requires a value")?.clone());
            }
            "--seed" => {
                i += 1;
                let value = argv.get(i).ok_or("--seed requires a value")?;
                seed = value
                    .parse::<u64>()
                    .map_err(|_| format!("--seed value is not a valid u64: {value}"))?;
            }
            other if out_dir.is_none() => out_dir = Some(PathBuf::from(other)),
            other => return Err(format!("unexpected argument: {other}")),
        }
        i += 1;
    }

    Ok(GenerateArgs {
        out_dir: out_dir.ok_or("missing required <out-dir>")?,
        profile: profile.ok_or("missing required --profile <name>")?,
        seed,
    })
}

fn run_generate(argv: &[String]) -> Result<String, (String, u8)> {
    let args = parse_generate(argv).map_err(|e| (format!("{e}\n\n{}", usage()), 2))?;
    let profile = Profile::by_name(&args.profile).ok_or_else(|| {
        (
            format!("unknown profile '{}'\n\n{}", args.profile, usage()),
            2,
        )
    })?;

    prepare_target(&args.out_dir).map_err(|e| (e, 1))?;
    let manifest = generate(&profile, args.seed, &args.out_dir)
        .map_err(|e| (format!("generation failed: {e}"), 1))?;
    let digest = digest::tree_hex(&args.out_dir)
        .map_err(|e| (format!("cannot digest the generated tree: {e}"), 1))?;

    Ok(format!(
        "{}\ntree {digest}\n-> {}",
        manifest.summary_line(),
        args.out_dir.display()
    ))
}

fn run_calibrate(argv: &[String]) -> Result<String, (String, u8)> {
    let [dir] = argv else {
        return Err((format!("calibrate takes one directory\n\n{}", usage()), 2));
    };
    let dir = PathBuf::from(dir);
    let stats =
        probe::measure(&dir).map_err(|e| (format!("cannot measure {}: {e}", dir.display()), 1))?;

    let mut out = format!(
        "{}\n{} documents, {} other files, {} directories\n\n",
        dir.display(),
        stats.documents,
        stats.non_markdown_files,
        stats.directories
    );
    for stat in Stat::all() {
        let observed = stat.read(&stats);
        let verdict = match CALIBRATION.iter().find(|t| t.stat == *stat) {
            None => "        (unconstrained)".to_string(),
            Some(target) if observed < target.min || observed > target.max => {
                format!("  OUTSIDE {}..={}", target.min, target.max)
            }
            Some(target) => format!("   inside {}..={}", target.min, target.max),
        };
        out.push_str(&format!("{:<38} {:>10}{verdict}\n", stat.name(), observed));
    }

    let deviations = probe::check(&stats, CALIBRATION);
    if deviations.is_empty() {
        out.push_str("\nevery constrained statistic is inside its envelope");
        return Ok(out);
    }
    out.push_str(&format!("\n{} outside the envelope:\n", deviations.len()));
    for deviation in &deviations {
        out.push_str(&format!("  {deviation}\n"));
    }
    Err((out, 1))
}

fn run_profiles() -> Result<String, (String, u8)> {
    let mut out = String::new();
    for profile in Profile::all() {
        out.push_str(&format!(
            "{:<10} {:>5} documents — {}\n",
            profile.name, profile.docs, profile.purpose
        ));
    }
    Ok(out.trim_end().to_string())
}

fn run() -> Result<String, (String, u8)> {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let Some((command, rest)) = argv.split_first() else {
        return Err((usage(), 2));
    };
    match command.as_str() {
        "generate" => run_generate(rest),
        "calibrate" => run_calibrate(rest),
        "profiles" => run_profiles(),
        "--help" | "-h" | "help" => Ok(usage().trim_end().to_string()),
        other => Err((format!("unknown command '{other}'\n\n{}", usage()), 2)),
    }
}

fn main() -> ExitCode {
    match run() {
        Ok(text) => {
            println!("{text}");
            ExitCode::SUCCESS
        }
        Err((message, code)) => {
            eprintln!("norn-fixtures: {message}");
            ExitCode::from(code)
        }
    }
}
