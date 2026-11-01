//! Runtime profile-override discovery for measurement guards.

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

/// Scan inputs supplied by the caller so configuration decisions can be tested
/// without changing the process environment or touching real files.
fn scan(
    root: &Path,
    env: &[(std::ffi::OsString, std::ffi::OsString)],
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
    scan(root, &std::env::vars_os().collect::<Vec<_>>(), |path| {
        std::fs::read_to_string(path)
    })
    .join("; ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_scan_covers_environment_manifest_ancestors_and_cargo_home() {
        let env = vec![
            ("HOME".into(), "/home/test".into()),
            ("CARGO_PROFILE_TEST_OPT_LEVEL".into(), "0".into()),
        ];
        let overrides = scan(Path::new("/workspace/repo"), &env, |path| {
            if [
                "/workspace/repo/Cargo.toml",
                "/workspace/.cargo/config",
                "/home/test/.cargo/config.toml",
            ]
            .iter()
            .any(|name| path == Path::new(name))
            {
                Ok("[profile.dev.package.\"*\"]\nopt-level = 3".into())
            } else {
                Err(std::io::ErrorKind::NotFound.into())
            }
        });
        assert_eq!(overrides.len(), 4, "{overrides:?}");
        for source in [
            "CARGO_PROFILE_TEST_OPT_LEVEL=0",
            "/workspace/repo/Cargo.toml:",
            "/workspace/.cargo/config:",
            "/home/test/.cargo/config.toml:",
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
            let overrides = scan(Path::new("/workspace/repo"), &env, |path| {
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
                    .any(|value| value.contains("cannot inspect profiles"))
            );
        }
    }

    #[test]
    fn absent_configs_and_profile_free_files_are_accepted() {
        let env = vec![("CARGO_HOME".into(), "/cargo".into())];
        assert!(
            scan(Path::new("/workspace/repo"), &env, |path| {
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
