//! What a demand is, said in the wire vocabulary.
//!
//! [`Demand`] is host-local: it carries this crate's own [`AliasConflict`](crate::AliasConflict) and
//! the registry's account of a root it cannot read. What leaves the host is
//! neither — it is `norn-wire`'s [`TrustState`] or its [`ErrorEnvelope`] — so
//! the translation between the two lives here, on the host side of the seam.
//! It lives in the host rather than in a serving crate because the vocabulary
//! an answer is spelled in is not a surface's to choose: every surface above
//! the host renders the one answer this module produces, and a second surface
//! arriving adds no second mapping.
//!
//! [`Demand::answer`] is total by its match, which carries no wildcard: a
//! demand variant minted without an arm here does not compile, so a refusal
//! the host can make and the vocabulary cannot spell is a build failure rather
//! than a code invented at a call site.
//!
//! **A warming entry is answered rather than refused.** Warming is polled — a
//! caller reads how far the entry has come and asks again — so it is a state
//! that crosses, not an envelope. Only a state saying reads cannot answer from
//! the entry becomes one.
//!
//! **Two demands reach `host/entry-untrusted`, and deliberately.** An entry
//! standing untrusted carries the reason its trust state carries. A root the
//! registry cannot read is the environment refusing the work, which is what
//! [`UntrustedReason::EnvironmentalRefusal`] already names, and the next
//! demand's recheck is what retires either one. The two are different reads
//! inside the host and one fact to a client: the derived state cannot be
//! trusted, because the environment refused.
//!
//! **The aliases of a duplicate root cross in one order.** An
//! [`AliasConflict`](crate::AliasConflict) holds ascending [`VaultName`]s and
//! [`ErrorDetail::duplicate_root`] sorts the strings it is handed, so the
//! order a client reads is the order the host holds. The two sorts are
//! separate acts over separate types, and the test below is the carrier that
//! fails if they ever answer differently.

use norn_config::VaultName;
use norn_wire::{ErrorDetail, ErrorEnvelope, TrustState, UntrustedReason};

use crate::lifecycle::Demand;

impl Demand {
    /// This demand in the wire vocabulary: the trust state it answers `name`
    /// with, or the refusal it is.
    ///
    /// The name is taken rather than read off the demand because a demand for
    /// a vault the registry does not hold has nothing to read it from, and the
    /// refusal that says so echoes the name that was asked for.
    pub fn answer(&self, name: &VaultName) -> Result<TrustState, ErrorEnvelope> {
        match self {
            Demand::State(state) => answer_state(state),
            Demand::MaintainerContended(incumbent) => Err(ErrorEnvelope::new(
                "another process maintains this vault's derived state",
                ErrorDetail::maintainer_contended(incumbent.clone()),
            )),
            Demand::DuplicateRoot(conflict) => Err(ErrorEnvelope::new(
                "more than one registered name resolves to this vault's root, so none of them \
                 is served",
                ErrorDetail::duplicate_root(conflict.aliases().iter().map(VaultName::as_str)),
            )),
            Demand::IdentityRefused(refusal) => Err(ErrorEnvelope::new(
                "the registry cannot read this vault's root",
                ErrorDetail::entry_untrusted(UntrustedReason::environmental_refusal(refusal)),
            )),
            Demand::UnknownVault => Err(ErrorEnvelope::new(
                format!("no vault is registered under the name `{name}`"),
                ErrorDetail::unknown_vault(name.as_str()),
            )),
        }
    }
}

/// The state a demand answers with, or the refusal that state is.
///
/// [`TrustState`] grows in `norn-wire`, so the match ends in a wildcard the
/// four arms above it leave unreached. A state is what crosses unless it says
/// reads cannot answer from the entry, and the state that says so is refused
/// above.
fn answer_state(state: &TrustState) -> Result<TrustState, ErrorEnvelope> {
    match state {
        TrustState::Untrusted { reason, .. } => Err(ErrorEnvelope::new(
            "this vault's derived state cannot be trusted, so the request is refused rather \
             than answered from it",
            ErrorDetail::entry_untrusted(reason.clone()),
        )),
        TrustState::Unattached | TrustState::Warming { .. } | TrustState::Ready => {
            Ok(state.clone())
        }
        _ => Ok(state.clone()),
    }
}

#[cfg(test)]
mod tests {
    use norn_wire::{MaintainerIdentity, ReasonCode, WarmingPhase, WatcherLossCause};

    use crate::registry::AliasConflict;

    use super::*;

    fn name(text: &str) -> VaultName {
        VaultName::new(text).expect("a legal vault name")
    }

    /// The demand shape a sample is of. The match carries no wildcard, so a
    /// demand variant minted without a sample below does not compile.
    fn shape(demand: &Demand) -> &'static str {
        match demand {
            Demand::State(_) => "state",
            Demand::MaintainerContended(_) => "maintainer-contended",
            Demand::DuplicateRoot(_) => "duplicate-root",
            Demand::IdentityRefused(_) => "identity-refused",
            Demand::UnknownVault => "unknown-vault",
        }
    }

    /// Every demand the host answers with, paired with the code it is refused
    /// under — or `None` where the demand is an answer rather than a refusal.
    /// Every trust state a demand can carry is sampled, because which of them
    /// refuses is part of the assignment.
    fn every_demand() -> Vec<(Demand, Option<ReasonCode>)> {
        vec![
            (Demand::State(TrustState::Ready), None),
            (Demand::State(TrustState::Unattached), None),
            (
                Demand::State(TrustState::warming(WarmingPhase::Healing, 3, Some(9))),
                None,
            ),
            (
                Demand::State(TrustState::untrusted(UntrustedReason::WatcherOverflow)),
                Some(ReasonCode::HostEntryUntrusted),
            ),
            (
                Demand::State(TrustState::untrusted(UntrustedReason::watcher_lost(
                    WatcherLossCause::Backend,
                    "the watcher backend stopped",
                ))),
                Some(ReasonCode::HostEntryUntrusted),
            ),
            (
                Demand::MaintainerContended(MaintainerIdentity::named(41, "0.1.0", 1_700_000_000)),
                Some(ReasonCode::HostMaintainerContended),
            ),
            (
                Demand::MaintainerContended(MaintainerIdentity::unknown()),
                Some(ReasonCode::HostMaintainerContended),
            ),
            (
                Demand::DuplicateRoot(AliasConflict::new([name("alpha"), name("beta")])),
                Some(ReasonCode::HostDuplicateRoot),
            ),
            (
                Demand::IdentityRefused("the root cannot be read".to_string()),
                Some(ReasonCode::HostEntryUntrusted),
            ),
            (Demand::UnknownVault, Some(ReasonCode::HostUnknownVault)),
        ]
    }

    /// Every demand reaches exactly one reason code, and the code it reaches
    /// is the one pinned beside it. The pairing between a code and its detail
    /// is the envelope's own, so the detail naming the code back is what says
    /// the refusal is one refusal rather than two facts side by side.
    #[test]
    fn every_demand_reaches_exactly_one_reason_code() {
        let asked = name("notes");
        for (demand, expected) in every_demand() {
            let answer = demand.answer(&asked);
            match (answer, &expected) {
                (Err(envelope), Some(code)) => {
                    assert_eq!(
                        envelope.code(),
                        code,
                        "{demand:?} is filed under another code"
                    );
                    assert_eq!(
                        envelope.detail().code(),
                        *code,
                        "{demand:?} carries a detail that names another code"
                    );
                    assert!(
                        !envelope.message().is_empty(),
                        "{demand:?} refuses without saying so in words"
                    );
                }
                (Ok(state), None) => {
                    assert_eq!(
                        Demand::State(state),
                        demand,
                        "{demand:?} answered with another state"
                    );
                }
                (answer, expected) => panic!("{demand:?} answered {answer:?} against {expected:?}"),
            }
        }
    }

    /// Every shape a demand takes is sampled above, so the assignment a new
    /// variant needs is pinned rather than merely compiled.
    #[test]
    fn every_demand_shape_is_sampled() {
        let sampled = every_demand()
            .iter()
            .map(|(demand, _)| shape(demand))
            .collect::<Vec<_>>();
        for expected in [
            "state",
            "maintainer-contended",
            "duplicate-root",
            "identity-refused",
            "unknown-vault",
        ] {
            assert!(
                sampled.contains(&expected),
                "no demand of shape `{expected}` is sampled"
            );
        }
    }

    /// The name the refusal echoes is the name that was asked for, which is
    /// the whole of what an unknown vault has to say.
    #[test]
    fn an_unknown_vault_is_refused_under_the_name_that_was_asked_for() {
        let envelope = Demand::UnknownVault
            .answer(&name("ledger"))
            .expect_err("an unknown vault refuses");
        assert_eq!(
            envelope.detail(),
            &ErrorDetail::unknown_vault("ledger"),
            "the refusal echoes another name"
        );
    }

    /// The order the host holds the aliases in is the order the envelope
    /// carries them in. The names below spread across the punctuation a vault
    /// name may hold, so an ordering the conflict and the detail disagree on
    /// shows here rather than in whichever surface reads them next.
    #[test]
    fn the_envelope_carries_the_aliases_in_the_order_the_conflict_holds_them() {
        let names = ["ab", "a0", "a.b", "a-b", "a+b", "a"].map(name);
        let conflict = AliasConflict::new(names);
        let held = conflict
            .aliases()
            .iter()
            .map(|alias| alias.as_str().to_owned())
            .collect::<Vec<_>>();

        let envelope = Demand::DuplicateRoot(conflict)
            .answer(&name("a"))
            .expect_err("a duplicate root refuses");
        let ErrorDetail::DuplicateRoot { aliases, .. } = envelope.detail() else {
            panic!("a duplicate root carries another detail: {envelope:?}");
        };
        assert_eq!(
            aliases, &held,
            "the conflict and the detail sort differently"
        );
    }
}
