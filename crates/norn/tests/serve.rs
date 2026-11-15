//! `norn serve` as a process: the real binary, started under an isolated
//! home, config and data directory, on a port chosen free for each test and
//! never a channel's default.
//!
//! What is held here is the start and the plain stop: the settings precedence
//! and refusals, the channel lock, the bind, `listening`, a 404 for every
//! request while no route is served, and `stopped` with status 0 on SIGTERM.
//! A host that outlives its test is killed by the pid this suite started, and
//! by nothing wider.
#![allow(clippy::disallowed_methods)] // Harness scaffolding: the settings files a start reads.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::time::{Duration, Instant};

use norn_config::ConfigDirs;
use norn_config::machine::default_endpoint;
use norn_testkit::process::{Run, RunStatus, Sandbox};

/// Long enough for a debug build to construct an empty host and bind.
const START_WITHIN: Duration = Duration::from_secs(30);
const STOP_WITHIN: Duration = Duration::from_secs(30);

struct Fixture {
    sandbox: Sandbox,
    binary: PathBuf,
}

impl Fixture {
    fn new(label: &str) -> Fixture {
        let sandbox = Sandbox::new(Path::new(env!("CARGO_TARGET_TMPDIR")), label)
            .expect("making the sandbox");
        let binary = sandbox
            .install_binary(Path::new(env!("CARGO_BIN_EXE_norn")))
            .expect("installing the binary");
        Fixture { sandbox, binary }
    }

    /// The directories the started binary resolves from the sandbox's
    /// environment.
    fn dirs(&self) -> ConfigDirs {
        let environment = self.sandbox.environment();
        ConfigDirs::new(
            &environment["XDG_CONFIG_HOME"],
            &environment["XDG_DATA_HOME"],
        )
        .expect("the sandbox's directories")
    }

    fn write_settings(&self, text: &str) -> PathBuf {
        let path = self.dirs().config_dir().join("config.toml");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, text).unwrap();
        path
    }

    /// A run to completion, for a start expected to refuse.
    fn run(&self, args: &[&str]) -> (RunStatus, String) {
        let outcome = Run::new(&self.sandbox, &self.binary)
            .args(args)
            .deadline(START_WITHIN)
            .wait()
            .expect("running norn");
        (outcome.status, outcome.stderr_text())
    }

    /// A host left running, read line by line.
    fn start(&self, args: &[&str]) -> Served {
        let mut child = Command::new(&self.binary)
            .args(args)
            .env_clear()
            .envs(self.sandbox.environment())
            .current_dir(self.sandbox.work_dir())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .expect("starting norn serve");
        let stderr = child.stderr.take().unwrap();
        let (sender, lines) = channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines() {
                let Ok(line) = line else { break };
                if sender.send(line).is_err() {
                    break;
                }
            }
        });
        Served {
            child,
            lines,
            seen: Vec::new(),
        }
    }
}

struct Served {
    child: Child,
    lines: Receiver<String>,
    seen: Vec<String>,
}

impl Served {
    /// The first log line starting with `prefix`, every line before it kept.
    fn line_starting(&mut self, prefix: &str) -> String {
        let deadline = Instant::now() + START_WITHIN;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.lines.recv_timeout(left) {
                Ok(line) => {
                    self.seen.push(line.clone());
                    if line.starts_with(prefix) {
                        return line;
                    }
                }
                Err(RecvTimeoutError::Timeout) => {
                    panic!(
                        "no `{prefix}` line within {START_WITHIN:?}: {:?}",
                        self.seen
                    )
                }
                Err(RecvTimeoutError::Disconnected) => {
                    panic!(
                        "norn serve closed stderr before `{prefix}`: {:?}",
                        self.seen
                    )
                }
            }
        }
    }

    /// The port the `listening` line names.
    fn listening_port(&mut self) -> u16 {
        let line = self.line_starting("listening:");
        let port = line
            .split(", ")
            .find_map(|part| part.strip_prefix("port "))
            .unwrap_or_else(|| panic!("`{line}` names no port"));
        port.parse()
            .unwrap_or_else(|_| panic!("`{line}` names no port"))
    }

    /// SIGTERM, then the status and every remaining log line.
    fn terminate(mut self) -> (ExitStatus, Vec<String>) {
        let pid = libc::pid_t::try_from(self.child.id()).unwrap();
        // SAFETY: a plain signal to the child this suite spawned and has not
        // reaped, so the pid still names it.
        assert_eq!(unsafe { libc::kill(pid, libc::SIGTERM) }, 0);
        let deadline = Instant::now() + STOP_WITHIN;
        let status = loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                break status;
            }
            assert!(
                Instant::now() < deadline,
                "norn serve did not stop within {STOP_WITHIN:?} of SIGTERM: {:?}",
                self.seen
            );
            std::thread::sleep(Duration::from_millis(20));
        };
        self.seen.extend(self.lines.try_iter());
        while let Ok(line) = self.lines.recv_timeout(Duration::from_secs(1)) {
            self.seen.push(line);
        }
        (status, std::mem::take(&mut self.seen))
    }
}

impl Drop for Served {
    fn drop(&mut self) {
        if let Ok(None) = self.child.try_wait() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

/// A port nothing listens on now, chosen by the kernel. Another process may
/// take it before the test binds it; that race is accepted.
fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .and_then(|listener| listener.local_addr())
        .expect("asking the kernel for a free port")
        .port()
}

fn ask(port: u16, path: &str) -> String {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connecting to norn serve");
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    write!(
        stream,
        "GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n"
    )
    .unwrap();
    let mut answer = String::new();
    stream.read_to_string(&mut answer).unwrap();
    answer
}

#[test]
fn serve_listens_on_the_resolved_port_answers_404_and_stops_on_sigterm() {
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
    for path in ["/", "/v1/hello", "/v1/wire/find"] {
        let answer = ask(port, path);
        assert!(answer.starts_with("HTTP/1.1 404"), "{path}: {answer}");
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
    let squatter = TcpListener::bind("127.0.0.1:0").unwrap();
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
