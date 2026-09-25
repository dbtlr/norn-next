//! The `vault status` handler: where one entry stands, or what every entry
//! adds up to.
//!
//! **A status is a lifecycle observation, not a read.** It takes each entry's
//! [`Observation`] under one hold of that entry's gate, fingerprints the
//! authored control files outside it, and reads the engine slot the host
//! composes beside the vault. It holds no reader, records no demand lease,
//! schedules nothing and attaches nothing, and it carries no answer reading:
//! nothing it reports came out of a snapshot of the derived store. So it
//! answers for an unattached entry as readily as for a ready one, and an
//! entry about to be detached for idleness is left exactly as it stood.
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

use norn_wire::{
    EngineSection, EngineStatus, ErrorEnvelope, RollUp, StatusParams, StatusReport, VaultStatus,
};

use crate::Registration;
use crate::address::registered_name;
use crate::lifecycle::{Demand, EntryOps, Host, Observation, Unserved};
use crate::refusal::control_file_failure;
use crate::reload::AuthoredDrift;

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
                Err((registration, unserved)) => self.unserved_status(registration, unserved),
            })
            .collect()
    }

    /// The status `observation` reports: what the entry publishes, rendered
    /// through [`Demand::published`], beside its retained facts, the drift of
    /// its authored control files, and its engine.
    fn status_of(&self, observation: Observation) -> VaultStatus {
        let Observation {
            registration,
            published,
            inspection,
            control_root,
        } = observation;
        let drift = AuthoredDrift::of(
            &registration,
            inspection.active_fingerprints,
            control_root.as_deref(),
        );
        let (section, engine) = self.engine_of(&registration);
        let published = published.published(&registration.name);
        let mut status = VaultStatus::new(registration, published, drift.into(), engine, section)
            .with_advisories(inspection.advisories);
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
    /// under the refusal every request against it meets, and with nothing
    /// else of it read — its gate answered that it is out of service, and
    /// that is all a status asks of it.
    fn unserved_status(&self, registration: Registration, unserved: Unserved) -> VaultStatus {
        let (section, engine) = self.engine_of(&registration);
        let published = Demand::from(unserved).published(&registration.name);
        VaultStatus::new(
            registration,
            published,
            AuthoredDrift::Inactive.into(),
            engine,
            section,
        )
    }

    /// What the host was delivered as `registration`'s engine section, and
    /// what its engine slot is doing, read from one delivery.
    ///
    /// **No delivery reads as an absent section.** A host that composes no
    /// engine delivers no section, and an entry that is not attached has had
    /// its delivery given back with its coverage; either way no section the
    /// host holds enables an engine, which is what an absent section says,
    /// and the slot beside it is off.
    fn engine_of(&self, registration: &Registration) -> (EngineSection, EngineStatus) {
        let Some(engines) = self.ops().semantic() else {
            return (EngineSection::absent(), EngineStatus::off());
        };
        let (section, status) = engines.reading(&registration.name);
        (section.unwrap_or_else(EngineSection::absent), status.into())
    }
}
