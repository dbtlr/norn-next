//! Cache diagnostics run separately from clocks; neither option selects a loader.
use std::ffi::OsStr;
use std::sync::Arc;

use norn_host::startup_experiment::InitialImportCache;
use serde_json::{Value, json};

pub struct CacheOptions {
    pub cache: Option<Arc<InitialImportCache>>,
    pub diagnostics: bool,
    pub kib: Option<u32>,
}

impl CacheOptions {
    pub fn parse(cache: Option<&OsStr>, diagnostics: Option<&OsStr>) -> Result<Self, String> {
        let diagnostics = match diagnostics.and_then(OsStr::to_str) {
            None if diagnostics.is_none() => false,
            Some("0") => false,
            Some("1") => true,
            _ => return Err("NORN_PERFORMANCE_CACHE_DIAGNOSTICS must be 0 or 1".into()),
        };
        let kib = cache
            .map(|value| {
                value
                    .to_str()
                    .and_then(|value| value.parse::<u32>().ok())
                    .ok_or_else(|| {
                        "NORN_PERFORMANCE_INITIAL_CACHE_KIB must be an integer in 1..=8192"
                            .to_string()
                    })
            })
            .transpose()?;
        if diagnostics && kib.is_none() {
            return Err("cache diagnostics require an explicit initial cache size".into());
        }
        let cache = kib
            .map(|kib| InitialImportCache::new(kib, diagnostics).map_err(|error| error.to_string()))
            .transpose()?;
        Ok(Self {
            cache,
            diagnostics,
            kib,
        })
    }

    pub fn receipt(&self) -> Value {
        self.cache
            .as_ref()
            .and_then(|cache| cache.receipt())
            .map_or(Value::Null, |receipt| {
                let writer = receipt.writer;
                json!({"before":writer.before,"during":writer.during,"after":writer.after,
                "cache_size_units":"negative KiB, positive pages",
                "import_succeeded":receipt.import_succeeded,
                "retained_cache_budget_kib":writer.retained_cache_budget_kib,
                "reclamation_verified":writer.reclamation_verified})
            })
    }
}

/// Removing clocks makes a diagnostic run unusable as an accepted timing sample.
pub fn suppress_clocks(value: &mut Value) {
    if let Some(object) = value.as_object_mut() {
        object.retain(|key, _| !key.ends_with("_ns") && key != "timings");
        object.insert("diagnostic_only".into(), Value::Bool(true));
    }
}
