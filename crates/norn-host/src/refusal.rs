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

use norn_semantic::EngineError;
use norn_store::{PageRefusal, ReadBound, StoreError};
use norn_wire::{
    AnswerShape, AttachMode, ControlFile, ControlFileFailure, ErrorDetail, ErrorEnvelope,
    ReadFailure, ReloadFailure, RequestBound, RequestPart, TrustState, UntrustedReason, VaultName,
};

use crate::lifecycle::{Demand, HostError, JobFailure, ReadRefusal, ServingRefusal};
use crate::registry::ResolveRefusal;
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
            Demand::UnsupportedMode(mode) => Err(unsupported_attach_mode(mode)),
        }
    }
}

/// The envelope a request for an attach mode this host has no lifecycle for is
/// refused with — whether a demand named the mode, or the address a request
/// named its vault by asked for it.
pub(crate) fn unsupported_attach_mode(mode: AttachMode) -> ErrorEnvelope {
    ErrorEnvelope::new(
        "this host attaches registered vaults durably and holds no lifecycle for the attach \
         mode this request asks for",
        ErrorDetail::unsupported_attach_mode(mode),
    )
}

impl ServingRefusal {
    /// This refusal in the wire vocabulary, for the vault `name` the change
    /// was asked for under.
    ///
    /// Both members are facts about the host's serving of an entry, so both
    /// are `host/…` and both echo the name: what a caller does about either
    /// one is addressed to that name.
    ///
    /// A dormant carrier for the vault namespace handlers. The serving set is
    /// changed by the registry verbs — `vault register` and `vault
    /// unregister` — and this is the rendering those verbs refuse through;
    /// their handlers are not built, so nothing calls it yet. They land in
    /// this crate, which is why the visibility is `pub(crate)`: the mapping
    /// waits for the handler beside it rather than for a surface above it.
    #[allow(
        dead_code,
        reason = "a dormant carrier: the vault namespace's registry verbs, not yet built, \
                  refuse through this mapping"
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

impl ResolveRefusal {
    /// This refusal in the wire vocabulary.
    ///
    /// A resolution is a question about a directory rather than about an
    /// entry, so its refusal is `vault/ambiguous-root` and never the
    /// `host/duplicate-root` an entry over the same conflict is refused with:
    /// the candidates are every name that reaches the root, and the ask names
    /// none of them to echo.
    pub(crate) fn answer(self) -> ErrorEnvelope {
        match self {
            ResolveRefusal::AmbiguousRoot(conflict) => {
                let candidates = conflict.aliases().clone();
                let named = candidates
                    .names()
                    .iter()
                    .map(|name| format!("`{name}`"))
                    .collect::<Vec<_>>()
                    .join(", ");
                ErrorEnvelope::new(
                    format!(
                        "the registrations {named} all contain this directory at one root, so it \
                         names no one vault"
                    ),
                    ErrorDetail::ambiguous_root(candidates),
                )
            }
        }
    }
}

impl ReloadRefusal {
    /// This refusal in the wire vocabulary, for the vault `name` the reload
    /// was asked for.
    ///
    /// Every reload outcome is a fact about the vault, so every one of them is
    /// `vault/…` — except where the host answers before a reload is a thing
    /// that happened at all: a name the registry does not hold
    /// (`host/unknown-vault`), and an entry that is not reloadable because of
    /// where it stands. That entry answers with its published demand rendered
    /// through [`Demand::answer`], the mapping `vault status` and a demand
    /// lease answer through: a park in its own code (`host/duplicate-root`,
    /// `host/maintainer-contended`, or `host/entry-untrusted` for a root the
    /// registry cannot read), an entry whose derived state cannot be trusted
    /// in `host/entry-untrusted`, an entry that holds nothing to reload yet in
    /// `host/entry-not-ready`, and a `Ready` entry a warm job holds in
    /// `vault/reload-busy`.
    ///
    /// `Err(HostError)` is the host being gone rather than the vault refusing.
    /// It carries no code and no detail: no reload outcome was learned, so
    /// there is nothing about the vault to report.
    ///
    /// [`Host::vault_reload`](crate::Host::vault_reload) renders every reload
    /// refusal through this, an activation's and a dry run's alike. The
    /// mapping lives here because the vocabulary an answer is spelled in is
    /// not a surface's to choose.
    pub fn answer(self, name: &VaultName) -> Result<ErrorEnvelope, HostError> {
        Ok(match self {
            ReloadRefusal::UnknownVault => ErrorEnvelope::new(
                format!("no vault is registered under the name `{name}`"),
                ErrorDetail::unknown_vault(name.clone()),
            ),
            ReloadRefusal::Unavailable(demand) => match demand.answer(name) {
                Err(envelope) => envelope,
                Ok(state) => unavailable(state),
            },
            ReloadRefusal::Core(error) => {
                reload_failed(ReloadFailure::control_file(control_file_failure(&error)))
            }
            ReloadRefusal::Runtime(failure) => reload_failed(runtime_failure(failure)),
            ReloadRefusal::Unsupported => reload_failed(ReloadFailure::unsupported()),
            ReloadRefusal::HostStopped => return Err(HostError::WorkerStopped),
        })
    }
}

/// What an entry that is not reloadable right now refuses with, where its
/// published demand is a state that [`Demand::answer`] answers rather than
/// refuses.
///
/// Two readings of that state, taken in the order that makes each of them
/// true: a state that holds nothing yet is filed as not ready, and what is left
/// is an entry that is ready and busy — a warm job holds it, which is what a
/// reload is refused behind. A park and a state that refuses never reach here:
/// [`Demand::answer`] renders them in their own codes.
///
/// **`Ready` reaches here by three paths, and on each the reload did not
/// run.** At the ask, a claim held or coverage out with a leg is a job holding
/// the entry. At a reload leg's epilogue, a claim superseded by newer work is a
/// reload that lost its turn, and the entry may stand `Ready` again by the
/// time that epilogue reports. A reload the host moved past before any leg ran
/// it is answered with the demand the entry publishes once it is dropped, and
/// where no park stands that too may be `Ready` again. A release does not reach it as `Ready`:
/// `begin_release` sets `detach_in_flight` and sets the trust to
/// `Warming(WarmingPhase::ReleasingCoverage)` in the same statement pair under
/// one hold of the gate, so the epilogue that reads the flag reads that state
/// beside it. `a_release_window_opened_over_reload_closes_at_the_job_epilogue`
/// pins the value that arrives. Either way the answer is the same: nothing of
/// the asker's ran, and a retry is what asks again.
fn unavailable(state: TrustState) -> ErrorEnvelope {
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
/// The reload refusals here are its first caller. Its second is the `vault
/// status` handler, one of the vault namespace handlers, which is not built
/// yet; it lands in this crate and reports the same failure as an entry's
/// last reload failure, so the visibility is `pub(crate)` rather than private
/// to this module.
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
    /// The read seam every read verb answers through is its caller, so a
    /// read verb refused before its builder ran is refused here and nowhere
    /// else.
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

/// A store read refusal as the read seam answers it.
#[derive(Debug)]
pub(crate) enum PageRefused {
    /// The refusal the read answers with.
    Answered(ErrorEnvelope),
    /// The store found its derived data damaged, told without the driver's
    /// words. The read seam publishes that damage on the entry, which then
    /// owes the rebuild that resolves it, and answers with what the entry
    /// publishes: `host/entry-untrusted` under the store-damaged-rebuilding
    /// reason, never `host/read-failed`.
    Damaged(String),
}

/// A store read refusal in the wire vocabulary.
///
/// Total by its match, which reads only the variants and carries no wildcard:
/// a refusal or store error minted in the store without an arm here, or
/// either enum turned `#[non_exhaustive]`, does not compile. Each arm carries
/// the variant's own facts into the detail its code spells, and the message is
/// the store's account in words, except a store error's, whose account is
/// told through [`store_refusal_told`] so it names no file.
///
/// A refusal a request earns is filed under a `request/` code, and one the
/// vault's own state earns under `vault/`. A store error
/// [`StoreError::damage`] reads as damage is [`PageRefused::Damaged`], which
/// the read seam answers from the entry. Every other statement the store refused is a failed read, and so is
/// [`PageRefusal::DeclarationNotPinned`], which is a host defect: a hold hands
/// a builder the content model its snapshot pins, so a read through a hold
/// never meets it.
pub(crate) fn page_refusal(refusal: PageRefusal) -> PageRefused {
    let message = refusal.to_string();
    let detail = match refusal {
        PageRefusal::UnreadableBound { key, value } => ErrorDetail::unreadable_bound(key, value),
        PageRefusal::DeclarationNotPinned {
            declared_under,
            pinned,
        } => ErrorDetail::read_failed(
            ReadFailure::declaration_not_pinned(declared_under, pinned),
            message.clone(),
        ),
        PageRefusal::OrderChanged(changed) => ErrorDetail::cursor_order_changed(changed),
        PageRefusal::CursorNotTaken { cursor, paged } => {
            ErrorDetail::cursor_not_taken(cursor, paged)
        }
        PageRefusal::SummaryNotPaged => {
            ErrorDetail::part_not_taken(RequestPart::Cursor, Some(AnswerShape::Summary))
        }
        PageRefusal::EmptyMembership { key } => {
            ErrorDetail::out_of_bound(RequestBound::empty_membership(key))
        }
        PageRefusal::OutOfBound { bound, given } => {
            let ceiling = bound.ceiling();
            ErrorDetail::out_of_bound(match bound {
                ReadBound::PageRows => RequestBound::page_rows(given, ceiling),
                ReadBound::MembershipValues { key } => {
                    RequestBound::membership_values(key, given, ceiling)
                }
            })
        }
        PageRefusal::UnknownPart { part } => ErrorDetail::part_not_taken(part, None),
        PageRefusal::AmbiguousTarget(ambiguity) => {
            let ambiguity = *ambiguity;
            ErrorDetail::ambiguous_target(ambiguity.target, ambiguity.head, ambiguity.hint)
        }
        PageRefusal::UnknownTarget { target } => ErrorDetail::unknown_target(target),
        PageRefusal::PartNotTaken { part, answer } => {
            ErrorDetail::part_not_taken(part, Some(answer))
        }
        // Damage is what the store's own verdict says it is, so a changeset
        // entry refused for damage is damage here as it is on a job leg.
        PageRefusal::Store(
            error @ (StoreError::Damaged { .. }
            | StoreError::Entry { .. }
            | StoreError::Path { .. }
            | StoreError::Sql { .. }
            | StoreError::Lifecycle { .. }
            | StoreError::Bound { .. }
            | StoreError::UnpinnedDeclaration { .. }
            | StoreError::KeySpace { .. }),
        ) => {
            let told = store_refusal_told(&error);
            return match error.damage() {
                Some(_) => PageRefused::Damaged(told),
                None => PageRefused::Answered(ErrorEnvelope::new(
                    "the store refused a statement this read ran",
                    ErrorDetail::read_failed(ReadFailure::statement(), told),
                )),
            };
        }
    };
    PageRefused::Answered(ErrorEnvelope::new(message, detail))
}

/// A store refusal in words, naming no file.
///
/// A refusal told here reaches a caller holding no hold — a read's refusal
/// detail, the vault inspection. A caller reading the derived database's path
/// opens its own connection over the same database and answers from it under
/// no adjudication, which is the escape the reader type carries no route to.
///
/// **The account is built from the refusal's typed facts and never from the
/// driver's words.** The driver writes the database's path into its own
/// account of an open it could not make, and nothing bounds where else it
/// might, so a message is not redacted but left out: a redaction can only
/// strike the spellings it knows, and the driver may spell a path another way
/// or name a sidecar beside it. What is kept is what a caller can act on —
/// the act that failed, SQLite's own description of the result code it
/// failed with (a full disk, a read-only database), that the store found its
/// data damaged (never the store's words for what it found), and the store's
/// own bounds and fingerprints. The whole account stays on the `StoreError`,
/// where a log line that needs it reads it.
///
/// The match carries no wildcard, so a variant minted in the store takes its
/// stance on what it tells here.
pub(crate) fn store_refusal_told(error: &StoreError) -> String {
    match error {
        StoreError::Path { problem, .. } => {
            format!("a path handed to the store is not a document path: {problem}")
        }
        StoreError::Sql {
            operation,
            condition: Some(condition),
            ..
        } => format!("{operation} failed: {condition}"),
        StoreError::Sql {
            operation,
            condition: None,
            ..
        }
        | StoreError::Lifecycle { operation, .. } => format!("{operation} failed"),
        StoreError::Damaged { .. } => "the store is damaged".to_string(),
        StoreError::Bound { what, limit, given } => {
            format!("{what} holds at most {limit}, and {given} were given")
        }
        // Rendered from schema fingerprints, a path order and the store's own
        // static words, none of which is a path.
        StoreError::UnpinnedDeclaration { .. } | StoreError::KeySpace { .. } => error.to_string(),
        StoreError::Entry { index, problem, .. } => {
            format!("changeset entry {index}: {}", store_refusal_told(problem))
        }
    }
}

/// A semantic engine refusal in words, naming no file.
///
/// The engine's sidecar is a database beside the store, and its substrate
/// refusals carry the sidecar's path and the driver's words the way a store's
/// do, so they are told through [`store_refusal_told`] on the store's reading
/// of the same substrate refusal. The embedder's refusals name the vault
/// document an input came from and the model, which a caller already reads,
/// and a score the scan cannot rank names the vault document its row stands
/// for.
///
/// The match carries no wildcard, so a variant minted in the engine takes its
/// stance on what it tells here.
pub(crate) fn engine_refusal_told(error: &EngineError) -> String {
    match error {
        EngineError::Db(refused) => store_refusal_told(&StoreError::from(refused.clone())),
        EngineError::Store(refused) => {
            format!("the lane-1 store refused: {}", store_refusal_told(refused))
        }
        EngineError::Embed { .. }
        | EngineError::WrongWidth { .. }
        | EngineError::NonFiniteScore { .. } => error.to_string(),
        EngineError::SidecarDamaged { .. } => "the sidecar is damaged".to_string(),
    }
}

/// A refusal met in the host's own data directory in words, naming no file.
///
/// The maintainer lock and the shadow home sit beside the derived database,
/// so a path any of them names is a path to the directory that database is
/// in. The account keeps the act that failed and what the operating system
/// said, and drops every path.
///
/// The match carries no wildcard, so a variant minted in the filesystem crate
/// takes its stance on what it tells here.
pub(crate) fn data_dir_refusal_told(error: &norn_fs::Refusal) -> String {
    match error {
        norn_fs::Refusal::Environment {
            operation, kind, ..
        } => format!("{operation} in the data directory failed: {kind}"),
        norn_fs::Refusal::LockFileReplaced { attempts, .. } => {
            format!("the lock file was replaced on each of {attempts} attempts to lock it")
        }
        norn_fs::Refusal::Drifted { .. } | norn_fs::Refusal::Republished { .. } => {
            "a file in the data directory changed under the host".to_string()
        }
        norn_fs::Refusal::DestinationExists { .. } => {
            "a file in the data directory already exists".to_string()
        }
        norn_fs::Refusal::SymlinkDestination { .. } => {
            "a file in the data directory is a symbolic link".to_string()
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
mod resolve_tests {
    use norn_wire::{ErrorDetail, NameSet, ReasonCode, VaultName};

    use crate::registry::{AliasConflict, ResolveRefusal};

    fn names() -> [VaultName; 2] {
        [
            VaultName::new("alpha").expect("a legal vault name"),
            VaultName::new("beta").expect("a legal vault name"),
        ]
    }

    /// A resolution refused over a root two registrations reach is
    /// `vault/ambiguous-root`, carrying every name that reaches the root and
    /// naming them in words: the ask names no one vault, so no entry's
    /// refusal answers for it.
    #[test]
    fn an_ambiguous_resolution_is_refused_naming_every_candidate() {
        let conflict = AliasConflict::new(names()).expect("two distinct registrations");
        let envelope = ResolveRefusal::AmbiguousRoot(conflict).answer();
        assert_eq!(
            envelope.detail(),
            &ErrorDetail::ambiguous_root(NameSet::new(names()).expect("two distinct names"))
        );
        assert_eq!(envelope.code(), &ReasonCode::VaultAmbiguousRoot);
        for name in names() {
            assert!(
                envelope.message().contains(name.as_str()),
                "the refusal does not name `{name}` in words: {}",
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

    use crate::lifecycle::{Demand, HostError, JobFailure};
    use crate::registry::AliasConflict;
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
    /// or store error variant minted without a shape does not compile.
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
                ReloadRefusal::Unavailable(Demand::State(TrustState::Ready)),
                Some(ErrorDetail::reload_busy()),
            ),
            (
                ReloadRefusal::Unavailable(Demand::State(TrustState::Unattached)),
                Some(ErrorDetail::entry_not_ready(NotReady::unattached())),
            ),
            (
                ReloadRefusal::Unavailable(Demand::State(TrustState::warming(
                    WarmingPhase::Healing,
                    3,
                    Some(9),
                ))),
                Some(ErrorDetail::entry_not_ready(NotReady::warming(
                    WarmingPhase::Healing,
                    3,
                    Some(9),
                ))),
            ),
            (
                ReloadRefusal::Unavailable(Demand::State(TrustState::untrusted(
                    UntrustedReason::WatcherOverflow,
                ))),
                Some(ErrorDetail::entry_untrusted(
                    UntrustedReason::WatcherOverflow,
                )),
            ),
            (
                ReloadRefusal::Unavailable(Demand::DuplicateRoot(
                    AliasConflict::new([name("notes"), name("journal")])
                        .expect("two distinct registrations"),
                )),
                Some(ErrorDetail::duplicate_root(
                    norn_wire::NameSet::new([name("notes"), name("journal")])
                        .expect("two distinct names"),
                )),
            ),
            (
                ReloadRefusal::Unavailable(Demand::MaintainerContended(
                    MaintainerIdentity::unknown(),
                )),
                Some(ErrorDetail::maintainer_contended(
                    MaintainerIdentity::unknown(),
                )),
            ),
            (
                ReloadRefusal::Unavailable(Demand::IdentityRefused(
                    "the root is a symlink cycle".to_string(),
                )),
                Some(ErrorDetail::entry_untrusted(
                    UntrustedReason::environmental_refusal("the root is a symlink cycle"),
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
    use norn_store::{MAX_PAGE, PageRefusal, ReadBound, StoreError, TargetAmbiguity};
    use norn_wire::{
        AnswerShape, CandidateHead, CursorOrderChanged, ErrorDetail, Hint, PagedRows, ReadFailure,
        ReasonCode, RequestBound, RequestPart, ResolutionTarget,
    };

    use super::{ErrorEnvelope, PageRefused, page_refusal};

    /// What a store read refusal is, apart from what it carries.
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum Shape {
        UnreadableBound,
        DeclarationNotPinned,
        OrderChanged,
        CursorNotTaken,
        SummaryNotPaged,
        EmptyMembership,
        OutOfBound,
        UnknownPart,
        AmbiguousTarget,
        UnknownTarget,
        PartNotTaken,
        StoreDamaged,
        Store,
    }

    impl Shape {
        /// The shape after this one, and nothing at the end of the walk. The
        /// match carries no wildcard, so a shape minted without a place in the
        /// walk does not compile.
        const fn after(self) -> Option<Shape> {
            match self {
                Shape::UnreadableBound => Some(Shape::DeclarationNotPinned),
                Shape::DeclarationNotPinned => Some(Shape::OrderChanged),
                Shape::OrderChanged => Some(Shape::CursorNotTaken),
                Shape::CursorNotTaken => Some(Shape::SummaryNotPaged),
                Shape::SummaryNotPaged => Some(Shape::EmptyMembership),
                Shape::EmptyMembership => Some(Shape::OutOfBound),
                Shape::OutOfBound => Some(Shape::UnknownPart),
                Shape::UnknownPart => Some(Shape::AmbiguousTarget),
                Shape::AmbiguousTarget => Some(Shape::UnknownTarget),
                Shape::UnknownTarget => Some(Shape::PartNotTaken),
                Shape::PartNotTaken => Some(Shape::StoreDamaged),
                Shape::StoreDamaged => Some(Shape::Store),
                Shape::Store => None,
            }
        }
    }

    /// Every shape there is, walked from the first.
    fn every_shape() -> Vec<Shape> {
        let mut shapes = vec![Shape::UnreadableBound];
        while let Some(next) = shapes.last().expect("the walk starts at a shape").after() {
            assert!(!shapes.contains(&next), "{next:?} is reached twice");
            shapes.push(next);
        }
        shapes
    }

    /// The shape a sample is of. The match carries no wildcard, so a refusal
    /// or store error variant minted without a shape does not compile.
    fn shape(refusal: &PageRefusal) -> Shape {
        match refusal {
            PageRefusal::UnreadableBound { .. } => Shape::UnreadableBound,
            PageRefusal::DeclarationNotPinned { .. } => Shape::DeclarationNotPinned,
            PageRefusal::OrderChanged(_) => Shape::OrderChanged,
            PageRefusal::CursorNotTaken { .. } => Shape::CursorNotTaken,
            PageRefusal::SummaryNotPaged => Shape::SummaryNotPaged,
            PageRefusal::EmptyMembership { .. } => Shape::EmptyMembership,
            PageRefusal::OutOfBound { .. } => Shape::OutOfBound,
            PageRefusal::UnknownPart { .. } => Shape::UnknownPart,
            PageRefusal::AmbiguousTarget(_) => Shape::AmbiguousTarget,
            PageRefusal::UnknownTarget { .. } => Shape::UnknownTarget,
            PageRefusal::PartNotTaken { .. } => Shape::PartNotTaken,
            PageRefusal::Store(StoreError::Damaged { .. }) => Shape::StoreDamaged,
            PageRefusal::Store(
                StoreError::Path { .. }
                | StoreError::Sql { .. }
                | StoreError::Lifecycle { .. }
                | StoreError::Bound { .. }
                | StoreError::UnpinnedDeclaration { .. }
                | StoreError::KeySpace { .. }
                | StoreError::Entry { .. },
            ) => Shape::Store,
        }
    }

    fn target(text: &str) -> ResolutionTarget {
        ResolutionTarget::new(text).expect("a legal target")
    }

    /// Every store read refusal, paired with the code it is filed under and
    /// the whole detail it carries. The detail is pinned rather than the code
    /// alone, because what a client reads off a refusal is the payload.
    fn every_refusal() -> Vec<(PageRefusal, ReasonCode, ErrorDetail)> {
        let changed =
            || CursorOrderChanged::new("fingerprint-a", Some("fingerprint-b".to_string()));
        let head = || CandidateHead::new([], 2).expect("a head of two");
        vec![
            (
                PageRefusal::UnreadableBound {
                    key: "due".to_string(),
                    value: "soon".to_string(),
                },
                ReasonCode::VaultUnreadableBound,
                ErrorDetail::unreadable_bound("due", "soon"),
            ),
            (
                PageRefusal::DeclarationNotPinned {
                    declared_under: Some("fingerprint-a".to_string()),
                    pinned: None,
                },
                ReasonCode::HostReadFailed,
                ErrorDetail::read_failed(
                    ReadFailure::declaration_not_pinned(Some("fingerprint-a".to_string()), None),
                    PageRefusal::DeclarationNotPinned {
                        declared_under: Some("fingerprint-a".to_string()),
                        pinned: None,
                    }
                    .to_string(),
                ),
            ),
            (
                PageRefusal::OrderChanged(changed()),
                ReasonCode::VaultCursorOrderChanged,
                ErrorDetail::cursor_order_changed(changed()),
            ),
            (
                PageRefusal::CursorNotTaken {
                    cursor: PagedRows::Hit,
                    paged: PagedRows::Document,
                },
                ReasonCode::RequestCursorNotTaken,
                ErrorDetail::cursor_not_taken(PagedRows::Hit, PagedRows::Document),
            ),
            (
                PageRefusal::SummaryNotPaged,
                ReasonCode::RequestPartNotTaken,
                ErrorDetail::part_not_taken(RequestPart::Cursor, Some(AnswerShape::Summary)),
            ),
            (
                PageRefusal::EmptyMembership {
                    key: "status".to_string(),
                },
                ReasonCode::RequestOutOfBound,
                ErrorDetail::out_of_bound(RequestBound::empty_membership("status")),
            ),
            (
                PageRefusal::OutOfBound {
                    bound: ReadBound::PageRows,
                    given: 0,
                },
                ReasonCode::RequestOutOfBound,
                ErrorDetail::out_of_bound(RequestBound::page_rows(0, MAX_PAGE)),
            ),
            (
                PageRefusal::OutOfBound {
                    bound: ReadBound::MembershipValues {
                        key: "status".to_string(),
                    },
                    given: 900,
                },
                ReasonCode::RequestOutOfBound,
                ErrorDetail::out_of_bound(RequestBound::membership_values(
                    "status",
                    900,
                    ReadBound::MembershipValues {
                        key: "status".to_string(),
                    }
                    .ceiling(),
                )),
            ),
            (
                PageRefusal::UnknownPart {
                    part: RequestPart::unknown("a sort key"),
                },
                ReasonCode::RequestPartNotTaken,
                ErrorDetail::part_not_taken(RequestPart::unknown("a sort key"), None),
            ),
            (
                PageRefusal::AmbiguousTarget(Box::new(TargetAmbiguity {
                    target: target("glossary"),
                    head: head(),
                    hint: Hint::resolves(target("glossary")),
                })),
                ReasonCode::VaultAmbiguousTarget,
                ErrorDetail::ambiguous_target(
                    target("glossary"),
                    head(),
                    Hint::resolves(target("glossary")),
                ),
            ),
            (
                PageRefusal::UnknownTarget {
                    target: target("nowhere"),
                },
                ReasonCode::VaultUnknownTarget,
                ErrorDetail::unknown_target(target("nowhere")),
            ),
            (
                PageRefusal::PartNotTaken {
                    part: RequestPart::Column,
                    answer: AnswerShape::Section,
                },
                ReasonCode::RequestPartNotTaken,
                ErrorDetail::part_not_taken(RequestPart::Column, Some(AnswerShape::Section)),
            ),
            (
                PageRefusal::Store(StoreError::Sql {
                    operation: "reading a page",
                    condition: Some("disk I/O error"),
                    message: "disk I/O error reading /data/notes/store.db".to_string(),
                }),
                ReasonCode::HostReadFailed,
                ErrorDetail::read_failed(
                    ReadFailure::statement(),
                    "reading a page failed: disk I/O error",
                ),
            ),
        ]
    }

    /// Every store read refusal the store finds its derived data damaged for:
    /// the read seam answers it from the entry rather than as a failed read.
    fn every_damage() -> Vec<PageRefusal> {
        let damaged = || StoreError::Damaged {
            what: "reading a page met a database that is not readable: /data/notes/store.db"
                .to_string(),
        };
        vec![
            PageRefusal::Store(damaged()),
            PageRefusal::Store(StoreError::Entry {
                index: 2,
                path: "notes/today.md".to_string(),
                problem: Box::new(damaged()),
            }),
        ]
    }

    /// The envelope a refusal the read answers as it stands is told with.
    fn answered(refusal: PageRefusal) -> ErrorEnvelope {
        match page_refusal(refusal) {
            PageRefused::Answered(envelope) => envelope,
            PageRefused::Damaged(detail) => panic!("answered from the entry as damage: {detail}"),
        }
    }

    /// Every store read refusal reaches exactly one code, carrying the facts
    /// its variant carries, and says so in words.
    #[test]
    fn every_store_read_refusal_reaches_its_code_and_detail() {
        for (refusal, code, detail) in every_refusal() {
            let envelope = answered(refusal.clone());
            assert_eq!(
                envelope.code(),
                &code,
                "{refusal:?} is filed under another code"
            );
            assert_eq!(
                envelope.detail(),
                &detail,
                "{refusal:?} carries another detail"
            );
            assert_eq!(
                envelope.code(),
                &detail.code(),
                "{refusal:?} is filed under a code its detail does not name"
            );
            assert!(
                !envelope.message().is_empty(),
                "{refusal:?} refuses without saying so"
            );
        }
    }

    /// Every shape a store read refusal takes is sampled above, so the code a
    /// new variant is filed under is pinned rather than merely compiled.
    #[test]
    fn every_store_read_refusal_shape_is_sampled() {
        let sampled: Vec<Shape> = every_refusal()
            .iter()
            .map(|(r, _, _)| shape(r))
            .chain(every_damage().iter().map(shape))
            .collect();
        for expected in every_shape() {
            assert!(
                sampled.contains(&expected),
                "no refusal of shape {expected:?} is sampled"
            );
        }
    }

    /// A store that finds its derived data damaged is never a failed read:
    /// it is handed to the read seam as damage, told without the driver's
    /// words, so the entry publishes it and owes the rebuild.
    #[test]
    fn damage_is_answered_from_the_entry_and_names_no_file() {
        for refusal in every_damage() {
            let PageRefused::Damaged(detail) = page_refusal(refusal.clone()) else {
                panic!("{refusal:?} is answered as it stands rather than from the entry");
            };
            assert!(
                !detail.contains("store.db") && !detail.contains("/data"),
                "the damage names the database file: {detail}"
            );
        }
    }

    /// A semantic engine refusal is told without the sidecar's path or the
    /// driver's words, whichever seam it came from.
    #[test]
    fn an_engine_refusal_names_no_file() {
        let lifecycle = || norn_db::DbError::Lifecycle {
            operation: "preparing the sidecar's directory",
            path: std::path::PathBuf::from("/data/notes/semantic.sqlite3"),
            message: "denied: /data/notes".to_string(),
        };
        for refused in [
            norn_semantic::EngineError::Db(lifecycle()),
            norn_semantic::EngineError::Store(StoreError::from(lifecycle())),
            norn_semantic::EngineError::SidecarDamaged {
                what: "reading /data/notes/semantic.sqlite3 met a file that is not a database"
                    .to_string(),
            },
        ] {
            let told = super::engine_refusal_told(&refused);
            assert!(
                !told.contains("/data") && !told.contains("semantic.sqlite3"),
                "the refusal names the sidecar: {told}"
            );
        }
    }

    /// A statement the store refused is told without the database's path, so
    /// a refusal handed to a caller holding no hold names no file to open.
    #[test]
    fn a_refused_statement_names_no_file() {
        let envelope = answered(PageRefusal::Store(StoreError::Lifecycle {
            operation: "opening the derived database",
            path: std::path::PathBuf::from("/data/notes/store.sqlite3"),
            message: "denied".to_string(),
        }));
        let ErrorDetail::ReadFailed { detail, .. } = envelope.detail() else {
            panic!("a refused statement refused under another detail: {envelope:?}");
        };
        assert!(
            !detail.contains("store.sqlite3") && !envelope.message().contains("store.sqlite3"),
            "the refusal names the database file: {envelope:?}"
        );
    }
}
