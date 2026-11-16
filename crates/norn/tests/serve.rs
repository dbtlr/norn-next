//! `norn serve` as a process: the real binary, started under an isolated
//! home, config and data directory, on a port chosen free for each test and
//! never a channel's default.
//!
//! What is held here is the start and the plain stop: the settings precedence
//! and refusals, the channel lock, the bind, `listening`, a 404 for every
//! authenticated request while no route is served behind the bearer, and
//! `stopped` with status 0 on SIGTERM. The token mint, the bearer and the
//! hello are held in `serve_tokens.rs` and `serve_hello.rs`.
#![allow(clippy::disallowed_methods)] // Harness scaffolding: the settings files a start reads.

mod support;

use norn_config::machine::default_endpoint;
use norn_testkit::process::{Run, RunStatus};
use support::{Fixture, bind_unissued, free_port, get};

#[test]
fn serve_listens_on_the_resolved_port_answers_an_unrouted_path_404_and_stops_on_sigterm() {
    let fixture = Fixture::new("serve-lifecycle");
    let port = free_port();
    fixture.write_settings(&format!("[endpoint]\nport = {port}\n"));

    let mut served = fixture.start(&["serve"]);
    assert_eq!(served.listening_port(), port);
    let listening = served.seen.last().unwrap().clone();
    assert!(
        listening.contains(&format!(
            "channel {}",
            norn_config::Channel::COMPILED.app_directory()
        )),
        "{listening}"
    );
    // An authenticated `GET` names no route — the wire route is `POST` — so
    // it is 404 `serve/unknown-verb`, and the same request without the bearer
    // a 401.
    let bearer = fixture.default_bearer();
    for path in ["/", "/v1/hello", "/v1/wire/find"] {
        let answer = get(port, path, Some(&bearer));
        assert_eq!(answer.status, 404, "{path}: {answer:?}");
        assert_eq!(answer.code(), "serve/unknown-verb", "{path}: {answer:?}");
        assert_eq!(get(port, path, None).status, 401, "{path}");
    }

    let (status, lines) = served.terminate();
    assert_eq!(status.code(), Some(0), "{lines:?}");
    assert!(
        lines.iter().any(|line| line == "stopped: SIGTERM"),
        "{lines:?}"
    );
}

#[test]
fn a_second_start_while_the_first_holds_the_channel_lock_refuses() {
    let fixture = Fixture::new("serve-lock-held");
    let port = free_port();
    let mut first = fixture.start(&["serve", "--port", &port.to_string()]);
    first.listening_port();

    let other = free_port();
    let (status, stderr) = fixture.run(&["serve", "--port", &other.to_string()]);
    assert_eq!(status, RunStatus::Exited(1), "{stderr}");
    assert!(
        stderr.contains("a norn host already holds this channel's lock"),
        "{stderr}"
    );
    assert!(!stderr.contains("listening"), "{stderr}");

    let (status, lines) = first.terminate();
    assert_eq!(status.code(), Some(0), "{lines:?}");
}

#[test]
fn a_port_another_process_holds_refuses_naming_the_port() {
    let fixture = Fixture::new("serve-port-held");
    let squatter = bind_unissued();
    let port = squatter.local_addr().unwrap().port();

    let (status, stderr) = fixture.run(&["serve", "--port", &port.to_string()]);
    assert_eq!(status, RunStatus::Exited(1), "{stderr}");
    assert!(
        stderr.contains(&format!(
            "port {port} on 127.0.0.1 is held by another process"
        )),
        "{stderr}"
    );
    assert!(stderr.contains("--port N"), "{stderr}");
    drop(squatter);
}

#[test]
fn a_settings_file_that_is_not_toml_refuses_naming_the_file() {
    let fixture = Fixture::new("serve-bad-file");
    let path = fixture.write_settings("[endpoint\nport = 1\n");

    let (status, stderr) = fixture.run(&["serve", "--port", &free_port().to_string()]);
    assert_eq!(status, RunStatus::Exited(1), "{stderr}");
    assert!(stderr.contains("refused:"), "{stderr}");
    assert!(stderr.contains(&path.display().to_string()), "{stderr}");
}

#[test]
fn a_missing_explicit_settings_file_refuses_naming_it() {
    let fixture = Fixture::new("serve-missing-file");
    let path = fixture.sandbox.root().join("nowhere.toml");

    let (status, stderr) = fixture.run(&["serve", "--config", path.to_str().unwrap()]);
    assert_eq!(status, RunStatus::Exited(1), "{stderr}");
    assert!(stderr.contains(&path.display().to_string()), "{stderr}");
}

#[test]
fn an_out_of_range_knob_warns_and_the_host_still_starts() {
    let fixture = Fixture::new("serve-bad-knob");
    let port = free_port();
    fixture.write_settings(&format!(
        "[endpoint]\nport = {port}\n[serve]\nread_connections = 1000\n"
    ));

    let mut served = fixture.start(&["serve"]);
    assert_eq!(served.listening_port(), port);
    assert!(
        served
            .seen
            .iter()
            .any(|line| line == "warning: serve.read_connections: invalid value; using default 4"),
        "{:?}",
        served.seen
    );
    let (status, lines) = served.terminate();
    assert_eq!(status.code(), Some(0), "{lines:?}");
}

/// The flag beats the file, and the file beats the channel default. The
/// default is asserted through the resolution both sides share rather than by
/// starting a host on it, because a suite never binds a channel's own port.
#[test]
fn the_port_flag_beats_the_settings_file_which_beats_the_default() {
    let fixture = Fixture::new("serve-precedence");
    assert_eq!(
        norn_client::endpoint(&fixture.dirs(), None).unwrap(),
        default_endpoint()
    );

    let in_file = free_port();
    fixture.write_settings(&format!("[endpoint]\nport = {in_file}\n"));
    let mut from_file = fixture.start(&["serve"]);
    assert_eq!(from_file.listening_port(), in_file);
    assert_eq!(from_file.terminate().0.code(), Some(0));

    let on_flag = free_port();
    let mut from_flag = fixture.start(&["serve", "--port", &on_flag.to_string()]);
    assert_eq!(from_flag.listening_port(), on_flag);
    assert_eq!(from_flag.terminate().0.code(), Some(0));
}

/// Exit bar 5's last clause: a host and a client reading the same file agree
/// on the endpoint, for the channel's own file and for one `--config` names.
#[test]
fn the_client_and_the_host_resolve_one_endpoint_from_one_file() {
    let fixture = Fixture::new("serve-shared-endpoint");
    let port = free_port();
    fixture.write_settings(&format!("[endpoint]\nport = {port}\n"));
    let mut served = fixture.start(&["serve"]);
    let listening = served.listening_port();
    assert_eq!(served.terminate().0.code(), Some(0));
    assert_eq!(
        norn_client::endpoint(&fixture.dirs(), None).unwrap().port(),
        listening
    );

    let elsewhere = fixture.sandbox.root().join("elsewhere.toml");
    let named = free_port();
    std::fs::write(&elsewhere, format!("[endpoint]\nport = {named}\n")).unwrap();
    let mut served = fixture.start(&["serve", "--config", elsewhere.to_str().unwrap()]);
    let listening = served.listening_port();
    assert_eq!(served.terminate().0.code(), Some(0));
    assert_eq!(listening, named);
    assert_eq!(
        norn_client::endpoint(&fixture.dirs(), Some(&elsewhere))
            .unwrap()
            .port(),
        listening
    );
}

#[test]
fn the_auto_launch_marker_with_a_port_is_a_usage_error() {
    let fixture = Fixture::new("serve-usage");
    let (status, stderr) = fixture.run(&["serve", "--auto-launched", "--port", "4100"]);
    assert_eq!(status, RunStatus::Exited(2), "{stderr}");
    let (status, stderr) = fixture.run(&["serve", "--port", "0"]);
    assert_eq!(status, RunStatus::Exited(2), "{stderr}");
}

/// Until the render seam lands, `norn serve --help` is clap's stock help on
/// stdout, and the auto-launch marker is not in it.
#[test]
fn serve_help_names_the_port_and_config_and_hides_the_marker() {
    let fixture = Fixture::new("serve-help");
    let outcome = Run::new(&fixture.sandbox, &fixture.binary)
        .args(["serve", "--help"])
        .wait()
        .expect("running norn");
    assert_eq!(outcome.status, RunStatus::Exited(0));
    let help = outcome.stdout_text();
    assert!(
        help.contains("--port") && help.contains("--config"),
        "{help}"
    );
    assert!(!help.contains("--auto-launched"), "{help}");
}

#[test]
fn invalid_registry_entries_refuse_naming_the_file_and_the_fix() {
    for (name, root) in [("notes", "relative"), ("Notes_1", "/vault")] {
        let fixture = Fixture::new("serve-invalid-registry");
        fixture.write_settings("");
        let path = fixture.config_dir().join("registry.toml");
        std::fs::write(
            &path,
            format!("version = 1\n[vaults.{name}]\nroot = {root:?}\n"),
        )
        .unwrap();
        let (status, stderr) = fixture.run(&["serve"]);
        assert_eq!(status, RunStatus::Exited(1), "{stderr}");
        assert_eq!(
            stderr.matches(&path.display().to_string()).count(),
            1,
            "{stderr}"
        );
        assert!(stderr.contains("repair the registry file"), "{stderr}");
    }
}

#[test]
fn a_registry_fifo_refuses_naming_the_file() {
    use std::os::unix::ffi::OsStrExt;
    let fixture = Fixture::new("serve-registry-fifo");
    fixture.write_settings("");
    let path = fixture.config_dir().join("registry.toml");
    let cpath = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
    // SAFETY: a live NUL-terminated path; no pointers are retained.
    assert_eq!(unsafe { libc::mkfifo(cpath.as_ptr(), 0o600) }, 0);
    let (status, stderr) = fixture.run(&["serve"]);
    assert_eq!(status, RunStatus::Exited(1), "{stderr}");
    assert_eq!(
        stderr.matches(&path.display().to_string()).count(),
        1,
        "{stderr}"
    );
    assert!(stderr.contains("not a regular file"), "{stderr}");
}

#[test]
fn signals_during_start_refuse_with_a_log_line() {
    for signal in [libc::SIGTERM, libc::SIGINT].into_iter().cycle().take(8) {
        let fixture = Fixture::new("serve-early-signal");
        // A long settings diagnostic stream keeps startup in the settings
        // step. Its first line proves the child has executed, avoiding the
        // unhandleable interval between spawn and the program entry.
        let mut settings = String::new();
        for n in 0..10_000 {
            settings.push_str(&format!("unknown_{n} = true\n"));
        }
        fixture.write_settings(&settings);
        let mut served = fixture.start(&["serve", "--port", &free_port().to_string()]);
        served.line_starting("warning:");
        let (status, lines) = served.signal(signal);
        assert_eq!(status.code(), Some(1), "{status}: {lines:?}");
        assert!(
            lines.iter().any(|line| line
                == &format!(
                    "refused: {} arrived before the host was listening",
                    if signal == libc::SIGTERM {
                        "SIGTERM"
                    } else {
                        "SIGINT"
                    }
                )),
            "no early-signal refusal in {} log lines",
            lines.len()
        );
        assert!(!lines.iter().any(|line| line.starts_with("listening:")));
    }
}
