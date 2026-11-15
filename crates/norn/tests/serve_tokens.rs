//! The bearer token as a running `norn serve` holds it: minted at the first
//! start and at no other, refused at the start when the file does not read,
//! and kept fresh against the file on every request, failing closed while a
//! changed file no longer reads.
//!
//! No route is served behind the bearer yet, so an authenticated request to
//! any path answers 404 with an empty body, and that is the success signal;
//! a refused one answers 401 `serve/unauthorized`.
#![allow(clippy::disallowed_methods, clippy::disallowed_types)] // Harness scaffolding: the token files a case arranges and damages.

mod support;

use std::io::{Seek, SeekFrom, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::Path;

use norn_config::machine::tokens::{self, TokenLabel};
use support::{Fixture, assert_refused_start, exchange, free_port, get, hello};

fn labels(fixture: &Fixture) -> Vec<String> {
    tokens::read(&fixture.dirs())
        .expect("reading the token file")
        .labels()
        .map(|label| label.to_string())
        .collect()
}

fn write_private(path: &Path, bytes: &[u8]) {
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .unwrap();
    file.write_all(bytes).unwrap();
}

const ONE_TOKEN: &str =
    "version = 1\n\n[tokens.default]\nsecret = \"00112233\"\ncreated = 1753900000\n";

// Exit bar 2: the mint.

#[test]
fn the_first_start_mints_one_default_token_and_a_restart_mints_none() {
    let fixture = Fixture::new("serve-mint-first");
    let port = free_port();
    let mut served = fixture.start(&["serve", "--port", &port.to_string()]);
    assert_eq!(served.listening_port(), port);
    assert_eq!(labels(&fixture), ["default"]);
    let bearer = fixture.default_bearer();
    assert_eq!(bearer.len(), 64, "a minted token is 32 bytes");
    let minted: Vec<&String> = served
        .seen
        .iter()
        .filter(|line| line.contains("minted"))
        .collect();
    assert_eq!(minted.len(), 1, "{:?}", served.seen);
    assert!(
        minted[0].contains(&fixture.tokens_file().display().to_string()),
        "{minted:?}"
    );
    let (status, lines) = served.terminate();
    assert_eq!(status.code(), Some(0), "{lines:?}");
    assert!(
        lines.iter().all(|line| !line.contains(&bearer)),
        "the secret reached the log: {lines:?}"
    );

    let mut again = fixture.start(&["serve", "--port", &port.to_string()]);
    assert_eq!(again.listening_port(), port);
    assert!(
        again.seen.iter().all(|line| !line.contains("minted")),
        "{:?}",
        again.seen
    );
    assert_eq!(labels(&fixture), ["default"]);
    assert_eq!(
        fixture.default_bearer(),
        bearer,
        "a restart keeps the token"
    );
    assert_eq!(again.terminate().0.code(), Some(0));
}

/// A file that already holds a token, under any label, is not minted into.
#[test]
fn a_start_over_a_held_token_mints_none() {
    let fixture = Fixture::new("serve-mint-held");
    fixture.config_dir();
    tokens::mutate(&fixture.dirs(), |tokens| {
        tokens.add(&TokenLabel::new("laptop").unwrap(), b"held")
    })
    .unwrap();
    let mut served = fixture.start(&["serve", "--port", &free_port().to_string()]);
    served.listening_port();
    assert_eq!(labels(&fixture), ["laptop"]);
    assert_eq!(served.terminate().0.code(), Some(0));
}

#[test]
fn two_concurrent_starts_on_one_config_directory_leave_exactly_one_token() {
    for round in 0..3 {
        let fixture = Fixture::new("serve-mint-race");
        let mut first = fixture.start(&["serve", "--port", &free_port().to_string()]);
        let mut second = fixture.start(&["serve", "--port", &free_port().to_string()]);
        // One takes the channel lock and listens; the other refuses.
        let (mut winner, mut loser) = loop {
            if first.child.try_wait().unwrap().is_some() {
                break (second, first);
            }
            if second.child.try_wait().unwrap().is_some() {
                break (first, second);
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        };
        winner.listening_port();
        let status = loser.child.wait().unwrap();
        assert_eq!(
            status.code(),
            Some(1),
            "round {round}: {:?}",
            loser.drain_lines()
        );
        assert_eq!(labels(&fixture), ["default"], "round {round}");
        assert_eq!(winner.terminate().0.code(), Some(0));
    }
}

#[test]
fn a_group_readable_token_file_refuses_the_start_naming_it() {
    let fixture = Fixture::new("serve-tokens-mode");
    fixture.config_dir();
    let path = fixture.tokens_file();
    write_private(&path, ONE_TOKEN.as_bytes());
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640)).unwrap();
    let (status, stderr) = fixture.run(&["serve", "--port", &free_port().to_string()]);
    assert_refused_start(status, &stderr, &path);
    assert!(stderr.contains("chmod 600"), "{stderr}");
}

#[test]
fn a_token_file_that_is_not_toml_refuses_the_start_naming_it() {
    let fixture = Fixture::new("serve-tokens-toml");
    fixture.config_dir();
    let path = fixture.tokens_file();
    write_private(&path, b"version = 1\n[tokens\n");
    let (status, stderr) = fixture.run(&["serve", "--port", &free_port().to_string()]);
    assert_refused_start(status, &stderr, &path);
    assert!(stderr.contains("move it aside"), "{stderr}");
}

#[test]
fn a_token_file_over_one_mebibyte_refuses_the_start_naming_it() {
    let fixture = Fixture::new("serve-tokens-large");
    fixture.config_dir();
    let path = fixture.tokens_file();
    let mut text = String::from(ONE_TOKEN);
    while text.len() <= 1 << 20 {
        text.push_str("# padding that keeps the file valid TOML\n");
    }
    write_private(&path, text.as_bytes());
    let (status, stderr) = fixture.run(&["serve", "--port", &free_port().to_string()]);
    assert_refused_start(status, &stderr, &path);
}

#[test]
fn a_fifo_at_the_token_file_refuses_the_start_naming_it() {
    let fixture = Fixture::new("serve-tokens-fifo");
    fixture.config_dir();
    let path = fixture.tokens_file();
    let cpath = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
    // SAFETY: a live NUL-terminated path; no pointers are retained.
    assert_eq!(unsafe { libc::mkfifo(cpath.as_ptr(), 0o600) }, 0);
    let (status, stderr) = fixture.run(&["serve", "--port", &free_port().to_string()]);
    assert_refused_start(status, &stderr, &path);
}

// Exit bar 3: freshness, against a running host.

#[test]
fn a_revocation_by_rewrite_is_refused_on_the_next_request() {
    let fixture = Fixture::new("serve-revoke");
    let port = free_port();
    let (served, old) = fixture.serve_on(port);
    assert_eq!(get(port, "/", Some(&old)).status, 404);

    let laptop = TokenLabel::new("laptop").unwrap();
    tokens::mutate(&fixture.dirs(), |tokens| {
        tokens.remove(&TokenLabel::default_label());
        tokens.add(&laptop, &[7; 32])
    })
    .unwrap();

    let refused = get(port, "/", Some(&old));
    assert_eq!(refused.status, 401, "{refused:?}");
    assert_eq!(refused.code(), "serve/unauthorized");
    assert_eq!(get(port, "/", Some(&fixture.bearer(&laptop))).status, 404);
    assert_eq!(served.terminate().0.code(), Some(0));
}

#[test]
fn an_in_place_same_size_edit_of_the_secret_is_refused_on_the_next_request() {
    let fixture = Fixture::new("serve-edit-in-place");
    let port = free_port();
    let (served, old) = fixture.serve_on(port);
    assert_eq!(get(port, "/", Some(&old)).status, 404);

    let path = fixture.tokens_file();
    let before = std::fs::metadata(&path).unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    let offset = text.find(&old).expect("the secret is in the file");
    // The same digits with the first one changed: same length, still hex.
    let first = if old.starts_with('0') { "1" } else { "0" };
    let new = format!("{first}{}", &old[1..]);
    let mut file = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
    file.seek(SeekFrom::Start(offset as u64)).unwrap();
    file.write_all(new.as_bytes()).unwrap();
    drop(file);
    let after = std::fs::metadata(&path).unwrap();
    assert_eq!((after.ino(), after.size()), (before.ino(), before.size()));

    let refused = get(port, "/", Some(&old));
    assert_eq!(refused.status, 401, "{refused:?}");
    assert_eq!(get(port, "/", Some(&new)).status, 404);
    assert_eq!(served.terminate().0.code(), Some(0));
}

#[test]
fn a_changed_file_that_no_longer_reads_refuses_every_request_until_it_reads_again() {
    let fixture = Fixture::new("serve-fail-closed");
    let port = free_port();
    let (mut served, bearer) = fixture.serve_on(port);
    assert_eq!(get(port, "/", Some(&bearer)).status, 404);
    assert_eq!(hello(port, "default").status, 200);

    let path = fixture.tokens_file();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640)).unwrap();
    for reply in [
        get(port, "/", Some(&bearer)),
        get(port, "/", None),
        get(port, "/v1/debug/counters", Some(&bearer)),
        hello(port, "default"),
        // The state is answered before a hello's body is read, so neither a
        // malformed body nor one over the cap is told apart from a good one.
        exchange(port, "POST", "/v1/hello", None, b"[1]"),
        exchange(port, "POST", "/v1/hello", None, b"notjson"),
        exchange(port, "POST", "/v1/hello", None, br#"{"label":"default"}"#),
        exchange(port, "POST", "/v1/hello", None, &[b' '; 5000]),
    ] {
        assert_eq!(reply.status, 500, "{reply:?}");
        assert_eq!(reply.code(), "serve/token-file-unreadable", "{reply:?}");
        assert!(!reply.body_text().contains("mac"), "{reply:?}");
    }
    // The log line is written by the host and read here by another thread,
    // so it is waited for rather than drained: a drain can run before the
    // line has crossed the pipe. That it is logged only once is counted over
    // the whole run, once the host has stopped.
    let warned = served.line_starting("warning: the token file");
    assert!(
        warned.contains(&path.display().to_string()) && warned.contains("chmod 600"),
        "{warned}"
    );

    // No pause before the restore: while the host fails closed it reads the
    // file again on every request, so a restore inside the damage's own
    // timestamp tick is still seen.
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    assert_eq!(get(port, "/", Some(&bearer)).status, 404);
    assert_eq!(hello(port, "default").status, 200);
    let (status, lines) = served.terminate();
    assert_eq!(status.code(), Some(0));
    let warnings = lines
        .iter()
        .filter(|line| line.starts_with("warning: the token file"))
        .count();
    assert_eq!(warnings, 1, "logged once on entering: {lines:?}");
}
