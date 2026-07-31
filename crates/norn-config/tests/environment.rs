//! The one entry point that reads the environment.
//!
//! # Why this is a binary of its own
//!
//! `std::env::set_var` is unsafe in edition 2024 because the environment is
//! process-wide and unsynchronized: a write races every read anywhere in the
//! process, including reads inside the standard library. That makes it
//! unusable in a suite whose other cases run beside it, so the cases that need
//! it live here — a separate integration target, and therefore a separate
//! process, in which nothing else runs.
//!
//! Within this binary the cases are serialized by [`ENVIRONMENT`], because
//! cargo still runs them on several threads. Each case takes the lock, sets
//! what it needs, reads, and restores — so the environment is only ever
//! touched while one case holds it.
//!
//! The *rules* the resolver applies — an absolute XDG base wins, a relative or
//! empty one is ignored, `HOME` is the fallback — are unit-tested purely, over
//! values passed in. What these cases pin is the wiring: that
//! `from_environment` reads the three variables it claims to and builds the
//! same value the injected constructor does.

use std::ffi::OsString;
use std::path::Path;
use std::sync::{Mutex, MutexGuard, OnceLock};

use norn_config::{Channel, ConfigDirs};

fn environment() -> MutexGuard<'static, ()> {
    static ENVIRONMENT: OnceLock<Mutex<()>> = OnceLock::new();
    ENVIRONMENT
        .get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// The variables the resolver reads, saved and put back around one case.
struct Saved {
    values: Vec<(&'static str, Option<OsString>)>,
}

impl Saved {
    fn take() -> Self {
        Saved {
            values: ["XDG_CONFIG_HOME", "XDG_DATA_HOME", "HOME"]
                .into_iter()
                .map(|variable| (variable, std::env::var_os(variable)))
                .collect(),
        }
    }

    fn set(variable: &str, value: Option<&str>) {
        // SAFETY: the environment is process-wide, and this binary holds the
        // lock every case takes before it reaches here. No other test target
        // shares this process.
        unsafe {
            match value {
                Some(value) => std::env::set_var(variable, value),
                None => std::env::remove_var(variable),
            }
        }
    }
}

impl Drop for Saved {
    fn drop(&mut self) {
        for (variable, value) in &self.values {
            Saved::set(variable, value.as_ref().and_then(|value| value.to_str()));
        }
    }
}

#[test]
fn the_xdg_bases_are_read_when_they_are_set() {
    let _lock = environment();
    let _saved = Saved::take();
    Saved::set("XDG_CONFIG_HOME", Some("/scratch/config"));
    Saved::set("XDG_DATA_HOME", Some("/scratch/data"));
    Saved::set("HOME", Some("/scratch/home"));

    let dirs = ConfigDirs::from_environment().expect("the machine's directories");
    assert_eq!(dirs, ConfigDirs::new("/scratch/config", "/scratch/data"));
    assert_eq!(dirs.channel(), Channel::COMPILED);
    assert_eq!(
        dirs.config_dir(),
        Path::new("/scratch/config").join(Channel::COMPILED.app_directory())
    );
}

#[test]
fn the_home_defaults_apply_when_the_xdg_bases_are_not_set() {
    let _lock = environment();
    let _saved = Saved::take();
    Saved::set("XDG_CONFIG_HOME", None);
    Saved::set("XDG_DATA_HOME", None);
    Saved::set("HOME", Some("/scratch/home"));

    let dirs = ConfigDirs::from_environment().expect("the machine's directories");
    assert_eq!(
        dirs,
        ConfigDirs::new("/scratch/home/.config", "/scratch/home/.local/share")
    );
}

/// The specification's rule: a relative base is ignored rather than honoured,
/// because it names a different directory to every process that reads it.
#[test]
fn a_relative_xdg_base_is_ignored_in_favour_of_the_home_default() {
    let _lock = environment();
    let _saved = Saved::take();
    Saved::set("XDG_CONFIG_HOME", Some("relative/config"));
    Saved::set("XDG_DATA_HOME", Some(""));
    Saved::set("HOME", Some("/scratch/home"));

    let dirs = ConfigDirs::from_environment().expect("the machine's directories");
    assert_eq!(
        dirs,
        ConfigDirs::new("/scratch/home/.config", "/scratch/home/.local/share")
    );
}

/// An environment naming nowhere is refused rather than guessed at.
#[test]
fn an_environment_naming_nowhere_is_refused() {
    let _lock = environment();
    let _saved = Saved::take();
    Saved::set("XDG_CONFIG_HOME", None);
    Saved::set("XDG_DATA_HOME", None);
    Saved::set("HOME", None);

    let error = ConfigDirs::from_environment().expect_err("nowhere to look");
    assert!(
        matches!(error, norn_config::ConfigError::Environment { .. }),
        "{error}"
    );
}
