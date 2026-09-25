//! The `vault status` handler: where one entry stands, or what every entry
//! adds up to.
//!
//! **A status is a lifecycle observation, not a read.** It takes each entry's
//! [`Observation`] under one hold of that entry's gate — the demand the entry
//! publishes, its retained facts, the store reading its last leg recorded and
//! its engine's report, all one instant — and then reads what lies outside
//! the host: the two authored control files of an entry serving active
//! fingerprints, judged against the fingerprints taken in that instant, and
//! the vault's `.gitignore` where the entry's last attachment staged shadows
//! in the vault-local fallback. It holds no reader, records no demand lease,
//! schedules nothing, attaches nothing, reads nothing of the derived store,
//! and carries no answer reading. So it answers for an unattached entry as
//! readily as for a ready one, and an entry about to be detached for idleness
//! is left exactly as it stood.
//!
//! **An untrusted entry is reported as a state.** A lease or a read refuses
//! an untrusted entry; a status renders the same instant's untrusted state as
//! the state it is, because it is what an operator asks a status about.
//!
//! **It reports the published demand whole.** An entry on a park reports the
//! park in its own code, in band, rather than being refused: the park is what
//! an operator asks a status about. The two answers that are refusals are the
//! two a status has nothing to report for — a name the host serves nothing
//! under, and an entry an unregistration holds — and a root, which addresses
//! a throwaway attach no lifecycle stands behind.
//!
//! The roll-up is computed from the same per-entry statuses, and `doctor`'s
//! registry half reads the same list, so the three never describe an entry
//! two ways.

use std::path::Path;

use norn_store::StoreReading;
use norn_wire::{
    Advisory, EngineSection, EngineStatus, ErrorEnvelope, RollUp, StatusParams, StatusReport,
    VaultName, VaultStatus,
};

use crate::address::registered_name;
use crate::lifecycle::{Demand, EntryOps, Held, Host, Observation};
use crate::refusal::control_file_failure;
use crate::reload::AuthoredDrift;
use crate::semantic::{SemanticEngines, engine_status};

/// Something an attachment met in its environment that a status reports as
/// an [`Advisory`], as the entry keeps it past that attachment's release.
///
/// **The shadow fallback is kept as its placement alone.** Whether the vault
/// ignores it is a fact about the vault's `.gitignore` as it is now, so it is
/// read when the advisory is reported ([`reported_advisories`]): a vault whose
/// `.gitignore` is corrected reports the fallback as ignored at the next
/// status, with no attach in between.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AttachmentAdvisory {
    /// The attachment stages its shadows in the vault-local fallback home at
    /// `home`, vault-relative.
    TmpFallbackInUse { home: String },
    /// The attachment's walk of the whole vault passed over the symbolic link
    /// at `path`, vault-relative.
    SymlinkSkipped { path: String },
}

/// The advisories `met` are reported as, for the vault whose operational root
/// is `root`: the one rendering `vault status` and `doctor` report an entry's
/// advisories through.
///
/// A fallback in use is reported with whether the vault ignores it, read here
/// from the `.gitignore` at `root` by [`norn_fs::fallback_ignored`]'s rule, at
/// most once however many advisories there are. A `.gitignore` that cannot be
/// read is reported as not ignoring the fallback: the question went
/// unanswered, and the answer that warns is the safe one. The match carries no
/// wildcard, so an advisory kept without a rendering here does not compile.
pub(crate) fn reported_advisories(met: &[AttachmentAdvisory], root: &Path) -> Vec<Advisory> {
    let mut ignored = None;
    met.iter()
        .map(|advisory| match advisory {
            AttachmentAdvisory::TmpFallbackInUse { home } => Advisory::tmp_fallback_in_use(
                home.clone(),
                *ignored.get_or_insert_with(|| norn_fs::fallback_ignored(root).unwrap_or(false)),
            ),
            AttachmentAdvisory::SymlinkSkipped { path } => Advisory::symlink_skipped(path.clone()),
        })
        .collect()
}

/// What a status reports of one vault's engine: the section delivered for it
/// and what its slot is doing, beside the store reading its entry recorded.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct EngineReport {
    pub(crate) section: EngineSection,
    pub(crate) status: EngineStatus,
}

impl EngineReport {
    /// The engine of the vault `name` in `engines`, judged against `store`.
    ///
    /// Taken under the hold of the entry's gate its observation is taken
    /// under, so the engine and the demand the entry publishes are one
    /// instant; it takes no slot lock, so it never waits behind a drain.
    ///
    /// **No delivery reads as an undelivered section.** A host that composes
    /// no engine delivers no section, and an entry that is not attached has
    /// had its delivery given back with its coverage; either way the host
    /// holds nothing of what the vault's config states, which is what the
    /// undelivered reading says, and the slot beside it is off.
    pub(crate) fn of(
        engines: Option<&SemanticEngines>,
        name: &VaultName,
        store: Option<&StoreReading>,
    ) -> Self {
        match engines.and_then(|engines| engines.reading(name)) {
            None => EngineReport {
                section: EngineSection::undelivered(),
                status: EngineStatus::off(),
            },
            Some((section, status)) => EngineReport {
                section,
                status: engine_status(status, store),
            },
        }
    }
}

impl<O: EntryOps> Host<O> {
    /// Answer a `vault status`: the standing of the vault the request names,
    /// or, naming none, the roll-up over every entry this host serves.
    ///
    /// A named vault is refused `host/unknown-vault` where the host serves no
    /// entry under the name, `host/entry-held` while an unregistration holds
    /// the entry, and `host/unsupported-attach-mode` where the address names
    /// a root. Everything else about an entry — a park, an untrusted state, a
    /// read seam that refuses, a reload that failed — is reported in the
    /// status rather than refused. The roll-up refuses nothing.
    pub fn vault_status(&self, params: &StatusParams) -> Result<StatusReport, ErrorEnvelope> {
        let Some(address) = &params.vault else {
            return Ok(StatusReport::roll_up(RollUp::of(&self.statuses())));
        };
        let name = registered_name(address)?;
        self.observe(name)
            .map(|observation| StatusReport::vault(self.status_of(observation)))
            .map_err(|unserved| unserved.answer(name))
    }

    /// The status of every entry this host serves, ascending by name: the
    /// list a roll-up is computed from.
    ///
    /// An entry an unregistration holds is still served until the change
    /// commits, as `vault list` shows it, so it is counted here too: as
    /// published under the refusal every request against it meets,
    /// `host/entry-held`, which is how the roll-up counts it among the parked
    /// and names it as wanting attention under that code. An entry the set
    /// let go of while this ran is left out.
    pub(crate) fn statuses(&self) -> Vec<VaultStatus> {
        self.observe_all()
            .into_iter()
            .map(|observed| match observed {
                Ok(observation) => self.status_of(observation),
                Err(held) => Self::unserved_status(held),
            })
            .collect()
    }

    /// The status `observation` reports: what the entry publishes, rendered
    /// through [`Demand::published`], beside its retained facts, its engine,
    /// the drift of its authored control files and its advisories.
    ///
    /// Everything but the last two is the observation's one instant. The
    /// drift reads the control files now, outside the gate, and judges them
    /// against the fingerprints taken in that instant; the advisories read
    /// the `.gitignore` now. Both files are the vault's own, which the host
    /// does not hold, and the comparison is to the observed instant.
    fn status_of(&self, observation: Observation) -> VaultStatus {
        let Observation {
            registration,
            published,
            inspection,
            control_root,
            engine,
        } = observation;
        let root = control_root
            .as_deref()
            .unwrap_or_else(|| registration.root.as_path());
        let drift = AuthoredDrift::of(
            &registration,
            inspection.active_fingerprints,
            control_root.as_deref(),
        );
        let advisories = reported_advisories(&inspection.advisories, root);
        let published = published.published(&registration.name);
        let mut status = VaultStatus::new(
            registration,
            published,
            drift.into(),
            engine.status,
            engine.section,
        )
        .with_advisories(advisories);
        if let Some(fingerprints) = inspection.active_fingerprints {
            status = status.with_fingerprints(fingerprints.into());
        }
        if let Some(error) = &inspection.last_reload_error {
            status = status.with_last_reload_failure(control_file_failure(error));
        }
        if let Some(unavailable) = &inspection.reader_unavailable {
            status = status.with_reads_refusing(unavailable.detail());
        }
        status
    }

    /// The status of an entry out of service that is still listed: published
    /// under the refusal every request against it meets, beside the engine
    /// read in the same hold of its gate, and with nothing else of it read —
    /// its gate answered that it is out of service, and that is all a status
    /// asks of it.
    fn unserved_status(held: Held) -> VaultStatus {
        let Held {
            registration,
            unserved,
            engine,
        } = held;
        let published = Demand::from(unserved).published(&registration.name);
        VaultStatus::new(
            registration,
            published,
            AuthoredDrift::Inactive.into(),
            engine.status,
            engine.section,
        )
    }
}
