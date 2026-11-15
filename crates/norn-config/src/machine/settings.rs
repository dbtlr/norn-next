//! Read-only channel settings shared by the host and its dialers.
use super::{Endpoint, default_endpoint};
use crate::ConfigDirs;
use std::fmt;
use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServingSettings {
    pub endpoint: Endpoint,
    pub read_connections: usize,
    pub read_idle_grace: Duration,
    pub count_result_max_bytes: Option<u64>,
    pub count_results_total_bytes: Option<u64>,
    pub background_threads: usize,
    pub background_turn_budget: Duration,
    pub idle_after: Duration,
    pub auto_exit_after: Duration,
    pub ready_wait: Duration,
    pub drain_bound: Duration,
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
#[derive(Clone, Copy, Debug)]
pub enum SettingsSource<'a> {
    ChannelDefault,
    Explicit(&'a Path),
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SettingsWarning {
    pub key: String,
    pub message: String,
}
#[derive(Debug)]
pub struct SettingsError {
    pub path: PathBuf,
    pub line: Option<usize>,
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
    pub fn read(
        dirs: &ConfigDirs,
        source: SettingsSource<'_>,
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
        let Some(bytes) = bytes else {
            return Ok((Self::default(), vec![]));
        };
        let text = std::str::from_utf8(&bytes)
            .map_err(|_| refusal(path, None, "save the settings file as UTF-8 TOML"))?;
        parse(text, path)
    }
    /// Apply the invocation's port after reading the file, preserving flag precedence.
    pub fn override_port(&mut self, port: u16) -> Result<Vec<SettingsWarning>, SettingsError> {
        if port == 0 {
            return Err(refusal(
                Path::new("--port"),
                None,
                "choose a port between 1 and 65535",
            ));
        }
        self.endpoint = Endpoint::at(port);
        Ok(port_warning(port).into_iter().collect())
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
fn port_warning(port: u16) -> Option<SettingsWarning> {
    crate::file::ephemeral_range()
        .filter(|(lo, hi)| (*lo..=*hi).contains(&port))
        .map(|_| SettingsWarning {
            key: "endpoint.port".into(),
            message: format!("port {port} is in the platform ephemeral range; using it"),
        })
}
fn parse(
    text: &str,
    path: &Path,
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
        if name == "client" && value.is_table() {
            continue;
        }
        if name != "endpoint" && name != "serve" {
            warnings.push(SettingsWarning {
                key: name,
                message: "unknown table; ignored".into(),
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
                warnings.extend(port_warning(port));
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
                    "idle_after" => time!(idle_after, 60_000, 604_800_000),
                    "auto_exit_after" => time!(auto_exit_after, 60_000, 86_400_000),
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
    Ok((settings, warnings))
}
// Diagnostics use the TOML parser's spans rather than guessing from repeated keys.
fn key_line(text: &str, key: &str) -> Option<usize> {
    use std::collections::BTreeMap;
    let offset = if key == "endpoint" {
        let table: BTreeMap<String, toml::Spanned<toml::Value>> = toml::from_str(text).ok()?;
        table.get("endpoint")?.span().start
    } else {
        let table: BTreeMap<String, BTreeMap<String, toml::Spanned<toml::Value>>> =
            toml::from_str(text).ok()?;
        table.get("endpoint")?.get(key)?.span().start
    };
    Some(text[..offset].bytes().filter(|b| *b == b'\n').count() + 1)
}
#[cfg(test)]
mod tests;
