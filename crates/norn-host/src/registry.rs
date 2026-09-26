use std::collections::{BTreeMap, BTreeSet};
use std::convert::Infallible;
use std::fmt;
use std::path::Path;

use norn_config::registry::{Entry, Registry, VaultRoot};
use norn_fs::{Identity, Refusal, canonical_spelling, path_identity, readable_directory};
use norn_wire::{
    DoctorRegistryParams, DoctorRegistryReport, EngineHealth, ErrorEnvelope, ListParams,
    ListReport, MaintainerIdentity, NameSet, RegisterParams, RegisterReport, RegistryProblem,
    RegistrySanity, ResolveParams, ResolveReport, RollUp, SetParams, SetReport, TooFewNames,
    UnregisterParams, UnregisterReport, VaultName,
};

use crate::lifecycle::{EntryOps, Host, ServingRefusal, StandingPark};

/// Every registry name that resolves to one filesystem root.
///
/// The conflict holds the wire's own [`NameSet`], so the two or more names, the
/// ascending order and the uniqueness are the value's rather than this type's
/// to keep, and the refusal a conflict is rendered as is built from the set
/// without judging it again. A conflict raised by classifying the whole
/// registry and a conflict raised by one attach meeting another alias's claim
/// are the same fact in the same shape.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AliasConflict {
    aliases: NameSet,
}

impl AliasConflict {
    /// The conflict the `aliases` name, or the reason those names are no
    /// conflict.
    ///
    /// A root one name reaches is a root nothing collides over, so a caller
    /// that collected fewer than two distinct names has no conflict to raise
    /// and is told so here rather than carrying a refusal that names one
    /// vault. `refuse_conflict` takes one entry gate per alias and holds them
    /// all at once: a name appearing twice would have that one thread wait on
    /// a lock it is already holding, and ascending order is what makes two
    /// concurrent refusals over overlapping alias sets take the gates they
    /// share in the same order rather than in opposite ones. The set keeps
    /// both.
    pub fn new(aliases: impl IntoIterator<Item = VaultName>) -> Result<Self, TooFewNames> {
        Ok(Self {
            aliases: NameSet::new(aliases)?,
        })
    }

    /// Every registered name that reaches the one root, ascending.
    pub fn aliases(&self) -> &NameSet {
        &self.aliases
    }
}

/// What one read of the served roots resolved for one registered name.
///
/// The classification a recheck runs resolves the root's identity on its way
/// to the conflict, so both come back from the one read: a caller that needs
/// the identity to claim an acquisition has it without asking the filesystem
/// the same question twice, and one refusal answers for both facts.
///
/// The reading is the host's own: the identity it carries is a `norn-fs` fact,
/// and the lifecycle inside this crate is the only caller that claims an
/// acquisition against one.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct RootReading {
    /// The filesystem identity the name's root resolves to. A registered root
    /// the filesystem answers for with nothing is registrable rather than
    /// resolved, and resolves to nothing here.
    pub(crate) identity: Option<Identity>,
    /// Every registered name that reaches this root, where more than one does.
    pub(crate) conflict: Option<AliasConflict>,
}

/// The registrations a host is built from: one name's root and schema source
/// per entry, ascending and each name once.
///
/// This is a read, not a collection the host keeps. [`Host::new`] turns each
/// registration here into a served entry and retains no second account of
/// them, so the roots a running host classifies are the roots its serving set
/// holds — including any the set has gained since this read.
///
/// [`Host::new`]: crate::Host::new
#[derive(Clone, Debug)]
pub struct RegistryRead {
    entries: BTreeMap<VaultName, Entry>,
}

impl RegistryRead {
    /// Read the registrations without choosing a winner among duplicate roots.
    /// Duplicates and missing roots are classified where a host acquires
    /// coverage over a root or is told coverage over one ended, against the
    /// roots that host serves at that moment.
    pub fn read(registry: &Registry) -> Self {
        Self::from_entries(registry.entries().cloned())
    }

    /// Take the registrations from an already-read registry projection.
    pub fn from_entries(entries: impl IntoIterator<Item = Entry>) -> Self {
        Self {
            entries: entries
                .into_iter()
                .map(|entry| (entry.name.clone(), entry))
                .collect(),
        }
    }

    /// The registrations themselves, ascending by name.
    pub(crate) fn into_entries(self) -> impl Iterator<Item = Entry> {
        self.entries.into_values()
    }
}

/// The registry requests: questions about the serving set as a whole, and
/// the changes to it.
///
/// Each answers from the set this host serves at the instant it is asked,
/// never from a fresh read of the registry file: the set is seeded from that
/// file at startup and is the one account of which names exist and where their
/// roots are after that, so a vault that joined after startup is answered for
/// and one that left is not. A change — a register, an unregister or a set —
/// writes the file first and the set after, and a change the file refused
/// leaves the set as it stood.
impl<O: EntryOps> Host<O> {
    /// Answer a `vault list`: every registration this host serves, ascending
    /// by name.
    ///
    /// Nothing refuses a listing. The set is read under its own lock and
    /// nothing else is read, so a host serving nothing answers an empty list.
    pub fn vault_list(&self, _params: &ListParams) -> ListReport {
        ListReport::new(self.registrations())
    }

    /// Answer a `vault resolve`: the registration whose root most specifically
    /// contains `params.directory`, or none where no root contains it.
    ///
    /// The directory is resolved to its [`canonical_spelling`], and a root
    /// contains it where the directory the root resolves to is that spelling
    /// or one of its ancestors: a link on either side is judged as the
    /// directory it reaches, and a directory that does not exist is judged on
    /// its spelling beneath what does. Every registration the set serves when
    /// asked is a candidate. The one refusal is a most specific root that more
    /// than one registration reaches, which is `vault/ambiguous-root` naming
    /// them all.
    pub fn vault_resolve(&self, params: &ResolveParams) -> Result<ResolveReport, ErrorEnvelope> {
        self.containing(params.directory.as_path())
            .map(|registration| {
                registration.map_or_else(ResolveReport::none, ResolveReport::registered)
            })
            .map_err(ResolveRefusal::answer)
    }

    /// Answer a `vault register`: the registration as admitted, and what its
    /// entry publishes once it is in.
    ///
    /// **The root is admitted at its [`canonical_spelling`]**, links anywhere
    /// in it resolved, and the report echoes the root as stored. A root that
    /// is nothing, is not a directory, or cannot be listed is refused
    /// `host/entry-untrusted` under the environmental-refusal reason, the
    /// account the registry recheck gives a root it cannot read.
    ///
    /// A name the host serves, and a name the registry file already records
    /// — another writer's registration, which the file keeps as it is — are
    /// each refused `host/already-served`. A root a
    /// served vault already reaches is refused `host/duplicate-root` naming
    /// every such vault beside this one, before anything is written — so an
    /// incumbent serving that root goes on serving it. That read is
    /// best-effort: the classification the join runs is the authority, and
    /// where the filesystem moved between the two and the join parks the
    /// root, the registration stands and `published` carries the park. A
    /// registry file that cannot be written is refused
    /// `host/registry-unwritable`, and the serving set does not change.
    ///
    /// Nothing is attached here. The entry joins unattached, and the demand
    /// that follows attaches it the way it attaches every registered vault.
    pub fn vault_register(&self, params: &RegisterParams) -> Result<RegisterReport, ErrorEnvelope> {
        let name = &params.registration.name;
        let (admitted, published) = self
            .register(params.registration.clone())
            .map_err(|refusal| refusal.answer(name))?;
        Ok(RegisterReport::new(admitted, published.published(name)))
    }

    /// Answer a `vault unregister`: the name removed, and whether its derived
    /// state was discarded.
    ///
    /// A name the host serves nothing under is refused `host/unknown-vault`,
    /// and an entry something holds — coverage, work, a read, a lease — is
    /// refused `host/entry-held`; the operator asks again once it is idle. An
    /// entry standing on a park that nothing holds is unregistered, and the
    /// park leaves with it: a name parked beside it on one root is classified
    /// again at once, and serves where nothing else reaches that root.
    /// Another process holding the vault's maintainer lock is refused
    /// `host/maintainer-contended` with nothing changed.
    ///
    /// Otherwise, under that lock, the registry file is read, the derived
    /// database, its sidecars and the shadow homes are discarded unless
    /// `keep_state` keeps them, and the file is written; the lock goes back
    /// then, and the entry leaves the serving set after it. The maintainer
    /// lock file is never removed, and nothing in the vault's own tree but
    /// its shadow homes is touched. From the moment the entry is found idle
    /// until the change commits or is refused, every request that asks the
    /// entry for anything is refused `host/entry-held`, while `vault list`
    /// and `vault resolve` still name the vault; after the commit the name is
    /// `host/unknown-vault`. A data directory that refuses the lock or the
    /// discard is refused `host/entry-untrusted` under the
    /// environmental-refusal reason, and a registry file that cannot be read
    /// or written is refused `host/registry-unwritable` — one that cannot be
    /// read before anything is discarded. After any refusal the registration
    /// that stood before still stands, served by the entry that stood.
    /// Derived state is rebuildable, so what a refused change did discard is
    /// derived again by the next attach.
    pub fn vault_unregister(
        &self,
        params: &UnregisterParams,
    ) -> Result<UnregisterReport, ErrorEnvelope> {
        self.unregister(&params.name, params.keep_state)
            .map_err(|refusal| refusal.answer(&params.name))?;
        Ok(UnregisterReport::new(
            params.name.clone(),
            !params.keep_state,
        ))
    }

    /// Answer `doctor`'s registry half: the roll-up `vault status` computes,
    /// whether the registry itself is in order, and each vault's engine
    /// health, ascending by name.
    ///
    /// **One reading of every entry answers all three.** The statuses are
    /// the ones a roll-up is computed from, so the roll-up here is the one
    /// `vault status` naming no vault answers, less the parks whose cause
    /// the sanity pass names as a registry problem, the engines are read off
    /// the same statuses, and the sanity pass reads the roots of the same
    /// registrations. Per-vault standing is not restated: the roll-up's
    /// attention reasons are what `doctor` reports of it, the advisories
    /// each vault's last attachment met among them, and one cause is named
    /// once ([`DoctorRegistryReport::new`]).
    ///
    /// Nothing refuses it, and nothing it does changes a registration, an
    /// entry or a vault: the statuses are observations that record no demand
    /// and schedule nothing. **The sanity pass is `doctor`'s one reading of
    /// a root's identity** — it states each served root once and lists each
    /// that resolves — and no lock is held while it does. The statuses read
    /// inside the vaults as a status does: the two control files of an entry
    /// serving active fingerprints, and the `.gitignore` of one whose last
    /// attachment staged shadows in the vault-local fallback.
    pub fn doctor_registry(&self, _params: &DoctorRegistryParams) -> DoctorRegistryReport {
        let statuses = self.statuses();
        let registry = sanity(statuses.iter().map(|status| {
            (
                &status.registration.name,
                status.registration.root.as_path(),
            )
        }));
        let engines = statuses.iter().map(|status| {
            EngineHealth::new(
                status.registration.name.clone(),
                status.section.clone(),
                status.engine.clone(),
            )
        });
        DoctorRegistryReport::new(RollUp::of(&statuses), registry, engines)
    }

    /// Answer a `vault set`: the registration as it now stands, and what its
    /// entry publishes after the edit.
    ///
    /// **The entry's standing is answered first.** An entry something holds
    /// is refused `host/entry-held`, and one standing on a park in the park's
    /// own code, before the edit's root is checked and whether or not the
    /// edit changes anything.
    ///
    /// A field the edit keeps stands as it was, a field it sets holds the
    /// value, and a field it clears falls back to its default. **A root the
    /// edit moves is pre-checked as a register checks one**: taken at its
    /// [`canonical_spelling`], refused `host/entry-untrusted` under the
    /// environmental-refusal reason where it is no readable directory, and
    /// refused `host/duplicate-root` naming every other served vault that
    /// already reaches it. A root reaching the directory the vault stands at
    /// already is no duplicate of itself. **A root whose canonical spelling
    /// is not the recorded root is a move**, even where the recorded spelling
    /// reaches the same directory: it records the canonical root and discards
    /// the derived state as every move does, because the store does not
    /// record the directory it was derived from. An edit that changes nothing
    /// answers the registration as it stands, and writes nothing.
    ///
    /// **The serving set is authoritative over a hand edit of the registry
    /// file.** The edit is made to the registration this host serves, and the
    /// file is written with that registration as edited: over a root or a
    /// field a hand edit changed under the name, and under a served name a
    /// hand edit took out. A hand edit of the file takes effect at the next
    /// start.
    ///
    /// A name the host serves nothing under is refused `host/unknown-vault`,
    /// and an entry something holds is refused `host/entry-held`; the
    /// operator asks again once it is idle. **An entry standing on a park is
    /// refused in the park's own code**, and the park stands: an edit never
    /// withdraws a park unseen. So a vault parked `host/duplicate-root` cannot
    /// be moved off the shared root by an edit; `vault unregister` of one of
    /// the names is the remedy.
    ///
    /// Otherwise the change takes the path an unregistration takes. The entry
    /// is withdrawn, and from then until the change commits or is refused
    /// every request that asks the entry for anything is refused
    /// `host/entry-held`, while `vault list` and `vault resolve` go on naming
    /// the registration as it stood. The registry file is written first —
    /// the entry's replacement is only served once the file records the
    /// edit, and a file that refuses leaves the entry that stood in service,
    /// the file and the set unchanged. **A root move retires the derived
    /// state the old root left**, under the vault's maintainer lock as
    /// `vault unregister` retires it, so nothing the old root held is served
    /// from the new one: another process holding that lock is refused
    /// `host/maintainer-contended`, and a data directory that refuses the
    /// lock or the discard is refused `host/entry-untrusted` under the
    /// environmental-refusal reason. A registry file that cannot be read or
    /// written is refused `host/registry-unwritable`. After any refusal the
    /// registration that stood before still stands, served by the entry that
    /// stood.
    ///
    /// Nothing is attached here. The edited registration is served by an
    /// entry that joins unattached, and the demand that follows attaches it
    /// under the registration as edited — its root, its schema source and
    /// its watch backend. The new entry holds nothing the old one's
    /// attachments recorded, so `vault status` reports its engine section
    /// undelivered and its advisories empty until that attach publishes them.
    pub fn vault_set(&self, params: &SetParams) -> Result<SetReport, ErrorEnvelope> {
        let name = &params.name;
        let (registration, published) = self.set(params).map_err(|refusal| refusal.answer(name))?;
        Ok(SetReport::new(registration, published.published(name)))
    }
}

/// Whether the registry is in order over the served `roots`, given ascending
/// by name: every root there, readable, and reached by one registration.
///
/// **Identity is the classification a recheck runs.** The roots are grouped by
/// the identity each resolves to through [`roots_by_identity`], and a group
/// of two or more names is one [`RegistryProblem::DuplicateRoot`] naming them
/// all, reported at its first name. A root the filesystem answers for with
/// nothing is [`RegistryProblem::RootMissing`], and one it refuses to answer
/// for is [`RegistryProblem::RootUnreadable`] carrying the refusal. A root
/// that resolves is then read the way a registration admits one — it is a
/// directory, and it lists — and a root that is not is unreadable too, so a
/// registration whose root became a file or lost its permissions is named.
///
/// The problems come in name order. The cost is one stat of every root and,
/// for each that resolves, one stat and one listing more.
pub(crate) fn sanity<'a>(
    roots: impl IntoIterator<Item = (&'a VaultName, &'a Path)>,
) -> RegistrySanity {
    let roots: Vec<(&VaultName, &Path)> = roots.into_iter().collect();
    let mut refused = BTreeMap::<VaultName, String>::new();
    let Ok(identities) = roots_by_identity(roots.iter().copied(), |name, refusal| {
        refused.insert(name.clone(), refusal.to_string());
        Ok::<(), Infallible>(())
    });
    let groups: BTreeMap<&VaultName, &BTreeSet<VaultName>> = identities
        .values()
        .flat_map(|names| names.iter().map(move |name| (name, names)))
        .collect();
    let mut problems = Vec::new();
    for (name, root) in roots {
        if let Some(detail) = refused.get(name) {
            problems.push(RegistryProblem::root_unreadable(
                name.clone(),
                detail.clone(),
            ));
            continue;
        }
        let Some(group) = groups.get(name) else {
            problems.push(RegistryProblem::root_missing(name.clone()));
            continue;
        };
        if let Err(refusal) = readable_directory(root) {
            problems.push(RegistryProblem::root_unreadable(
                name.clone(),
                refusal.to_string(),
            ));
        }
        if group.first() == Some(name)
            && let Ok(conflict) = AliasConflict::new(group.iter().cloned())
        {
            problems.push(RegistryProblem::duplicate_root(conflict.aliases().clone()));
        }
    }
    RegistrySanity::problems(problems).unwrap_or(RegistrySanity::sound())
}

/// Why a registration was not recorded in the registry file.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RecordRefusal {
    /// The file already records a registration under the name. The file is
    /// the durable record, so a registration another writer put there is not
    /// replaced.
    AlreadyRecorded,
    /// The file could not be read or written.
    Unwritable(RegistryUnwritable),
}

/// Why the registry file was not read or written: the act that failed and
/// what refused it, naming no file, for a person reading a message or a log.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RegistryUnwritable {
    detail: String,
}

impl RegistryUnwritable {
    pub fn new(detail: impl Into<String>) -> Self {
        RegistryUnwritable {
            detail: detail.into(),
        }
    }

    /// The refusal in words.
    pub fn detail(&self) -> &str {
        &self.detail
    }
}

impl fmt::Display for RegistryUnwritable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.detail)
    }
}

impl std::error::Error for RegistryUnwritable {}

/// Why a vault's registration was not retired or amended, and how far the
/// change got.
///
/// In each case the registry file still records the registration that stood.
/// Where the lock was not taken nothing ran; past it, what a refusal may have
/// taken is derived state alone. An amendment that leaves the root standing
/// takes no lock and discards nothing, so it refuses only as unrecorded.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RetireRefusal {
    /// Another process holds the vault's maintainer lock. Nothing ran.
    MaintainerContended(MaintainerIdentity),
    /// The maintainer lock could not be taken, for a reason other than another
    /// holder: the environment's account, naming no file. Nothing ran.
    Unclaimed(String),
    /// The derived state could not be discarded: the environment's account,
    /// naming no file. The registry file was not written.
    Undiscarded(String),
    /// The registry file could not be read, and nothing was discarded; or it
    /// could not be written after the discard.
    Unrecorded(RegistryUnwritable),
}

impl From<RetireRefusal> for RegistrationRefusal {
    fn from(refused: RetireRefusal) -> Self {
        match refused {
            RetireRefusal::MaintainerContended(incumbent) => {
                RegistrationRefusal::MaintainerContended(incumbent)
            }
            RetireRefusal::Unclaimed(refusal) | RetireRefusal::Undiscarded(refusal) => {
                RegistrationRefusal::StateRefused(refusal)
            }
            RetireRefusal::Unrecorded(unwritable) => {
                RegistrationRefusal::RegistryUnwritable(unwritable)
            }
        }
    }
}

/// Why a registration change left the registration that stood before it
/// standing.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum RegistrationRefusal {
    /// The serving set refused the change: the name is served already, or the
    /// entry holds something or is held.
    Serving(ServingRefusal),
    /// The entry stands on this park, and an edit does not withdraw one.
    Parked(StandingPark),
    /// The set serves no entry under the name.
    UnknownVault,
    /// The registry file already records the name.
    AlreadyRecorded,
    /// Another registration already reaches the root. Every name here reaches
    /// it, the one asked for among them.
    DuplicateRoot(AliasConflict),
    /// The root does not exist, is not a directory, or cannot be read: the
    /// registry's account of it.
    RootRefused(String),
    /// Another process maintains the vault's derived state.
    MaintainerContended(MaintainerIdentity),
    /// The data directory refused the maintainer lock or the discard: the
    /// environment's account, naming no file.
    StateRefused(String),
    /// The registry file was not read or not written.
    RegistryUnwritable(RegistryUnwritable),
}

/// `registration` as admitted: its root at its [`canonical_root`], with the
/// identity [`readable_root`] reads for it.
pub(crate) fn admitted(mut registration: Entry) -> Result<(Entry, Identity), RegistrationRefusal> {
    registration.root = canonical_root(&registration.root)?;
    let identity = readable_root(&registration.root)?;
    Ok((registration, identity))
}

/// `root` at its [`canonical_spelling`]: a spelling that resolves through a
/// link is taken as the directory the link reaches, so no root taken here has
/// a link as its last component.
pub(crate) fn canonical_root(root: &VaultRoot) -> Result<VaultRoot, RegistrationRefusal> {
    VaultRoot::new(canonical_spelling(root.as_path()))
        .map_err(|illegal| RegistrationRefusal::RootRefused(illegal.to_string()))
}

/// The identity of the directory `root` names, where it is a readable one.
///
/// **Only a readable directory is admitted.** A root is read by listing it, so
/// nothing at the spelling, something other than a directory, and a directory
/// this process may not list are each refused with the environment's account.
pub(crate) fn readable_root(root: &VaultRoot) -> Result<Identity, RegistrationRefusal> {
    readable_directory(root.as_path())
        .map_err(|refusal| RegistrationRefusal::RootRefused(refusal.to_string()))
}

/// Every name among the served `roots` whose root reaches `identity`.
///
/// One stat per root. A root the filesystem answers for with nothing, or
/// refuses to answer for, reaches nothing.
pub(crate) fn reaching<'a>(
    roots: impl IntoIterator<Item = (&'a VaultName, &'a Path)>,
    identity: Identity,
) -> BTreeSet<VaultName> {
    let Ok(mut identities) = roots_by_identity(roots, |_, _| Ok::<(), Infallible>(()));
    identities.remove(&identity).unwrap_or_default()
}

/// Why a resolution names no one registration.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ResolveRefusal {
    /// The most specific root containing the directory is reached by more
    /// than one registration, and every name here reaches it. Neither root is
    /// under the other, so neither is more specific.
    AmbiguousRoot(AliasConflict),
}

/// The registration among `registrations` whose root most specifically
/// contains `directory`, none where no root contains it, or the refusal where
/// that root is reached by more than one of them.
///
/// The registrations are keyed by name, so each name is one candidate and the
/// names a refusal gathers are distinct.
///
/// **Containment is judged on filesystem identity**, the one notion by which
/// the host decides two roots are one: a root contains the directory when the
/// directory's resolved spelling, or one of its ancestors, is the directory
/// that root resolves to. The directory is resolved through
/// [`canonical_spelling`] first, so a `..` or a link in it is taken by the
/// filesystem and every ancestor walked is the directory it names; the
/// deepest ancestor that is some root is the most specific root, and a
/// sibling whose name merely begins with a root's name is a different
/// directory. A root spelled through a link, and two spellings of one root,
/// are judged as the directory they reach, which is also what makes two
/// registrations over one root a conflict here exactly as they are one at an
/// attach.
///
/// A directory that does not exist is judged on the spelling
/// [`canonical_spelling`] gives it: its components past what resolves have no
/// identity and contain nothing, and the ancestors that do resolve are walked
/// like any other. A root or an ancestor the filesystem answers for with
/// nothing, or refuses to answer for, is no root here, which is how a recheck
/// classifies a root too.
///
/// The cost is one stat per registered root, one resolution of the directory,
/// and one stat per ancestor walked until one is a root — none of them taken
/// under the serving set's lock, and none where nothing is registered.
pub(crate) fn containing(
    registrations: &BTreeMap<VaultName, Entry>,
    directory: &Path,
) -> Result<Option<Entry>, ResolveRefusal> {
    let Ok(mut roots) = roots_by_identity(
        registrations
            .iter()
            .map(|(name, registration)| (name, registration.root.as_path())),
        |_, _| Ok::<(), Infallible>(()),
    );
    if roots.is_empty() {
        return Ok(None);
    }
    let directory = canonical_spelling(directory);
    for ancestor in directory.ancestors() {
        let Ok(Some(identity)) = path_identity(ancestor) else {
            continue;
        };
        let Some(reaching) = roots.remove(&identity) else {
            continue;
        };
        return match AliasConflict::new(reaching.iter().cloned()) {
            Ok(conflict) => Err(ResolveRefusal::AmbiguousRoot(conflict)),
            // One name reaches the root, so its registration is the answer.
            Err(_) => Ok(reaching
                .first()
                .and_then(|name| registrations.get(name))
                .cloned()),
        };
    }
    Ok(None)
}

/// The served `roots` grouped by the filesystem identity each resolves to,
/// with every name whose root reaches that identity: one stat per root.
///
/// A root the filesystem answers for with nothing is registrable rather than
/// resolved and joins no group. A root the filesystem refuses to answer for is
/// handed to `refused` with its name, which either passes over it — the root
/// joins no group — or ends the grouping with the error it returns.
fn roots_by_identity<'a, E>(
    roots: impl IntoIterator<Item = (&'a VaultName, &'a Path)>,
    mut refused: impl FnMut(&VaultName, Refusal) -> Result<(), E>,
) -> Result<BTreeMap<Identity, BTreeSet<VaultName>>, E> {
    let mut identities = BTreeMap::<Identity, BTreeSet<VaultName>>::new();
    for (name, root) in roots {
        match path_identity(root) {
            Ok(Some(identity)) => {
                identities.entry(identity).or_default().insert(name.clone());
            }
            Ok(None) => {}
            Err(refusal) => refused(name, refusal)?,
        }
    }
    Ok(identities)
}

/// Classify every root the host serves against the others, answering for
/// `requested` alone: the identity its root resolved to, and the conflict it
/// stands in where more than one served name reaches that root.
///
/// A refusal over `requested`'s own root is this read's refusal; a refusal over
/// any other root belongs to that name and is left for the read that asks about
/// it. A root the filesystem answers for with nothing is registrable rather
/// than resolved, and classifies against nothing.
pub(crate) fn recheck<'a>(
    roots: impl IntoIterator<Item = (&'a VaultName, &'a Path)>,
    requested: &VaultName,
) -> Result<RootReading, Refusal> {
    let identities = roots_by_identity(roots, |name, refusal| {
        if name == requested {
            Err(refusal)
        } else {
            Ok(())
        }
    })?;
    let resolved = identities
        .iter()
        .find_map(|(identity, names)| names.contains(requested).then_some(*identity));
    Ok(RootReading {
        identity: resolved,
        conflict: conflicts_from_identities(identities).remove(requested),
    })
}

fn conflicts_from_identities(
    identities: BTreeMap<Identity, BTreeSet<VaultName>>,
) -> BTreeMap<VaultName, AliasConflict> {
    let mut conflicts = BTreeMap::new();
    // A root one registration reaches is no conflict, and the floor the
    // conflict keeps is what says so: the names an identity gathered either
    // make a conflict or there was never one to record.
    for aliases in identities.into_values() {
        let Ok(conflict) = AliasConflict::new(aliases.iter().cloned()) else {
            continue;
        };
        for alias in aliases {
            conflicts.insert(alias, conflict.clone());
        }
    }
    conflicts
}

#[cfg(test)]
#[allow(clippy::disallowed_methods)] // fixtures impersonate external filesystem retargets.
mod tests {
    use super::*;
    use norn_config::registry::VaultRoot;
    use norn_testkit::scratch::Scratch;

    fn entry(name: &str, root: &str) -> Entry {
        Entry::new(VaultName::new(name).unwrap(), VaultRoot::new(root).unwrap())
    }

    /// Classify `entries` as a host serving exactly those roots would.
    fn recheck_over(entries: &[Entry], requested: &VaultName) -> Result<RootReading, Refusal> {
        recheck(
            entries
                .iter()
                .map(|entry| (&entry.name, entry.root.as_path())),
            requested,
        )
    }

    /// A tree of one case's own: directories under one scratch root, created
    /// as the case names them.
    struct Tree(Scratch);

    impl Tree {
        fn new(label: &str) -> Self {
            Tree(Scratch::new(&format!("norn-host-resolve-{label}")))
        }

        /// `relative` under the tree, created as a directory.
        fn dir(&self, relative: &str) -> std::path::PathBuf {
            let path = self.0.join(relative);
            std::fs::create_dir_all(&path).unwrap();
            path
        }

        /// `relative` under the tree, created as a link to `target`.
        fn link(&self, relative: &str, target: &Path) -> std::path::PathBuf {
            let path = self.0.join(relative);
            std::os::unix::fs::symlink(target, &path).unwrap();
            path
        }

        /// `relative` under the tree, created as nothing.
        fn path(&self, relative: &str) -> std::path::PathBuf {
            self.0.join(relative)
        }
    }

    fn served(name: &str, root: &Path) -> Entry {
        Entry::new(VaultName::new(name).unwrap(), VaultRoot::new(root).unwrap())
    }

    /// `registrations` keyed by name, as the serving set holds them.
    fn by_name(registrations: &[Entry]) -> BTreeMap<VaultName, Entry> {
        registrations
            .iter()
            .map(|registration| (registration.name.clone(), registration.clone()))
            .collect()
    }

    /// The name of the one registration `directory` resolves to among
    /// `registrations`, or nothing.
    fn resolved(registrations: &[Entry], directory: &Path) -> Option<String> {
        containing(&by_name(registrations), directory)
            .expect("the resolution names one registration or none")
            .map(|registration| registration.name.to_string())
    }

    /// A directory under one registered root resolves to that registration
    /// and to no other.
    #[test]
    fn a_directory_resolves_to_the_registration_whose_root_contains_it() {
        let tree = Tree::new("contained");
        let registrations = [
            served("notes", &tree.dir("notes")),
            served("work", &tree.dir("work")),
        ];
        assert_eq!(
            resolved(&registrations, &tree.dir("notes/journal/2026")),
            Some("notes".to_owned())
        );
    }

    /// Where registered roots nest, the most specific root containing the
    /// directory answers, and the outer one still answers for the rest of
    /// its tree.
    #[test]
    fn nested_roots_resolve_to_the_most_specific_one() {
        let tree = Tree::new("nested");
        let registrations = [
            served("outer", &tree.dir("vault")),
            served("inner", &tree.dir("vault/inner")),
        ];
        assert_eq!(
            resolved(&registrations, &tree.dir("vault/inner/deep")),
            Some("inner".to_owned())
        );
        assert_eq!(
            resolved(&registrations, &tree.dir("vault/beside")),
            Some("outer".to_owned())
        );
    }

    /// A root contains itself: the directory a registration is rooted at
    /// resolves to that registration rather than to one around it.
    #[test]
    fn a_directory_that_is_a_root_resolves_to_its_registration() {
        let tree = Tree::new("itself");
        let registrations = [
            served("outer", &tree.dir("vault")),
            served("inner", &tree.dir("vault/inner")),
        ];
        assert_eq!(
            resolved(&registrations, &tree.path("vault/inner")),
            Some("inner".to_owned())
        );
    }

    /// Containment is by whole components: a sibling whose name begins with
    /// a root's name is not under that root.
    #[test]
    fn a_root_does_not_contain_a_sibling_that_shares_its_prefix() {
        let tree = Tree::new("prefix");
        let registrations = [served("a", &tree.dir("v/a"))];
        assert_eq!(resolved(&registrations, &tree.dir("v/ab")), None);
        assert_eq!(resolved(&registrations, &tree.dir("v/ab/deeper")), None);
    }

    /// A directory under no registered root resolves to none, which is an
    /// answer rather than a refusal.
    #[test]
    fn a_directory_no_root_contains_resolves_to_none() {
        let tree = Tree::new("uncontained");
        let registrations = [served("notes", &tree.dir("notes"))];
        assert_eq!(resolved(&registrations, &tree.dir("elsewhere")), None);
    }

    /// Two registrations whose roots are one directory name no one vault for
    /// a directory under it, so the resolution is refused naming both.
    #[test]
    fn two_registrations_over_one_root_refuse_naming_both() {
        let tree = Tree::new("ambiguous");
        let shared = tree.dir("shared");
        let registrations = [
            served("beta", &tree.link("alias", &shared)),
            served("alpha", &shared),
        ];
        assert_eq!(
            containing(&by_name(&registrations), &tree.dir("shared/sub")),
            Err(ResolveRefusal::AmbiguousRoot(
                AliasConflict::new([
                    VaultName::new("alpha").unwrap(),
                    VaultName::new("beta").unwrap(),
                ])
                .unwrap()
            ))
        );
    }

    /// An alias pair is ambiguous only where it is the most specific root:
    /// a directory under a single registration nested inside the pair
    /// resolves to that registration.
    #[test]
    fn an_alias_pair_around_a_more_specific_root_does_not_refuse() {
        let tree = Tree::new("ambiguous-outer");
        let shared = tree.dir("shared");
        let registrations = [
            served("alpha", &shared),
            served("beta", &tree.link("alias", &shared)),
            served("inner", &tree.dir("shared/inner")),
        ];
        assert_eq!(
            resolved(&registrations, &tree.dir("shared/inner/deep")),
            Some("inner".to_owned())
        );
    }

    /// A directory reached through a link resolves to the registration of
    /// the directory the link reaches.
    #[test]
    fn a_directory_reached_through_a_link_resolves_to_its_targets_registration() {
        let tree = Tree::new("linked-directory");
        let root = tree.dir("vault");
        tree.dir("vault/sub");
        let registrations = [served("notes", &root)];
        let link = tree.link("shortcut", &root);
        assert_eq!(
            resolved(&registrations, &link.join("sub")),
            Some("notes".to_owned())
        );
    }

    /// A `..` after a link steps out of the directory the link reaches, so a
    /// directory spelled through a root's link and back out of it is not
    /// under that root.
    #[test]
    fn a_parent_step_after_a_link_leaves_the_links_target() {
        let tree = Tree::new("linked-parent");
        let root = tree.dir("elsewhere/vault");
        let registrations = [served("notes", &root)];
        let link = tree.link("shortcut", &root);
        assert_eq!(resolved(&registrations, &link.join("..")), None);
    }

    /// A root spelled through a link contains the directory the link reaches,
    /// so a directory asked about by its own spelling resolves to it.
    #[test]
    fn a_root_spelled_through_a_link_contains_the_directory_it_reaches() {
        let tree = Tree::new("linked-root");
        let target = tree.dir("target");
        let registrations = [served("notes", &tree.link("root-link", &target))];
        assert_eq!(
            resolved(&registrations, &tree.dir("target/sub")),
            Some("notes".to_owned())
        );
    }

    /// A directory that does not exist is judged on its spelling beneath what
    /// the filesystem resolves, links included, rather than refused.
    #[test]
    fn a_directory_that_does_not_exist_is_judged_on_its_spelling() {
        let tree = Tree::new("absent");
        let root = tree.dir("vault");
        let registrations = [served("notes", &root)];
        let link = tree.link("shortcut", &root);
        assert_eq!(
            resolved(&registrations, &tree.path("vault/not/yet")),
            Some("notes".to_owned())
        );
        assert_eq!(
            resolved(&registrations, &link.join("not-yet")),
            Some("notes".to_owned())
        );
        assert_eq!(resolved(&registrations, &tree.path("nowhere/yet")), None);
    }

    /// A `..` past what exists steps back into a root, and a link named
    /// after it is judged as the directory it reaches: into another root, or
    /// out of every root.
    #[test]
    fn a_link_after_a_parent_step_past_what_exists_is_judged_as_its_target() {
        let tree = Tree::new("absent-parent-link");
        let root = tree.dir("v/root");
        let other = tree.dir("other");
        tree.dir("other/r2/x");
        tree.link("v/root/sub", &tree.path("other/r2"));
        tree.link("v/root/out", &tree.dir("elsewhere"));
        let registrations = [served("root", &root), served("other", &other)];
        assert_eq!(
            resolved(&registrations, &root.join("notyet/../sub/x")),
            Some("other".to_owned())
        );
        assert_eq!(resolved(&registrations, &root.join("notyet/../out")), None);
    }

    /// An ancestor the filesystem refuses to stat — a name beneath a file, a
    /// link that loops — is no root, and the walk goes on to the root above
    /// it.
    #[test]
    fn an_ancestor_the_filesystem_refuses_is_passed_over() {
        let tree = Tree::new("refused-ancestor");
        let root = tree.dir("v");
        std::fs::write(tree.path("v/file.md"), b"").unwrap();
        tree.link("v/loop", &tree.path("v/loop"));
        let registrations = [served("notes", &root)];
        assert!(path_identity(&tree.path("v/file.md/sub")).is_err());
        assert!(path_identity(&tree.path("v/loop")).is_err());
        assert_eq!(
            resolved(&registrations, &tree.path("v/file.md/sub")),
            Some("notes".to_owned())
        );
        assert_eq!(
            resolved(&registrations, &tree.path("v/loop/sub")),
            Some("notes".to_owned())
        );
    }

    /// A registered root the filesystem refuses to stat contains nothing, and
    /// the other roots still answer.
    #[test]
    fn a_root_the_filesystem_refuses_contains_nothing_and_the_rest_answer() {
        let tree = Tree::new("refused-root");
        let root = tree.dir("vault");
        let looping = tree.link("loop", &tree.path("loop"));
        assert!(path_identity(&looping).is_err());
        let registrations = [served("broken", &looping), served("notes", &root)];
        assert_eq!(
            resolved(&registrations, &tree.dir("vault/sub")),
            Some("notes".to_owned())
        );
        assert_eq!(resolved(&registrations, &tree.path("loop/sub")), None);
    }

    /// The registry's sanity over `registrations`, as a doctor reads it.
    fn sanity_over(registrations: &[Entry]) -> RegistrySanity {
        sanity(
            registrations
                .iter()
                .map(|entry| (&entry.name, entry.root.as_path())),
        )
    }

    fn name(name: &str) -> VaultName {
        VaultName::new(name).unwrap()
    }

    /// **Registrations each over a root of their own, every root there and
    /// readable, are a sound registry**, and a registry of none is sound too.
    #[test]
    fn roots_of_their_own_that_are_there_and_readable_are_sound() {
        let tree = Tree::new("sanity-sound");
        let registrations = [
            served("notes", &tree.dir("notes")),
            served("work", &tree.dir("work")),
        ];
        assert_eq!(sanity_over(&registrations), RegistrySanity::sound());
        assert_eq!(sanity_over(&[]), RegistrySanity::sound());
    }

    /// **Registrations reaching one root are one problem naming them all**,
    /// however each spells the root.
    #[cfg(unix)]
    #[test]
    fn registrations_reaching_one_root_are_a_duplicate_naming_them_all() {
        let tree = Tree::new("sanity-duplicate");
        let shared = tree.dir("shared");
        let registrations = [
            served("alpha", &shared),
            served("beta", &tree.link("alias", &shared)),
            served("gamma", &tree.dir("gamma")),
        ];
        assert_eq!(
            sanity_over(&registrations),
            RegistrySanity::problems([RegistryProblem::duplicate_root(
                NameSet::new([name("alpha"), name("beta")]).unwrap()
            )])
            .unwrap()
        );
    }

    /// **A root that is not there is missing**, and the registrations beside
    /// it are judged on their own roots.
    #[test]
    fn a_root_that_is_not_there_is_missing() {
        let tree = Tree::new("sanity-missing");
        let registrations = [
            served("gone", &tree.path("gone")),
            served("notes", &tree.dir("notes")),
        ];
        assert_eq!(
            sanity_over(&registrations),
            RegistrySanity::problems([RegistryProblem::root_missing(name("gone"))]).unwrap()
        );
    }

    /// **A root the filesystem refuses to answer for, and a root that is
    /// there and is no directory a vault is read from, are unreadable**, each
    /// carrying what refused.
    #[cfg(unix)]
    #[test]
    fn a_root_that_refuses_or_is_no_directory_is_unreadable() {
        let tree = Tree::new("sanity-unreadable");
        let looping = tree.link("loop", &tree.path("loop"));
        let file = tree.path("file");
        std::fs::write(&file, b"").unwrap();
        let registrations = [served("file", &file), served("looping", &looping)];

        let RegistrySanity::Problems { problems, .. } = sanity_over(&registrations) else {
            panic!("a registry over a file and a loop is sound");
        };
        let unreadable: Vec<&VaultName> = problems
            .iter()
            .map(|problem| match problem {
                RegistryProblem::RootUnreadable { name, detail, .. } if !detail.is_empty() => name,
                other => panic!("{other:?}"),
            })
            .collect();
        assert_eq!(unreadable, [&name("file"), &name("looping")]);
    }

    /// A conflict is between at least two registrations, and the floor is the
    /// name set's: a caller that collected one name or none has no conflict to
    /// raise and is told so at construction, so no later reader holds a
    /// duplicate-root refusal that names one vault.
    #[test]
    fn a_conflict_refuses_a_root_only_one_registration_reaches() {
        let alpha = VaultName::new("alpha").unwrap();
        assert!(AliasConflict::new([]).is_err());
        assert!(AliasConflict::new([alpha.clone()]).is_err());
        assert!(AliasConflict::new([alpha.clone(), alpha.clone()]).is_err());
        let beta = VaultName::new("beta").unwrap();
        assert_eq!(
            AliasConflict::new([beta.clone(), alpha.clone()])
                .expect("two distinct registrations")
                .aliases()
                .names(),
            [alpha, beta]
        );
    }

    /// A root only one registration reaches is recorded as no conflict at all,
    /// which is the same floor read off the registry rather than off a caller.
    #[test]
    fn a_root_one_registration_reaches_records_no_conflict() {
        let entries = [entry("alpha", "/tmp")];
        assert!(
            recheck_over(&entries, &VaultName::new("alpha").unwrap())
                .unwrap()
                .conflict
                .is_none()
        );
    }

    #[test]
    fn every_alias_is_refused_and_names_the_whole_conflict() {
        let entries = [entry("alpha", "/tmp"), entry("beta", "/tmp/.")];
        let expected = vec![
            VaultName::new("alpha").unwrap(),
            VaultName::new("beta").unwrap(),
        ];
        for requested in &expected {
            assert_eq!(
                recheck_over(&entries, requested)
                    .unwrap()
                    .conflict
                    .unwrap()
                    .aliases()
                    .names(),
                expected
            );
        }
    }

    #[test]
    fn a_missing_root_stays_registrable() {
        let entries = [entry("later", "/tmp/norn-host-root-that-does-not-exist")];
        let reading = recheck_over(&entries, &VaultName::new("later").unwrap()).unwrap();
        assert!(reading.conflict.is_none());
        assert!(reading.identity.is_none());
    }

    /// A recheck classifies every served root, and the identity it answers
    /// with is the requested name's own. The other roots are read for the
    /// conflict alone: a reading that carried one of them would file the
    /// acquisition claim under a root the caller never asked about.
    #[test]
    fn a_recheck_resolves_the_requested_root_among_several() {
        let scratch = Scratch::new("norn-host-registry-requested-identity");
        let base = scratch.root();
        let alpha_root = base.join("alpha");
        let beta_root = base.join("beta");
        std::fs::create_dir_all(&alpha_root).unwrap();
        std::fs::create_dir_all(&beta_root).unwrap();
        let alpha = VaultName::new("alpha").unwrap();
        let beta = VaultName::new("beta").unwrap();
        let entries = [
            Entry::new(alpha.clone(), VaultRoot::new(&alpha_root).unwrap()),
            Entry::new(beta.clone(), VaultRoot::new(&beta_root).unwrap()),
        ];

        let alpha_identity = path_identity(&alpha_root).unwrap();
        let beta_identity = path_identity(&beta_root).unwrap();
        assert!(
            alpha_identity.is_some() && alpha_identity != beta_identity,
            "the two roots are one root, so nothing here can tell them apart"
        );

        for (requested, expected) in [(&alpha, alpha_identity), (&beta, beta_identity)] {
            assert_eq!(
                recheck_over(&entries, requested).unwrap().identity,
                expected,
                "the recheck over {requested:?} resolved another registered root"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn recheck_of_a_healthy_entry_ignores_an_unrelated_identity_refusal() {
        use std::os::unix::fs::symlink;

        let scratch = Scratch::new("norn-host-registry-refusal-isolation");
        let base = scratch.root();
        let healthy = base.join("healthy");
        let refused = base.join("refused");
        std::fs::create_dir_all(&healthy).unwrap();
        std::fs::create_dir_all(&refused).unwrap();
        let entries = [
            Entry::new(
                VaultName::new("healthy").unwrap(),
                VaultRoot::new(&healthy).unwrap(),
            ),
            Entry::new(
                VaultName::new("refused").unwrap(),
                VaultRoot::new(&refused).unwrap(),
            ),
        ];
        std::fs::remove_dir(&refused).unwrap();
        symlink("refused", &refused).unwrap();

        let healthy_reading = recheck_over(&entries, &VaultName::new("healthy").unwrap())
            .expect("the healthy root reads");
        assert!(healthy_reading.conflict.is_none());
        assert!(
            healthy_reading.identity.is_some(),
            "a recheck that passed resolved the root it classified"
        );
        assert!(recheck_over(&entries, &VaultName::new("refused").unwrap()).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn a_recheck_keeps_healthy_entries_serviceable_when_another_identity_refuses() {
        use std::os::unix::fs::symlink;

        let scratch = Scratch::new("norn-host-registry-startup-refusal");
        let base = scratch.root();
        let healthy = base.join("healthy");
        let healthy_alias = healthy.join(".");
        let refused = base.join("refused");
        std::fs::create_dir_all(&healthy).unwrap();
        std::fs::create_dir_all(base).unwrap();
        symlink("refused", &refused).unwrap();
        let healthy_name = VaultName::new("healthy").unwrap();
        let alias_name = VaultName::new("healthy-alias").unwrap();
        let refused_name = VaultName::new("refused").unwrap();

        let entries = [
            Entry::new(healthy_name.clone(), VaultRoot::new(&healthy).unwrap()),
            Entry::new(alias_name.clone(), VaultRoot::new(&healthy_alias).unwrap()),
            Entry::new(refused_name.clone(), VaultRoot::new(&refused).unwrap()),
        ];
        assert_eq!(
            recheck_over(&entries, &healthy_name)
                .expect("an unrelated refusal must not reach the healthy root")
                .conflict
                .unwrap()
                .aliases()
                .names(),
            vec![healthy_name, alias_name]
        );
        assert!(recheck_over(&entries, &refused_name).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn recheck_keeps_global_duplicate_classification_while_isolating_a_refusal() {
        use std::os::unix::fs::symlink;

        let scratch = Scratch::new("norn-host-registry-global-aliases");
        let base = scratch.root();
        let shared = base.join("shared");
        let alias = base.join("alias");
        let refused = base.join("refused");
        std::fs::create_dir_all(&shared).unwrap();
        std::fs::create_dir_all(&alias).unwrap();
        std::fs::create_dir_all(&refused).unwrap();
        let alpha = VaultName::new("alpha").unwrap();
        let beta = VaultName::new("beta").unwrap();
        let entries = [
            Entry::new(alpha.clone(), VaultRoot::new(&shared).unwrap()),
            Entry::new(beta.clone(), VaultRoot::new(&alias).unwrap()),
            Entry::new(
                VaultName::new("refused").unwrap(),
                VaultRoot::new(&refused).unwrap(),
            ),
        ];
        std::fs::remove_dir(&alias).unwrap();
        symlink(&shared, &alias).unwrap();
        std::fs::remove_dir(&refused).unwrap();
        symlink("refused", &refused).unwrap();

        assert_eq!(
            recheck_over(&entries, &alpha)
                .unwrap()
                .conflict
                .unwrap()
                .aliases()
                .names(),
            vec![alpha, beta]
        );
    }
}
