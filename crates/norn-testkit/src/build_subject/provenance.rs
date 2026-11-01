//! Runtime profile-override discovery for measurement guards.

use std::path::{Path, PathBuf};

/// Only optimization flags invalidate these bars; unrelated codegen choices
/// such as a linker selection do not establish an optimized measurement subject.
pub(super) fn optimization_flag<'a>(args: impl IntoIterator<Item = &'a str>) -> Option<&'a str> {
    let mut args = args.into_iter();
    let mut level = None;
    while let Some(arg) = args.next() {
        if arg == "-O" {
            level = Some("3");
        } else {
            let option = if arg == "-C" {
                args.next().unwrap_or("")
            } else {
                arg.strip_prefix("-C").unwrap_or("")
            };
            if let Some(value) = option.strip_prefix("opt-level=") {
                level = Some(value);
            } else if option == "opt-level" {
                level = Some(args.next().unwrap_or(""));
            }
        }
    }
    level
}

fn configuration_issue(text: &str) -> Option<String> {
    let value = match text.trim_start_matches('\u{feff}').parse::<toml::Table>() {
        Ok(value) => value,
        Err(error) => return Some(format!("cannot parse Cargo TOML: {error}")),
    };
    if value.get("profile").is_some() {
        return Some("top-level profile declaration".into());
    }
    let optimized = |flags: &toml::Value| {
        if let Some(text) = flags.as_str() {
            optimization_flag(text.split_whitespace()).is_some()
        } else if let Some(args) = flags.as_array() {
            optimization_flag(args.iter().filter_map(toml::Value::as_str)).is_some()
        } else {
            false
        }
    };
    if value
        .get("build")
        .and_then(|build| build.get("rustflags"))
        .is_some_and(optimized)
    {
        return Some("build.rustflags contains optimization flags".into());
    }
    if let Some(targets) = value.get("target").and_then(toml::Value::as_table) {
        for (target, settings) in targets {
            if settings.get("rustflags").is_some_and(optimized) {
                return Some(format!(
                    "target.{target}.rustflags contains optimization flags"
                ));
            }
        }
    }
    None
}

/// Bound work even in an unexpectedly large checkout, refusing an incomplete
/// scan. Symlink directories are not followed, so cycles cannot extend it.
#[allow(clippy::disallowed_methods)] // Harness Cargo configuration discovery.
fn workspace_configs(root: &Path) -> std::io::Result<Vec<PathBuf>> {
    let mut pending = vec![(root.to_path_buf(), 0)];
    let mut paths = Vec::new();
    let mut visited = 0;
    while let Some((dir, depth)) = pending.pop() {
        if depth > 64 {
            return Err(std::io::Error::other(
                "Cargo config walk exceeded 64 levels",
            ));
        }
        for entry in std::fs::read_dir(&dir)? {
            let entry = entry?;
            visited += 1;
            if visited > 50_000 {
                return Err(std::io::Error::other(
                    "Cargo config walk exceeded 50000 entries",
                ));
            }
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name == "target" || (name.starts_with('.') && name != ".cargo") {
                continue;
            }
            if name == ".cargo" {
                // Read named config paths even when .cargo itself is a symlink;
                // do not traverse its contents or follow other directory links.
                for file in ["config.toml", "config"] {
                    paths.push(entry.path().join(file));
                }
            } else if entry.file_type()?.is_dir() {
                pending.push((entry.path(), depth + 1));
            }
        }
    }
    Ok(paths)
}

/// Scan inputs supplied by the caller so configuration decisions can be tested
/// without changing the process environment or touching real files.
fn scan(
    root: &Path,
    env: &[(std::ffi::OsString, std::ffi::OsString)],
    configs: &[PathBuf],
    mut read: impl FnMut(&Path) -> std::io::Result<String>,
) -> Vec<String> {
    let mut overrides = Vec::new();
    for (name, value) in env {
        if name.to_string_lossy().starts_with("CARGO_PROFILE_") {
            overrides.push(format!(
                "{}={}",
                name.to_string_lossy(),
                value.to_string_lossy()
            ));
        }
    }
    let mut paths = vec![root.join("Cargo.toml")];
    paths.extend_from_slice(configs);
    // Match scripts/performance/provenance.py's ancestor and Cargo-home sources.
    for ancestor in root.ancestors() {
        for name in ["config.toml", "config"] {
            paths.push(ancestor.join(".cargo").join(name));
        }
    }
    let lookup = |key: &str| {
        env.iter()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value)
    };
    let home = lookup("CARGO_HOME").map(PathBuf::from).or_else(|| {
        lookup("HOME")
            .or_else(|| lookup("USERPROFILE"))
            .map(|home| PathBuf::from(home).join(".cargo"))
    });
    match home {
        Some(home) if home.is_absolute() => {
            for name in ["config.toml", "config"] {
                paths.push(home.join(name));
            }
        }
        Some(_) => overrides.push(
            "relative Cargo home: use an absolute CARGO_HOME/HOME to inspect profile overrides"
                .into(),
        ),
        None => overrides.push("cannot locate Cargo home to inspect profile overrides".into()),
    }
    paths.sort();
    paths.dedup();
    for path in paths {
        match read(&path) {
            Ok(text) => {
                if let Some(declaration) = configuration_issue(&text) {
                    overrides.push(format!("{}: {declaration}", path.display()));
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => overrides.push(format!(
                "{}: cannot inspect Cargo configuration: {error}",
                path.display()
            )),
        }
    }
    overrides.sort();
    overrides
}

/// Read overrides at test run time, including newly added variables and files.
#[allow(clippy::disallowed_methods)] // Harness configuration, not vault I/O.
pub(super) fn current_overrides() -> String {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("workspace package under crates");
    let configs = match workspace_configs(root) {
        Ok(paths) => paths,
        Err(error) => {
            return format!(
                "{}: cannot discover Cargo configuration: {error}",
                root.display()
            );
        }
    };
    scan(
        root,
        &std::env::vars_os().collect::<Vec<_>>(),
        &configs,
        |path| std::fs::read_to_string(path),
    )
    .join("; ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn optimization_spellings_are_refused_but_linker_flags_are_accepted() {
        for args in [
            vec!["-O"],
            vec!["-C", "opt-level=0"],
            vec!["-Copt-level=s"],
            vec!["-C", "opt-level", "3"],
        ] {
            assert!(optimization_flag(args).is_some());
        }
        for text in [
            "[build]\nrustflags=['-C','linker=clang']",
            "[package]\nname='profile'",
            "# [profile.dev]",
            "[profiles.dev]",
        ] {
            assert_eq!(configuration_issue(text), None, "{text}");
        }
        for text in [
            "[ 'profile' . test ]",
            "profile.dev.opt-level=1",
            "[\"pro\\u0066ile\".test]",
        ] {
            assert!(configuration_issue(text).is_some(), "{text}");
        }
    }

    #[test]
    #[allow(clippy::disallowed_methods)] // Isolated harness configuration tree.
    fn nested_configs_are_scanned_without_build_or_hidden_trees() {
        let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.scratch");
        std::fs::create_dir_all(&base).unwrap();
        let tree = crate::scratch::Scratch::under(&base, "config-discovery").unwrap();
        let root = tree.root();
        for dir in [
            "crates/host/.cargo",
            "target/nested/.cargo",
            ".git/.cargo",
            ".hidden/.cargo",
        ] {
            std::fs::create_dir_all(root.join(dir)).unwrap();
            std::fs::write(
                root.join(dir).join("config.toml"),
                "[profile.dev]\nopt-level=1",
            )
            .unwrap();
        }
        let configs = workspace_configs(root).unwrap();
        assert_eq!(configs.len(), 2);
        let env = vec![(
            "CARGO_HOME".into(),
            root.join("cargo-home").into_os_string(),
        )];
        let overrides = scan(root, &env, &configs, |path| std::fs::read_to_string(path));
        assert_eq!(overrides.len(), 1, "{overrides:?}");
        assert!(overrides[0].contains("crates/host/.cargo/config.toml"));
    }

    #[test]
    fn runtime_scan_covers_environment_manifest_ancestors_and_cargo_home() {
        let env = vec![
            ("HOME".into(), "/home/test".into()),
            ("CARGO_PROFILE_TEST_OPT_LEVEL".into(), "0".into()),
        ];
        let overrides = scan(Path::new("/workspace/repo"), &env, &[], |path| {
            if [
                "/workspace/repo/Cargo.toml",
                "/workspace/.cargo/config",
                "/home/test/.cargo/config.toml",
                "/home/test/.cargo/config",
            ]
            .iter()
            .any(|name| path == Path::new(name))
            {
                Ok("[profile.dev.package.\"*\"]\nopt-level = 3".into())
            } else {
                Err(std::io::ErrorKind::NotFound.into())
            }
        });
        assert_eq!(overrides.len(), 5, "{overrides:?}");
        for source in [
            "CARGO_PROFILE_TEST_OPT_LEVEL=0",
            "/workspace/repo/Cargo.toml:",
            "/workspace/.cargo/config:",
            "/home/test/.cargo/config.toml:",
            "/home/test/.cargo/config:",
        ] {
            assert!(
                overrides.iter().any(|value| value.contains(source)),
                "{overrides:?}"
            );
        }
    }

    #[test]
    fn unreadable_configuration_and_unresolved_home_refuse_the_subject() {
        for env in [vec![], vec![("CARGO_HOME".into(), "relative".into())]] {
            let overrides = scan(Path::new("/workspace/repo"), &env, &[], |path| {
                if path == Path::new("/workspace/repo/Cargo.toml") {
                    Err(std::io::ErrorKind::PermissionDenied.into())
                } else {
                    Err(std::io::ErrorKind::NotFound.into())
                }
            });
            assert_eq!(overrides.len(), 2, "{overrides:?}");
            assert!(overrides.iter().any(|value| value.contains("Cargo home")));
            assert!(
                overrides
                    .iter()
                    .any(|value| value.contains("cannot inspect Cargo configuration"))
            );
        }
    }

    #[test]
    fn absent_configs_and_profile_free_files_are_accepted() {
        let env = vec![("CARGO_HOME".into(), "/cargo".into())];
        assert!(
            scan(Path::new("/workspace/repo"), &env, &[], |path| {
                if path == Path::new("/workspace/repo/Cargo.toml") {
                    Ok("[workspace]\nmembers = []".into())
                } else {
                    Err(std::io::ErrorKind::NotFound.into())
                }
            })
            .is_empty()
        );
    }

    #[test]
    fn configuration_refuses_profiles_optimization_and_invalid_toml() {
        for text in [
            "\u{feff}[profile.dev.package.norn-host]\nopt-level = 1",
            "profile = { dev = { opt-level = 1 } }",
            "[build]\nrustflags = ['-C', 'opt-level=3']",
            "[target.'cfg(unix)']\nrustflags = '-O'",
            "[broken",
        ] {
            assert!(configuration_issue(text).is_some(), "{text}");
        }
    }
}
