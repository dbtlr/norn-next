//! The generic per-vault config envelope.

use norn_config::vault::{VaultConfig, VaultConfigError};

#[test]
fn an_absent_file_is_an_empty_envelope() {
    let config = VaultConfig::parse(None).expect("an absent config file");

    assert!(config.engine("sample").is_none());
}

#[test]
fn an_empty_file_is_an_empty_envelope() {
    let config = VaultConfig::parse(Some(b"")).expect("an empty config file");

    assert!(config.engine("sample").is_none());
}

#[test]
fn an_engine_table_is_available_by_name() {
    let config =
        VaultConfig::parse(Some(b"[engine.sample]\nlimit = 4\n")).expect("an engine section");

    let sample = config.engine("sample").expect("the sample section");
    assert_eq!(
        sample
            .table()
            .get("limit")
            .and_then(toml::Value::as_integer),
        Some(4)
    );
    assert!(config.engine("other").is_none());
}

#[test]
fn invalid_toml_is_a_typed_parse_error() {
    let error = VaultConfig::parse(Some(b"[engine.sample\n")).expect_err("invalid TOML");

    assert!(matches!(error, VaultConfigError::InvalidToml { .. }));
}

#[test]
fn unknown_top_level_data_is_ignored() {
    let config = VaultConfig::parse(Some(
        b"title = 'notes'\n\n[future]\nenabled = true\n\n[engine.sample]\nlimit = 4\n",
    ))
    .expect("unknown top-level data");

    assert!(config.engine("title").is_none());
    assert!(config.engine("future").is_none());
    assert!(config.engine("sample").is_some());
}

#[test]
fn invalid_utf8_is_a_typed_parse_error() {
    let error = VaultConfig::parse(Some(&[0xff])).expect_err("invalid UTF-8");

    assert!(matches!(error, VaultConfigError::InvalidUtf8 { .. }));
}

#[test]
fn the_engine_key_must_hold_named_tables() {
    let error = VaultConfig::parse(Some(b"engine = true\n")).expect_err("a scalar engine key");
    assert!(matches!(error, VaultConfigError::InvalidEngineTable { .. }));

    let error =
        VaultConfig::parse(Some(b"engine.sample = true\n")).expect_err("a scalar engine section");
    assert!(matches!(
        error,
        VaultConfigError::InvalidEngineSection { ref name, .. } if name == "sample"
    ));
}
