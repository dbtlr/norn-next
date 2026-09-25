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
//! **A refusal a request earns renders here; the host being gone does not.**
//! [`ReloadRefusal::answer`] hands back an envelope for everything a reload
//! can be refused for and `Err(HostError)` for the one thing that is not a
//! refusal at all: the worker pool has stopped, so no reload outcome was
//! learned and there is nothing about the vault to report. The reply channel
//! can fail after the reload was dispatched as well as before it, so what the
//! host can say is that it learned nothing, never that nothing ran. A code
//! minted for it would describe the vault, and what stopped is the host.
//!
//! **A read cannot answer with a state, and that is the one place these
//! mappings differ.** [`Demand::answer`] hands a warming or unattached entry
//! back as a state, because a poll walks out of it. A read is not a poll: it
//! has nothing to read from either entry, so [`ReadRefusal::answer`] takes the
//! same states through [`TrustState::not_ready`] and files them under
//! `host/entry-not-ready`.
//!
//! **Two demands reach `host/entry-untrusted`, and deliberately.** An entry
//! standing untrusted carries the reason its trust state carries. A root the
//! registry cannot read is the environment refusing the work, which is what
//! [`UntrustedReason::EnvironmentalRefusal`] already names, and the attach the
//! next demand schedules is what retires either one. The two are different
//! reads inside the host and one fact to a client: the derived state cannot be
//! trusted, because the environment refused.

use norn_wire::{
    ControlFile, ControlFileFailure, ErrorDetail, ErrorEnvelope, ReloadFailure, TrustState,
    UntrustedReason, VaultName,
};

use crate::lifecycle::{Demand, HostError, JobFailure, ReadRefusal, ServingRefusal};
use crate::reload::{ReloadError, ReloadFile, ReloadRefusal, ReloadStage};

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
                ErrorDetail::duplicate_root(conflict.aliases().clone()),
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

impl ServingRefusal {
    /// This refusal in the wire vocabulary, for the vault `name` the change
    /// was asked for under.
    ///
    /// Both members are facts about the host's serving of an entry, so both
    /// are `host/…` and both echo the name: what a caller does about either
    /// one is addressed to that name.
    ///
    /// Nothing calls it yet. The serving set is changed by the registry verbs
    /// — `vault register` and `vault unregister` — and this is the rendering
    /// those verbs refuse through. Their handlers land in this crate
    /// (NORN-231), which is why the visibility is `pub(crate)`: the mapping
    /// waits for the handler beside it rather than for a surface above it.
    #[allow(
        dead_code,
        reason = "the registry verbs that refuse through this mapping land in NORN-231"
    )]
    pub(crate) fn answer(self, name: &VaultName) -> ErrorEnvelope {
        match self {
            ServingRefusal::AlreadyServed => ErrorEnvelope::new(
                format!("this host already serves a vault under the name `{name}`"),
                ErrorDetail::already_served(name.clone()),
            ),
            ServingRefusal::Held => ErrorEnvelope::new(
                format!(
                    "the entry serving `{name}` is holding something, or something is holding \
                     it, so it stays in service"
                ),
                ErrorDetail::entry_held(name.clone()),
            ),
        }
    }
}

impl ReloadRefusal {
    /// This refusal in the wire vocabulary, for the vault `name` the reload
    /// was asked for.
    ///
    /// Every reload outcome is a fact about the vault, so every one of them is
    /// `vault/…` — except the three the host answers before a reload is a
    /// thing that happened at all: a name the registry does not hold
    /// (`host/unknown-vault`), an entry that holds nothing to reload yet
    /// (`host/entry-not-ready`), and an entry whose derived state cannot be
    /// trusted (`host/entry-untrusted`).
    ///
    /// `Err(HostError)` is the host being gone rather than the vault refusing.
    /// It carries no code and no detail: no reload outcome was learned, so
    /// there is nothing about the vault to report.
    ///
    /// Nothing calls it yet. The `vault reload` handler is the one caller this
    /// mapping has, and it lands above this layer (NORN-230); the mapping
    /// lands here because the vocabulary an answer is spelled in is not a
    /// surface's to choose, so the handler that arrives renders this rather
    /// than minting refusals of its own.
    pub fn answer(self, name: &VaultName) -> Result<ErrorEnvelope, HostError> {
        Ok(match self {
            ReloadRefusal::UnknownVault => ErrorEnvelope::new(
                format!("no vault is registered under the name `{name}`"),
                ErrorDetail::unknown_vault(name.clone()),
            ),
            ReloadRefusal::Unavailable(state) => unavailable(state),
            ReloadRefusal::Core(error) => {
                reload_failed(ReloadFailure::control_file(control_file_failure(&error)))
            }
            ReloadRefusal::Runtime(failure) => reload_failed(runtime_failure(failure)),
            ReloadRefusal::Unsupported => reload_failed(ReloadFailure::unsupported()),
            ReloadRefusal::HostStopped => return Err(HostError::WorkerStopped),
        })
    }
}

/// What an entry that is not reloadable right now refuses with.
///
/// Three readings of one trust state, taken in the order that makes each of
/// them true: a state that refuses carries its own reason, a state that holds
/// nothing yet is filed as not ready, and what is left is an entry that is
/// ready and busy — a warm job holds it, which is what a reload waits behind.
///
/// **`Ready` reaches here by two paths, and on both the reload did not run.**
/// At the ask, a claim held or coverage out with a leg is a job holding the
/// entry. At a reload leg's epilogue, a claim superseded by newer work is a
/// reload that lost its turn, and the entry may stand `Ready` again by the
/// time that epilogue reports. A release does not reach it as `Ready`:
/// `begin_release` sets `detach_in_flight` and sets the trust to
/// `Warming(WarmingPhase::ReleasingCoverage)` in the same statement pair under
/// one hold of the gate, so the epilogue that reads the flag reads that state
/// beside it. `a_release_window_opened_over_reload_closes_at_the_job_epilogue`
/// pins the value that arrives. Either way the answer is the same: nothing of
/// the asker's ran, and a retry is what asks again.
fn unavailable(state: TrustState) -> ErrorEnvelope {
    if let Some(reason) = state.refusal() {
        return ErrorEnvelope::new(
            "this vault's derived state cannot be trusted, so its control files are not \
             re-read over it",
            ErrorDetail::entry_untrusted(reason.clone()),
        );
    }
    if let Some(not_ready) = state.not_ready() {
        return ErrorEnvelope::new(
            "this vault holds nothing to re-read its control files over yet",
            ErrorDetail::entry_not_ready(not_ready),
        );
    }
    ErrorEnvelope::new(
        "this vault is already being worked over, so the reload was not started",
        ErrorDetail::reload_busy(),
    )
}

/// The envelope a reload that ran and did not finish its work is filed under.
fn reload_failed(failure: ReloadFailure) -> ErrorEnvelope {
    ErrorEnvelope::new(
        "re-reading this vault's control files did not leave it serving what they state",
        ErrorDetail::reload_failed(failure),
    )
}

/// A core reload error as the wire control-file failure it is: which file,
/// which boundary, and the reader's own account of it.
///
/// The reload refusals here are its first caller; its second is the `vault
/// status` handler, which lands in this crate (NORN-231) and reports the same
/// failure as an entry's last reload failure, so the visibility is
/// `pub(crate)` rather than private to this module.
pub(crate) fn control_file_failure(error: &ReloadError) -> ControlFileFailure {
    let file = match error.file() {
        ReloadFile::Schema => ControlFile::Schema,
        ReloadFile::Config => ControlFile::Config,
    };
    let stage = match error.stage() {
        ReloadStage::Read => norn_wire::ReloadStage::Read,
        ReloadStage::Parse => norn_wire::ReloadStage::Parse,
        ReloadStage::Apply => norn_wire::ReloadStage::Apply,
    };
    ControlFileFailure::new(file, stage, error.to_string())
}

/// A job failure as the wire failure it is. The match carries no wildcard, so
/// a failure a job can end in and the vocabulary cannot spell is a build
/// failure rather than prose invented here.
fn runtime_failure(failure: JobFailure) -> ReloadFailure {
    match failure {
        JobFailure::Environmental(detail) => ReloadFailure::environmental(detail),
        JobFailure::Reload(error) => ReloadFailure::control_file(control_file_failure(&error)),
        JobFailure::StoreDamaged(detail) => ReloadFailure::store_damaged(detail),
        JobFailure::WatcherTerminal(error) => ReloadFailure::watcher_terminal(error.to_string()),
        JobFailure::LostMaintainership => ReloadFailure::lost_maintainership(),
        JobFailure::MaintainerContended(incumbent) => {
            ReloadFailure::maintainer_contended(incumbent)
        }
    }
}

impl ReadRefusal {
    /// This refusal in the wire vocabulary, for the vault `name` the read was
    /// asked for.
    ///
    /// A read has nothing to answer with but rows, so every read refusal is an
    /// envelope: [`TrustState::not_ready`] is what files a warming or
    /// unattached entry under `host/entry-not-ready`, and a demand that is
    /// already a refusal keeps its own.
    ///
    /// **A `Ready` demand renders too, and it is this mapping's own reading of
    /// a shape the host never produces.** `NotServing(Demand::State(Ready))`
    /// is constructible and `begin_read` never builds one, because an entry
    /// that is ready is serving. Read as a refusal it says exactly one thing:
    /// the entry published `Ready` and served every surface but this read,
    /// which is what `host/reader-unavailable` means and what no other code
    /// here means. So it renders under that code rather than being returned to
    /// a caller as a state, and a read that met it is refused rather than
    /// answered from an entry that gave it nothing.
    ///
    /// Nothing calls it yet. The read verbs — `find`, `search`, `get`,
    /// `count`, `validate`, `describe` — are the callers this mapping has, and
    /// their handlers land above this layer (NORN-230/231); the mapping lands
    /// here because the vocabulary an answer is spelled in is not a surface's
    /// to choose.
    pub fn answer(self, name: &VaultName) -> ErrorEnvelope {
        match self {
            ReadRefusal::NotServing(demand) => match demand.answer(name) {
                Err(envelope) => envelope,
                Ok(state) => match state.not_ready() {
                    Some(not_ready) => ErrorEnvelope::new(
                        "this vault holds nothing to answer the read from yet",
                        ErrorDetail::entry_not_ready(not_ready),
                    ),
                    None => reader_unavailable(
                        "the entry published `ready` and served every surface but this read",
                    ),
                },
            },
            ReadRefusal::ReaderUnavailable(reason) => reader_unavailable(reason.detail()),
        }
    }
}

/// The envelope an entry that is serving with no read seam refuses with.
fn reader_unavailable(detail: impl Into<String>) -> ErrorEnvelope {
    ErrorEnvelope::new(
        "this vault is served and its read seam is not, so the read is refused",
        ErrorDetail::reader_unavailable(detail),
    )
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
    use norn_wire::{AttachMode, MaintainerIdentity, NameSet, WarmingPhase, WatcherLossCause};

    use crate::registry::AliasConflict;

    use super::*;

    fn name(text: &str) -> VaultName {
        VaultName::new(text).expect("a legal vault name")
    }

    /// Two colliding names, as the set a collision is spelled with.
    fn two_names(first: &str, second: &str) -> NameSet {
        NameSet::new([name(first), name(second)]).expect("two distinct colliding names")
    }

    /// The conflict two colliding registrations raise.
    fn a_conflict(first: &str, second: &str) -> AliasConflict {
        AliasConflict::new([name(first), name(second)]).expect("two distinct colliding names")
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
                Demand::DuplicateRoot(a_conflict("alpha", "beta")),
                Some(ErrorDetail::duplicate_root(two_names("alpha", "beta"))),
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
}

#[cfg(test)]
mod serving_tests {
    use norn_wire::{ErrorDetail, VaultName};

    use crate::lifecycle::ServingRefusal;

    fn name(text: &str) -> VaultName {
        VaultName::new(text).expect("a legal vault name")
    }

    /// Every refusal the serving set makes, paired with the detail it is filed
    /// under. The match below carries no wildcard, so a refusal minted without
    /// a row here does not compile.
    fn every_refusal() -> Vec<(ServingRefusal, ErrorDetail)> {
        let refusals = [ServingRefusal::AlreadyServed, ServingRefusal::Held];
        refusals
            .into_iter()
            .map(|refusal| {
                let detail = match refusal {
                    ServingRefusal::AlreadyServed => ErrorDetail::already_served(name("notes")),
                    ServingRefusal::Held => ErrorDetail::entry_held(name("notes")),
                };
                (refusal, detail)
            })
            .collect()
    }

    /// Every serving refusal reaches exactly one detail, under the name the
    /// change was asked for, and says so in words.
    #[test]
    fn every_serving_refusal_reaches_exactly_one_refusal_detail() {
        for (refusal, expected) in every_refusal() {
            let envelope = refusal.answer(&name("notes"));
            assert_eq!(
                envelope.detail(),
                &expected,
                "{refusal:?} refuses with another detail"
            );
            assert_eq!(envelope.code(), &expected.code());
            assert!(
                !envelope.message().is_empty(),
                "{refusal:?} refuses without saying so"
            );
            assert!(
                envelope.message().contains("notes"),
                "{refusal:?} refuses without naming the vault: {}",
                envelope.message()
            );
        }
    }
}

#[cfg(test)]
mod reload_tests {
    use norn_fs::WatchError;
    use norn_wire::{
        ControlFile, ControlFileFailure, ErrorDetail, MaintainerIdentity, NotReady, ReloadFailure,
        TrustState, UntrustedReason, VaultName, WarmingPhase,
    };

    use crate::lifecycle::{HostError, JobFailure};
    use crate::reload::{ReloadError, ReloadRefusal};

    fn name(text: &str) -> VaultName {
        VaultName::new(text).expect("a legal vault name")
    }

    /// Every shape a reload refusal takes, so a variant minted without a row
    /// in the walk below fails the census rather than passing unexercised.
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum Shape {
        UnknownVault,
        Unsupported,
        Unavailable,
        Core,
        Runtime,
        HostStopped,
    }

    impl Shape {
        const fn after(self) -> Option<Shape> {
            match self {
                Shape::UnknownVault => Some(Shape::Unsupported),
                Shape::Unsupported => Some(Shape::Unavailable),
                Shape::Unavailable => Some(Shape::Core),
                Shape::Core => Some(Shape::Runtime),
                Shape::Runtime => Some(Shape::HostStopped),
                Shape::HostStopped => None,
            }
        }
    }

    fn every_shape() -> Vec<Shape> {
        let mut shapes = vec![Shape::UnknownVault];
        while let Some(next) = shapes.last().expect("the walk starts at a shape").after() {
            assert!(!shapes.contains(&next), "{next:?} is reached twice");
            shapes.push(next);
        }
        shapes
    }

    /// The shape a sample is of. The match carries no wildcard, so a refusal
    /// variant minted without a shape does not compile.
    fn shape(refusal: &ReloadRefusal) -> Shape {
        match refusal {
            ReloadRefusal::UnknownVault => Shape::UnknownVault,
            ReloadRefusal::Unsupported => Shape::Unsupported,
            ReloadRefusal::Unavailable(_) => Shape::Unavailable,
            ReloadRefusal::Core(_) => Shape::Core,
            ReloadRefusal::Runtime(_) => Shape::Runtime,
            ReloadRefusal::HostStopped => Shape::HostStopped,
        }
    }

    /// Every reload refusal, paired with the detail it is filed under — or
    /// `None` where the refusal is the host being gone and carries no code.
    fn every_refusal() -> Vec<(ReloadRefusal, Option<ErrorDetail>)> {
        let unreadable = || ReloadError::SchemaParse("line 3 is not a mapping".to_string());
        vec![
            (
                ReloadRefusal::UnknownVault,
                Some(ErrorDetail::unknown_vault(name("notes"))),
            ),
            (
                ReloadRefusal::Unsupported,
                Some(ErrorDetail::reload_failed(ReloadFailure::unsupported())),
            ),
            (
                ReloadRefusal::Unavailable(TrustState::Ready),
                Some(ErrorDetail::reload_busy()),
            ),
            (
                ReloadRefusal::Unavailable(TrustState::Unattached),
                Some(ErrorDetail::entry_not_ready(NotReady::unattached())),
            ),
            (
                ReloadRefusal::Unavailable(TrustState::warming(WarmingPhase::Healing, 3, Some(9))),
                Some(ErrorDetail::entry_not_ready(NotReady::warming(
                    WarmingPhase::Healing,
                    3,
                    Some(9),
                ))),
            ),
            (
                ReloadRefusal::Unavailable(TrustState::untrusted(UntrustedReason::WatcherOverflow)),
                Some(ErrorDetail::entry_untrusted(
                    UntrustedReason::WatcherOverflow,
                )),
            ),
            (
                ReloadRefusal::Core(unreadable()),
                Some(ErrorDetail::reload_failed(ReloadFailure::control_file(
                    ControlFileFailure::new(
                        ControlFile::Schema,
                        norn_wire::ReloadStage::Parse,
                        unreadable().to_string(),
                    ),
                ))),
            ),
            (
                ReloadRefusal::Runtime(JobFailure::Reload(unreadable())),
                Some(ErrorDetail::reload_failed(ReloadFailure::control_file(
                    ControlFileFailure::new(
                        ControlFile::Schema,
                        norn_wire::ReloadStage::Parse,
                        unreadable().to_string(),
                    ),
                ))),
            ),
            (
                ReloadRefusal::Runtime(JobFailure::Environmental("the disk is full".to_string())),
                Some(ErrorDetail::reload_failed(ReloadFailure::environmental(
                    "the disk is full",
                ))),
            ),
            (
                ReloadRefusal::Runtime(JobFailure::StoreDamaged(
                    "the image is malformed".to_string(),
                )),
                Some(ErrorDetail::reload_failed(ReloadFailure::store_damaged(
                    "the image is malformed",
                ))),
            ),
            (
                ReloadRefusal::Runtime(JobFailure::WatcherTerminal(WatchError::Backend(
                    "the backend stopped".to_string(),
                ))),
                Some(ErrorDetail::reload_failed(ReloadFailure::watcher_terminal(
                    WatchError::Backend("the backend stopped".to_string()).to_string(),
                ))),
            ),
            (
                ReloadRefusal::Runtime(JobFailure::LostMaintainership),
                Some(ErrorDetail::reload_failed(
                    ReloadFailure::lost_maintainership(),
                )),
            ),
            (
                ReloadRefusal::Runtime(JobFailure::MaintainerContended(
                    MaintainerIdentity::unknown(),
                )),
                Some(ErrorDetail::reload_failed(
                    ReloadFailure::maintainer_contended(MaintainerIdentity::unknown()),
                )),
            ),
            (ReloadRefusal::HostStopped, None),
        ]
    }

    /// Every reload refusal reaches exactly one detail, payload and all — or
    /// leaves the envelope entirely, which is what the host being gone does.
    #[test]
    fn every_reload_refusal_reaches_exactly_one_refusal_detail() {
        for (refusal, expected) in every_refusal() {
            match (refusal.clone().answer(&name("notes")), &expected) {
                (Ok(envelope), Some(detail)) => {
                    assert_eq!(
                        envelope.detail(),
                        detail,
                        "{refusal:?} refuses with another detail"
                    );
                    assert_eq!(envelope.code(), &detail.code());
                    assert!(
                        !envelope.message().is_empty(),
                        "{refusal:?} refuses without saying so"
                    );
                }
                (Err(HostError::WorkerStopped), None) => {}
                (answer, expected) => {
                    panic!("{refusal:?} answered {answer:?} against {expected:?}")
                }
            }
        }
    }

    /// Every shape a reload refusal takes is sampled above.
    #[test]
    fn every_reload_refusal_shape_is_sampled() {
        let sampled: Vec<Shape> = every_refusal().iter().map(|(r, _)| shape(r)).collect();
        for expected in every_shape() {
            assert!(
                sampled.contains(&expected),
                "no refusal of shape {expected:?} is sampled"
            );
        }
    }

    /// Every core reload error names its file and its boundary as the typed
    /// wire pair, so a client branches on which control file refused where.
    #[test]
    fn a_core_reload_error_carries_its_file_and_its_boundary() {
        for (error, file, stage) in [
            (
                ReloadError::SchemaParse("bad".to_string()),
                ControlFile::Schema,
                norn_wire::ReloadStage::Parse,
            ),
            (
                ReloadError::SchemaApply("bad".to_string()),
                ControlFile::Schema,
                norn_wire::ReloadStage::Apply,
            ),
            (
                ReloadError::ConfigParse(norn_config::vault::VaultConfigError::InvalidToml {
                    message: "bad".to_string(),
                }),
                ControlFile::Config,
                norn_wire::ReloadStage::Parse,
            ),
            (
                ReloadError::ConfigRead(norn_fs::Refusal::Environment {
                    operation: "reading the config",
                    path: std::path::PathBuf::from("/vault/.norn/config.toml"),
                    kind: std::io::ErrorKind::PermissionDenied,
                    raw_os_error: None,
                    message: "denied".to_string(),
                }),
                ControlFile::Config,
                norn_wire::ReloadStage::Read,
            ),
            (
                ReloadError::SchemaRead(norn_fs::Refusal::Environment {
                    operation: "reading the schema",
                    path: std::path::PathBuf::from("/vault/.norn/schema.yaml"),
                    kind: std::io::ErrorKind::NotFound,
                    raw_os_error: None,
                    message: "missing".to_string(),
                }),
                ControlFile::Schema,
                norn_wire::ReloadStage::Read,
            ),
        ] {
            let envelope = ReloadRefusal::Core(error.clone())
                .answer(&name("notes"))
                .expect("a core error carries a code");
            assert_eq!(
                envelope.detail(),
                &ErrorDetail::reload_failed(ReloadFailure::control_file(ControlFileFailure::new(
                    file,
                    stage,
                    error.to_string()
                )))
            );
        }
    }
}

#[cfg(test)]
mod read_tests {
    use norn_wire::{
        ErrorDetail, NotReady, ReasonCode, TrustState, UntrustedReason, VaultName, WarmingPhase,
    };

    use crate::lifecycle::{Demand, ReadRefusal, ReaderUnavailable};

    fn name(text: &str) -> VaultName {
        VaultName::new(text).expect("a legal vault name")
    }

    /// A read cannot answer with a state, so the two states a poll walks out
    /// of become `host/entry-not-ready` here, carrying the counters a poll
    /// would have read.
    #[test]
    fn a_read_over_an_entry_that_holds_nothing_yet_is_not_ready() {
        for (state, expected) in [
            (TrustState::Unattached, NotReady::unattached()),
            (
                TrustState::warming(WarmingPhase::Healing, 3, Some(9)),
                NotReady::warming(WarmingPhase::Healing, 3, Some(9)),
            ),
            (
                TrustState::warming(WarmingPhase::ReleasingCoverage, 0, None),
                NotReady::warming(WarmingPhase::ReleasingCoverage, 0, None),
            ),
        ] {
            let envelope =
                ReadRefusal::NotServing(Demand::State(state.clone())).answer(&name("notes"));
            assert_eq!(
                envelope.detail(),
                &ErrorDetail::entry_not_ready(expected),
                "{state:?} refuses with another detail"
            );
            assert!(!envelope.message().is_empty());
        }
    }

    /// A demand that is already a refusal keeps its own refusal: the read does
    /// not restate an untrusted entry as an entry that is merely not ready.
    #[test]
    fn a_read_over_a_refusing_demand_keeps_that_demands_refusal() {
        let envelope = ReadRefusal::NotServing(Demand::State(TrustState::untrusted(
            UntrustedReason::WatcherOverflow,
        )))
        .answer(&name("notes"));
        assert_eq!(
            envelope.detail(),
            &ErrorDetail::entry_untrusted(UntrustedReason::WatcherOverflow)
        );

        let envelope = ReadRefusal::NotServing(Demand::UnknownVault).answer(&name("ledger"));
        assert_eq!(
            envelope.detail(),
            &ErrorDetail::unknown_vault(name("ledger"))
        );
    }

    /// An entry that is serving with its read seam down is its own refusal,
    /// carrying the account the act that failed produced.
    #[test]
    fn a_read_over_a_served_entry_with_no_read_seam_is_reader_unavailable() {
        let envelope = ReadRefusal::ReaderUnavailable(ReaderUnavailable::new(
            "this coverage mints no read handle",
        ))
        .answer(&name("notes"));
        assert_eq!(
            envelope.detail(),
            &ErrorDetail::reader_unavailable("this coverage mints no read handle")
        );
    }

    /// A `Ready` demand is a shape the lifecycle never publishes as a read
    /// refusal, and read as one it says the entry served every surface but
    /// this read — which is `host/reader-unavailable` and nothing else. It
    /// renders under that code rather than leaving the mapping as a state a
    /// caller has to decide about.
    #[test]
    fn a_ready_entry_read_as_a_refusal_is_the_read_seam_being_down() {
        let envelope =
            ReadRefusal::NotServing(Demand::State(TrustState::Ready)).answer(&name("notes"));
        assert_eq!(envelope.code(), &ReasonCode::HostReaderUnavailable);
        let ErrorDetail::ReaderUnavailable { detail, .. } = envelope.detail() else {
            panic!("a ready entry refused under another detail: {envelope:?}");
        };
        assert!(
            detail.contains("ready") && detail.contains("this read"),
            "the account does not say the entry served every surface but this read: {detail}"
        );
    }
}

#[cfg(test)]
mod page_refusal_tests {
    use norn_store::{PageRefusal, ReadBound, StoreError};
    use norn_wire::{AnswerShape, PagedRows, ReasonCode, RequestPart};

    /// The code each store read refusal is filed under. A compile guard over
    /// [`PageRefusal`]: the match reads only the variant and carries no
    /// wildcard, so a variant minted without a code, or the enum turned
    /// `#[non_exhaustive]`, does not compile.
    fn code_of(refusal: &PageRefusal) -> ReasonCode {
        match refusal {
            PageRefusal::UnreadableBound { .. } => ReasonCode::VaultUnreadableBound,
            PageRefusal::DeclarationNotPinned { .. } | PageRefusal::Store(_) => {
                ReasonCode::HostReadFailed
            }
            PageRefusal::OrderChanged(_) => ReasonCode::VaultCursorOrderChanged,
            PageRefusal::CursorNotTaken { .. } => ReasonCode::RequestCursorNotTaken,
            PageRefusal::EmptyMembership { .. } | PageRefusal::OutOfBound { .. } => {
                ReasonCode::RequestOutOfBound
            }
            PageRefusal::SummaryNotPaged
            | PageRefusal::UnknownPart { .. }
            | PageRefusal::PartNotTaken { .. } => ReasonCode::RequestPartNotTaken,
            PageRefusal::AmbiguousTarget(_) => ReasonCode::VaultAmbiguousTarget,
            PageRefusal::UnknownTarget { .. } => ReasonCode::VaultUnknownTarget,
        }
    }

    /// A refusal a request earns is filed under a `request/` code, and one the
    /// host earns by asking wrongly under `host/read-failed`.
    #[test]
    fn a_store_read_refusal_is_filed_under_its_wire_code() {
        for (refusal, expected) in [
            (
                PageRefusal::CursorNotTaken {
                    cursor: PagedRows::Hit,
                    paged: PagedRows::Document,
                },
                ReasonCode::RequestCursorNotTaken,
            ),
            (
                PageRefusal::OutOfBound {
                    bound: ReadBound::PageRows,
                    given: 0,
                },
                ReasonCode::RequestOutOfBound,
            ),
            (
                PageRefusal::PartNotTaken {
                    part: RequestPart::Column,
                    answer: AnswerShape::Section,
                },
                ReasonCode::RequestPartNotTaken,
            ),
            (PageRefusal::SummaryNotPaged, ReasonCode::RequestPartNotTaken),
            (
                PageRefusal::Store(StoreError::Damaged {
                    what: "a row".to_string(),
                }),
                ReasonCode::HostReadFailed,
            ),
        ] {
            assert_eq!(code_of(&refusal), expected, "{refusal:?}");
        }
    }
}
