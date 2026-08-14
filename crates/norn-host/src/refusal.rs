//! What a demand is, said in the wire vocabulary.
//!
//! [`Demand`] is host-local: it carries this crate's own
//! [`AliasConflict`](crate::AliasConflict) and the registry's own account of a
//! root it cannot read. What leaves the host is
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
//! that crosses, not an envelope. What becomes an envelope is a state that
//! polling does not walk out of: one standing until a re-heal, a client
//! demanding one, or an environment that stops refusing retires it. Reads
//! answer from a warming entry no more than from an untrusted one, so what
//! reads can do with a state is not the line; what retires the state is.
//!
//! **Two demands reach `host/entry-untrusted`, and deliberately.** An entry
//! standing untrusted carries the reason its trust state carries. A root the
//! registry cannot read is the environment refusing the work, which is what
//! [`UntrustedReason::EnvironmentalRefusal`] already names, and the attach the
//! next demand schedules is what retires either one. The two are different
//! reads inside the host and one fact to a client: the derived state cannot be
//! trusted, because the environment refused.
//!
//! **The aliases of a duplicate root cross in one order.** An
//! [`AliasConflict`](crate::AliasConflict) holds ascending [`VaultName`]s and
//! [`ErrorDetail::duplicate_root`] sorts the strings it is handed, so the
//! order a client reads is the order the host holds. The two sorts are
//! separate acts over separate types, and the test below is the carrier that
//! fails if they ever answer differently.

use norn_wire::{ErrorDetail, ErrorEnvelope, TrustState, UntrustedReason, VaultName};

use crate::lifecycle::Demand;

impl Demand {
    /// This demand in the wire vocabulary: the trust state it answers `name`
    /// with, or the refusal it is.
    ///
    /// `name` is the name that was asked for, and [`Demand::UnknownVault`] is
    /// the only demand that echoes it: a demand for a vault the registry does
    /// not hold has no entry to read a name off, so the ask is all the refusal
    /// has to name. Every other demand answers out of what it carries and
    /// reads nothing from `name`. A caller holding the entry's lease answers
    /// through [`DemandLease::answer`](crate::DemandLease::answer), which is
    /// the entry point that supplies the name the lease itself holds; this one
    /// is the mapping that entry point renders, taking the name as a parameter
    /// because a demand carries none of its own.
    pub fn answer(self, name: &VaultName) -> Result<TrustState, ErrorEnvelope> {
        match self {
            Demand::State(state) => answer_state(state),
            Demand::MaintainerContended(incumbent) => Err(ErrorEnvelope::new(
                "another process maintains this vault's derived state",
                ErrorDetail::maintainer_contended(incumbent),
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
                ErrorDetail::unknown_vault(name.clone()),
            )),
            Demand::UnsupportedMode(mode) => Err(ErrorEnvelope::new(
                "this host attaches registered vaults durably and holds no lifecycle for the \
                 mode this demand named",
                ErrorDetail::unsupported_attach_mode(mode),
            )),
        }
    }
}

/// The state a demand answers with, or the refusal that state is.
///
/// [`TrustState`] grows in `norn-wire` and is `#[non_exhaustive]`, so which
/// states refuse is [`TrustState::refusal`]'s answer, given beside the states
/// themselves: a state a poll does not walk out of carries a reason there, and
/// this function is the envelope that reason is spelled in. Nothing here
/// matches on a state, so a variant minted in that crate cannot cross by
/// falling through a wildcard the host wrote.
fn answer_state(state: TrustState) -> Result<TrustState, ErrorEnvelope> {
    if let Some(reason) = state.refusal() {
        return Err(ErrorEnvelope::new(
            "this vault's derived state cannot be trusted, so the request is refused rather \
             than answered from it",
            ErrorDetail::entry_untrusted(reason.clone()),
        ));
    }
    Ok(state)
}

#[cfg(test)]
mod tests {
    use norn_wire::{AttachMode, MaintainerIdentity, WarmingPhase, WatcherLossCause};

    use crate::registry::AliasConflict;

    use super::*;

    fn name(text: &str) -> VaultName {
        VaultName::new(text).expect("a legal vault name")
    }

    /// The name every sample below is answered under, and the name the one
    /// refusal that echoes a name carries.
    fn asked() -> VaultName {
        name("notes")
    }

    /// What a demand is, apart from what it carries.
    ///
    /// The samples below pin what a payload does to an answer; a shape is what
    /// says a variant is sampled at all.
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum Shape {
        State,
        MaintainerContended,
        DuplicateRoot,
        IdentityRefused,
        UnknownVault,
        UnsupportedMode,
    }

    impl Shape {
        /// The shape after this one, and nothing at the end of the walk. The
        /// match carries no wildcard, so a shape minted without a place in the
        /// walk does not compile.
        const fn after(self) -> Option<Shape> {
            match self {
                Shape::State => Some(Shape::MaintainerContended),
                Shape::MaintainerContended => Some(Shape::DuplicateRoot),
                Shape::DuplicateRoot => Some(Shape::IdentityRefused),
                Shape::IdentityRefused => Some(Shape::UnknownVault),
                Shape::UnknownVault => Some(Shape::UnsupportedMode),
                Shape::UnsupportedMode => None,
            }
        }
    }

    /// Every shape there is, walked from the first. The list is the shapes'
    /// own account of themselves rather than one written beside them, so a
    /// shape that reaches the walk without reaching a sample fails the test
    /// below instead of passing it unexercised.
    fn every_shape() -> Vec<Shape> {
        let mut shapes = vec![Shape::State];
        while let Some(next) = shapes.last().expect("the walk starts at a shape").after() {
            assert!(
                !shapes.contains(&next),
                "{next:?} is reached twice, so the walk is a cycle rather than a census"
            );
            shapes.push(next);
        }
        shapes
    }

    /// The shape a sample is of. The match carries no wildcard, so a demand
    /// variant minted without a shape does not compile.
    fn shape(demand: &Demand) -> Shape {
        match demand {
            Demand::State(_) => Shape::State,
            Demand::MaintainerContended(_) => Shape::MaintainerContended,
            Demand::DuplicateRoot(_) => Shape::DuplicateRoot,
            Demand::IdentityRefused(_) => Shape::IdentityRefused,
            Demand::UnknownVault => Shape::UnknownVault,
            Demand::UnsupportedMode(_) => Shape::UnsupportedMode,
        }
    }

    /// Every demand the host answers with, paired with the detail it is
    /// refused under — or `None` where the demand is an answer rather than a
    /// refusal. The whole detail is pinned rather than the code alone, because
    /// what a client reads off a refusal is the payload: a code reached with
    /// another vault's incumbent, or another root's account of itself, is the
    /// wrong refusal filed under the right name.
    ///
    /// Every trust state a demand can carry is sampled, phases included,
    /// because which of them refuses is part of the assignment.
    fn every_demand() -> Vec<(Demand, Option<ErrorDetail>)> {
        let lost = || {
            UntrustedReason::watcher_lost(WatcherLossCause::Backend, "the watcher backend stopped")
        };
        let refused = || UntrustedReason::environmental_refusal("the vault root stopped reading");
        let incumbent = || MaintainerIdentity::named(41, "0.1.0", 1_700_000_000);
        vec![
            (Demand::State(TrustState::Ready), None),
            (Demand::State(TrustState::Unattached), None),
            (
                Demand::State(TrustState::warming(
                    WarmingPhase::InstallingCoverage,
                    0,
                    None,
                )),
                None,
            ),
            (
                Demand::State(TrustState::warming(WarmingPhase::Healing, 3, Some(9))),
                None,
            ),
            (
                Demand::State(TrustState::warming(
                    WarmingPhase::ReleasingCoverage,
                    0,
                    None,
                )),
                None,
            ),
            (
                Demand::State(TrustState::untrusted(UntrustedReason::WatcherOverflow)),
                Some(ErrorDetail::entry_untrusted(
                    UntrustedReason::WatcherOverflow,
                )),
            ),
            (
                Demand::State(TrustState::untrusted(lost())),
                Some(ErrorDetail::entry_untrusted(lost())),
            ),
            (
                Demand::State(TrustState::untrusted(refused())),
                Some(ErrorDetail::entry_untrusted(refused())),
            ),
            (
                Demand::MaintainerContended(incumbent()),
                Some(ErrorDetail::maintainer_contended(incumbent())),
            ),
            (
                Demand::MaintainerContended(MaintainerIdentity::unknown()),
                Some(ErrorDetail::maintainer_contended(
                    MaintainerIdentity::unknown(),
                )),
            ),
            (
                Demand::DuplicateRoot(AliasConflict::new([name("alpha"), name("beta")])),
                Some(ErrorDetail::duplicate_root(["alpha", "beta"])),
            ),
            (
                Demand::IdentityRefused("the root cannot be read".to_string()),
                Some(ErrorDetail::entry_untrusted(
                    UntrustedReason::environmental_refusal("the root cannot be read"),
                )),
            ),
            (
                Demand::UnknownVault,
                Some(ErrorDetail::unknown_vault(asked())),
            ),
            (
                Demand::UnsupportedMode(AttachMode::Throwaway),
                Some(ErrorDetail::unsupported_attach_mode(AttachMode::Throwaway)),
            ),
        ]
    }

    /// Every demand reaches exactly one refusal, carrying the detail pinned
    /// beside it — payload and all, so a demand answered with another demand's
    /// account of itself fails here rather than passing on a shared code. The
    /// code follows from the detail, and the envelope's own code naming it back
    /// is what says the refusal is one refusal rather than two facts side by
    /// side.
    #[test]
    fn every_demand_reaches_exactly_one_refusal_detail() {
        for (demand, expected) in every_demand() {
            let answer = demand.clone().answer(&asked());
            match (answer, &expected) {
                (Err(envelope), Some(detail)) => {
                    assert_eq!(
                        envelope.detail(),
                        detail,
                        "{demand:?} refuses with another detail"
                    );
                    assert_eq!(
                        envelope.code(),
                        &detail.code(),
                        "{demand:?} is filed under a code its detail does not name"
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
        for expected in every_shape() {
            assert!(
                sampled.contains(&expected),
                "no demand of shape {expected:?} is sampled"
            );
        }
    }

    /// A root the registry cannot read and an entry untrusted for an
    /// environmental refusal are one fact on the wire. The host reads them at
    /// two doors — a recheck of the registry, a state the entry published —
    /// and a client is told the one thing both mean: the derived state cannot
    /// be trusted, because the environment refused. Nothing a client matches
    /// on — the code or the detail — says which door it came through, and this
    /// is where that stays true; the message is prose, and each door has its
    /// own thing to say to a person.
    #[test]
    fn a_refused_root_and_an_environmental_refusal_are_one_refusal() {
        let account = "the vault root stopped being readable";
        let refused = Demand::IdentityRefused(account.to_string())
            .answer(&asked())
            .expect_err("a root the registry cannot read refuses");
        let untrusted = Demand::State(TrustState::untrusted(
            UntrustedReason::environmental_refusal(account),
        ))
        .answer(&asked())
        .expect_err("an entry the environment refused refuses");

        assert_eq!(
            refused.detail(),
            untrusted.detail(),
            "the two reads reach details a client can tell apart"
        );
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
            &ErrorDetail::unknown_vault(name("ledger")),
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
