//! The vaults this host serves.
//!
//! One collection answers what the host serves and at which roots. Each entry
//! carries its own registration beside its lifecycle state, so the root a job
//! attaches, the root a recheck classifies and the root a refusal names are one
//! fact read from one place — there is no second account of the serving set to
//! disagree with this one about which names exist or where their roots are.
//!
//! # Joining and leaving
//!
//! [`ServingSet::insert`] and [`ServingSet::remove`] are how a vault joins and
//! leaves. Startup takes the first: [`Host::new`] inserts one entry per
//! registration it was built from, so a vault gained later joins the set
//! exactly the way every vault in it joined. Nothing else in this crate calls
//! either verb today; the registration verbs a product surface offers are what
//! call them, and they are the seam those verbs land on.
//!
//! [`Host::new`]: crate::Host::new

use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};

use norn_config::VaultName;
use norn_config::registry::Entry as Registration;
use norn_fs::Refusal;

use super::{Entry, SnapshotSource};
use crate::registry::{RootReading, recheck};

/// Why the serving set stands unchanged.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ServingRefusal {
    /// The set already serves the name. The entry standing there keeps its
    /// registration and its lifecycle state: an insertion that replaced it
    /// would strand whatever that entry holds under a name nothing reaches it
    /// by any more.
    AlreadyServed,
    /// The entry holds something, or something holds the entry. Removal is
    /// refused rather than made to wait or to tear down, because a teardown
    /// here would run [`EntryOps::detach`] under the set's own lock and race
    /// every leg standing against the entry. A caller that wants a held entry
    /// gone lets it fall idle first and removes it after.
    ///
    /// [`EntryOps::detach`]: crate::EntryOps::detach
    Held,
}

/// Every vault this host serves, keyed by the name it is registered under.
///
/// The map is behind a lock because the set is insertable, and every read
/// clones the entry's handle out and lets the lock go: no read holds the set
/// lock across an entry gate, a filesystem read or an [`EntryOps`] call, and
/// the entry a caller holds outlives its removal from the set.
///
/// What a caller keeps from that read differs on the two sides of a dispatch,
/// and both halves carry their own rule. A hold already standing against an
/// entry — a read's handle, a leg's coverage, a lease — ends against that
/// entry rather than against the map it was reached through, so a removal
/// strands nothing that is running. Work in the job channel keeps no handle:
/// it carries a name and an epoch, and the worker that takes it resolves that
/// name through the set again, so a job whose name the set no longer serves
/// reaches no entry and does nothing where it arrives.
///
/// That second read is what a removal is admitted under while jobs are in
/// flight. A job carrying the entry it was scheduled against would spare the
/// lookup and attach a vault the host has stopped serving, leaving the
/// maintainer lock and the watcher that attach acquired standing under a name
/// nothing reaches to give them back.
///
/// [`ServingSet::remove`] is the one move that holds both locks, and it takes
/// them in that order — the set, then the entry's gate. Every other holder of
/// an entry gate reached the entry through a read of the set that has already
/// let go, so no path takes them the other way round.
///
/// The cost of an insertable set over one frozen at construction is one
/// uncontended read lock and one refcount per lookup, and one allocation per
/// pass over every entry. A scan reads the set once and works from that
/// reading, so a vault that joins mid-scan is served from the next pass.
///
/// [`EntryOps`]: crate::EntryOps
pub(crate) struct ServingSet<A: SnapshotSource> {
    entries: RwLock<BTreeMap<VaultName, Arc<Entry<A>>>>,
}

impl<A: SnapshotSource> ServingSet<A> {
    /// A set serving nothing.
    pub(crate) fn new() -> Self {
        Self {
            entries: RwLock::new(BTreeMap::new()),
        }
    }

    /// The entry serving `name`, and nothing where the set serves no such name.
    pub(crate) fn get(&self, name: &VaultName) -> Option<Arc<Entry<A>>> {
        self.entries
            .read()
            .expect("serving set poisoned")
            .get(name)
            .cloned()
    }

    /// Every entry the set serves at this instant, ascending by name.
    pub(crate) fn snapshot(&self) -> Vec<Arc<Entry<A>>> {
        self.entries
            .read()
            .expect("serving set poisoned")
            .values()
            .cloned()
            .collect()
    }

    /// Classify `name`'s root against every other root the set serves.
    ///
    /// The reading is taken over the set as it stands, so a vault that joined
    /// after startup is classified like every other one: an alias of an
    /// established root cannot enter unseen, and the name it duplicates learns
    /// of it at the next read of its own root.
    ///
    /// A name the set does not serve has no root to read, and classifying the
    /// rest on its behalf would only spend filesystem reads on an answer that
    /// is about no entry. The map itself is what answers that membership, under
    /// the read guard the roots are cloned out of: an unserved name costs one
    /// keyed lookup rather than a pass over a snapshot.
    ///
    /// The guard goes back before the classification runs, so the filesystem
    /// reads below stand outside the set's lock like every other read here.
    pub(crate) fn recheck(&self, name: &VaultName) -> Result<RootReading, Refusal> {
        let entries = {
            let entries = self.entries.read().expect("serving set poisoned");
            if !entries.contains_key(name) {
                return Ok(RootReading::default());
            }
            entries.values().cloned().collect::<Vec<_>>()
        };
        recheck(
            entries
                .iter()
                .map(|entry| (&entry.registration.name, entry.registration.root.as_path())),
            name,
        )
    }

    /// Serve one more vault, from now.
    ///
    /// The entry appears exactly as an entry read at startup does — Unattached,
    /// holding nothing, demandable — so the demand that follows attaches it the
    /// way it attaches any registered vault, classification and maintainer
    /// singleton included.
    pub(crate) fn insert(&self, registration: Registration) -> Result<(), ServingRefusal> {
        let mut entries = self.entries.write().expect("serving set poisoned");
        if entries.contains_key(&registration.name) {
            return Err(ServingRefusal::AlreadyServed);
        }
        entries.insert(
            registration.name.clone(),
            Arc::new(Entry::unattached(registration)),
        );
        Ok(())
    }

    /// Stop serving `name`, where the entry serving it holds nothing and
    /// nothing holds it.
    ///
    /// The predicate is read under the entry's own gate and under the set's
    /// write lock together, so an entry that becomes held between the two is
    /// not one this removes: a demand reaches the entry through the set, and
    /// the set is not readable while this decides.
    ///
    /// A name the set does not serve is already not served, and removing it
    /// changes nothing.
    ///
    /// The entry the set gives up is dropped after the write lock goes back.
    /// The last handle to an entry runs its state's drop glue — the reader the
    /// caller's coverage minted among it — and that is work, which is work no
    /// holder of this lock does.
    // Insertion is on the startup path and removal has no caller in this crate
    // outside its own cases, so the allow is what says the seam is built and
    // waiting for the registration verb that calls it rather than unfinished.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn remove(&self, name: &VaultName) -> Result<(), ServingRefusal> {
        let mut entries = self.entries.write().expect("serving set poisoned");
        let Some(entry) = entries.get(name) else {
            return Ok(());
        };
        if entry
            .gate
            .lock()
            .expect("entry gate poisoned")
            .held_by_anything()
        {
            return Err(ServingRefusal::Held);
        }
        let removed = entries.remove(name);
        drop(entries);
        drop(removed);
        Ok(())
    }
}
