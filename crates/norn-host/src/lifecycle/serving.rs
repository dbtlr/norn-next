//! The vaults this host serves.
//!
//! One collection answers what the host serves and at which roots. Each entry
//! carries its own registration beside its lifecycle state, so the root a job
//! attaches, the root a recheck classifies, the root a refusal names and the
//! registration a registry request answers with are one fact read from one
//! place — there is no second account of the serving set to disagree with this
//! one about which names exist or where their roots are.
//!
//! # Joining and leaving
//!
//! [`ServingSet::insert`] and [`ServingSet::remove`] are how a vault joins and
//! leaves. Startup takes the first: [`Host::new`] inserts one entry per
//! registration it was built from, so a vault gained later joins the set
//! exactly the way every vault in it joined.
//!
//! Joining while the host runs carries a classification with it, and that is
//! the lifecycle's own move rather than this module's: a set knows which roots
//! it holds, and whether two of them are one root is a filesystem reading taken
//! against the entries a refusal then acts on. The registration verbs —
//! [`Host::vault_register`] and [`Host::vault_unregister`] — land on that
//! move and on [`ServingSet::remove`], each under the one lock every
//! registration change holds, so no insertion or removal but startup's runs
//! outside it.
//!
//! [`Host::new`]: crate::Host::new
//! [`Host::vault_register`]: crate::Host::vault_register
//! [`Host::vault_unregister`]: crate::Host::vault_unregister

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, RwLock};

use norn_config::registry::Entry as Registration;
use norn_fs::Refusal;
use norn_wire::VaultName;

use std::collections::BTreeSet;

use norn_fs::Identity;

use super::{Entry, SnapshotSource};
use crate::registry::{ResolveRefusal, RootReading, containing, reaching, recheck};

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
/// uncontended read lock and one refcount per lookup, and per pass over every
/// entry one allocation plus whatever that pass copies out of the map. Three
/// passes exist. [`ServingSet::snapshot`] copies the handles — N refcount
/// increments taking the reading and N decrements dropping it — and a
/// dispatcher tick takes three such readings. [`ServingSet::recheck`] copies
/// the name and root of every entry instead: two allocations each and no
/// refcount, and it stats every one of those roots. It runs on the legs that
/// acquire coverage and on the signals that invalidate it, never on a request
/// path. [`ServingSet::registrations`] copies every entry's whole
/// registration and reads nothing else, and a listing answers from it;
/// [`ServingSet::containing`] is that pass followed by a stat of every root it
/// copied, which makes it the one request path that stats the served roots,
/// and it is counted as a classification for that. That is the price of
/// insertability: a pass over a set nothing can join borrows its entries in
/// place and pays neither. A scan reads the set once and works from that
/// reading, so a vault that joins mid-scan is served from the next pass.
///
/// [`EntryOps`]: crate::EntryOps
pub(crate) struct ServingSet<A: SnapshotSource> {
    entries: RwLock<BTreeMap<VaultName, Arc<Entry<A>>>>,
    /// How many classifications this set has run. Each one stats every served
    /// root, so this counts the set's whole filesystem cost, and a path that
    /// leaves it unmoved spent no stat on the registry.
    classifications: AtomicUsize,
}

impl<A: SnapshotSource> ServingSet<A> {
    /// A set serving nothing.
    pub(crate) fn new() -> Self {
        Self {
            entries: RwLock::new(BTreeMap::new()),
            classifications: AtomicUsize::new(0),
        }
    }

    /// How many passes that stat every served root have run against this set.
    ///
    /// A caller reads this to hold a path to the root stats it spends: the
    /// counter moves once per [`ServingSet::recheck`] and once per
    /// [`ServingSet::containing`], and each of those stats every served root.
    /// It counts those stats alone, not the other reads a pass takes.
    ///
    /// The set is this crate's own, so a bar outside it reads this through
    /// `Host::classifications`, which is the narrowest surface that reaches it.
    /// Both stand on the same terms: the count is written by every pass that
    /// stats the roots and read by the cases and suites that assert what an
    /// act stated, so the reader is compiled for this crate's own cases and
    /// for the harness-reachable feature and for nothing else.
    #[cfg(any(feature = "induced-failure", test))]
    pub(crate) fn classifications(&self) -> usize {
        self.classifications.load(Ordering::SeqCst)
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

    /// Every registration the set serves at this instant, ascending by name.
    ///
    /// The registrations are copied out under the read guard and the guard
    /// goes back before this returns, so what a caller then does with them —
    /// a filesystem read included — stands outside the set's lock and holds
    /// no entry alive.
    pub(crate) fn registrations(&self) -> Vec<Registration> {
        self.entries
            .read()
            .expect("serving set poisoned")
            .values()
            .map(|entry| entry.registration.clone())
            .collect()
    }

    /// The registration whose root most specifically contains `directory`,
    /// judged by [`containing`] against every registration the set serves at
    /// this instant.
    ///
    /// The registrations are copied out through [`ServingSet::registrations`]
    /// and keyed by the name the set keys them by, so the stats the judgement
    /// takes stand outside the set's lock. Those stats reach every served
    /// root, which is a classification's cost, so the pass is counted as one.
    pub(crate) fn containing(
        &self,
        directory: &Path,
    ) -> Result<Option<Registration>, ResolveRefusal> {
        let registrations = self
            .registrations()
            .into_iter()
            .map(|registration| (registration.name.clone(), registration))
            .collect();
        self.classifications.fetch_add(1, Ordering::SeqCst);
        containing(&registrations, directory)
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
    /// What comes out under it is the name and root the classification reads
    /// and nothing else: a pass carrying entry handles instead would hold every
    /// entry in the set alive across the filesystem reads, and a removal
    /// concurrent with one would drop its entry here rather than at the
    /// removal.
    pub(crate) fn recheck(&self, name: &VaultName) -> Result<RootReading, Refusal> {
        let roots = {
            let entries = self.entries.read().expect("serving set poisoned");
            if !entries.contains_key(name) {
                return Ok(RootReading::default());
            }
            entries
                .values()
                .map(|entry| {
                    (
                        entry.registration.name.clone(),
                        entry.registration.root.clone(),
                    )
                })
                .collect::<Vec<_>>()
        };
        self.classifications.fetch_add(1, Ordering::SeqCst);
        recheck(
            roots.iter().map(|(name, root)| (name, root.as_path())),
            name,
        )
    }

    /// Every name the set serves whose root reaches `identity` at this
    /// instant.
    ///
    /// The name and root of every entry are copied out under the read guard,
    /// and the stats run after it goes back, as [`ServingSet::recheck`]'s do.
    /// They reach every served root, so the pass is counted as a
    /// classification.
    pub(crate) fn reaching(&self, identity: Identity) -> BTreeSet<VaultName> {
        let roots = self
            .entries
            .read()
            .expect("serving set poisoned")
            .values()
            .map(|entry| {
                (
                    entry.registration.name.clone(),
                    entry.registration.root.clone(),
                )
            })
            .collect::<Vec<_>>();
        self.classifications.fetch_add(1, Ordering::SeqCst);
        reaching(
            roots.iter().map(|(name, root)| (name, root.as_path())),
            identity,
        )
    }

    /// Serve one more vault, from now.
    ///
    /// The entry appears exactly as an entry read at startup does — Unattached,
    /// holding nothing, demandable — so the demand that follows attaches it the
    /// way it attaches any registered vault, classification and maintainer
    /// singleton included. What a join into a *running* host owes beyond that
    /// is the classification of the incumbents, which is the lifecycle's move
    /// around this one.
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
    /// **The entry is retired under that same gate hold.** A caller that read
    /// the entry out of the set before this took it out still holds it, and
    /// every door such a caller asks through reads the retirement first — so
    /// what it is answered is the name being unknown, never work scheduled
    /// against an entry the set no longer serves.
    ///
    /// A name the set does not serve is already not served, and removing it
    /// changes nothing. Whether the name was served is the caller's to ask
    /// first where it answers differently for the two.
    ///
    /// The entry the set gives up is dropped after the write lock goes back.
    /// The last handle to an entry runs its state's drop glue — the reader the
    /// caller's coverage minted among it — which is work no holder of this
    /// lock does.
    pub(crate) fn remove(&self, name: &VaultName) -> Result<(), ServingRefusal> {
        let mut entries = self.entries.write().expect("serving set poisoned");
        let Some(entry) = entries.get(name) else {
            return Ok(());
        };
        {
            let mut state = entry.gate.lock().expect("entry gate poisoned");
            if state.held_by_anything() {
                return Err(ServingRefusal::Held);
            }
            state.retired = true;
        }
        let removed = entries.remove(name);
        drop(entries);
        drop(removed);
        Ok(())
    }
}
