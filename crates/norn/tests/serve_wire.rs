//! The wire route over a real `norn serve`: every served verb answered over
//! loopback behind the bearer, a failure told apart from a success by its
//! status and envelope, and — under the `counters` feature — the readiness
//! retry's counters across an idle detach, an immediate refusal, and the
//! transport and request bars.
#![allow(clippy::disallowed_methods)] // Harness scaffolding: the vault a case serves.

mod support;

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use norn_wire::{
    ApplyParams, CountParams, DeleteParams, DescribeParams, DoctorRegistryParams, EditParams,
    FindParams, GetParams, InitParams, ListParams, MigrateParams, MoveParams, NewParams,
    RegisterParams, ReloadParams, RepairParams, ResolveParams, RewriteWikilinkParams, SearchParams,
    SetParams, StatusParams, UnregisterParams, ValidateParams, VaultSetParams, Verb,
};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use support::{Fixture, Reply, Served, exchange, free_port};

/// A vault of three documents, two linked, under the fixture's work
/// directory.
fn a_vault(fixture: &Fixture, name: &str) -> PathBuf {
    let root = fixture.sandbox.work_dir().join(name);
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(
        root.join("alpha.md"),
        "---\nstatus: draft\n---\n# Alpha\n\nalpha body, see [[beta]]\n",
    )
    .unwrap();
    std::fs::write(root.join("beta.md"), "# Beta\n\nbeta body\n").unwrap();
    std::fs::write(
        root.join("delta.md"),
        "# Delta\n\ndelta body, see [[beta]]\n",
    )
    .unwrap();
    root
}

/// Params as the JSON they serialize to, whatever their type.
mod erased {
    pub trait Serialize {
        fn to_value(&self) -> serde_json::Value;
    }

    impl<T: serde::Serialize> Serialize for T {
        fn to_value(&self) -> serde_json::Value {
            serde_json::to_value(self).expect("params serialize")
        }
    }
}

fn target(text: &str) -> norn_wire::ResolutionTarget {
    norn_wire::ResolutionTarget::new(text).unwrap()
}

fn address(name: &str) -> Value {
    json!({ "by": "name", "name": name })
}

/// A host on a fresh port, with `settings` as its `config.toml`.
fn host(label: &str, settings: &str) -> (Fixture, Served, u16, String) {
    let fixture = Fixture::new(label);
    fixture.write_settings(settings);
    let port = free_port();
    let (served, bearer) = fixture.serve_on(port);
    (fixture, served, port, bearer)
}

/// `verb` with `body`, behind `bearer`. The body is first read as the verb's
/// params here, so a body this suite spelled wrong fails as that, not as the
/// host's refusal.
fn call<P: DeserializeOwned>(port: u16, bearer: &str, verb: Verb, body: &Value) -> Reply {
    let bytes = serde_json::to_vec(body).unwrap();
    if let Err(error) = serde_json::from_slice::<P>(&bytes) {
        panic!("this suite's `{verb}` body is not its params: {error}: {body}");
    }
    exchange(
        port,
        "POST",
        &format!("/v1/wire/{}", verb.as_str()),
        Some(bearer),
        &bytes,
    )
}

fn json_of(reply: &Reply) -> Value {
    serde_json::from_slice(&reply.body)
        .unwrap_or_else(|_| panic!("not JSON: {}", reply.body_text()))
}

/// The first resolved plan anywhere in `value`.
fn resolved_plan(value: &Value) -> Option<Value> {
    match value {
        Value::Object(map) => {
            if map.get("plan").and_then(Value::as_str) == Some("resolved") {
                return Some(value.clone());
            }
            map.values().find_map(resolved_plan)
        }
        Value::Array(items) => items.iter().find_map(resolved_plan),
        _ => None,
    }
}

fn register(port: u16, bearer: &str, name: &str, root: &Path) -> Reply {
    call::<RegisterParams>(
        port,
        bearer,
        Verb::VaultRegister,
        &json!({ "registration": { "name": name, "root": root } }),
    )
}

/// **Exit bar 1.** A foreground `norn serve` serves every wire verb over
/// loopback behind the bearer: each served verb is called once, with a body
/// of its own params, and each is answered by its host method — a report, or
/// a refusal of the vault's own, never `serve/unknown-verb` or
/// `serve/bad-request`.
#[test]
fn every_served_verb_is_answered_over_loopback_behind_the_bearer() {
    let (fixture, served, port, bearer) = host("serve-wire-every-verb", "");
    let root = a_vault(&fixture, "notes");
    let notes = address("notes");
    let mut called: BTreeSet<&'static str> = BTreeSet::new();
    let mut answered = |verb: Verb, reply: Reply| -> Reply {
        assert!(
            !matches!(
                reply.code().as_str(),
                "serve/unknown-verb" | "serve/bad-request"
            ),
            "`{verb}` was not served: {} {}",
            reply.status,
            reply.body_text()
        );
        assert_eq!(
            reply.header("content-type").as_deref(),
            Some("application/json"),
            "`{verb}`"
        );
        called.insert(verb.as_str());
        reply
    };

    // No bearer, no verb.
    let refused = exchange(port, "POST", "/v1/wire/vault_list", None, b"{}");
    assert_eq!(refused.status, 401);
    assert_eq!(refused.code(), "serve/unauthorized");

    let reply = answered(Verb::VaultRegister, register(port, &bearer, "notes", &root));
    assert_eq!(reply.status, 200, "{}", reply.body_text());
    let reply = answered(
        Verb::VaultList,
        call::<ListParams>(port, &bearer, Verb::VaultList, &json!({})),
    );
    assert_eq!(reply.status, 200);
    assert!(reply.body_text().contains("notes"));
    let reply = answered(
        Verb::VaultResolve,
        call::<ResolveParams>(
            port,
            &bearer,
            Verb::VaultResolve,
            &json!({ "directory": root }),
        ),
    );
    assert_eq!(reply.status, 200, "{}", reply.body_text());
    let reply = answered(
        Verb::VaultStatus,
        call::<StatusParams>(port, &bearer, Verb::VaultStatus, &json!({ "vault": notes })),
    );
    assert_eq!(reply.status, 200, "{}", reply.body_text());
    let reply = answered(
        Verb::DoctorRegistry,
        call::<DoctorRegistryParams>(port, &bearer, Verb::DoctorRegistry, &json!({})),
    );
    assert_eq!(reply.status, 200, "{}", reply.body_text());

    // The reads: the first meets a warming entry and is answered once it
    // serves, by the readiness retry.
    let typed = || norn_wire::VaultAddress::name(norn_wire::VaultName::new("notes").unwrap());
    let value = |params: &dyn erased::Serialize| params.to_value();
    for (verb, body) in [
        (Verb::Find, value(&FindParams::new(typed()))),
        (Verb::Search, value(&SearchParams::new(typed(), "alpha"))),
        (Verb::Get, value(&GetParams::new(typed(), target("alpha")))),
        (Verb::Count, value(&CountParams::new(typed()))),
        (Verb::Validate, value(&ValidateParams::new(typed()))),
        (Verb::Describe, value(&DescribeParams::new(typed()))),
    ] {
        let reply = match verb {
            Verb::Find => call::<FindParams>(port, &bearer, verb, &body),
            Verb::Search => call::<SearchParams>(port, &bearer, verb, &body),
            Verb::Get => call::<GetParams>(port, &bearer, verb, &body),
            Verb::Count => call::<CountParams>(port, &bearer, verb, &body),
            Verb::Validate => call::<ValidateParams>(port, &bearer, verb, &body),
            _ => call::<DescribeParams>(port, &bearer, verb, &body),
        };
        let reply = answered(verb, reply);
        assert_eq!(reply.status, 200, "`{verb}`: {}", reply.body_text());
    }

    // The writes, each applied or previewed through its own verb.
    let reply = answered(
        Verb::Set,
        call::<SetParams>(
            port,
            &bearer,
            Verb::Set,
            &json!({
                "vault": notes, "mode": "apply", "path": "alpha.md",
                "changes": [{ "change": "set", "field": "status", "value": "done" }],
            }),
        ),
    );
    assert_eq!(reply.status, 200, "{}", reply.body_text());
    assert!(
        std::fs::read_to_string(root.join("alpha.md"))
            .unwrap()
            .contains("status: done")
    );
    let reply = answered(
        Verb::Edit,
        call::<EditParams>(
            port,
            &bearer,
            Verb::Edit,
            &json!({
                "vault": notes, "mode": "apply", "path": "alpha.md",
                "edits": [{ "edit": "str_replace", "old_str": "alpha body", "new_str": "alpha text" }],
            }),
        ),
    );
    assert_eq!(reply.status, 200, "{}", reply.body_text());
    let reply = answered(
        Verb::New,
        call::<NewParams>(
            port,
            &bearer,
            Verb::New,
            &json!({ "vault": notes, "mode": "apply", "path": "gamma.md", "content": "# Gamma\n" }),
        ),
    );
    assert_eq!(reply.status, 200, "{}", reply.body_text());
    let reply = answered(
        Verb::Move,
        call::<MoveParams>(
            port,
            &bearer,
            Verb::Move,
            &json!({ "vault": notes, "mode": "apply", "from": "gamma.md", "to": "archive/gamma.md" }),
        ),
    );
    assert_eq!(reply.status, 200, "{}", reply.body_text());
    let reply = answered(
        Verb::RewriteWikilink,
        call::<RewriteWikilinkParams>(
            port,
            &bearer,
            Verb::RewriteWikilink,
            &json!({ "vault": notes, "mode": "preview", "old": "beta", "new": "delta" }),
        ),
    );
    assert_eq!(reply.status, 200, "{}", reply.body_text());
    let reply = answered(
        Verb::Delete,
        call::<DeleteParams>(
            port,
            &bearer,
            Verb::Delete,
            &json!({ "vault": notes, "mode": "apply", "path": "archive/gamma.md" }),
        ),
    );
    assert_eq!(reply.status, 200, "{}", reply.body_text());

    // `apply`: a set previewed, then its resolved plan sent back to apply.
    let previewed = call::<SetParams>(
        port,
        &bearer,
        Verb::Set,
        &json!({
            "vault": notes, "mode": "preview", "path": "beta.md",
            "changes": [{ "change": "set", "field": "status", "value": "done" }],
        }),
    );
    assert_eq!(previewed.status, 200, "{}", previewed.body_text());
    let plan = resolved_plan(&json_of(&previewed)).expect("a preview carries its resolved plan");
    let reply = answered(
        Verb::Apply,
        call::<ApplyParams>(
            port,
            &bearer,
            Verb::Apply,
            &json!({ "mode": "apply", "plan": plan }),
        ),
    );
    assert_eq!(reply.status, 200, "{}", reply.body_text());
    assert!(
        std::fs::read_to_string(root.join("beta.md"))
            .unwrap()
            .contains("status: done")
    );

    for (verb, body) in [
        (Verb::Repair, json!({ "vault": notes, "mode": "preview" })),
        (Verb::Init, json!({ "vault": notes, "mode": "preview" })),
        (
            Verb::VaultMigrate,
            json!({ "vault": notes, "mode": "preview" }),
        ),
        (
            Verb::VaultReload,
            json!({ "vault": notes, "dry_run": false }),
        ),
    ] {
        let reply = match verb {
            Verb::Repair => call::<RepairParams>(port, &bearer, verb, &body),
            Verb::Init => call::<InitParams>(port, &bearer, verb, &body),
            Verb::VaultMigrate => call::<MigrateParams>(port, &bearer, verb, &body),
            _ => call::<ReloadParams>(port, &bearer, verb, &body),
        };
        let reply = answered(verb, reply);
        assert!(
            reply.status == 200 || reply.code().starts_with("vault/"),
            "`{verb}`: {} {}",
            reply.status,
            reply.body_text()
        );
    }

    let reply = answered(
        Verb::VaultSet,
        call::<VaultSetParams>(
            port,
            &bearer,
            Verb::VaultSet,
            &value(&VaultSetParams::new(
                norn_wire::VaultName::new("notes").unwrap(),
            )),
        ),
    );
    // A registration change over an entry holding its attachment is refused
    // by the host, as its own envelope.
    assert!(
        reply.status == 200 || reply.code() == "host/entry-held",
        "{} {}",
        reply.status,
        reply.body_text()
    );
    let reply = answered(
        Verb::VaultUnregister,
        call::<UnregisterParams>(
            port,
            &bearer,
            Verb::VaultUnregister,
            &value(&UnregisterParams::new(
                norn_wire::VaultName::new("notes").unwrap(),
            )),
        ),
    );
    // An entry still holding its attachment refuses to be retired; that is
    // the host method's own answer, carried as its envelope.
    assert!(
        reply.status == 200 || reply.code() == "host/entry-held",
        "{} {}",
        reply.status,
        reply.body_text()
    );
    if reply.status == 200 {
        let reply =
            call::<FindParams>(port, &bearer, Verb::Find, &value(&FindParams::new(typed())));
        assert_eq!(reply.status, 404, "an unregistered vault is missing");
        assert_eq!(reply.code(), "host/unknown-vault");
    }

    let every: BTreeSet<&str> = Verb::ALL.iter().map(Verb::as_str).collect();
    assert_eq!(called, every, "every verb in the vocabulary is served");
    assert_eq!(served.terminate().0.code(), Some(0));
}

/// A path naming no verb, and a body that is not the verb's params, are the
/// server's own refusals.
#[test]
fn an_unknown_verb_is_404_and_an_unparsed_body_is_400() {
    let (_fixture, served, port, bearer) = host("serve-wire-refusals", "");
    let reply = exchange(port, "POST", "/v1/wire/frobnicate", Some(&bearer), b"{}");
    assert_eq!(
        (reply.status, reply.code().as_str()),
        (404, "serve/unknown-verb")
    );
    let reply = exchange(
        port,
        "POST",
        "/v1/wire/find",
        Some(&bearer),
        b"{\"vault\":1}",
    );
    assert_eq!(
        (reply.status, reply.code().as_str()),
        (400, "serve/bad-request")
    );
    let reply = exchange(port, "POST", "/v1/wire/set", Some(&bearer), b"not json");
    assert_eq!(
        (reply.status, reply.code().as_str()),
        (400, "serve/bad-request")
    );
    assert_eq!(served.terminate().0.code(), Some(0));
}

/// **Failure is carried in the protocol's native channel.** On the real
/// route, a completed apply, a preflight refusal and an apply that partly
/// landed are told apart by status and envelope alone, with no in-band
/// report read: 200 with a report; 409 `vault/plan-refused`, nothing
/// written; 503 `vault/plan-interrupted`, naming what landed. Each failure's
/// envelope is the one shape every refusal takes, its code machine-readable.
#[test]
fn a_completed_apply_a_preflight_refusal_and_a_partial_apply_are_told_apart_by_status() {
    let (fixture, served, port, bearer) = host("serve-wire-native-channel", "");
    let root = a_vault(&fixture, "notes");
    let notes = address("notes");
    assert_eq!(register(port, &bearer, "notes", &root).status, 200);

    // Completed.
    let completed = call::<SetParams>(
        port,
        &bearer,
        Verb::Set,
        &json!({
            "vault": notes, "mode": "apply", "path": "alpha.md",
            "changes": [{ "change": "set", "field": "status", "value": "done" }],
        }),
    );
    assert_eq!(completed.status, 200, "{}", completed.body_text());
    assert_eq!(completed.code(), "", "a success carries no envelope");

    // Refused before anything was written: a create whose name is taken.
    let refused = call::<NewParams>(
        port,
        &bearer,
        Verb::New,
        &json!({ "vault": notes, "mode": "apply", "path": "beta.md", "content": "# Taken\n" }),
    );
    assert_eq!(refused.status, 409, "{}", refused.body_text());
    assert_eq!(refused.code(), "vault/plan-refused");
    let envelope = json_of(&refused);
    assert_eq!(envelope["detail"]["code"], "vault/plan-refused");
    assert!(envelope["message"].is_string());
    assert_eq!(
        std::fs::read_to_string(root.join("beta.md")).unwrap(),
        "# Beta\n\nbeta body\n",
        "nothing was written"
    );

    // Partly landed: a plan that creates one document, then another in a
    // folder that refuses the write. The first lands; publication stops at
    // the second.
    let locked = root.join("locked");
    std::fs::create_dir(&locked).unwrap();
    let preview = call::<ApplyParams>(
        port,
        &bearer,
        Verb::Apply,
        &json!({
            "mode": "preview",
            "plan": {
                "plan": "operations",
                "vault": notes,
                "operations": [
                    { "kind": "create_document", "fields": { "path": "first.md", "content": "# First\n" } },
                    { "kind": "create_document", "fields": { "path": "locked/second.md", "content": "# Second\n" } },
                ],
            },
        }),
    );
    assert_eq!(preview.status, 200, "{}", preview.body_text());
    let plan = resolved_plan(&json_of(&preview)).expect("a preview carries its resolved plan");
    set_mode(&locked, 0o555);
    let partial = call::<ApplyParams>(
        port,
        &bearer,
        Verb::Apply,
        &json!({ "mode": "apply", "plan": plan }),
    );
    set_mode(&locked, 0o755);
    assert_eq!(partial.status, 503, "{}", partial.body_text());
    assert_eq!(partial.code(), "vault/plan-interrupted");
    let detail = &json_of(&partial)["detail"];
    assert_eq!(detail["code"], "vault/plan-interrupted");
    assert!(
        detail.to_string().contains("first.md"),
        "it names what landed: {detail}"
    );
    assert!(root.join("first.md").exists(), "the first target landed");
    assert!(!locked.join("second.md").exists());
    assert_eq!(served.terminate().0.code(), Some(0));
}

fn set_mode(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
}

#[cfg(feature = "counters")]
mod counted {
    use std::collections::BTreeMap;
    use std::time::{Duration, Instant};

    use super::*;
    use support::get;

    pub(super) type Counters = BTreeMap<String, u64>;

    pub(super) fn counters(port: u16, bearer: &str) -> Counters {
        let reply = get(port, "/v1/debug/counters", Some(bearer));
        assert_eq!(reply.status, 200, "{reply:?}");
        serde_json::from_slice(&reply.body).expect("one object of name to integer")
    }

    pub(super) fn delta(before: &Counters, after: &Counters, name: &str) -> u64 {
        after[name] - before[name]
    }

    /// The readiness retry's bound for a `ready_wait` of `wait`, by the
    /// route's formula: the pauses 50, 100, 200, 400, 800 ms, then 1 s each,
    /// whose running sum stays at or below `wait`.
    pub(super) fn most_redispatches(wait: Duration) -> u64 {
        let (mut pause, mut waited, mut count) = (Duration::from_millis(50), Duration::ZERO, 0);
        while waited + pause <= wait {
            waited += pause;
            count += 1;
            pause = (pause * 2).min(Duration::from_secs(1));
        }
        count
    }

    fn get_alpha(port: u16, bearer: &str) -> Reply {
        call::<GetParams>(
            port,
            bearer,
            Verb::Get,
            &serde_json::to_value(GetParams::new(
                norn_wire::VaultAddress::name(norn_wire::VaultName::new("notes").unwrap()),
                target("alpha"),
            ))
            .unwrap(),
        )
    }

    fn state_of(port: u16, bearer: &str) -> String {
        let reply = call::<StatusParams>(
            port,
            bearer,
            Verb::VaultStatus,
            &json!({ "vault": address("notes") }),
        );
        json_of(&reply)["status"]["published"]["state"]["state"]
            .as_str()
            .unwrap_or_default()
            .to_string()
    }

    /// **Exit bar 8, and the production idle-detach policy.** A vault idle
    /// past `idle_after` detaches; the next read is answered, not refused,
    /// within `ready_wait`; its re-dispatches stay within the bound; and
    /// once it is answered nothing more is dispatched. The re-attach of the
    /// unchanged vault derives nothing and opens each document once.
    #[test]
    fn an_idle_detach_is_answered_within_ready_wait_and_nothing_follows_the_answer() {
        let (fixture, served, port, bearer) =
            host("serve-wire-idle-detach", "[serve]\nidle_after = \"1m\"\n");
        let root = a_vault(&fixture, "notes");
        assert_eq!(register(port, &bearer, "notes", &root).status, 200);
        assert_eq!(get_alpha(port, &bearer).status, 200);

        let detached_by = Instant::now() + Duration::from_secs(150);
        while state_of(port, &bearer) != "unattached" {
            assert!(
                Instant::now() < detached_by,
                "the vault never detached: {}",
                state_of(port, &bearer)
            );
            std::thread::sleep(Duration::from_secs(1));
        }

        let before = counters(port, &bearer);
        let asked = Instant::now();
        let reply = get_alpha(port, &bearer);
        let took = asked.elapsed();
        assert_eq!(reply.status, 200, "{}", reply.body_text());
        let after = counters(port, &bearer);
        let dispatches = delta(&before, &after, "serve.dispatches");
        let retries = delta(&before, &after, "serve.ready_retries");
        let ready_wait = Duration::from_secs(30);
        eprintln!(
            "first get after an idle detach: {took:?}, {dispatches} dispatches, {retries} \
             re-dispatches; re-attach: {} documents opened, {} derived, {} changesets",
            delta(&before, &after, "host.jobs.document_opens"),
            delta(&before, &after, "host.jobs.documents_derived"),
            delta(&before, &after, "host.jobs.changesets_applied"),
        );
        assert!(took < ready_wait, "{took:?}");
        assert_eq!(dispatches, retries + 1);
        assert!(retries <= most_redispatches(ready_wait), "{retries}");
        // The re-attach of an unchanged vault derives nothing, writes nothing,
        // and opens each of its three documents once.
        assert_eq!(delta(&before, &after, "host.jobs.documents_derived"), 0);
        assert_eq!(delta(&before, &after, "host.jobs.changesets_applied"), 0);
        assert!(delta(&before, &after, "host.jobs.document_opens") <= 3);

        // No dispatch follows the answer.
        std::thread::sleep(Duration::from_millis(1500));
        let later = counters(port, &bearer);
        assert_eq!(delta(&after, &later, "serve.dispatches"), 0);
        assert_eq!(served.terminate().0.code(), Some(0));
    }

    /// **Early admission is not delayed, and an immediate refusal passes
    /// through at once.** A request over a vault this host does not serve is
    /// one dispatch and no retry, and a ready vault's read is one dispatch.
    #[test]
    fn an_immediate_refusal_and_a_ready_read_are_one_dispatch_each() {
        let (fixture, served, port, bearer) = host("serve-wire-one-dispatch", "");
        let root = a_vault(&fixture, "notes");
        assert_eq!(register(port, &bearer, "notes", &root).status, 200);
        assert_eq!(get_alpha(port, &bearer).status, 200);

        let before = counters(port, &bearer);
        let asked = Instant::now();
        let reply = call::<GetParams>(
            port,
            &bearer,
            Verb::Get,
            &serde_json::to_value(GetParams::new(
                norn_wire::VaultAddress::name(norn_wire::VaultName::new("elsewhere").unwrap()),
                target("alpha"),
            ))
            .unwrap(),
        );
        let took = asked.elapsed();
        assert_eq!(
            (reply.status, reply.code().as_str()),
            (404, "host/unknown-vault")
        );
        let after = counters(port, &bearer);
        assert_eq!(delta(&before, &after, "serve.dispatches"), 1);
        assert_eq!(delta(&before, &after, "serve.ready_retries"), 0);
        assert!(took < Duration::from_millis(500), "{took:?}");

        let before = counters(port, &bearer);
        for _ in 0..10 {
            assert_eq!(get_alpha(port, &bearer).status, 200);
        }
        let after = counters(port, &bearer);
        assert_eq!(delta(&before, &after, "serve.dispatches"), 10);
        assert_eq!(delta(&before, &after, "serve.ready_retries"), 0);
        assert_eq!(served.terminate().0.code(), Some(0));
    }

    /// **Exit bar 8: a parked vault's refusal passes through at once.** A
    /// vault whose root's identity cannot be read stands parked once an attach
    /// has met it, and a read of it is then refused with the park's own code:
    /// one dispatch, no readiness retry, answered at once.
    #[test]
    fn a_parked_vaults_refusal_passes_through_at_once() {
        let (fixture, served, port, bearer) = host("serve-wire-parked", "");
        let root = a_vault(&fixture, "locked/notes");
        assert_eq!(register(port, &bearer, "notes", &root).status, 200);
        assert_eq!(get_alpha(port, &bearer).status, 200);
        assert_eq!(served.terminate().0.code(), Some(0));

        // The root's parent refuses every lookup, so the next attach cannot
        // read the root's identity and parks the entry: the first read after
        // the start is the one whose attach meets it, and answers the park.
        let locked = root.parent().unwrap().to_path_buf();
        set_mode(&locked, 0o000);
        let port = free_port();
        let (served, bearer) = fixture.serve_on(port);
        let parking = get_alpha(port, &bearer);
        let before = counters(port, &bearer);
        let asked = Instant::now();
        let reply = get_alpha(port, &bearer);
        let took = asked.elapsed();
        let after = counters(port, &bearer);
        // Put back before any assertion, so a failing case leaves a tree its
        // sandbox can remove.
        set_mode(&locked, 0o755);
        assert_eq!(
            parking.code(),
            "host/entry-untrusted",
            "{}",
            parking.body_text()
        );
        eprintln!(
            "a read of a parked vault: {took:?}, {} {}",
            reply.status,
            reply.code()
        );
        assert_eq!(
            (reply.status, reply.code().as_str()),
            (500, "host/entry-untrusted"),
            "{}",
            reply.body_text()
        );
        assert_eq!(delta(&before, &after, "serve.dispatches"), 1);
        assert_eq!(delta(&before, &after, "serve.ready_retries"), 0);
        assert!(took < Duration::from_millis(500), "{took:?}");
        assert_eq!(served.terminate().0.code(), Some(0));
    }

    /// A host serving `count` vaults, `notes` among them, with `notes` ready.
    fn serving_vaults(label: &str, count: usize) -> (Fixture, Served, u16, String) {
        let (fixture, served, port, bearer) = host(label, "");
        for n in 0..count {
            let name = if n == 0 {
                "notes".to_string()
            } else {
                format!("other{n}")
            };
            let root = a_vault(&fixture, &name);
            assert_eq!(register(port, &bearer, &name, &root).status, 200);
        }
        assert_eq!(get_alpha(port, &bearer).status, 200);
        (fixture, served, port, bearer)
    }

    /// **Serving walks no registration or connection list it does not
    /// address.** A read of one vault looks at no registration and walks no
    /// connection, at one registration and at four, with idle connections
    /// open beside it; the entries it looks up are the same few at both.
    #[test]
    fn a_served_read_walks_no_registration_or_connection_list() {
        const READS: u64 = 10;
        // Raw totals, not per-read quotients: a quotient would round 19
        // lookups over 10 reads down to 1 and pass the extra ones.
        let mut entries_by_registrations = Vec::new();
        for registrations in [1, 4] {
            let (_fixture, served, port, bearer) =
                serving_vaults("serve-wire-no-walk", registrations);
            let idle: Vec<std::net::TcpStream> = (0..registrations * 4)
                .map(|_| std::net::TcpStream::connect(("127.0.0.1", port)).unwrap())
                .collect();
            let before = counters(port, &bearer);
            for _ in 0..READS {
                assert_eq!(get_alpha(port, &bearer).status, 200);
            }
            let after = counters(port, &bearer);
            let registrations_visited =
                delta(&before, &after, "host.request.registrations_visited");
            let connections_visited = delta(&before, &after, "serve.connections_visited");
            let entries = delta(&before, &after, "host.request.entries_visited");
            eprintln!(
                "{registrations} registrations, {} idle connections, {READS} gets: \
                 registrations visited {registrations_visited}, connections visited \
                 {connections_visited}, entries visited {entries}",
                idle.len()
            );
            assert_eq!(registrations_visited, 0);
            assert_eq!(connections_visited, 0);
            entries_by_registrations.push(entries);
            drop(idle);
            assert_eq!(served.terminate().0.code(), Some(0));
        }
        assert_eq!(
            entries_by_registrations[0], entries_by_registrations[1],
            "a read's entry lookups follow the vault it names, not the registrations"
        );
        assert!(
            entries_by_registrations[0] <= READS * ENTRIES_PER_READ,
            "{entries_by_registrations:?} over {READS} reads"
        );
    }

    /// The serving-set lookups one warm `get` makes: measured at 1 per read on
    /// the first run (10 over 10 reads), at one registration and at four.
    const ENTRIES_PER_READ: u64 = 1;

    /// **Transport bytes are the report bytes plus a fixed allowance per
    /// call.** A report crosses as itself — its bytes are the body's, a
    /// multiple of one — and a refusal's envelope is at most the allowance.
    /// Every wire answer carries the host time.
    #[test]
    fn transport_bytes_are_report_bytes_plus_a_fixed_allowance_per_call() {
        let (_fixture, served, port, bearer) = serving_vaults("serve-wire-transport", 1);
        let before = counters(port, &bearer);
        let mut calls = 0u64;
        for _ in 0..5 {
            let reply = get_alpha(port, &bearer);
            assert_eq!(reply.status, 200);
            assert!(reply.header("x-norn-host-time-us").is_some());
            let reply = call::<FindParams>(
                port,
                &bearer,
                Verb::Find,
                &serde_json::to_value(FindParams::new(norn_wire::VaultAddress::name(
                    norn_wire::VaultName::new("notes").unwrap(),
                )))
                .unwrap(),
            );
            assert_eq!(reply.status, 200);
            let refused = call::<GetParams>(
                port,
                &bearer,
                Verb::Get,
                &serde_json::to_value(GetParams::new(
                    norn_wire::VaultAddress::name(norn_wire::VaultName::new("notes").unwrap()),
                    target("no-such-document"),
                ))
                .unwrap(),
            );
            assert_eq!(refused.code(), "vault/unknown-target");
            assert!(refused.header("x-norn-host-time-us").is_some());
            calls += 3;
        }
        let after = counters(port, &bearer);
        let body = delta(&before, &after, "serve.body_bytes_encoded");
        let report = delta(&before, &after, "serve.report_bytes");
        eprintln!(
            "{calls} calls: {body} body bytes, {report} report bytes, {} per call beyond the reports",
            (body - report) / calls
        );
        assert!(body >= report);
        assert!(
            body <= REPORT_MULTIPLE * report + ALLOWANCE_PER_CALL * calls,
            "{body} body bytes over {report} report bytes and {calls} calls"
        );
        assert_eq!(served.terminate().0.code(), Some(0));
    }

    /// A report's body is the report itself.
    const REPORT_MULTIPLE: u64 = 1;

    /// The most a call adds beyond its report: a refusal's whole envelope. Set
    /// from a first measurement over this workload — 745 bytes beyond the
    /// reports across 15 calls, each of the 5 `vault/unknown-target`
    /// envelopes 149 bytes and every report crossing as itself — with room for
    /// an envelope whose message and detail run longer.
    const ALLOWANCE_PER_CALL: u64 = 256;
}
