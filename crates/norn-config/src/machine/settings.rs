//! Read-only channel settings shared by the host and its dialers.
use super::{Endpoint, default_endpoint};
use crate::ConfigDirs;
use std::fmt;
use std::num::NonZeroU16;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Resolved endpoint and serving resource limits, with recoverable knobs defaulted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServingSettings {
    /// Loopback endpoint after flag, file and built-in precedence.
    pub endpoint: Endpoint,
    /// Host-wide actual read-connection cap.
    pub read_connections: usize,
    /// How long unused read connections remain warm.
    pub read_idle_grace: Duration,
    /// Per-result count retention quota; `None` leaves bytes unlimited.
    pub count_result_max_bytes: Option<u64>,
    /// Total count retention quota; `None` leaves bytes unlimited.
    pub count_results_total_bytes: Option<u64>,
    /// Host-wide background lane threads.
    pub background_threads: usize,
    /// Time budget for one background turn.
    pub background_turn_budget: Duration,
    /// Inactivity before an attached vault retires.
    pub idle_after: Duration,
    /// Inactivity before an auto-launched host exits.
    pub auto_exit_after: Duration,
    /// Maximum readiness wait for one request.
    pub ready_wait: Duration,
    /// Maximum graceful drain duration.
    pub drain_bound: Duration,
    /// Whether the serving layer logs requests.
    pub request_log: bool,
}
impl Default for ServingSettings {
    fn default() -> Self {
        Self {
            endpoint: default_endpoint(),
            read_connections: 4,
            read_idle_grace: Duration::from_secs(5),
            count_result_max_bytes: None,
            count_results_total_bytes: None,
            background_threads: 1,
            background_turn_budget: Duration::from_millis(100),
            idle_after: Duration::from_secs(3600),
            auto_exit_after: Duration::from_secs(1800),
            ready_wait: Duration::from_secs(30),
            drain_bound: Duration::from_secs(10),
            request_log: false,
        }
    }
}
/// Which settings file to read; explicit paths must exist.
#[derive(Clone, Copy, Debug)]
pub enum SettingsSource<'a> {
    /// The channel config directory's optional `config.toml`.
    ChannelDefault,
    /// A required invocation-selected settings file.
    Explicit(&'a Path),
}
/// A recoverable setting problem and the fallback used.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SettingsWarning {
    /// Qualified setting name, or top-level name.
    pub key: String,
    /// Problem and the default or ignored value it selects.
    pub message: String,
}
/// A settings refusal naming the file, available source line, and resolution.
#[derive(Debug)]
pub struct SettingsError {
    /// Settings file that could not be read or validated.
    pub path: PathBuf,
    /// One-based TOML line when a source span is available.
    pub line: Option<usize>,
    /// Diagnostic with the action that resolves the refusal.
    pub fix: String,
}
impl fmt::Display for SettingsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.path.display())?;
        if let Some(line) = self.line {
            write!(f, ":{line}")?;
        }
        write!(f, ": {}", self.fix)
    }
}
impl std::error::Error for SettingsError {}
fn refusal(path: &Path, line: Option<usize>, fix: impl Into<String>) -> SettingsError {
    SettingsError {
        path: path.into(),
        line,
        fix: fix.into(),
    }
}
impl ServingSettings {
    /// Read a bounded regular file, following links, then resolve the optional port flag.
    ///
    /// Only an absent channel default uses all defaults silently. Invalid files
    /// and endpoint values refuse; bad serving knobs warn and default. Only the
    /// resolved port is checked against the platform ephemeral range.
    pub fn read(
        dirs: &ConfigDirs,
        source: SettingsSource<'_>,
        port: Option<NonZeroU16>,
    ) -> Result<(Self, Vec<SettingsWarning>), SettingsError> {
        let default = dirs.config_dir().join("config.toml");
        let (path, optional) = match source {
            SettingsSource::ChannelDefault => (default.as_path(), true),
            SettingsSource::Explicit(path) => (path, false),
        };
        let bytes = crate::file::read_settings(path, optional).map_err(|e| {
            refusal(
                path,
                None,
                format!("{e}; provide a readable regular settings file of at most 1 MiB"),
            )
        })?;
        let ephemeral = crate::file::ephemeral_range();
        // An absent file is every default, and the resolved port is still
        // judged against the ephemeral range, the same as a file's would be.
        let Some(bytes) = bytes else {
            return parse_with("", path, port, ephemeral);
        };
        let text = std::str::from_utf8(&bytes)
            .map_err(|_| refusal(path, None, "save the settings file as UTF-8 TOML"))?;
        parse_with(text, path, port, ephemeral)
    }
}

fn duration(text: &str) -> Option<Duration> {
    let split = text.find(|c: char| !c.is_ascii_digit())?;
    if split == 0 {
        return None;
    }
    let multiplier = match &text[split..] {
        "ms" => 1,
        "s" => 1000,
        "m" => 60_000,
        "h" => 3_600_000,
        "d" => 86_400_000,
        _ => return None,
    };
    text[..split]
        .parse::<u64>()
        .ok()?
        .checked_mul(multiplier)
        .map(Duration::from_millis)
}
/// The warning for a resolved `port` inside the platform's ephemeral range.
/// A range is read in either order, since a kernel may hold it descending.
fn port_warning(port: u16, ephemeral: Option<(u16, u16)>) -> Option<SettingsWarning> {
    ephemeral
        .map(|(a, b)| (a.min(b), a.max(b)))
        .filter(|(lo, hi)| (*lo..=*hi).contains(&port))
        .map(|_| SettingsWarning {
            key: "endpoint.port".into(),
            message: format!("port {port} is in the platform ephemeral range; using it"),
        })
}
/// `text` parsed with no ephemeral range, so a test's warnings do not depend
/// on this machine's kernel settings; range tests call [`parse_with`].
#[cfg(test)]
fn parse(
    text: &str,
    path: &Path,
) -> Result<(ServingSettings, Vec<SettingsWarning>), SettingsError> {
    parse_with(text, path, None, None)
}
/// Parse `text` as the settings file at `path`, with the `--port` flag
/// resolved inside, judging the resolved port against `ephemeral`.
fn parse_with(
    text: &str,
    path: &Path,
    port: Option<NonZeroU16>,
    ephemeral: Option<(u16, u16)>,
) -> Result<(ServingSettings, Vec<SettingsWarning>), SettingsError> {
    let table = text.parse::<toml::Table>().map_err(|e| {
        let line = e
            .span()
            .map(|s| text[..s.start].bytes().filter(|b| *b == b'\n').count() + 1);
        refusal(path, line, format!("repair the TOML syntax: {e}"))
    })?;
    let mut settings = ServingSettings::default();
    let mut warnings = Vec::new();
    for (name, value) in table {
        if name == "client" {
            if !value.is_table() {
                warnings.push(SettingsWarning {
                    key: name,
                    message: "expected a table; ignored".into(),
                });
            }
            continue;
        }
        if name != "endpoint" && name != "serve" {
            warnings.push(SettingsWarning {
                key: name,
                // An array of tables (`[[name]]`) is a table to the person who wrote it.
                message: if value.is_table()
                    || value.as_array().is_some_and(|items| {
                        !items.is_empty() && items.iter().all(|item| item.is_table())
                    }) {
                    "unknown table; ignored"
                } else {
                    "unknown key; ignored"
                }
                .into(),
            });
            continue;
        }
        let Some(entries) = value.as_table() else {
            if name == "endpoint" {
                return Err(refusal(
                    path,
                    key_line(text, "endpoint"),
                    "make endpoint a table with an integer port between 1 and 65535",
                ));
            }
            warnings.push(SettingsWarning {
                key: name,
                message: "expected a table; using all serve defaults".into(),
            });
            continue;
        };
        for (key, value) in entries {
            let qualified = format!("{name}.{key}");
            if name == "endpoint" && key == "port" {
                let port = value
                    .as_integer()
                    .and_then(|n| u16::try_from(n).ok())
                    .filter(|p| *p != 0)
                    .ok_or_else(|| {
                        refusal(
                            path,
                            key_line(text, "port"),
                            "set endpoint.port to an integer between 1 and 65535",
                        )
                    })?;
                settings.endpoint = Endpoint::at(port);
                continue;
            }
            let mut known = name == "serve";
            let valid = if name != "serve" {
                known = false;
                false
            } else {
                macro_rules! integer {
                    ($field:ident, $min:expr, $max:expr) => {{
                        if let Some(n) = value.as_integer().filter(|n| ($min..=$max).contains(n)) {
                            settings.$field = n as usize;
                            true
                        } else {
                            false
                        }
                    }};
                }
                macro_rules! time {
                    ($field:ident, $min:expr, $max:expr) => {{
                        if let Some(d) = value.as_str().and_then(duration).filter(|d| {
                            (Duration::from_millis($min)..=Duration::from_millis($max)).contains(d)
                        }) {
                            settings.$field = d;
                            true
                        } else {
                            false
                        }
                    }};
                }
                match key.as_str() {
                    "read_connections" => integer!(read_connections, 2, 64),
                    "background_threads" => integer!(background_threads, 1, 8),
                    "read_idle_grace" => time!(read_idle_grace, 0, 600_000),
                    "background_turn_budget" => time!(background_turn_budget, 10, 1000),
                    "idle_after" => time!(idle_after, 1000, 604_800_000),
                    "auto_exit_after" => time!(auto_exit_after, 1000, 86_400_000),
                    "ready_wait" => time!(ready_wait, 1000, 300_000),
                    "drain_bound" => time!(drain_bound, 1000, 20_000),
                    "request_log" => {
                        if let Some(b) = value.as_bool() {
                            settings.request_log = b;
                            true
                        } else {
                            false
                        }
                    }
                    "count_result_max_bytes" | "count_results_total_bytes" => {
                        if let Some(n) = value.as_integer().filter(|n| *n >= 1) {
                            if key == "count_result_max_bytes" {
                                settings.count_result_max_bytes = Some(n as u64);
                            } else {
                                settings.count_results_total_bytes = Some(n as u64);
                            }
                            true
                        } else {
                            false
                        }
                    }
                    _ => {
                        known = false;
                        false
                    }
                }
            };
            if !valid {
                let message = if known {
                    let defaults = ServingSettings::default();
                    let default = match key.as_str() {
                        "read_connections" => defaults.read_connections.to_string(),
                        "background_threads" => defaults.background_threads.to_string(),
                        "read_idle_grace" => "5s".into(),
                        "background_turn_budget" => "100ms".into(),
                        "idle_after" => "1h".into(),
                        "auto_exit_after" => "30m".into(),
                        "ready_wait" => "30s".into(),
                        "drain_bound" => "10s".into(),
                        "request_log" => "false".into(),
                        _ => "none".into(),
                    };
                    format!("invalid value; using default {default}")
                } else {
                    "unknown key; ignored".into()
                };
                warnings.push(SettingsWarning {
                    key: qualified,
                    message,
                });
            }
        }
    }
    if matches!((settings.count_result_max_bytes, settings.count_results_total_bytes), (Some(max), Some(total)) if max > total)
    {
        settings.count_result_max_bytes = None;
        settings.count_results_total_bytes = None;
        warnings.push(SettingsWarning {
            key: "serve.count_result_max_bytes/serve.count_results_total_bytes".into(),
            message: "inconsistent quotas; using default none for both".into(),
        });
    }
    if let Some(port) = port {
        settings.endpoint = Endpoint::at(port.get());
    }
    warnings.extend(port_warning(settings.endpoint.port(), ephemeral));
    Ok((settings, warnings))
}
// Diagnostics use the TOML parser's spans rather than guessing from repeated keys.
fn key_line(text: &str, key: &str) -> Option<usize> {
    let table = toml::de::DeTable::parse(text).ok()?;
    let endpoint = table.get_ref().get("endpoint")?;
    let value = if key == "endpoint" {
        endpoint
    } else {
        endpoint.get_ref().get(key)?
    };
    let offset = value.span().start;
    Some(text[..offset].bytes().filter(|b| *b == b'\n').count() + 1)
}
#[cfg(test)]
mod tests;
