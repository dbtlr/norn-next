//! What the `norn serve` process suites share: the real binary started under
//! an isolated home, config and data directory, on a port chosen free for
//! each test and never a channel's default, and a plain HTTP/1.1 exchange
//! with it over a socket of the suite's own.
//!
//! A host that outlives its test is killed by the pid the suite started, and
//! by nothing wider.
#![allow(dead_code)] // Each suite uses its own share of this module.
#![allow(clippy::disallowed_methods, clippy::disallowed_types)] // Harness scaffolding: the settings and token files a case arranges.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::time::{Duration, Instant};

use norn_config::ConfigDirs;
use norn_config::machine::tokens::{self, TokenLabel};
use norn_testkit::process::{Run, RunStatus, Sandbox};

/// Long enough for a debug build to construct an empty host and bind.
pub const START_WITHIN: Duration = Duration::from_secs(30);
pub const STOP_WITHIN: Duration = Duration::from_secs(30);

pub struct Fixture {
    pub sandbox: Sandbox,
    pub binary: PathBuf,
}

impl Fixture {
    pub fn new(label: &str) -> Fixture {
        let sandbox = Sandbox::new(Path::new(env!("CARGO_TARGET_TMPDIR")), label)
            .expect("making the sandbox");
        let binary = sandbox
            .install_binary(Path::new(env!("CARGO_BIN_EXE_norn")))
            .expect("installing the binary");
        Fixture { sandbox, binary }
    }

    /// The directories the started binary resolves from the sandbox's
    /// environment.
    pub fn dirs(&self) -> ConfigDirs {
        let environment = self.sandbox.environment();
        ConfigDirs::new(
            &environment["XDG_CONFIG_HOME"],
            &environment["XDG_DATA_HOME"],
        )
        .expect("the sandbox's directories")
    }

    /// The channel's config directory, made owner-only as `norn-config` makes
    /// it: the token file in it is refused under a directory the group can
    /// enter.
    pub fn config_dir(&self) -> PathBuf {
        let dir = self.dirs().config_dir().to_path_buf();
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&dir)
            .unwrap();
        dir
    }

    pub fn write_settings(&self, text: &str) -> PathBuf {
        let path = self.config_dir().join("config.toml");
        std::fs::write(&path, text).unwrap();
        path
    }

    /// Where the token file sits; the external-reader contract names it.
    pub fn tokens_file(&self) -> PathBuf {
        self.dirs().config_dir().join("tokens.toml")
    }

    /// The `default` token's secret as a bearer spells it.
    pub fn default_bearer(&self) -> String {
        self.bearer(&TokenLabel::default_label())
    }

    pub fn bearer(&self, label: &TokenLabel) -> String {
        let tokens = tokens::read(&self.dirs()).expect("reading the token file");
        let token = tokens
            .get(label)
            .unwrap_or_else(|| panic!("no token labelled `{label}`"));
        norn_wire::hex::encode(token.secret())
    }

    /// A run to completion, for a start expected to refuse.
    pub fn run(&self, args: &[&str]) -> (RunStatus, String) {
        let outcome = Run::new(&self.sandbox, &self.binary)
            .args(args)
            .deadline(START_WITHIN)
            .wait()
            .expect("running norn");
        (outcome.status, outcome.stderr_text())
    }

    /// A host left running, read line by line.
    pub fn start(&self, args: &[&str]) -> Served {
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

    /// A host started on `port` and listening, with its `default` bearer.
    pub fn serve_on(&self, port: u16) -> (Served, String) {
        let mut served = self.start(&["serve", "--port", &port.to_string()]);
        assert_eq!(served.listening_port(), port);
        let bearer = self.default_bearer();
        (served, bearer)
    }
}

pub struct Served {
    pub child: Child,
    lines: Receiver<String>,
    pub seen: Vec<String>,
}

impl Served {
    /// The first log line starting with `prefix`, every line before it kept.
    pub fn line_starting(&mut self, prefix: &str) -> String {
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

    /// Every line logged so far, without waiting.
    pub fn drain_lines(&mut self) -> &[String] {
        self.seen.extend(self.lines.try_iter());
        &self.seen
    }

    /// The port the `listening` line names.
    pub fn listening_port(&mut self) -> u16 {
        let line = self.line_starting("listening:");
        let port = line
            .split(", ")
            .find_map(|part| part.strip_prefix("port "))
            .unwrap_or_else(|| panic!("`{line}` names no port"));
        port.parse()
            .unwrap_or_else(|_| panic!("`{line}` names no port"))
    }

    /// SIGTERM, then the status and every remaining log line.
    pub fn terminate(self) -> (ExitStatus, Vec<String>) {
        self.signal(libc::SIGTERM)
    }

    pub fn signal(mut self, signal: libc::c_int) -> (ExitStatus, Vec<String>) {
        let pid = libc::pid_t::try_from(self.child.id()).unwrap();
        // SAFETY: a plain signal to the child this suite spawned and has not
        // reaped, so the pid still names it.
        assert_eq!(unsafe { libc::kill(pid, signal) }, 0);
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
pub fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .and_then(|listener| listener.local_addr())
        .expect("asking the kernel for a free port")
        .port()
}

/// One HTTP answer, as a plain client reads it.
#[derive(Debug)]
pub struct Reply {
    pub status: u16,
    pub head: String,
    pub body: Vec<u8>,
}

impl Reply {
    pub fn body_text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    /// The `code` of an error envelope body, or the empty string.
    pub fn code(&self) -> String {
        serde_json::from_slice::<serde_json::Value>(&self.body)
            .ok()
            .and_then(|value| value["code"].as_str().map(str::to_string))
            .unwrap_or_default()
    }

    /// A header's value, by a case-insensitive name.
    pub fn header(&self, name: &str) -> Option<String> {
        self.head.lines().skip(1).find_map(|line| {
            let (key, value) = line.split_once(':')?;
            key.eq_ignore_ascii_case(name)
                .then(|| value.trim().to_string())
        })
    }
}

/// One request on a connection of its own, with `Connection: close`.
pub fn exchange(port: u16, method: &str, path: &str, bearer: Option<&str>, body: &[u8]) -> Reply {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connecting to norn serve");
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let mut request = format!(
        "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\nContent-Length: {}\r\n",
        body.len()
    );
    if let Some(bearer) = bearer {
        request.push_str(&format!("Authorization: Bearer {bearer}\r\n"));
    }
    request.push_str("\r\n");
    stream.write_all(request.as_bytes()).unwrap();
    // A refusal may come before the body is read; a write the peer closed
    // under is not this helper's failure.
    let _ = stream.write_all(body);
    let mut answer = Vec::new();
    let _ = stream.read_to_end(&mut answer);
    parse_reply(&answer)
}

fn parse_reply(answer: &[u8]) -> Reply {
    let split = answer
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .unwrap_or_else(|| panic!("no HTTP head in {:?}", String::from_utf8_lossy(answer)));
    let head = String::from_utf8_lossy(&answer[..split]).into_owned();
    let status = head
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse().ok())
        .unwrap_or_else(|| panic!("no status in `{head}`"));
    Reply {
        status,
        head,
        body: answer[split + 4..].to_vec(),
    }
}

/// A bearer request, `GET` with no body.
pub fn get(port: u16, path: &str, bearer: Option<&str>) -> Reply {
    exchange(port, "GET", path, bearer, b"")
}

/// A hello with this label and a fixed, well-formed nonce.
pub fn hello(port: u16, label: &str) -> Reply {
    let body = serde_json::json!({ "label": label, "nonce": "ab".repeat(32) }).to_string();
    exchange(port, "POST", "/v1/hello", None, body.as_bytes())
}

/// Wait, by a bounded poll, until `condition` holds.
pub fn eventually(what: &str, mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + START_WITHIN;
    while !condition() {
        assert!(
            Instant::now() < deadline,
            "{what} did not happen within {START_WITHIN:?}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

pub fn assert_refused_start(status: RunStatus, stderr: &str, file: &Path) {
    assert_eq!(status, RunStatus::Exited(1), "{stderr}");
    assert!(stderr.contains("refused:"), "{stderr}");
    assert!(stderr.contains(&file.display().to_string()), "{stderr}");
    assert!(!stderr.contains("listening"), "{stderr}");
}
