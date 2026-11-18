//! `norn serve` as a process: the real binary, started under an isolated
//! home, config and data directory, on a port chosen free for each test and
//! never a channel's default.
//!
//! What is held here is the start and the stop: the settings precedence and
//! refusals, the channel lock and the takeover, the port probe, the bind,
//! `listening`, a 404 for every authenticated request to a path no route
//! serves, the start-bound watchdog, the signals and the drain they begin,
//! the lock-name recheck, and an auto-launched host's idle exit. The token
//! mint, the bearer and the hello are held in `serve_tokens.rs` and
//! `serve_hello.rs`; the drain's answers to running writes in
//! `serve_drain.rs`; a host killed outright in `serve_kill.rs`.
#![allow(clippy::disallowed_methods)] // Harness scaffolding: the settings files a start reads.

mod support;

use std::time::Duration;

use norn_config::ChannelLock;
use norn_config::machine::default_endpoint;
use norn_testkit::process::{Run, RunStatus};
use support::{Fixture, bind_unissued, free_port, get, hello};

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

/// **An explicit start over a held lock with an explicit host exits 1 at
/// once**: the hello verifies the host at the endpoint, and an explicit host
/// is never displaced.
#[test]
fn a_second_start_while_an_explicit_host_serves_the_channel_exits_1_at_once() {
    let fixture = Fixture::new("serve-lock-held");
    let port = free_port();
    let mut first = fixture.start(&["serve", "--port", &port.to_string()]);
    first.listening_port();

    let started = std::time::Instant::now();
    let (status, stderr) = fixture.run(&["serve", "--port", &port.to_string()]);
    assert_eq!(status, RunStatus::Exited(1), "{stderr}");
    assert!(
        stderr.contains("a norn host already serves this channel"),
        "{stderr}"
    );
    assert!(!stderr.contains("listening"), "{stderr}");
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "the start waited on a host it had verified"
    );

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

/// **A signal before listening ends the start with exit 1**, naming it. The
/// suite holds the channel lock, so the start is in its held-lock wait — past
/// exec and the signal mask, and before listening — when the signal lands.
#[test]
fn signals_during_start_refuse_with_a_log_line() {
    for signal in [libc::SIGTERM, libc::SIGINT, libc::SIGHUP] {
        let fixture = Fixture::new("serve-early-signal");
        fixture.config_dir();
        let _held = ChannelLock::try_take(&fixture.dirs())
            .unwrap()
            .expect("a free lock");
        let mut served = fixture.start(&["serve", "--port", &free_port().to_string()]);
        served.line_starting("the channel lock in");
        let (status, lines) = served.signal(signal);
        assert_eq!(status.code(), Some(1), "{status}: {lines:?}");
        let name = match signal {
            libc::SIGTERM => "SIGTERM",
            libc::SIGINT => "SIGINT",
            _ => "SIGHUP",
        };
        assert!(
            lines
                .iter()
                .any(|line| line
                    == &format!("refused: {name} arrived before the host was listening")),
            "{lines:?}"
        );
        assert!(!lines.iter().any(|line| line.starts_with("listening:")));
    }
}

/// An auto-launched host on `port`, by the settings file, since the marker
/// refuses `--port`.
fn auto_launched(fixture: &Fixture, port: u16, settings: &str) -> support::Served {
    fixture.write_settings(&format!("[endpoint]\nport = {port}\n{settings}"));
    let mut served = fixture.start(&["serve", "--auto-launched"]);
    assert_eq!(served.listening_port(), port);
    served
}

/// **An explicit start takes over a verified auto-launched host**: it drains
/// it, the drained host exits 0, and the explicit start takes the lock it
/// freed and listens on the same port.
#[test]
fn an_explicit_start_takes_over_a_verified_auto_launched_host() {
    let fixture = Fixture::new("serve-takeover");
    let port = free_port();
    let auto = auto_launched(&fixture, port, "");
    let mut explicit = fixture.start(&["serve"]);
    assert_eq!(explicit.listening_port(), port);
    assert!(
        explicit
            .seen
            .iter()
            .any(|line| line.contains("asked to drain")),
        "{:?}",
        explicit.seen
    );
    let (status, lines) = auto.wait_within(support::STOP_WITHIN);
    assert_eq!(status.code(), Some(0), "{lines:?}");
    assert!(
        lines.iter().any(|line| line == "stopped: a drain request"),
        "{lines:?}"
    );
    assert_eq!(hello(port, "default").status, 200);
    assert_eq!(explicit.terminate().0.code(), Some(0));
}

/// Bar 6e: a launched start meeting a verified serving host exits 10 at once.
#[test]
fn a_launched_start_meeting_a_verified_host_exits_10() {
    let fixture = Fixture::new("serve-launch-race");
    let port = free_port();
    fixture.write_settings(&format!("[endpoint]\nport = {port}\n"));
    let mut explicit = fixture.start(&["serve"]);
    assert_eq!(explicit.listening_port(), port);
    let started = std::time::Instant::now();
    let (status, stderr) = fixture.run(&["serve", "--auto-launched"]);
    // Five seconds allows process launch and a one-second hello on a loaded
    // runner, while remaining below the 10 s drain and 40 s release bounds.
    assert!(started.elapsed() < Duration::from_secs(5), "{stderr}");
    assert_eq!(status, RunStatus::Exited(10), "{stderr}");
    assert!(!stderr.contains("listening"), "{stderr}");
    assert_eq!(explicit.terminate().0.code(), Some(0));
}

/// Bar 6f: a launched start meeting a held lock with no verified answer exits 11
/// once the release bound has passed, with the facts it observed and nothing
/// more.
#[test]
fn a_launched_start_over_a_held_lock_with_no_verified_answer_exits_11() {
    let fixture = Fixture::new("serve-launch-held");
    fixture.write_settings(&format!("[endpoint]\nport = {}\n", free_port()));
    let _held = ChannelLock::try_take(&fixture.dirs())
        .unwrap()
        .expect("a free lock");
    let started = std::time::Instant::now();
    let outcome = Run::new(&fixture.sandbox, &fixture.binary)
        .args(["serve", "--auto-launched"])
        .deadline(Duration::from_secs(44))
        .wait()
        .expect("running norn");
    let stderr = outcome.stderr_text();
    assert_eq!(outcome.status, RunStatus::Exited(11), "{stderr}");
    assert!(started.elapsed() >= Duration::from_secs(40), "{stderr}");
    // Four seconds covers the final one-second dial, polling and reaping.
    assert!(started.elapsed() < Duration::from_secs(44), "{stderr}");
    assert!(
        stderr.contains("the channel lock is held and nothing verified answers"),
        "{stderr}"
    );
    assert!(
        stderr.contains("deleting serve.lock does not stop"),
        "{stderr}"
    );
    eprintln!("Bar 6f elapsed: {:?}", started.elapsed());
    for unobserved in ["verification", "drain request", "draining"] {
        assert!(!stderr.contains(unobserved), "{unobserved}: {stderr}");
    }
}

/// **The probe refuses a port another process holds on the wildcard
/// address**, which a loopback bind alone would pass beside on BSD.
#[test]
fn a_port_held_on_the_wildcard_address_refuses_the_start() {
    let fixture = Fixture::new("serve-port-wildcard");
    let port = free_port();
    let squatter = std::net::TcpListener::bind(("0.0.0.0", port)).expect("the wildcard");
    let (status, stderr) = fixture.run(&["serve", "--port", &port.to_string()]);
    assert_eq!(status, RunStatus::Exited(1), "{stderr}");
    assert!(stderr.contains("is held by another process"), "{stderr}");
    drop(squatter);
}

/// **A restart on the port a stopped host served listens at once**: the
/// stop's connections leave sockets in TIME_WAIT on the port, and the probe
/// passes them.
#[test]
fn a_restart_on_the_port_a_host_just_left_listens() {
    let fixture = Fixture::new("serve-time-wait");
    let port = free_port();
    let mut first = fixture.start(&["serve", "--port", &port.to_string()]);
    first.listening_port();
    // A kept-alive connection the host closes first at its stop.
    let mut open = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
    let body = serde_json::json!({ "label": "default", "nonce": "ab".repeat(32) }).to_string();
    use std::io::{Read, Write};
    write!(
        open,
        "POST /v1/hello HTTP/1.1\r\nHost: x\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    )
    .unwrap();
    let mut answer = [0u8; 16];
    open.read_exact(&mut answer).unwrap();
    assert_eq!(first.terminate().0.code(), Some(0));
    let mut rest = Vec::new();
    let _ = open.read_to_end(&mut rest);
    drop(open);

    let mut second = fixture.start(&["serve", "--port", &port.to_string()]);
    assert_eq!(second.listening_port(), port);
    assert_eq!(second.terminate().0.code(), Some(0));
}

/// **A deleted `serve.lock` is re-taken without a drain**: within a recheck
/// the host holds a lock at the name again, and it keeps serving.
#[test]
fn a_deleted_channel_lock_is_taken_again_and_the_host_keeps_serving() {
    let fixture = Fixture::new("serve-lock-deleted");
    let port = free_port();
    let mut served = fixture.start(&["serve", "--port", &port.to_string()]);
    served.listening_port();
    let lock = fixture.dirs().config_dir().join("serve.lock");
    std::fs::remove_file(&lock).unwrap();
    served.line_starting("the channel lock file in");
    assert!(
        ChannelLock::try_take(&fixture.dirs()).unwrap().is_none(),
        "the re-made lock is not held"
    );
    assert!(served.running());
    assert_eq!(hello(port, "default").status, 200);
    let (status, lines) = served.terminate();
    assert_eq!(status.code(), Some(0), "{lines:?}");
    assert!(
        !lines
            .iter()
            .any(|line| line.starts_with("draining: another")),
        "{lines:?}"
    );
}

/// Bar 6i: the start-bound watchdog: a start that holds the lock and cannot reach
/// listening within the start bound exits 1, and the last thing it logged
/// names what it was waiting on. The suite holds the token file's lock, so
/// the mint waits.
#[test]
#[allow(clippy::disallowed_types)] // Harness scaffolding: the token file's lock, held as another start would.
fn a_start_that_passes_its_start_bound_exits_1_naming_its_wait() {
    use std::os::unix::io::AsRawFd;
    let fixture = Fixture::new("serve-watchdog");
    let dir = fixture.config_dir();
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(dir.join(".tokens.toml.lock"))
        .unwrap();
    // SAFETY: a plain advisory lock on a descriptor this case owns.
    assert_eq!(unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX) }, 0);
    let started = std::time::Instant::now();
    let (status, stderr) = fixture.run(&["serve", "--port", &free_port().to_string()]);
    assert_eq!(status, RunStatus::Exited(1), "{stderr}");
    assert!(started.elapsed() >= Duration::from_secs(10), "{stderr}");
    // Two seconds for scheduling and reaping beyond the 10 s watchdog.
    assert!(started.elapsed() < Duration::from_secs(12), "{stderr}");
    assert_eq!(
        stderr.lines().last(),
        Some("starting: taking the token file's lock to mint or read the token"),
        "{stderr}"
    );
    assert!(!stderr.contains("listening"), "{stderr}");
    eprintln!("Bar 6i elapsed: {:?}", started.elapsed());
    drop(lock);
    let (following, _) = fixture.serve_on(free_port());
    assert_eq!(following.terminate().0.code(), Some(0));
}

/// **SIGHUP drains and exits 0** — a closed terminal.
#[test]
fn sighup_drains_and_exits_0() {
    let fixture = Fixture::new("serve-sighup");
    let (served, _) = fixture.serve_on(free_port());
    let (status, lines) = served.signal(libc::SIGHUP);
    assert_eq!(status.code(), Some(0), "{lines:?}");
    assert!(
        lines.iter().any(|line| line == "stopped: SIGHUP"),
        "{lines:?}"
    );
}

/// **A SIGHUP inherited as ignored stays ignored**, as `nohup` arranges: the
/// host keeps serving through it, and SIGTERM still drains.
#[test]
fn an_inherited_ignored_sighup_stays_ignored() {
    use std::os::unix::process::CommandExt;
    let fixture = Fixture::new("serve-nohup");
    let port = free_port();
    let mut served = fixture.start_with(&["serve", "--port", &port.to_string()], |command| {
        // SAFETY: `signal` is async-signal-safe, and nothing else runs in the
        // child between fork and exec.
        unsafe {
            command.pre_exec(|| {
                libc::signal(libc::SIGHUP, libc::SIG_IGN);
                Ok(())
            });
        }
    });
    served.listening_port();
    served.send(libc::SIGHUP);
    served.send(libc::SIGHUP);
    assert_eq!(hello(port, "default").status, 200);
    assert!(served.running());
    let (status, lines) = served.terminate();
    assert_eq!(status.code(), Some(0), "{lines:?}");
    assert!(
        lines.iter().any(|line| line == "stopped: SIGTERM"),
        "{lines:?}"
    );
    assert!(
        !lines.iter().any(|line| line.contains("SIGHUP")),
        "{lines:?}"
    );
}

/// **An auto-launched host exits 0 after `auto_exit_after` without
/// activity; an explicit host never idle-exits.** Both are started with the
/// shortest idle exit; the explicit one is still serving after the
/// auto-launched one has gone, and after it has outlived that exit several
/// times over.
///
/// One second is the setting's floor. Nothing in the case depends on a host
/// staying up across a pause: the auto-launched host is meant to exit, its
/// `listening:` line is read from a buffer whether or not it has already
/// gone, and the explicit host's survival is what the case checks. The
/// negative check waits three times the value, since a host that exits
/// after one idle second must be seen to have outlived it.
#[test]
fn an_auto_launched_host_idles_out_and_an_explicit_one_does_not() {
    const AUTO_EXIT_AFTER: Duration = Duration::from_secs(1);
    // What a loaded runner may add to the exit; the minute this replaced
    // carried the same slack on top of its value.
    const EXIT_SLACK: Duration = Duration::from_secs(60);
    let auto_fixture = Fixture::new("serve-idle-auto");
    let explicit_fixture = Fixture::new("serve-idle-explicit");
    let explicit_port = free_port();
    explicit_fixture.write_settings(&format!(
        "[endpoint]\nport = {explicit_port}\n[serve]\nauto_exit_after = \"1s\"\n"
    ));
    let mut explicit = explicit_fixture.start(&["serve"]);
    explicit.listening_port();
    let explicit_up = std::time::Instant::now();
    let started = std::time::Instant::now();
    let auto = auto_launched(
        &auto_fixture,
        free_port(),
        "[serve]\nauto_exit_after = \"1s\"\n",
    );
    let (status, lines) = auto.wait_within(AUTO_EXIT_AFTER + EXIT_SLACK);
    assert_eq!(status.code(), Some(0), "{lines:?}");
    assert!(started.elapsed() >= AUTO_EXIT_AFTER, "{lines:?}");
    assert!(
        lines.iter().any(|line| line == "stopped: the idle exit"),
        "{lines:?}"
    );
    // Observe the explicit host for longer than the exit it must not take.
    // The explicit host sees no activity here: the hello below comes after.
    while explicit_up.elapsed() < AUTO_EXIT_AFTER * 3 {
        assert!(explicit.running(), "the explicit host idled out");
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(explicit.running(), "the explicit host idled out");
    assert_eq!(hello(explicit_port, "default").status, 200);
    assert_eq!(explicit.terminate().0.code(), Some(0));
}

/// **A second SIGINT during a drain `_exit`s 1 at once.** A connection that
/// pipelines hellos and reads none of the answers keeps one answer unflushed,
/// so a request stays in flight and the first SIGINT's drain has its whole
/// bound to run; the second ends it.
///
/// No server-side reading shows that a hello is admitted and its answer
/// unflushed — a build without `counters` serves no counters, and the hello
/// is not in the request log — so the first SIGINT can land before the
/// filled connection has left an answer unflushed. That attempt's drain
/// finds nothing in flight and exits 0, well inside the drain bound, which a
/// host that ignored the second SIGINT could not; it proves nothing, and is
/// made again on a fresh host, within a bound on attempts.
#[test]
fn a_second_sigint_during_a_drain_exits_1_at_once() {
    const ATTEMPTS: usize = 5;
    for attempt in 1..=ATTEMPTS {
        let fixture = Fixture::new(&format!("serve-second-sigint-{attempt}"));
        let port = free_port();
        fixture.write_settings(&format!(
            "[endpoint]\nport = {port}\n[serve]\ndrain_bound = \"20s\"\n"
        ));
        let mut served = fixture.start(&["serve"]);
        served.listening_port();
        let stuck = unread_answers(port);
        served.send(libc::SIGINT);
        served.line_starting("draining: SIGINT");
        let second = std::time::Instant::now();
        served.send(libc::SIGINT);
        // Within half the drain bound: a host held by an unflushed answer
        // that ignored the second SIGINT would still be draining.
        let (status, lines) = served.wait_within(Duration::from_secs(10));
        drop(stuck);
        if status.code() == Some(0) {
            eprintln!("attempt {attempt}: the drain had nothing in flight: {lines:?}");
            continue;
        }
        assert_eq!(status.code(), Some(1), "{lines:?}");
        assert!(second.elapsed() < Duration::from_secs(5), "{lines:?}");
        return;
    }
    panic!("in {ATTEMPTS} attempts no drain had an unflushed answer in flight");
}

/// A connection that pipelines hellos to `port` with a receive buffer of a
/// few kilobytes and reads nothing, written until the host stops reading it:
/// the host is then blocked flushing an answer.
fn unread_answers(port: u16) -> std::net::TcpStream {
    use std::io::Write;
    use std::os::unix::io::AsRawFd;
    let stream = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
    let small: libc::c_int = 4096;
    // SAFETY: a valid descriptor and a pointer to a live `c_int` of the
    // length given.
    let set = unsafe {
        libc::setsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_RCVBUF,
            (&small as *const libc::c_int).cast(),
            std::mem::size_of::<libc::c_int>() as libc::socklen_t,
        )
    };
    assert_eq!(set, 0);
    stream.set_nonblocking(true).unwrap();
    let body = serde_json::json!({ "label": "default", "nonce": "ab".repeat(32) }).to_string();
    let request = format!(
        "POST /v1/hello HTTP/1.1\r\nHost: x\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    let request = request.as_bytes();
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    let mut writer = &stream;
    // Where the next write resumes in the repeating request stream: a short
    // write leaves the rest of its request to go first, so every request
    // the host reads is whole.
    let mut offset = 0;
    loop {
        match writer.write(&request[offset..]) {
            Ok(written) => offset = (offset + written) % request.len(),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
            Err(error) => panic!("pipelining hellos failed: {error}"),
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the host kept reading a connection whose answers nobody reads"
        );
    }
    stream
}

/// Bar 5c: a bad file port refuses even when a valid flag would override it.
#[test]
fn bad_endpoint_ports_in_the_file_refuse_the_start() {
    for value in ["0", "65536", "\"invalid\""] {
        let fixture = Fixture::new("serve-bad-port");
        let path = fixture.write_settings(&format!("[endpoint]\nport = {value}\n"));
        let (status, stderr) = fixture.run(&["serve", "--port", &free_port().to_string()]);
        support::assert_refused_start(status, &stderr, &path);
    }
}

/// Bar 5e: an unknown serving key warns and is ignored; the known endpoint
/// and the authenticated hello still work.
#[test]
fn an_unknown_setting_warns_and_is_ignored() {
    let fixture = Fixture::new("serve-unknown-setting");
    let port = free_port();
    fixture.write_settings(&format!(
        "[endpoint]\nport = {port}\n[serve]\nunknown_knob = 1\n"
    ));
    let mut served = fixture.start(&["serve"]);
    assert_eq!(served.listening_port(), port);
    assert!(
        served
            .seen
            .iter()
            .any(|line| line.contains("warning:") && line.contains("unknown_knob")),
        "{:?}",
        served.seen
    );
    assert_eq!(hello(port, "default").status, 200);
    assert_eq!(served.terminate().0.code(), Some(0));
}

/// Bar 6j: B observes A's held channel lock while A is blocked on its mint.
/// A's 10 s watchdog precedes B's 40 s facts deadline, so B cannot report a
/// still-blocked A. B instead takes the freed lock, then hits its own 10 s
/// mint watchdog. No verification failure or drain was observed by B, and
/// none may be reported. A separate silent-holder case proves the 40 s
/// facts report; extending A with SIGSTOP would suspend the start contract.
#[test]
#[cfg(target_os = "linux")]
fn a_second_start_during_a_blocked_mint_reports_only_its_observations() {
    let started = std::time::Instant::now();
    let fixture = Fixture::new("serve-mint-waiter");
    let held = support::hold_file_lock(&fixture.config_dir().join(".tokens.toml.lock"));
    let mut a = fixture.start(&["serve", "--port", &free_port().to_string()]);
    a.line_starting("starting: taking the token");
    support::wait_for_file_lock(&a);
    let mut b = fixture.start(&["serve", "--port", &free_port().to_string()]);
    b.line_starting("the channel lock in");
    let (status, lines) = a.wait_within(Duration::from_secs(12));
    assert_eq!(status.code(), Some(1), "{lines:?}");
    let (status, lines) = b.wait_within(Duration::from_secs(12));
    assert_eq!(status.code(), Some(1), "{lines:?}");
    eprintln!("Bar 6j elapsed: {:?}", started.elapsed());
    let report = lines.join("\n");
    assert!(
        report.contains("starting: taking the token file's lock"),
        "{report}"
    );
    for unobserved in ["verification failed", "drain request", "draining"] {
        assert!(!report.contains(unobserved), "{report}");
    }
    drop(held);
}

/// Bar 7c: accepted hellos defer a launched host's two-second idle exit.
/// Stop activity after six seconds, then observe the idle drain and exit.
/// Bar 7d observation limit: an idle host has no in-flight work to hold its
/// drain open. Neither log line witnesses listener close: draining is logged
/// before close, stopped after it. A refused connection after exit is stable;
/// a refusal while the child is alive would race an unbounded scheduler pause.
#[test]
fn activity_defers_the_auto_launched_hosts_idle_exit() {
    let fixture = Fixture::new("serve-idle-activity");
    let port = free_port();
    let mut served = auto_launched(&fixture, port, "[serve]\nauto_exit_after = \"2s\"\n");
    let started = std::time::Instant::now();
    while started.elapsed() < Duration::from_secs(6) {
        assert_eq!(hello(port, "default").status, 200);
        assert!(served.running());
        // This is the activity cadence, not a wait for an observable state.
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(served.running());
    let (status, lines) = served.wait_within(Duration::from_secs(8));
    assert_eq!(status.code(), Some(0), "{lines:?}");
    let draining = lines
        .iter()
        .position(|line| line == "draining: the idle exit")
        .unwrap();
    let stopped = lines
        .iter()
        .position(|line| line == "stopped: the idle exit")
        .unwrap();
    assert!(draining < stopped, "{lines:?}");
    assert!(
        std::net::TcpStream::connect(("127.0.0.1", port))
            .is_err_and(|error| error.kind() == std::io::ErrorKind::ConnectionRefused)
    );
}
