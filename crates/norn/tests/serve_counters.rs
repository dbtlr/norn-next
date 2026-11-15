//! The token counter bar (R1), read through the counters route of a binary
//! built with the `counters` feature.
//!
//! An unchanged token file costs one fingerprint per request and no read, and
//! authentication is one lookup by label or by digest, at one token and at
//! many. Each reading is a delta between two reads of the route, and the
//! second read is itself a bearer request: it costs one fingerprint and one
//! lookup of its own, which every expectation below names as `+ 1`.
//!
//! Every reading is taken on a settled token file: a fingerprint taken
//! within 3 s of the file's last write is not trusted, and each check reads
//! the file again until it is that old. So each case waits once, after its
//! last write to the file, before the reading it asserts the steady state
//! on; the first check after the wait may still read once, and it falls
//! before the reading.
//!
//! The host counts a lookup inside the held token set's two lookup methods,
//! by digest and by label, so `serve.token_lookups` is the lookups made
//! through the index. A walk over the tokens added beside the index makes no
//! counted lookup, and this bar cannot see it: `norn-serve`'s poisoned-index
//! unit test catches one, and review catches the rest.
#![cfg(feature = "counters")]

mod support;

use std::collections::BTreeMap;

use norn_config::machine::tokens::{self, TokenLabel};
use support::{Fixture, free_port, get, hello, settle_token_file};

type Counters = BTreeMap<String, u64>;

fn counters(port: u16, bearer: &str) -> Counters {
    let reply = get(port, "/v1/debug/counters", Some(bearer));
    assert_eq!(reply.status, 200, "{reply:?}");
    serde_json::from_slice(&reply.body).expect("one object of name to integer")
}

fn delta(before: &Counters, after: &Counters) -> [u64; 3] {
    [
        "serve.token_fingerprints",
        "serve.token_file_reads",
        "serve.token_lookups",
    ]
    .map(|name| after[name] - before[name])
}

/// A host over `count` tokens, `default` among them.
fn host_over(label: &str, count: usize) -> (Fixture, support::Served, u16, String) {
    let fixture = Fixture::new(label);
    fixture.config_dir();
    tokens::mutate(&fixture.dirs(), |tokens| {
        tokens.add(&TokenLabel::default_label(), &[1; 32])?;
        for n in 1..count {
            tokens.add(&TokenLabel::new(format!("t{n}")).unwrap(), &[n as u8; 32])?;
        }
        Ok(())
    })
    .unwrap();
    let port = free_port();
    let (served, bearer) = fixture.serve_on(port);
    (fixture, served, port, bearer)
}

const REQUESTS: u64 = 20;

#[test]
fn an_unchanged_file_costs_one_fingerprint_no_read_and_one_lookup_per_request() {
    for count in [1, 64] {
        let (fixture, served, port, bearer) = host_over("counters-unchanged", count);
        settle_token_file(&fixture.tokens_file());
        let before = counters(port, &bearer);
        for _ in 0..REQUESTS {
            assert_eq!(get(port, "/", Some(&bearer)).status, 404);
        }
        let measured = delta(&before, &counters(port, &bearer));
        eprintln!(
            "{count} tokens, {REQUESTS} bearer requests: [fingerprints, reads, lookups] = {measured:?}"
        );
        assert_eq!(measured, [REQUESTS + 1, 0, REQUESTS + 1], "{count} tokens");
        assert_eq!(served.terminate().0.code(), Some(0));
    }
}

#[test]
fn one_hello_costs_one_fingerprint_and_one_lookup() {
    for count in [1, 64] {
        let (fixture, served, port, bearer) = host_over("counters-hello", count);
        settle_token_file(&fixture.tokens_file());
        let before = counters(port, &bearer);
        assert_eq!(hello(port, "default").status, 200);
        let measured = delta(&before, &counters(port, &bearer));
        eprintln!("{count} tokens, one hello: [fingerprints, reads, lookups] = {measured:?}");
        assert_eq!(measured, [1 + 1, 0, 1 + 1], "{count} tokens");
        assert_eq!(served.terminate().0.code(), Some(0));
    }
}

/// An edit costs one re-read, on the next request, and the request after
/// reads nothing again. A changed check takes two fingerprints. The one wait
/// falls between the edit and that request, so the fingerprint the re-read
/// holds is settled.
#[test]
fn an_edit_costs_exactly_one_re_read_on_the_next_request() {
    let (fixture, served, port, bearer) = host_over("counters-edit", 1);
    let before = counters(port, &bearer);
    tokens::mutate(&fixture.dirs(), |tokens| {
        tokens.add(&TokenLabel::new("laptop").unwrap(), &[9; 32])
    })
    .unwrap();
    settle_token_file(&fixture.tokens_file());
    assert_eq!(get(port, "/", Some(&bearer)).status, 404);
    let measured = delta(&before, &counters(port, &bearer));
    eprintln!("one edit, one bearer request: [fingerprints, reads, lookups] = {measured:?}");
    assert_eq!(measured, [2 + 1, 1, 1 + 1]);

    let before = counters(port, &bearer);
    assert_eq!(get(port, "/", Some(&bearer)).status, 404);
    assert_eq!(delta(&before, &counters(port, &bearer)), [1 + 1, 0, 1 + 1]);
    assert_eq!(served.terminate().0.code(), Some(0));
}
