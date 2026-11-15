//! The regular-file read and lifetime lock at their filesystem seam.
#![allow(clippy::disallowed_methods)] // Isolated filesystem fixtures.
use norn_config::{
    ConfigDirs,
    machine::settings::{ServingSettings, SettingsSource},
};
use norn_testkit::scratch::Scratch;
fn fixture() -> (Scratch, ConfigDirs) {
    let scratch = Scratch::new("serving-config");
    let dirs = ConfigDirs::new(scratch.root(), scratch.root()).unwrap();
    std::fs::create_dir_all(dirs.config_dir()).unwrap();
    (scratch, dirs)
}
#[test]
fn absent_default_is_silent_but_explicit_missing_refuses() {
    let (_scratch, dirs) = fixture();
    let (s, w) = ServingSettings::read(&dirs, SettingsSource::ChannelDefault, None).unwrap();
    assert_eq!(s, ServingSettings::default());
    // The only warning an absent file can bring is the default port inside
    // this machine's ephemeral range.
    assert!(w.iter().all(|w| w.key == "endpoint.port"), "{w:?}");
    assert!(
        ServingSettings::read(
            &dirs,
            SettingsSource::Explicit(&dirs.config_dir().join("missing")),
            None
        )
        .is_err()
    );
}
#[test]
fn directory_is_refused() {
    let (_scratch, dirs) = fixture();
    assert!(
        ServingSettings::read(&dirs, SettingsSource::Explicit(dirs.config_dir()), None).is_err()
    );
}
#[test]
fn fifo_without_writer_is_refused_without_waiting() {
    use std::os::unix::ffi::OsStrExt;
    let (_scratch, dirs) = fixture();
    let path = dirs.config_dir().join("config.toml");
    let cpath = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
    // SAFETY: a live NUL-terminated path; no pointers are retained.
    assert_eq!(unsafe { libc::mkfifo(cpath.as_ptr(), 0o600) }, 0);
    assert!(ServingSettings::read(&dirs, SettingsSource::ChannelDefault, None).is_err());
}
#[test]
fn symlink_to_regular_file_is_followed() {
    let (_scratch, dirs) = fixture();
    let target = dirs.config_dir().join("linked");
    std::fs::write(&target, "[endpoint]\nport=12345").unwrap();
    std::os::unix::fs::symlink(target, dirs.config_dir().join("config.toml")).unwrap();
    assert_eq!(
        ServingSettings::read(&dirs, SettingsSource::ChannelDefault, None)
            .unwrap()
            .0
            .endpoint
            .port(),
        12345
    );
}
#[test]
fn oversized_file_is_refused() {
    let (_scratch, dirs) = fixture();
    std::fs::write(dirs.config_dir().join("config.toml"), vec![b' '; 1_048_577]).unwrap();
    assert!(ServingSettings::read(&dirs, SettingsSource::ChannelDefault, None).is_err());
}
#[test]
fn channel_lock_excludes_second_open_until_drop() {
    let (_scratch, dirs) = fixture();
    let lock = norn_config::ChannelLock::try_take(&dirs).unwrap().unwrap();
    assert!(norn_config::ChannelLock::try_take(&dirs).unwrap().is_none());
    assert!(lock.matches_name().unwrap());
    drop(lock);
    assert!(norn_config::ChannelLock::try_take(&dirs).unwrap().is_some());
    assert!(
        std::fs::read(dirs.config_dir().join("serve.lock"))
            .unwrap()
            .is_empty()
    );
}
#[test]
fn channel_lock_refuses_symlink() {
    let (_scratch, dirs) = fixture();
    let target = dirs.config_dir().join("other");
    std::fs::write(&target, "").unwrap();
    std::os::unix::fs::symlink(target, dirs.config_dir().join("serve.lock")).unwrap();
    assert!(norn_config::ChannelLock::try_take(&dirs).is_err());
}
#[test]
fn channel_lock_detects_replacement() {
    let (_scratch, dirs) = fixture();
    let lock = norn_config::ChannelLock::try_take(&dirs).unwrap().unwrap();
    let replacement = dirs.config_dir().join("replacement");
    std::fs::write(&replacement, "").unwrap();
    std::fs::rename(replacement, dirs.config_dir().join("serve.lock")).unwrap();
    assert!(!lock.matches_name().unwrap());
    assert!(norn_config::ChannelLock::try_take(&dirs).unwrap().is_some());
}

#[test]
fn invalid_utf8_is_a_file_refusal() {
    let (_scratch, dirs) = fixture();
    std::fs::write(dirs.config_dir().join("config.toml"), [0xff]).unwrap();
    let error = ServingSettings::read(&dirs, SettingsSource::ChannelDefault, None).unwrap_err();
    assert!(error.fix.contains("UTF-8"));
}

#[test]
fn explicit_file_uses_the_same_endpoint_resolution() {
    let (_scratch, dirs) = fixture();
    let path = dirs.config_dir().join("config.toml");
    std::fs::write(&path, "[endpoint]\nport=12345").unwrap();
    let (default, _) = ServingSettings::read(&dirs, SettingsSource::ChannelDefault, None).unwrap();
    let (explicit, _) =
        ServingSettings::read(&dirs, SettingsSource::Explicit(&path), None).unwrap();
    assert_eq!(default.endpoint, explicit.endpoint);
}

#[test]
fn dangling_default_refuses_with_path_and_fix() {
    let (_scratch, dirs) = fixture();
    let path = dirs.config_dir().join("config.toml");
    std::os::unix::fs::symlink("missing", &path).unwrap();
    let error = ServingSettings::read(&dirs, SettingsSource::ChannelDefault, None).unwrap_err();
    assert_eq!(error.path, path);
    assert!(error.fix.contains("symlink"), "{error}");
}

#[test]
fn channel_lock_refuses_fifo() {
    use std::os::unix::ffi::OsStrExt;
    let (_scratch, dirs) = fixture();
    let path = dirs.config_dir().join("serve.lock");
    let cpath = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
    // SAFETY: a live NUL-terminated path; no pointers are retained.
    assert_eq!(unsafe { libc::mkfifo(cpath.as_ptr(), 0o600) }, 0);
    assert!(norn_config::ChannelLock::try_take(&dirs).is_err());
}

#[test]
fn flag_port_is_resolved_before_ephemeral_warning() {
    let (_scratch, dirs) = fixture();
    let path = dirs.config_dir().join("config.toml");
    std::fs::write(&path, "[endpoint]\nport=65535").unwrap();
    let (settings, warnings) = ServingSettings::read(
        &dirs,
        SettingsSource::ChannelDefault,
        std::num::NonZeroU16::new(12345),
    )
    .unwrap();
    assert_eq!(settings.endpoint.port(), 12345);
    // Whether 12345 is ephemeral is the machine's; the file's 65535 is never
    // judged once the flag resolves the port.
    assert!(
        warnings.iter().all(|w| !w.message.contains("65535")),
        "{warnings:?}"
    );
    std::fs::remove_file(&path).unwrap();
    let (settings, warnings) = ServingSettings::read(
        &dirs,
        SettingsSource::ChannelDefault,
        std::num::NonZeroU16::new(65535),
    )
    .unwrap();
    assert_eq!(settings.endpoint.port(), 65535);
    assert!(warnings.len() <= 1);
}
