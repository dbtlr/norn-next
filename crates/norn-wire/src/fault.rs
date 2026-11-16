//! What kind of fault a refusal is, read from its code and its detail.
//!
//! **A class answers what the caller should do next**, and nothing finer: change
//! the request, present a credential, name something that is there, re-plan
//! against state that moved, send less, ask again, fix the environment, or
//! report a defect. Every surface reads one class per refusal — the wire
//! route's HTTP status, the command line's exit code — so two surfaces cannot
//! disagree about whether a failure is worth asking again.
//!
//! **The class follows from the code and the detail together.** Most codes
//! carry one class whatever their detail. Three do not: an untrusted entry is
//! worth asking about again when its watcher or store is being rebuilt, and is
//! not when the environment refused it or its schema does not read; a failed
//! read is retryable when shared count storage was busy and is a defect when
//! the declaration was not the pinned one; and an apply that did not run takes
//! the class of the cause that kept it from running. So the class is decided
//! here, by a match over [`ErrorDetail`] — which carries exactly one variant
//! per code — and over the detail enums the class depends on, with no wildcard
//! arm: a code, or a variant of those detail enums, minted without a class does
//! not compile. That match is legal only inside this crate, because every one
//! of those enums is `#[non_exhaustive]` to the crates that read them.
//!
//! **[`FaultClass`] is closed, deliberately.** It is not `#[non_exhaustive]`,
//! so a surface mapping a class to a status or an exit code matches it
//! without a wildcard, and a class added here fails to compile at every
//! surface until each one has decided what the new class means to its caller.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::error::{ErrorDetail, ErrorEnvelope, ReadFailure};
use crate::trust::UntrustedReason;

/// What a refusal asks of the caller.
///
/// On the wire a class is the flat string itself: `"request"`,
/// `"unavailable"`.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FaultClass {
    /// The caller must change the request: as sent it will be refused
    /// however often it is sent.
    Request,
    /// The request carried no credential the host accepts.
    Unauthorized,
    /// The request names something that is not there.
    Missing,
    /// The request meets state the caller can change, or must re-plan or
    /// re-choose against.
    Conflict,
    /// The request is over a size bound.
    TooLarge,
    /// Asking again, with the same request, may succeed.
    Unavailable,
    /// A setting, a file or the environment must change; asking again as is
    /// will not help.
    Operator,
    /// A host defect.
    Defect,
}

impl FaultClass {
    /// Every class, in declaration order.
    pub const ALL: [FaultClass; 8] = [
        FaultClass::Request,
        FaultClass::Unauthorized,
        FaultClass::Missing,
        FaultClass::Conflict,
        FaultClass::TooLarge,
        FaultClass::Unavailable,
        FaultClass::Operator,
        FaultClass::Defect,
    ];
}

impl ErrorEnvelope {
    /// The class of fault this refusal is.
    ///
    /// Read from the detail, which names the same refusal the code does (an
    /// envelope whose two disagree does not parse), and from the detail's own
    /// reason where the class depends on it.
    pub fn fault_class(&self) -> FaultClass {
        self.detail().fault_class()
    }
}

impl ErrorDetail {
    /// The class of fault the refusal this detail belongs to is.
    ///
    /// No arm is a wildcard, here or in the two detail enums below: see the
    /// module documentation.
    pub fn fault_class(&self) -> FaultClass {
        match self {
            ErrorDetail::BadRequest {}
            | ErrorDetail::OutOfBound { .. }
            | ErrorDetail::PartNotTaken { .. }
            | ErrorDetail::CursorNotTaken { .. }
            | ErrorDetail::PlanInvalid { .. }
            | ErrorDetail::Unsatisfied { .. }
            | ErrorDetail::UnreadableBound { .. } => FaultClass::Request,
            ErrorDetail::Unauthorized {} | ErrorDetail::HelloRefused {} => {
                FaultClass::Unauthorized
            }
            ErrorDetail::UnknownVerb {}
            | ErrorDetail::UnknownVault { .. }
            | ErrorDetail::UnknownTarget { .. }
            | ErrorDetail::UnknownRule { .. } => FaultClass::Missing,
            ErrorDetail::AmbiguousRoot { .. }
            | ErrorDetail::AmbiguousTarget { .. }
            | ErrorDetail::AlreadyServed { .. }
            | ErrorDetail::DuplicateRoot { .. }
            | ErrorDetail::SharedSchema { .. }
            | ErrorDetail::CursorOrderChanged { .. }
            | ErrorDetail::PlanRefused { .. }
            | ErrorDetail::RootChanged { .. }
            | ErrorDetail::MigrationRefused { .. }
            | ErrorDetail::EngineNotEnabled { .. }
            | ErrorDetail::DrainRefused {} => FaultClass::Conflict,
            ErrorDetail::RequestTooLarge {} => FaultClass::TooLarge,
            ErrorDetail::EntryNotReady { .. }
            | ErrorDetail::MaintainerContended { .. }
            | ErrorDetail::EntryHeld { .. }
            // Its detail is a string clients do not match on, and its causes
            // — contention past the read's bound, damage being rebuilt — both
            // pass.
            | ErrorDetail::ReaderUnavailable { .. }
            | ErrorDetail::ReloadBusy {}
            // The documented remedy of each of these three is to send the
            // same plan or request again, which finishes or re-plans it.
            | ErrorDetail::ApplyOutcomeUnknown { .. }
            | ErrorDetail::PlanInterrupted { .. }
            | ErrorDetail::OutcomeUnknown {}
            | ErrorDetail::EngineUnavailable { .. }
            | ErrorDetail::Draining {}
            | ErrorDetail::Busy {} => FaultClass::Unavailable,
            ErrorDetail::TokenFileUnreadable {}
            | ErrorDetail::RegistryUnwritable { .. }
            | ErrorDetail::WriteFailed { .. }
            | ErrorDetail::ReloadFailed { .. } => FaultClass::Operator,
            ErrorDetail::EngineFailed { .. } => FaultClass::Defect,
            ErrorDetail::EntryUntrusted { reason } => untrusted_class(reason),
            ErrorDetail::ReadFailed { failure, .. } => read_failure_class(failure),
            // Nothing was written in any case: the apply is worth sending
            // again exactly when what kept it from running is worth asking
            // about again.
            ErrorDetail::ApplyNotRun { cause, .. } => cause.fault_class(),
        }
    }
}

fn untrusted_class(reason: &UntrustedReason) -> FaultClass {
    match reason {
        UntrustedReason::WatcherOverflow
        | UntrustedReason::WatcherLost { .. }
        | UntrustedReason::StoreDamagedRebuilding { .. }
        | UntrustedReason::StoreDamagedAwaitingDemand { .. } => FaultClass::Unavailable,
        UntrustedReason::EnvironmentalRefusal { .. } | UntrustedReason::SchemaUnreadable { .. } => {
            FaultClass::Operator
        }
        UntrustedReason::LegUnwound { .. } => FaultClass::Defect,
    }
}

fn read_failure_class(failure: &ReadFailure) -> FaultClass {
    match failure {
        ReadFailure::CountCursorExpired {} => FaultClass::Conflict,
        ReadFailure::CountResultCapacity { .. }
        | ReadFailure::CountResultBusy {}
        | ReadFailure::CountResult {}
        | ReadFailure::Statement {}
        | ReadFailure::UnreadableDocument {} => FaultClass::Unavailable,
        ReadFailure::CountResultTooLarge { .. } => FaultClass::Operator,
        ReadFailure::DeclarationNotPinned { .. } => FaultClass::Defect,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ReasonCode;
    use crate::error::tests::{a_detail, a_name, a_plan};
    use crate::trust::WatcherLossCause;

    fn envelope(detail: ErrorDetail) -> ErrorEnvelope {
        ErrorEnvelope::new("refused", detail)
    }

    /// The class each code's members fall in, as the table states it, for the
    /// codes whose class does not depend on their detail. The match carries
    /// no wildcard, so a code minted without a row here does not compile; the
    /// three whose class depends on the detail answer `None` and are walked
    /// variant by variant below.
    fn stated(code: &ReasonCode) -> Option<FaultClass> {
        use FaultClass::*;
        Some(match code {
            ReasonCode::ServeBadRequest
            | ReasonCode::RequestOutOfBound
            | ReasonCode::RequestPartNotTaken
            | ReasonCode::RequestCursorNotTaken
            | ReasonCode::RequestPlanInvalid
            | ReasonCode::RequestUnsatisfied
            | ReasonCode::VaultUnreadableBound => Request,
            ReasonCode::ServeUnauthorized | ReasonCode::ServeHelloRefused => Unauthorized,
            ReasonCode::ServeUnknownVerb
            | ReasonCode::HostUnknownVault
            | ReasonCode::VaultUnknownTarget
            | ReasonCode::VaultUnknownRule => Missing,
            ReasonCode::VaultAmbiguousRoot
            | ReasonCode::VaultAmbiguousTarget
            | ReasonCode::HostAlreadyServed
            | ReasonCode::HostDuplicateRoot
            | ReasonCode::HostSharedSchema
            | ReasonCode::VaultCursorOrderChanged
            | ReasonCode::VaultPlanRefused
            | ReasonCode::VaultRootChanged
            | ReasonCode::VaultMigrationRefused
            | ReasonCode::EngineNotEnabled
            | ReasonCode::ServeDrainRefused => Conflict,
            ReasonCode::ServeRequestTooLarge => TooLarge,
            ReasonCode::HostEntryNotReady
            | ReasonCode::HostMaintainerContended
            | ReasonCode::HostEntryHeld
            | ReasonCode::HostReaderUnavailable
            | ReasonCode::VaultReloadBusy
            | ReasonCode::HostApplyOutcomeUnknown
            | ReasonCode::VaultPlanInterrupted
            | ReasonCode::ServeOutcomeUnknown
            | ReasonCode::EngineUnavailable
            | ReasonCode::ServeDraining
            | ReasonCode::ServeBusy => Unavailable,
            ReasonCode::ServeTokenFileUnreadable
            | ReasonCode::HostRegistryUnwritable
            | ReasonCode::VaultWriteFailed
            | ReasonCode::VaultReloadFailed => Operator,
            ReasonCode::EngineFailed => Defect,
            ReasonCode::HostEntryUntrusted
            | ReasonCode::HostReadFailed
            | ReasonCode::HostApplyNotRun => return None,
        })
    }

    /// Every untrusted reason, beside the class the table puts it in.
    fn untrusted_reasons() -> Vec<(UntrustedReason, FaultClass)> {
        vec![
            (UntrustedReason::WatcherOverflow, FaultClass::Unavailable),
            (
                UntrustedReason::watcher_lost(WatcherLossCause::Backend, "the stream ended"),
                FaultClass::Unavailable,
            ),
            (
                UntrustedReason::store_damaged_rebuilding("a torn page"),
                FaultClass::Unavailable,
            ),
            (
                UntrustedReason::store_damaged_awaiting_demand("a torn page"),
                FaultClass::Unavailable,
            ),
            (
                UntrustedReason::environmental_refusal("permission denied"),
                FaultClass::Operator,
            ),
            (
                UntrustedReason::schema_unreadable("not yaml"),
                FaultClass::Operator,
            ),
            (
                UntrustedReason::leg_unwound("a worker panicked"),
                FaultClass::Defect,
            ),
        ]
    }

    /// Every read failure, beside the class the table puts it in.
    fn read_failures() -> Vec<(ReadFailure, FaultClass)> {
        vec![
            (ReadFailure::count_cursor_expired(), FaultClass::Conflict),
            (
                ReadFailure::count_result_capacity(Some(500)),
                FaultClass::Unavailable,
            ),
            (ReadFailure::count_result_busy(), FaultClass::Unavailable),
            (ReadFailure::count_result(), FaultClass::Unavailable),
            (ReadFailure::statement(), FaultClass::Unavailable),
            (ReadFailure::unreadable_document(), FaultClass::Unavailable),
            (
                ReadFailure::count_result_too_large(1024),
                FaultClass::Operator,
            ),
            (
                ReadFailure::declaration_not_pinned(Some("a".into()), Some("b".into())),
                FaultClass::Defect,
            ),
        ]
    }

    /// Every code whose class is its own reaches that class, through the
    /// envelope a caller is handed.
    #[test]
    fn every_code_reaches_the_class_the_table_states() {
        for code in ReasonCode::ALL {
            let Some(class) = stated(&code) else {
                continue;
            };
            assert_eq!(envelope(a_detail(&code)).fault_class(), class, "{code:?}");
        }
    }

    #[test]
    fn an_untrusted_entry_takes_its_reasons_class() {
        for (reason, class) in untrusted_reasons() {
            assert_eq!(
                envelope(ErrorDetail::entry_untrusted(reason.clone())).fault_class(),
                class,
                "{reason:?}"
            );
        }
    }

    #[test]
    fn a_failed_read_takes_its_failures_class() {
        for (failure, class) in read_failures() {
            assert_eq!(
                envelope(ErrorDetail::read_failed(failure.clone(), "refused")).fault_class(),
                class,
                "{failure:?}"
            );
        }
    }

    /// An apply that did not run is worth resending exactly when its cause
    /// is worth asking about again: a park passes, an unreadable schema does
    /// not, and a cause that is itself detail-dependent is read through.
    #[test]
    fn an_apply_not_run_takes_its_causes_class() {
        let causes = [
            (ErrorDetail::entry_held(a_name()), FaultClass::Unavailable),
            (
                ErrorDetail::entry_untrusted(UntrustedReason::schema_unreadable("not yaml")),
                FaultClass::Operator,
            ),
            (ErrorDetail::unknown_vault(a_name()), FaultClass::Missing),
            (ErrorDetail::draining(), FaultClass::Unavailable),
        ];
        for (cause, class) in causes {
            let not_run = ErrorDetail::apply_not_run(envelope(cause.clone()), Some(a_plan()));
            assert_eq!(envelope(not_run).fault_class(), class, "{cause:?}");
        }
    }

    /// Every class has a member, so no status or exit code a surface maps a
    /// class to is unreachable.
    #[test]
    fn every_class_has_a_member() {
        let mut reached: Vec<FaultClass> = ReasonCode::ALL
            .iter()
            .filter_map(stated)
            .chain(untrusted_reasons().into_iter().map(|(_, class)| class))
            .chain(read_failures().into_iter().map(|(_, class)| class))
            .collect();
        reached.sort_by_key(|class| FaultClass::ALL.iter().position(|c| c == class));
        reached.dedup();
        assert_eq!(reached, FaultClass::ALL.to_vec());
    }
}
