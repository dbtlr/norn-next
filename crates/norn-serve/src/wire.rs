//! The native wire route: wire params in, wire reports out, dispatched to
//! `norn-host`.
//!
//! `POST /v1/wire/{verb}`, where `{verb}` is [`Verb`]'s wire name and the body
//! is that verb's params as JSON. An answer is 200 with the verb's report as
//! JSON, or a non-2xx status with an `ErrorEnvelope` as JSON.
//!
//! **One route table, and no verb logic.** [`dispatch`] maps each verb to its
//! params type and the typed `norn-host` method that answers it: parse, call,
//! encode. [`lane`] says which pool a verb runs on — reads on the read pool,
//! and mutations (every write verb, the registration verbs, `init`,
//! `vault migrate` and `vault reload`) on the mutation pool, keyed by the
//! vault whose turn they take. Because [`Verb`] is `#[non_exhaustive]` to this
//! crate, each match over it needs a wildcard arm, so a new verb is caught by
//! a test rather than the compiler: the route table's test walks [`Verb::ALL`]
//! and holds each verb to being routed or on [`NOT_SERVED`], whose verbs answer
//! 404 `serve/unknown-verb` until their phase lands.
//!
//! **A host-death error has a code.** `vault_reload` answers
//! `Result<Result<_, ErrorEnvelope>, HostError>`, and its outer error — the
//! host's worker is gone — carries no wire code. It is answered
//! `serve/outcome-unknown`, since whether the reload took effect is not known,
//! and it stops the process (`stop.rs`).
//!
//! **An apply waits in slices of the drain's deadline.** The mutation-pool
//! thread an apply runs on waits for its answer at most 100 ms at a time,
//! reading the process-wide drain deadline between slices; once that has
//! passed it abandons the apply ([`PendingApply::abandon`]) with a
//! `serve/draining` cause, and answers what the abandonment returns: the
//! answer that arrived, `host/apply-outcome-unknown` with the plan once
//! publication began, or `host/apply-not-run` with the plan where planning
//! finished. The abandoned mark and the job's check before publication are
//! one lock's two holds, so an apply answered not run never publishes.
//!
//! **Nothing reaches the vault before that check.** An apply plans and stages
//! first: staging writes each target's shadow in the shadow home, which is
//! the derived directory's own temporary directory, or — where the data
//! directory sits on another filesystem — a dot-directory beneath the vault
//! root that the walk excludes wholesale and no reading surface shows. The
//! folders a create needs are made at publication, after the check. An apply
//! that stands down at the check removes every shadow it staged; a removal
//! that fails leaves an inert shadow to the shadow home's sweep. So "not run"
//! is also "no document and no folder was written".

use std::time::Instant;

use norn_host::{Host, HostError, PendingApply, ProductionEntryOps};
use norn_wire::{
    ApplyMode, ApplyParams, CountParams, DeleteParams, DescribeParams, DoctorRegistryParams,
    EditParams, ErrorEnvelope, FindParams, GetParams, InitParams, ListParams, MigrateParams,
    MoveParams, NewParams, RegisterParams, ReloadParams, RepairParams, ResolveParams,
    RewriteWikilinkParams, SearchParams, SetParams, StatusParams, UnregisterParams, ValidateParams,
    VaultAddress, VaultName, VaultSetParams, Verb,
};
use serde::Deserialize;
use serde::de::DeserializeOwned;

use crate::bounds::APPLY_WAIT_SLICE;
use crate::drain::Drain;
use crate::request::{AtBound, Dispatch, Dispatched};

/// The verbs the wire route does not serve in this phase: the
/// installation-scoped verbs (the token commands, `service`, `self-update`,
/// `completions`, `manpage`), which land with their own phase. None is in
/// the vocabulary yet; each joins this list when it is, until its phase
/// routes it.
pub(crate) const NOT_SERVED: &[Verb] = &[];

/// Which pool a routed verb runs on.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Runs {
    Read,
    Mutation,
}

/// The pool `verb` runs on, or nothing for a verb this route does not serve.
pub(crate) fn lane(verb: Verb) -> Option<Runs> {
    if NOT_SERVED.contains(&verb) {
        return None;
    }
    match verb {
        Verb::Find
        | Verb::Search
        | Verb::Get
        | Verb::Count
        | Verb::Validate
        | Verb::Describe
        | Verb::VaultList
        | Verb::VaultResolve
        | Verb::VaultStatus
        | Verb::DoctorRegistry => Some(Runs::Read),
        Verb::Apply
        | Verb::Set
        | Verb::Edit
        | Verb::New
        | Verb::Move
        | Verb::Delete
        | Verb::RewriteWikilink
        | Verb::Repair
        | Verb::Init
        | Verb::VaultRegister
        | Verb::VaultUnregister
        | Verb::VaultSet
        | Verb::VaultReload
        | Verb::VaultMigrate => Some(Runs::Mutation),
        // A verb minted after this table; its test fails until it is routed
        // or listed as not served.
        _ => None,
    }
}

/// What the drain does with `verb` still running at its bound: a read's
/// connection closes with the process, an apply is abandoned, and a
/// synchronous mutation — the registration verbs, `init`, `vault migrate`
/// and an activating `vault reload` — is answered `serve/outcome-unknown`. A
/// dry-run reload and a `init` or `vault migrate` preview change nothing, so
/// each is cut off as a read is.
pub(crate) fn at_bound(verb: Verb, body: &[u8]) -> AtBound {
    #[derive(Deserialize)]
    struct DryRun {
        #[serde(default)]
        dry_run: bool,
    }
    #[derive(Deserialize)]
    struct Mode {
        mode: Option<ApplyMode>,
    }
    match lane(verb) {
        Some(Runs::Read) | None => AtBound::CloseWithProcess,
        Some(Runs::Mutation) => match verb {
            Verb::Init | Verb::VaultMigrate => {
                if serde_json::from_slice::<Mode>(body)
                    .is_ok_and(|params| params.mode == Some(ApplyMode::Preview))
                {
                    AtBound::CloseWithProcess
                } else {
                    AtBound::OutcomeUnknown
                }
            }
            Verb::VaultRegister | Verb::VaultUnregister | Verb::VaultSet => AtBound::OutcomeUnknown,
            Verb::VaultReload => {
                if serde_json::from_slice::<DryRun>(body).is_ok_and(|reload| reload.dry_run) {
                    AtBound::CloseWithProcess
                } else {
                    AtBound::OutcomeUnknown
                }
            }
            _ => AtBound::Abandon,
        },
    }
}

/// The vault a mutation's turn is taken on, read off its body before the
/// body is parsed whole: the address a vault verb names, the plan's address
/// for an apply, the registration's name for the registration verbs.
///
/// Read on the I/O thread, which has already touched every byte of the body
/// to read it; everything but the one field is skipped unparsed, and the
/// whole body is parsed on the mutation pool.
pub(crate) fn mutation_vault(verb: Verb, body: &[u8]) -> Result<VaultName, String> {
    #[derive(Deserialize)]
    struct Addressed {
        vault: VaultAddress,
    }
    #[derive(Deserialize)]
    struct Planned {
        plan: Addressed,
    }
    #[derive(Deserialize)]
    struct Named {
        name: VaultName,
    }
    #[derive(Deserialize)]
    struct Registered {
        registration: Named,
    }
    fn read<T: DeserializeOwned>(body: &[u8]) -> Result<T, String> {
        serde_json::from_slice(body).map_err(|error| error.to_string())
    }
    match verb {
        Verb::Apply => {
            read::<Planned>(body).map(|planned| planned.plan.vault.registered_name().clone())
        }
        Verb::VaultRegister => {
            read::<Registered>(body).map(|registered| registered.registration.name)
        }
        Verb::VaultUnregister | Verb::VaultSet => read::<Named>(body).map(|named| named.name),
        _ => read::<Addressed>(body).map(|addressed| addressed.vault.registered_name().clone()),
    }
}

/// **The route table**: parse `body` as `verb`'s params, call the host method
/// that answers it, and encode what it answered.
pub(crate) fn dispatch(
    host: &Host<ProductionEntryOps>,
    verb: Verb,
    body: &[u8],
    drain: &Drain,
) -> Dispatched {
    let applied = |pending| applied(pending, drain);
    match verb {
        Verb::Find => call(body, |params: FindParams| answered(host.find(&params))),
        Verb::Search => call(body, |params: SearchParams| answered(host.search(&params))),
        Verb::Get => call(body, |params: GetParams| answered(host.get(&params))),
        Verb::Count => call(body, |params: CountParams| answered(host.count(&params))),
        Verb::Validate => call(body, |params: ValidateParams| {
            answered(host.validate(&params))
        }),
        Verb::Describe => call(body, |params: DescribeParams| {
            answered(host.describe(&params))
        }),
        Verb::Apply => call(body, |params: ApplyParams| applied(host.apply(params))),
        Verb::Set => call(body, |params: SetParams| applied(host.set(params))),
        Verb::Edit => call(body, |params: EditParams| applied(host.edit(params))),
        Verb::New => call(body, |params: NewParams| applied(host.new_document(params))),
        Verb::Move => call(body, |params: MoveParams| applied(host.move_path(params))),
        Verb::Delete => call(body, |params: DeleteParams| applied(host.delete(params))),
        Verb::RewriteWikilink => call(body, |params: RewriteWikilinkParams| {
            applied(host.rewrite_wikilink(params))
        }),
        Verb::Repair => call(body, |params: RepairParams| applied(host.repair(params))),
        Verb::Init => call(body, |params: InitParams| encoded(host.init(&params))),
        Verb::VaultRegister => call(body, |params: RegisterParams| {
            encoded(host.vault_register(&params))
        }),
        Verb::VaultUnregister => call(body, |params: UnregisterParams| {
            encoded(host.vault_unregister(&params))
        }),
        Verb::VaultList => call(body, |params: ListParams| {
            encoded(Ok(host.vault_list(&params)))
        }),
        Verb::VaultSet => call(body, |params: VaultSetParams| {
            encoded(host.vault_set(&params))
        }),
        Verb::VaultResolve => call(body, |params: ResolveParams| {
            encoded(host.vault_resolve(&params))
        }),
        Verb::VaultStatus => call(body, |params: StatusParams| {
            encoded(host.vault_status(&params))
        }),
        Verb::VaultReload => call(body, |params: ReloadParams| {
            host_answered(host.vault_reload(&params))
        }),
        Verb::VaultMigrate => call(body, |params: MigrateParams| {
            encoded(host.vault_migrate(&params))
        }),
        Verb::DoctorRegistry => call(body, |params: DoctorRegistryParams| {
            encoded(Ok(host.doctor_registry(&params)))
        }),
        // Not routed: `lane` refuses it before any dispatch, so nothing
        // reaches here but a verb minted after this table.
        _ => Dispatched::Refused(ErrorEnvelope::new(
            format!("this host does not serve `{verb}` on the wire route"),
            norn_wire::ErrorDetail::unknown_verb(),
        )),
    }
}

/// Parse `body` as `P`, then answer it with `answer`.
fn call<P: DeserializeOwned>(body: &[u8], answer: impl FnOnce(P) -> Dispatched) -> Dispatched {
    match serde_json::from_slice::<P>(body) {
        Ok(params) => answer(params),
        Err(error) => Dispatched::BadRequest(error.to_string()),
    }
}

/// A report or a refusal, encoded.
fn encoded<R: serde::Serialize>(answer: Result<R, ErrorEnvelope>) -> Dispatched {
    match answer {
        Ok(report) => Dispatched::Report(serde_json::to_vec(&report).expect("a report serializes")),
        Err(envelope) => Dispatched::Refused(envelope),
    }
}

/// A read's answer, which crosses as the answer under its reading, not with
/// the work its builder did.
fn answered<R: serde::Serialize, W>(
    answer: Result<norn_host::Answered<R, W>, ErrorEnvelope>,
) -> Dispatched {
    encoded(answer.map(|answered| answered.answer))
}

/// An apply's answer, waited for on this thread: the mutation pool's, which
/// holds the vault's turn while it waits.
fn applied(pending: Result<PendingApply, ErrorEnvelope>, drain: &Drain) -> Dispatched {
    encoded(pending.and_then(|pending| within_the_drain(pending, drain)))
}

/// Wait for `pending` in slices, abandoning it once the drain's deadline has
/// passed.
fn within_the_drain(
    mut pending: PendingApply,
    drain: &Drain,
) -> Result<norn_wire::VaultAnswer<norn_wire::ApplyReport>, ErrorEnvelope> {
    loop {
        let now = Instant::now();
        let deadline = drain.deadline();
        if deadline.is_some_and(|deadline| now >= deadline) {
            return pending.abandon(ErrorEnvelope::new(
                "the host stopped before this apply could run, and drained without running it",
                norn_wire::ErrorDetail::draining(),
            ));
        }
        let slice = now + APPLY_WAIT_SLICE;
        let until = deadline.map_or(slice, |deadline| slice.min(deadline));
        match pending.wait_until(until) {
            Ok(answer) => return answer,
            Err(waiting) => pending = waiting,
        }
    }
}

/// An answer behind a host-death error.
fn host_answered<R: serde::Serialize>(
    answer: Result<Result<R, ErrorEnvelope>, HostError>,
) -> Dispatched {
    match answer {
        Ok(answer) => encoded(answer),
        Err(error) => Dispatched::HostStopped(error.to_string()),
    }
}

/// The production dispatcher: the route table over this process's host.
pub(crate) struct HostRoute {
    pub(crate) host: Host<ProductionEntryOps>,
    /// The drain whose deadline an apply's wait reads.
    pub(crate) drain: std::sync::Arc<Drain>,
}

impl Dispatch for HostRoute {
    fn dispatch(&self, verb: Verb, body: &[u8]) -> Dispatched {
        // Under the counters feature each call is measured alone on the
        // thread that runs it, into the host's request totals.
        #[cfg(feature = "counters")]
        return self
            .host
            .tallied(|| dispatch(&self.host, verb, body, &self.drain))
            .0;
        #[cfg(not(feature = "counters"))]
        dispatch(&self.host, verb, body, &self.drain)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every verb in the vocabulary is routed to a pool or listed as not
    /// served, never both: a verb minted without a row here fails this test.
    #[test]
    fn every_verb_is_routed_or_listed_as_not_served() {
        for verb in Verb::ALL {
            let routed = lane(verb).is_some();
            let listed = NOT_SERVED.contains(&verb);
            assert!(
                routed != listed,
                "`{verb}`: routed {routed}, listed {listed}"
            );
        }
    }

    /// The mutations are exactly the verbs that change something: every
    /// write verb, the registration verbs, `init`, `vault migrate` and
    /// `vault reload`.
    #[test]
    fn the_mutation_pool_takes_exactly_the_verbs_that_change_something() {
        let mutations: Vec<Verb> = Verb::ALL
            .into_iter()
            .filter(|verb| lane(*verb) == Some(Runs::Mutation))
            .collect();
        assert_eq!(
            mutations,
            [
                Verb::Apply,
                Verb::Set,
                Verb::Edit,
                Verb::New,
                Verb::Move,
                Verb::Delete,
                Verb::RewriteWikilink,
                Verb::Repair,
                Verb::Init,
                Verb::VaultRegister,
                Verb::VaultUnregister,
                Verb::VaultSet,
                Verb::VaultReload,
                Verb::VaultMigrate,
            ]
        );
    }

    /// Every verb's kind at the drain's bound: reads close with the process,
    /// the write verbs and `repair` are abandoned, and the synchronous
    /// mutations are answered `serve/outcome-unknown`, a dry-run reload
    /// aside.
    #[test]
    fn each_verb_is_answered_at_the_drain_bound_by_its_kind() {
        for verb in Verb::ALL {
            let kind = at_bound(verb, b"{}");
            let expected = match verb {
                Verb::Init
                | Verb::VaultRegister
                | Verb::VaultUnregister
                | Verb::VaultSet
                | Verb::VaultMigrate
                | Verb::VaultReload => AtBound::OutcomeUnknown,
                Verb::Apply
                | Verb::Set
                | Verb::Edit
                | Verb::New
                | Verb::Move
                | Verb::Delete
                | Verb::RewriteWikilink
                | Verb::Repair => AtBound::Abandon,
                _ => AtBound::CloseWithProcess,
            };
            assert_eq!(kind, expected, "{verb}");
        }
        assert_eq!(
            at_bound(Verb::VaultReload, br#"{"vault":{},"dry_run":true}"#),
            AtBound::CloseWithProcess
        );
        assert_eq!(
            at_bound(Verb::VaultReload, br#"{"vault":{},"dry_run":false}"#),
            AtBound::OutcomeUnknown
        );
        for verb in [Verb::Init, Verb::VaultMigrate] {
            assert_eq!(
                at_bound(verb, br#"{"vault":{},"mode":"preview"}"#),
                AtBound::CloseWithProcess,
                "{verb} preview"
            );
            assert_eq!(
                at_bound(verb, br#"{"vault":{},"mode":"apply"}"#),
                AtBound::OutcomeUnknown,
                "{verb} apply"
            );
        }
    }

    /// **`init` and `vault migrate` are classified at the bound as the host
    /// parses them.** A body cut off with the process is one the verb's own
    /// params read as a preview, and every other body, one they refuse
    /// included, is answered unknown: `"bogus"` names no mode, and
    /// `{"preview":null}` is serde's map spelling of the preview variant.
    #[test]
    fn an_init_or_migrate_mode_is_classified_as_its_params_parse_it() {
        let preview_by_params = |verb: Verb, body: &[u8]| match verb {
            Verb::Init => serde_json::from_slice::<norn_wire::InitParams>(body)
                .map(|params| params.mode == ApplyMode::Preview),
            Verb::VaultMigrate => serde_json::from_slice::<norn_wire::MigrateParams>(body)
                .map(|params| params.mode == ApplyMode::Preview),
            _ => unreachable!("{verb} is neither init nor vault migrate"),
        };
        let vault = r#""vault":{"by":"name","name":"notes"}"#;
        for verb in [Verb::Init, Verb::VaultMigrate] {
            let bogus = format!(r#"{{{vault},"mode":"bogus"}}"#);
            assert!(
                preview_by_params(verb, bogus.as_bytes()).is_err(),
                "{verb}: `bogus` parsed"
            );
            assert_eq!(
                at_bound(verb, bogus.as_bytes()),
                AtBound::OutcomeUnknown,
                "{verb} bogus"
            );
            let mapped = format!(r#"{{{vault},"mode":{{"preview":null}}}}"#);
            assert_eq!(
                preview_by_params(verb, mapped.as_bytes()).ok(),
                Some(true),
                "{verb}: the map spelling is not read as a preview"
            );
            assert_eq!(
                at_bound(verb, mapped.as_bytes()),
                AtBound::CloseWithProcess,
                "{verb} {{\"preview\":null}}"
            );
        }
    }

    /// A mutation's vault is read off each shape a mutation's params take.
    #[test]
    fn a_mutations_vault_is_read_off_its_body() {
        let address = r#"{"by":"name","name":"notes"}"#;
        let cases = [
            (
                Verb::Set,
                format!(r#"{{"vault":{address},"mode":"apply"}}"#),
            ),
            (
                Verb::Apply,
                format!(r#"{{"mode":"apply","plan":{{"plan":"operations","vault":{address}}}}}"#),
            ),
            (
                Verb::VaultRegister,
                r#"{"registration":{"name":"notes","root":"/v"}}"#.to_string(),
            ),
            (Verb::VaultUnregister, r#"{"name":"notes"}"#.to_string()),
            (Verb::VaultSet, r#"{"name":"notes"}"#.to_string()),
        ];
        for (verb, body) in cases {
            assert_eq!(
                mutation_vault(verb, body.as_bytes()),
                Ok(VaultName::new("notes").unwrap()),
                "{verb}: {body}"
            );
        }
        assert!(mutation_vault(Verb::Set, b"{}").is_err());
        assert!(
            mutation_vault(Verb::Set, br#"{"vault":{"by":"name","name":"Not A Name"}}"#).is_err()
        );
    }
}
