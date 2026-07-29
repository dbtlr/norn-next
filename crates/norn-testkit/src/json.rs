//! Reading a JSON data file, in the one shape the data-backed suites use.
//!
//! Two strata are data files consumed by one loader — the coverage corpus and
//! the regression registry — and both read the same way: bytes off disk, then
//! a strict deserialization that refuses an unknown field. The two failures
//! are worth telling apart at the call site, so this returns which one
//! happened and each loader maps it into the error type its callers already
//! match on.

use std::path::Path;

use serde::Deserialize;

/// Which half of a read failed.
#[derive(Debug)]
pub(crate) enum JsonError {
    Read(std::io::Error),
    Parse(serde_json::Error),
}

/// Read `path` and deserialize it.
#[allow(clippy::disallowed_methods)] // Reads the data files the suites load.
pub(crate) fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T, JsonError> {
    let text = std::fs::read_to_string(path).map_err(JsonError::Read)?;
    serde_json::from_str(&text).map_err(JsonError::Parse)
}
