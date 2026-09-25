//! The vault namespace's handlers: the verbs that act on one vault's standing
//! rather than read its documents.

use norn_wire::{ErrorEnvelope, ReloadParams, ReloadReport};

use crate::address::registered_name;
use crate::lifecycle::{EntryOps, Host, HostError};

impl<O: EntryOps> Host<O> {
    /// Answer a `vault reload`: validate the vault's schema-and-config
    /// candidate and activate it, or with `dry_run` validate it and activate
    /// nothing.
    ///
    /// Both are admitted and run as one reload is, so a dry run is refused
    /// whenever an activation asked at the same moment would be — among them
    /// `vault/reload-busy` while something works over the vault — and answers
    /// the outcome and fingerprints that activation would. Every refusal
    /// renders through [`ReloadRefusal::answer`](crate::ReloadRefusal::answer).
    ///
    /// `Err(HostError)` is a stopped host, which is transport death and carries
    /// no code: nothing about the vault was learned.
    pub fn vault_reload(
        &self,
        params: &ReloadParams,
    ) -> Result<Result<ReloadReport, ErrorEnvelope>, HostError> {
        let name = match registered_name(&params.vault) {
            Ok(name) => name,
            Err(refused) => return Ok(Err(refused)),
        };
        let judged = if params.dry_run {
            self.judge_reload(name)
        } else {
            self.reload(name)
        };
        match judged {
            Ok(judgment) => {
                let report =
                    ReloadReport::new(judgment.outcome.into(), judgment.fingerprints.into());
                Ok(Ok(if params.dry_run {
                    report.validated()
                } else {
                    report
                }))
            }
            Err(refusal) => refusal.answer(name).map(Err),
        }
    }
}
