//! Dependency-free build-script support, included by the measured packages.
//! It intentionally adds no build dependency (fixtures is an isolated leaf).

use std::path::{Path, PathBuf};

/// Conservatively recognize a profile table or dotted/inline assignment without
/// a TOML dependency. Quoted first keys containing escapes are also refused:
/// they can spell `profile`. A matching line inside a multiline string can be
/// a false positive; rejecting an ambiguous configuration is safer than silently
/// accepting a dependency-only optimization.
fn profile_declaration(text: &str) -> Option<&str> {
    text.lines().map(str::trim).find(|line| {
        let key = line.trim_start_matches('[').trim_start();
        if line.starts_with('#') || key.is_empty() {
            return false;
        }
        let rest = if let Some(quote @ ('\"' | '\'')) = key.chars().next() {
            let Some(end) = key[1..].find(quote) else {
                return false;
            };
            let name = &key[1..=end];
            if name.contains('\\') {
                return true;
            }
            if name != "profile" {
                return false;
            }
            &key[end + 2..]
        } else if let Some(rest) = key.strip_prefix("profile") {
            rest
        } else {
            return false;
        };
        rest.trim_start().starts_with(['.', ']', '='])
    })
}

#[allow(clippy::disallowed_macros)] // Cargo build-script directives, not product rendering.
fn read_env(name: &str) -> Option<std::ffi::OsString> {
    println!("cargo:rerun-if-env-changed={name}");
    std::env::var_os(name)
}

#[allow(clippy::disallowed_macros, clippy::disallowed_methods)] // Inspect build configuration and emit Cargo dependency directives.
fn inspect_file(path: &Path, overrides: &mut Vec<String>) {
    println!("cargo:rerun-if-changed={}", path.display());
    match std::fs::read_to_string(path) {
        Ok(text) => {
            if let Some(declaration) = profile_declaration(&text) {
                overrides.push(format!("{}: {declaration}", path.display()));
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => overrides.push(format!(
            "{}: cannot inspect profiles: {error}",
            path.display()
        )),
    }
}

/// Record the measured package's build, not the helper crate's build. Cargo
/// exposes the effective OPT_LEVEL for both its tests and bins. Host memory
/// children are copies of current_exe(), so they retain exactly this provenance.
///
/// Cargo does not expose its CLI --config arguments or config include graph to
/// build scripts. Those can hide dependency-only overrides and must not be used
/// for measurement runs. Environment inspection covers variables visible here.
/// A relative CARGO_HOME cannot be resolved against Cargo's invocation directory
/// here, so it is refused rather than risking an uninspected profile override.
#[allow(clippy::disallowed_macros)] // Cargo build-script protocol, not product rendering.
pub fn emit() {
    let opt_level = read_env("OPT_LEVEL").expect("Cargo supplies OPT_LEVEL");
    println!(
        "cargo:rustc-env=NORN_MEASURED_OPT_LEVEL={}",
        opt_level.to_string_lossy()
    );
    let manifest_dir =
        PathBuf::from(read_env("CARGO_MANIFEST_DIR").expect("Cargo supplies manifest directory"));
    let root = manifest_dir
        .parent()
        .and_then(Path::parent)
        .expect("workspace package under crates");
    let mut overrides = Vec::new();
    for (name, _) in std::env::vars_os() {
        if name.to_string_lossy().starts_with("CARGO_PROFILE_") {
            let name = name.to_string_lossy();
            let value = read_env(&name).expect("enumerated environment variable");
            overrides.push(format!("{name}={}", value.to_string_lossy()));
        }
    }
    inspect_file(&root.join("Cargo.toml"), &mut overrides);
    for ancestor in root.ancestors() {
        for name in ["config.toml", "config"] {
            inspect_file(&ancestor.join(".cargo").join(name), &mut overrides);
        }
    }
    let cargo_home = read_env("CARGO_HOME");
    let home = read_env("HOME").or_else(|| read_env("USERPROFILE"));
    if let Some(home) = cargo_home
        .map(PathBuf::from)
        .or_else(|| home.map(|p| PathBuf::from(p).join(".cargo")))
    {
        if home.is_relative() {
            overrides.push(
                "relative Cargo home: use an absolute CARGO_HOME/HOME to inspect profile overrides"
                    .to_owned(),
            );
        } else {
            for name in ["config.toml", "config"] {
                inspect_file(&home.join(name), &mut overrides);
            }
        }
    } else {
        overrides.push("cannot locate Cargo home to inspect profile overrides".to_owned());
    }
    // Cargo cannot watch an environment-variable prefix. A deliberately absent
    // file makes this lightweight scan run on every Cargo invocation, including
    // when a previously unseen CARGO_PROFILE_* variable or config file appears.
    let out_dir = PathBuf::from(read_env("OUT_DIR").expect("Cargo supplies OUT_DIR"));
    println!(
        "cargo:rerun-if-changed={}",
        out_dir.join("profile-provenance-refresh").display()
    );
    overrides.sort();
    println!(
        "cargo:rustc-env=NORN_MEASURED_PROFILE_OVERRIDES={}",
        overrides.join("; ").replace(['\n', '\r'], " ")
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_declarations_include_dependency_tables_and_inline_forms() {
        for text in [
            "[profile.dev.package.\"*\"]\nopt-level = 3",
            "[ 'profile' . test ]",
            "profile = { dev = { opt-level = 1 } }",
            "profile.dev.opt-level = 1",
            "[profile]\ndev = {opt-level = 1}",
            "[\"pro\\u0066ile\".test]",
        ] {
            assert!(profile_declaration(text).is_some(), "{text}");
        }
        for text in [
            "# [profile.dev]",
            "[build]\njobs = 2",
            "[package]\nname = 'profile'",
            "[profiles.dev]",
        ] {
            assert_eq!(profile_declaration(text), None, "{text}");
        }
    }
}
