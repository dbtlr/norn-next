use std::collections::BTreeSet;
use std::fmt;
use std::iter::Peekable;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use norn_config::registry::{Entry as Registration, PollBackend};
use norn_config::{ConfigDirs, IN_VAULT_SCHEMA_PATH, VaultName};
use norn_fs::{
    Acquisition, Maintainership, MaintainershipKey, OwnWrites, Placement, RescanScope, ShadowHome,
    Subscription, WatchError, try_acquire, walk, walk_subtree, watch, watch_polling,
};
use norn_store::{
    BlockFact, Change, DirectoryPrefix, DocumentFacts, DocumentPath, FindingFacts,
    FrontmatterValue, HeadingFact, IncrementProvenance, LinkFact, LinkFamily, Provenance,
    SchemaPin, Span, Store, StoreError, StoredDocument, StoredPathOrder, TagFact, TagSource,
};
use norn_text::{Document, SourceSpan, Value};
use norn_wire::{FindingKind, MaintainerIdentity, Severity};

use crate::{EntryOps, Healing, JobFailure, ProgressReporter, ReconcileWork, SnapshotSource};

/// Maximum number of document changes materialized for one store transaction.
pub const MAX_CHANGESET_SIZE: usize = 1024;
/// How long an attachment goes between verifications of its own derived state.
///
/// An authored bound rather than a measured one. `PRAGMA integrity_check` reads
/// the whole database, so the cadence is what keeps that a background cost
/// rather than a recurring one: it runs on the bounded lifecycle worker pool,
/// never on the request path, and an entry pays it once an hour whatever its
/// query traffic. Shorter would spend a full scan to shorten a window nothing
/// is racing; longer would leave silent damage standing across a working day.
///
/// **What the interval is traded against.** The maintenance leg holds the
/// entry's coverage while it runs, and an entry whose coverage is out with a
/// leg drains no watcher batch, so the scan is time the subscription's queue
/// fills unattended. On a store large enough for one scan to outlast that
/// queue, the overflow and the full rescan answering it are the accepted cost
/// of asking the question at all: silent damage that nothing else meets is
/// worse than a re-read of a vault that is sound. The scan's cost grows with
/// the database, so a corpus that makes the trade unaffordable is a reason to
/// measure the scan and move this bound, not to skip it.
pub const STORE_VERIFICATION_INTERVAL: Duration = Duration::from_secs(60 * 60);

/// Maximum time one lifecycle worker may wait for watcher coverage to become live.
///
/// This is operational containment, not a performance threshold. Loaded runs
/// measure its headroom; observations do not tune it automatically.
pub const WATCH_SYNCHRONIZATION_DEADLINE: Duration = Duration::from_secs(15);

/// Resource bounds for the concrete filesystem/store adapter.
#[derive(Clone, Copy, Debug)]
pub struct ProductionPolicy {
    /// Maximum stored rows read at once during an ordered heal merge.
    pub store_page_size: usize,
    /// Maximum changes committed by one increment transaction.
    pub changeset_size: usize,
}

impl ProductionPolicy {
    pub fn new(
        store_page_size: usize,
        changeset_size: usize,
    ) -> Result<Self, ProductionPolicyError> {
        if store_page_size == 0 || store_page_size > norn_store::MAX_STORED_DOCUMENT_PAGE {
            return Err(ProductionPolicyError::StorePageSize(store_page_size));
        }
        if changeset_size == 0 || changeset_size > MAX_CHANGESET_SIZE {
            return Err(ProductionPolicyError::ChangesetSize(changeset_size));
        }
        Ok(Self {
            store_page_size,
            changeset_size,
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProductionPolicyError {
    StorePageSize(usize),
    ChangesetSize(usize),
}

impl fmt::Display for ProductionPolicyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::StorePageSize(given) => write!(
                f,
                "store page size must be between 1 and {}, got {given}",
                norn_store::MAX_STORED_DOCUMENT_PAGE
            ),
            Self::ChangesetSize(given) => write!(
                f,
                "changeset size must be between 1 and {MAX_CHANGESET_SIZE}, got {given}"
            ),
        }
    }
}

impl std::error::Error for ProductionPolicyError {}

/// The production implementation of the lifecycle effect seam.
///
/// It keeps no account of the serving set. Every registration it acts on
/// arrives with the call that acts on it — an attach is handed the entry's own
/// registration, and what the attachment keeps from it is what the reconciles
/// and recoveries that follow read. Nothing here can name a vault the host does
/// not serve, or place a served vault at a root the host does not.
pub struct ProductionEntryOps {
    dirs: ConfigDirs,
    policy: ProductionPolicy,
}

pub struct ProductionAttachment {
    /// The registration this attachment was established from: the root it
    /// derives, and where its schema is read.
    registration: Registration,
    subscription: Option<Subscription>,
    store: Store,
    heal_observed: norn_fs::Batch,
    /// Layer 4 plan-apply consumes this recorder at the product composition
    /// site: successful writes stay beside coverage so their watcher echoes
    /// can be hash-confirmed without hiding external edits.
    _own_writes: OwnWrites,
    _shadows: ShadowHome,
    last_shadow_sweep: Instant,
    /// When the store is next asked to answer for its own consistency.
    ///
    /// Logical damage is silent: a full-text index that stopped agreeing with
    /// the column it indexes, a value outside a closed vocabulary, a foreign key
    /// pointing at a row that is gone. None of it fails a read that does not
    /// touch it, so nothing meets it until a client's query does — which is why
    /// the question is asked on a schedule instead.
    ///
    /// The deadline is carried rather than the last answer, so the one clock
    /// reading says both whether the question is due and, once it is answered,
    /// when it is due again.
    store_verification_due: Instant,
    /// The maintainer lock, declared last because fields drop in declaration
    /// order: an attachment dropped rather than released gives its resources
    /// back in the order [`release`] gives them back, so the lock never ends
    /// while this process's watch over the vault still stands.
    maintainership: Maintainership,
}

type WatchEntrypoint = fn(&Path, &Path) -> Result<(Subscription, OwnWrites), WatchError>;

impl SnapshotSource for ProductionAttachment {
    /// The store's own read-only handle. `norn-store` is where a connection is
    /// opened, so the handle a vault's reads run on is the store's type and
    /// never one composed out here.
    type Reader = norn_store::SnapshotReader;

    /// [`norn_store::SnapshotReader`] is uninhabited, so an attachment holding
    /// a live store mints no reader and the entry beside it serves no reads.
    /// The connection this answers with arrives with the store's read
    /// builders; what stands here is the seam it arrives through.
    fn open_reader(&self) -> Option<Self::Reader> {
        None
    }
}

impl ProductionEntryOps {
    pub fn new(dirs: ConfigDirs, policy: ProductionPolicy) -> Self {
        Self { dirs, policy }
    }

    fn derived(&self, name: &VaultName) -> PathBuf {
        self.dirs.derived_dir(name)
    }

    /// Resolve the registry's single backend selection directly to the fs
    /// effect entrypoint. Named function pointers make this composition route
    /// independently assertable without exposing backend introspection.
    fn watch_entrypoint(registration: &Registration) -> WatchEntrypoint {
        match registration.poll_backend {
            Some(PollBackend::Poll) => watch_polling,
            None => watch,
        }
    }

    fn start_watch(
        registration: &Registration,
        schema: &Path,
    ) -> Result<(Subscription, OwnWrites), WatchError> {
        Self::watch_entrypoint(registration)(registration.root.as_path(), schema)
    }

    fn schema_path(registration: &Registration) -> PathBuf {
        registration
            .schema_source
            .as_ref()
            .map(|s| s.as_path().to_owned())
            .unwrap_or_else(|| registration.root.as_path().join(IN_VAULT_SCHEMA_PATH))
    }

    /// The directory a schema read is anchored at, and the name it reaches
    /// below it.
    ///
    /// One rule decides both modes: **what an operator wrote down is resolved as
    /// written, and what norn appends to it is contained.** A configured source
    /// is a whole path an operator chose, so the directory holding it is the
    /// anchor and only the file's own name is refused as a link. The default is
    /// the vault root — which the operator wrote down — plus the two names norn
    /// appends, so `.norn` and `schema.yaml` are both read through the vault's
    /// own directories or not at all.
    ///
    /// That makes a link at either of those two names an attach that refuses,
    /// naming the component. It is the same statement the read seam makes about
    /// a document: the schema is the file at the name inside the vault, never
    /// what something in the vault points at. An operator sharing one schema
    /// across vaults says so in the registry, where the path is theirs and is
    /// read as spelled.
    fn schema_anchor(registration: &Registration) -> Result<(PathBuf, PathBuf), JobFailure> {
        let Some(source) = registration.schema_source.as_ref() else {
            return Ok((
                registration.root.as_path().to_owned(),
                PathBuf::from(IN_VAULT_SCHEMA_PATH),
            ));
        };
        let source = source.as_path();
        let (Some(directory), Some(name)) = (source.parent(), source.file_name()) else {
            return Err(environmental(format!(
                "schema source names no file: {}",
                source.display()
            )));
        };
        Ok((directory.to_owned(), PathBuf::from(name)))
    }

    /// Re-read the vault schema and pin what it says.
    ///
    /// The answer carries whether the pin moved the fingerprint, which is the
    /// fact a caller re-derives on: a pin that moved discarded every finding
    /// keyed by the fingerprint it replaced, and only a re-derivation of the
    /// schema-keyed tables records what holds under the new one.
    fn pin_schema(store: &mut Store, registration: &Registration) -> Result<SchemaPin, JobFailure> {
        let (anchor, name) = Self::schema_anchor(registration)?;
        let observed = norn_fs::read_and_hash(&anchor, &name).map_err(effect)?;
        std::str::from_utf8(observed.bytes())
            .map_err(|e| environmental(format!("schema is not UTF-8: {e}")))?;
        store
            .begin_request()
            .pin_vault_schema(observed.bytes(), &observed.content_hash().to_string())
            .map_err(store_effect)
    }

    fn heal(
        &self,
        attachment: &mut ProductionAttachment,
        progress: &ProgressReporter<ProductionAttachment>,
    ) -> Result<(), JobFailure> {
        if !attachment.maintainership.still_current().map_err(effect)? {
            return Err(JobFailure::LostMaintainership);
        }
        // Coverage is established by the time a heal runs, and what follows is
        // counted document work. Entering the phase is what yields the handle
        // that work counts through, so the three callers never have to
        // remember to announce it.
        let healing = progress.healing();
        let exclusions = exclusions(&attachment.registration, &attachment._shadows);
        // Pinning first is what makes this the re-derivation leg of a schema
        // change: every finding the walk below records carries the fingerprint
        // the pin just installed. What the pin discarded stands again because a
        // finding only ever sits where no document row does — `Pending::flush`
        // refuses one at a path that has a row — and the walk reads every such
        // path whatever its bytes say. The rest of the walk is hash-gated: a
        // path whose document row stands is re-derived only when its content
        // hash moved, and no finding sits at such a path.
        Self::pin_schema(&mut attachment.store, &attachment.registration)?;
        heal_documents(
            &mut attachment.store,
            attachment.registration.root.as_path(),
            &exclusions,
            self.policy,
            &healing,
        )
    }

    /// Establish the two proofs required before lifecycle publication.
    ///
    /// Attach and recovery both install coverage before entering here. This
    /// one serial path then waits for the backend boundary and runs the
    /// hash-authoritative heal. The lifecycle performs its nonblocking batch
    /// drain before it can publish `Ready`.
    fn synchronize_and_heal(
        &self,
        attachment: &mut ProductionAttachment,
        progress: &ProgressReporter<ProductionAttachment>,
    ) -> Result<(), JobFailure> {
        attachment
            .subscription
            .as_ref()
            .ok_or_else(|| environmental("watcher coverage is not installed"))?
            .synchronize(WATCH_SYNCHRONIZATION_DEADLINE)
            .map_err(watcher)?;
        self.heal_under_coverage(attachment, progress)
    }

    /// Run one hash-authoritative heal against coverage that is already live,
    /// keeping the facts the watcher reports while it runs.
    ///
    /// The synchronization boundary is not crossed again here, because it is
    /// not a property that expires: coverage proven synchronized when it was
    /// installed is still the coverage this heal runs under. What is re-read is
    /// the vault, and that is the point.
    fn heal_under_coverage(
        &self,
        attachment: &mut ProductionAttachment,
        progress: &ProgressReporter<ProductionAttachment>,
    ) -> Result<(), JobFailure> {
        attachment
            .subscription
            .as_ref()
            .ok_or_else(|| environmental("watcher coverage is not installed"))?
            .begin_heal();
        let result = self.heal(attachment, progress);
        let observed = attachment
            .subscription
            .as_ref()
            .expect("subscription remains installed through its heal")
            .finish_heal()
            .map_err(watcher)?;
        attachment.heal_observed.merge(observed);
        result
    }

    /// Discard damaged derived state and derive it again from the vault — heal
    /// rung 3, with the vault as the truth it rebuilds against.
    ///
    /// **The equivalence claim is that this is the from-scratch derivation.**
    /// The database the store hands back is the one a create produces, holding
    /// no row; the heal that follows is the same hash-authoritative walk an
    /// attach over an empty database runs, reading every document under the
    /// registered root through the same code path. There is nothing here that a
    /// first attach does not do, which is what makes the result equal to a first
    /// attach's rather than merely close to it.
    ///
    /// The verification afterwards is the other half: a rebuilt database that
    /// cannot answer for itself is not derived state anything may be published
    /// over, and reporting damage here rather than `Ready` is what keeps a
    /// second damaged store from being served as a sound one.
    ///
    /// Coverage stands through all of it. The watcher and the maintainer lock
    /// were not what was damaged, and the facts the watcher reports during the
    /// heal are kept exactly as an attach keeps them.
    fn rung_three(
        &self,
        mut attachment: ProductionAttachment,
        progress: &ProgressReporter<ProductionAttachment>,
    ) -> Result<ProductionAttachment, JobFailure> {
        // Opening the derived state a read answers from is prologue work that
        // counts no document, which is the phase an attach installs it under.
        progress.installing_coverage();
        // The store is consumed here, so the attachment is not whole again
        // until the reopen lands. A failure between the two gives back what is
        // left in the order a detach gives it back: coverage first and the
        // maintainer lock last, so no second maintainer takes this vault while
        // a watch over it still stands.
        match attachment.store.discard_and_reopen() {
            Ok(store) => attachment.store = store,
            Err(error) => {
                drop(attachment.subscription);
                drop(attachment.maintainership);
                return Err(store_effect(error));
            }
        }
        attachment.store_verification_due = Instant::now() + STORE_VERIFICATION_INTERVAL;
        let derived = self
            .heal_under_coverage(&mut attachment, progress)
            .and_then(|()| {
                attachment
                    .store
                    .verify_integrity()
                    .map_err(store_effect)
                    .map_err(|failure| match failure {
                        JobFailure::StoreDamaged(detail) => JobFailure::StoreDamaged(format!(
                            "a store rebuilt from the vault is still damaged: {detail}"
                        )),
                        other => other,
                    })
            });
        match derived {
            Ok(()) => Ok(attachment),
            Err(failure) => {
                release(attachment);
                Err(failure)
            }
        }
    }
}

/// Give back everything an attachment holds, in the order it has to be given
/// back.
///
/// Coverage ends first and the maintainer lock last: a lock released while a
/// watch over the vault still stands is a window another process takes
/// maintainership in while this one is still reporting facts about the tree.
/// The store is closed between them, which is where a throwaway store's file
/// goes.
///
/// This is the spelling that reports the store's own teardown. The order it
/// runs in is [`ProductionAttachment`]'s field order too, so an attachment that
/// reaches a drop instead of this call still gives its resources back the same
/// way round.
fn release(attachment: ProductionAttachment) {
    drop(attachment.subscription);
    let _ = attachment.store.close();
    drop(attachment.maintainership);
}

/// The roots inside a vault a walk of it does not read: staged shadows wherever
/// this entry's placement puts them, and the schema when it is a file in the
/// vault.
///
/// **What these roots mean.** Every entry is vault-relative, and `norn-fs`
/// answers membership against the vault root whatever subtree a walk is narrowed
/// to. A fallback home is excluded by the **fallback root** rather than by
/// itself, so one entry cuts every maintainership's home under it: naming one
/// home would leave the subtree heal a watcher event on `<vault>/.norn`
/// schedules free to descend into every other home over this root and read
/// staged bytes as documents.
///
/// In [`Placement::DataRoot`] the home is outside the vault entirely and nothing
/// is excluded for it — unless this vault's root happens to contain the data
/// directory, which `strip_prefix` is what notices, and then the home is named
/// by the path it actually has.
fn exclusions(registration: &Registration, shadows: &ShadowHome) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    let root = registration.root.as_path();
    if let Some(shadows) = shadow_exclusion(shadows.placement(), shadows.directory(), root) {
        paths.push(shadows);
    }
    let schema = ProductionEntryOps::schema_path(registration);
    if let Ok(relative) = schema.strip_prefix(root) {
        paths.push(relative.to_owned());
    }
    paths
}

/// The vault-relative root a walk must not read on account of staged shadows,
/// or `None` when the placement puts them outside the vault entirely.
///
/// The two arms are the two facts a walk can be told. A fallback home is
/// excluded by its **root** rather than by itself, so one entry cuts the whole
/// dot-directory whatever key's home is under it and wherever the walk is
/// rooted. A data-root home is outside the vault, which `strip_prefix` is what
/// establishes — and where it does not, because this vault's root contains the
/// data directory, the home is named by the path it actually has.
fn shadow_exclusion(placement: Placement, home: &Path, root: &Path) -> Option<PathBuf> {
    match placement {
        Placement::VaultFallback => Some(PathBuf::from(norn_fs::FALLBACK)),
        Placement::DataRoot => home.strip_prefix(root).ok().map(Path::to_owned),
    }
}

/// The key this entry's maintainer lock and shadow home are both taken under.
///
/// One join, in one place, so that the home is keyed by the same three
/// coordinates the lock is: the lock is keyed by sitting inside
/// [`ConfigDirs::derived_dir`], and this is what carries that directory's own
/// identity to a home the device comparison may place anywhere.
fn maintainership_key(dirs: &ConfigDirs, name: &VaultName) -> MaintainershipKey {
    let key = dirs.derived_key(name);
    MaintainershipKey::new(key.channel(), key.vault(), key.data_base())
        .expect("a channel name, a vault name and a hex digest are each one path component")
}

impl EntryOps for ProductionEntryOps {
    type Attachment = ProductionAttachment;

    fn attach(
        &self,
        registration: &Registration,
        progress: &ProgressReporter<Self::Attachment>,
    ) -> Result<Self::Attachment, JobFailure> {
        // Everything up to the heal takes the maintainer lock, sweeps the
        // shadow home, installs watcher coverage and opens the store — work
        // that counts no document and can take a while on a loaded machine.
        // The phase is entered before it so a caller reading the entry sees
        // what it is waiting on rather than a heal that appears not to start.
        progress.installing_coverage();
        let root = registration.root.as_path();
        let derived = self.derived(&registration.name);
        let maintainership = match try_acquire(&derived.join("maintainer.lock")).map_err(effect)? {
            Acquisition::Acquired(guard) => guard,
            Acquisition::Contended { incumbent } => {
                return Err(JobFailure::MaintainerContended(map_incumbent(incumbent)));
            }
        };
        // The lock and the shadow home are two mechanisms of one maintainership,
        // so both are keyed by the coordinates the derived directory is keyed
        // by: the lock by sitting in that directory, the home by carrying the
        // key wherever the device comparison places it.
        let key = maintainership_key(&self.dirs, &registration.name);
        let shadows = ShadowHome::resolve(root, &derived.join("tmp"), &key).map_err(effect)?;
        shadows.sweep(Duration::ZERO).map_err(effect)?;
        if shadows.placement() == Placement::VaultFallback {
            // Residue no key's own sweep will ever open again: what a build that
            // staged before homes were keyed left directly under the fallback
            // root, and what a home whose key nothing resolves any more still
            // holds. A failure here is dropped rather than reported: this pass
            // is over ground no lock covers, so not managing it is no reason to
            // refuse maintainership of this store, and the next attach comes
            // round.
            let _ = norn_fs::sweep_fallback_root(root);
            let _ = norn_fs::sweep_fallback_tree(root);
        }
        let schema = Self::schema_path(registration);
        let (subscription, own_writes) =
            Self::start_watch(registration, &schema).map_err(watcher)?;
        let store = Store::open(derived.join("store.sqlite3")).map_err(store_effect)?;
        let mut attachment = ProductionAttachment {
            registration: registration.clone(),
            maintainership,
            store,
            subscription: Some(subscription),
            heal_observed: norn_fs::Batch::default(),
            _own_writes: own_writes,
            _shadows: shadows,
            last_shadow_sweep: Instant::now(),
            store_verification_due: Instant::now() + STORE_VERIFICATION_INTERVAL,
        };
        // The open resolves damage it can see in the store schema, and the heal
        // is where damage in the pages under it is met: a corrupt page an open
        // never read is met by the first read that does. Rung 3 runs here
        // rather than being reported, because an attach that reported it would
        // be answered by another attach that opened the same file and met the
        // same page.
        match self.synchronize_and_heal(&mut attachment, progress) {
            Ok(()) => Ok(attachment),
            Err(JobFailure::StoreDamaged(_)) => self.rung_three(attachment, progress),
            Err(failure) => Err(failure),
        }
    }

    fn reconcile(
        &self,
        _: &VaultName,
        attachment: &mut Self::Attachment,
        work: ReconcileWork,
        progress: &ProgressReporter<Self::Attachment>,
    ) -> Result<(), JobFailure> {
        if !attachment.maintainership.still_current().map_err(effect)? {
            return Err(JobFailure::LostMaintainership);
        }
        // A reconcile derives against coverage that is already installed,
        // whichever rung of the ladder the envelope reaches for.
        let schema =
            work.batch.schema_dirty() || work.batch.rescans().contains(&RescanScope::Schema);
        // A re-pin that moved the fingerprint discarded every finding keyed by
        // the old one, and the paths those findings sit at are not the paths
        // this batch names — so the scoped increment cannot record them again
        // and the vault-wide heal is the leg that does: a finding sits only
        // where no document row does, and the heal reads every such path. A
        // schema event whose re-read pins the same bytes moved nothing and
        // discarded nothing, so it stays on the increment it arrived as.
        let repinned =
            schema && Self::pin_schema(&mut attachment.store, &attachment.registration)?.repinned;
        if repinned || work.batch.rescans().contains(&RescanScope::Vault) {
            return self.heal(attachment, progress);
        }
        let healing = progress.healing();
        scoped_increment(
            &mut attachment.store,
            attachment.registration.root.as_path(),
            work.batch.vault_roots(),
            self.policy,
            &healing,
            &exclusions(&attachment.registration, &attachment._shadows),
        )
    }

    fn recover(
        &self,
        _: &VaultName,
        attachment: &mut Self::Attachment,
        progress: &ProgressReporter<Self::Attachment>,
    ) -> Result<(), JobFailure> {
        if !attachment.maintainership.still_current().map_err(effect)? {
            return Err(JobFailure::LostMaintainership);
        }
        // Recovery re-installs coverage before it re-heals, so it enters the
        // same prologue phase an attach does.
        progress.installing_coverage();
        let schema = Self::schema_path(&attachment.registration);
        let (subscription, own_writes) =
            Self::start_watch(&attachment.registration, &schema).map_err(watcher)?;
        attachment.subscription = Some(subscription);
        attachment._own_writes = own_writes;
        self.synchronize_and_heal(attachment, progress)
    }

    fn poll(
        &self,
        _: &VaultName,
        attachment: &mut Self::Attachment,
    ) -> Result<Option<norn_fs::Batch>, JobFailure> {
        if !attachment.maintainership.still_current().map_err(effect)? {
            return Err(JobFailure::LostMaintainership);
        }
        if !attachment.heal_observed.is_empty() {
            return Ok(Some(std::mem::take(&mut attachment.heal_observed)));
        }
        poll_subscription(attachment)
    }

    fn rebuild(
        &self,
        _: &VaultName,
        attachment: Self::Attachment,
        progress: &ProgressReporter<Self::Attachment>,
    ) -> Result<Self::Attachment, JobFailure> {
        // The attachment is this call's, and a failure here is the last thing
        // that touches it: the lifecycle reads a failed rebuild as coverage the
        // leg consumed and calls no detach for it. Both refusals below
        // therefore give the resources back through [`release`] rather than by
        // dropping the attachment, which would release the maintainer lock
        // ahead of the watch it is declared before.
        match attachment.maintainership.still_current() {
            Ok(true) => self.rung_three(attachment, progress),
            Ok(false) => {
                release(attachment);
                Err(JobFailure::LostMaintainership)
            }
            Err(refusal) => {
                let failure = effect(refusal);
                release(attachment);
                Err(failure)
            }
        }
    }

    /// Maintenance is due when either act's own clock is up. Which acts then
    /// run is [`ProductionEntryOps::maintain`]'s reading of the same two
    /// clocks: the leg is one leg, and the cadences it serves are two.
    fn maintenance_due(&self, _: &VaultName, attachment: &Self::Attachment) -> bool {
        attachment.last_shadow_sweep.elapsed() >= norn_fs::SHADOW_AGE_THRESHOLD
            || Instant::now() >= attachment.store_verification_due
    }

    fn maintain(&self, _: &VaultName, attachment: &mut Self::Attachment) -> Result<(), JobFailure> {
        if !attachment.maintainership.still_current().map_err(effect)? {
            return Err(JobFailure::LostMaintainership);
        }
        // Shadow residue is inert, and a sweep is only bounded cleanup. Losing
        // that cleanup opportunity must not withdraw an otherwise healthy
        // attachment or force a full heal; try again at the next normal cadence.
        if attachment.last_shadow_sweep.elapsed() >= norn_fs::SHADOW_AGE_THRESHOLD {
            let _ = attachment._shadows.sweep(norn_fs::SHADOW_AGE_THRESHOLD);
            attachment.last_shadow_sweep = Instant::now();
        }
        // The store's own consistency is the other maintenance question, and it
        // is asked here for the same reason the sweep is: it is bounded work
        // off the request path. Its clock is read separately from the sweep's,
        // because the two acts cost differently: a sweep reads one directory,
        // and this reads every page of the database. One leg serving both
        // cadences on whichever clock is up would spend the read on the
        // sweep's. A verdict of damage is **not** swallowed the way a failed
        // sweep is — a sweep that did not run leaves derived state sound, and
        // this is the one caller that learns it is not.
        if Instant::now() >= attachment.store_verification_due {
            attachment.store.verify_integrity().map_err(store_effect)?;
            attachment.store_verification_due = Instant::now() + STORE_VERIFICATION_INTERVAL;
        }
        Ok(())
    }

    fn detach(&self, _: &VaultName, attachment: Self::Attachment) {
        release(attachment);
    }
}

/// One settled batch, or no facts.
///
/// A terminal watch error takes the subscription and is reported once. An
/// attachment without a subscription therefore reports no facts rather than a
/// second failure: the cause that ended coverage is already published, and only
/// a re-attach installs coverage again.
fn poll_subscription(
    attachment: &mut ProductionAttachment,
) -> Result<Option<norn_fs::Batch>, JobFailure> {
    let Some(subscription) = attachment.subscription.as_ref() else {
        return Ok(None);
    };
    match subscription.try_recv() {
        Ok(batch) => Ok(batch),
        Err(error) => {
            attachment.subscription.take();
            Err(watcher(error))
        }
    }
}

fn heal_documents(
    store: &mut Store,
    root: &Path,
    exclusions: &[PathBuf],
    policy: ProductionPolicy,
    progress: &Healing<'_, ProductionAttachment>,
) -> Result<(), JobFailure> {
    let walk = walk(root, exclusions).map_err(effect)?;
    let sensitivity = walk.case_sensitivity();
    let order = store_order(sensitivity);
    let mut files = walk
        .filter_map(|fact| match fact {
            Ok(norn_fs::WalkFact::File(file)) if is_markdown(file.path().as_path()) => {
                Some(Ok(file))
            }
            Ok(norn_fs::WalkFact::File(_)) => None,
            Ok(norn_fs::WalkFact::Skipped(_)) => None,
            Err(e) => Some(Err(e)),
        })
        .peekable();
    let mut after: Option<DocumentPath> = None;
    let mut stored = Vec::new();
    let mut index = 0usize;
    let mut exhausted = false;
    let mut pending = Pending::new(store, policy.changeset_size);
    let mut healed = 0;
    loop {
        if index == stored.len() && !exhausted {
            stored = pending
                .store
                .begin_request()
                .stored_documents_after_ordered(after.as_ref(), policy.store_page_size, order)
                .map_err(store_effect)?;
            index = 0;
            exhausted = stored.is_empty();
        }
        let fs_path = next_nameable(&mut files, &mut pending)?;
        let db_path = stored.get(index).map(|d| d.path.as_str().to_owned());
        match (fs_path, db_path) {
            (None, None) => break,
            (Some(fp), Some(dp)) if sensitivity.compare(&fp, &dp).is_eq() => {
                let file = files.next().expect("peeked").map_err(effect)?;
                let read = file.read().map_err(effect)?;
                if read.content_hash().to_string() != stored[index].content_hash {
                    pending.rederive(
                        Path::new(&fp),
                        &fp,
                        read.bytes(),
                        read.content_hash().to_string(),
                        Some(&stored[index].path),
                    );
                }
                after = Some(stored[index].path.clone());
                index += 1;
            }
            (Some(fp), Some(dp)) if sensitivity.compare(&fp, &dp).is_lt() => {
                let file = files.next().expect("peeked").map_err(effect)?;
                let read = file.read().map_err(effect)?;
                pending.derive(&fp, read.bytes(), read.content_hash().to_string());
            }
            (_, Some(_)) => {
                let path = stored[index].path.clone();
                after = Some(path.clone());
                index += 1;
                pending.push(Change::Death {
                    path,
                    provenance: Provenance::HealPrune,
                });
            }
            (Some(fp), None) => {
                let file = files.next().expect("peeked").map_err(effect)?;
                let read = file.read().map_err(effect)?;
                pending.derive(&fp, read.bytes(), read.content_hash().to_string());
            }
        }
        if pending.is_full() {
            pending.flush()?;
        }
        healed += 1;
        progress.report(healed, None);
    }
    pending.flush()
}

fn scoped_increment(
    store: &mut Store,
    root: &Path,
    dirty: &std::collections::BTreeSet<norn_fs::NormalizedPath>,
    policy: ProductionPolicy,
    progress: &Healing<'_, ProductionAttachment>,
    exclusions: &[PathBuf],
) -> Result<(), JobFailure> {
    let normalizer = norn_fs::PathNormalizer::detect(root).map_err(effect)?;
    let sensitivity = normalizer.case_sensitivity();
    let excluded = norn_fs::Exclusions::new(&normalizer, exclusions).map_err(effect)?;
    let mut pending = Pending::new(store, policy.changeset_size);
    for (index, relative) in dirty.iter().enumerate() {
        let path = relative.as_path();
        if excluded.excludes(relative) {
            continue;
        }
        // Both the identity and the range it addresses are read before anything
        // that would use them, because what a spelling names and what it holds
        // are different questions: `..md` names no document and is still where
        // the documents under it are stored. What they say is acted on per kind:
        // only the arm that has a file in hand has a document to quarantine.
        let identity = document_path(path);
        let prefix = path
            .to_str()
            .and_then(|spelling| DirectoryPrefix::new(spelling).ok());
        let scope = match (&identity, &prefix) {
            (Ok(root), _) => Some(SubtreeScope::Subtree(root)),
            (Err(_), Some(prefix)) => Some(SubtreeScope::Prefix(prefix)),
            (Err(_), None) => None,
        };
        match norn_fs::path_kind(&root.join(path)).map_err(effect)? {
            norn_fs::PathKind::Directory => {
                pending.flush()?;
                match scope {
                    Some(scope) => {
                        heal_subtree(pending.store, root, scope, policy, progress, exclusions)?;
                    }
                    None => {
                        quarantine_subtree(
                            pending.store,
                            root,
                            path,
                            policy,
                            progress,
                            exclusions,
                        )?;
                    }
                }
                continue;
            }
            // A path that is not there has no document to quarantine. It still
            // has rows to prune wherever it addresses any, which a spelling no
            // prefix admits does not: such a spelling poisons every path beneath
            // it, so the range would be empty and none is opened.
            norn_fs::PathKind::Missing | norn_fs::PathKind::Other => {
                pending.flush()?;
                if let Some(scope) = scope {
                    prune_subtree_ordered(
                        pending.store,
                        scope,
                        policy,
                        progress,
                        store_order(sensitivity),
                    )?;
                }
                continue;
            }
            norn_fs::PathKind::RegularFile => {}
        }
        // The file is on disk, so a spelling the grammar refuses is a document
        // to quarantine rather than an event about a path that is already gone.
        let document_path = match identity {
            Ok(document) => document,
            Err(quarantine) => {
                if is_markdown(path) {
                    pending.quarantine(path, quarantine);
                    if pending.is_full() {
                        pending.flush()?;
                    }
                    progress.report((index + 1) as u64, Some(dirty.len() as u64));
                }
                continue;
            }
        };
        pending.flush()?;
        prune_descendants_and_aliases(
            pending.store,
            &document_path,
            policy,
            progress,
            sensitivity,
        )?;
        if !is_markdown(path) {
            continue;
        }
        // Read from the vault root down, so a dirty path reaches the same file
        // the vault walk reaches under that spelling or reaches nothing. A
        // watcher backend that resolved a link reports paths through one, and
        // an absolute join would follow it: the row derived there is one the
        // vault walk never yields and the next heal prunes.
        match norn_fs::read_optional_and_hash(root, path).map_err(effect)? {
            Some(observed) => {
                let hash = observed.content_hash().to_string();
                let standing = pending
                    .store
                    .begin_request()
                    .stored_document(&document_path)
                    .map_err(store_effect)?;
                if standing.as_ref().is_none_or(|row| row.content_hash != hash) {
                    pending.rederive(
                        path,
                        document_path.as_str(),
                        observed.bytes(),
                        hash,
                        standing.as_ref().map(|_| &document_path),
                    );
                }
            }
            // A death is news about a row, so it is recorded only where one
            // stands. A spelling that reaches nothing and never had a row is a
            // spelling the vault heal does not produce either — a path resolved
            // through a link by the watcher backend, a name that stopped being a
            // regular file between the kind above and the read — and a tombstone
            // there would state the removal of a document this vault never held,
            // at a path no heal ever prunes.
            None => {
                let standing = pending
                    .store
                    .begin_request()
                    .stored_document(&document_path)
                    .map_err(store_effect)?;
                if standing.is_some() {
                    pending.push(Change::Death {
                        path: document_path,
                        provenance: Provenance::WatcherRemoval,
                    });
                }
            }
        }
        if pending.is_full() {
            pending.flush()?;
        }
        progress.report((index + 1) as u64, Some(dirty.len() as u64));
    }
    pending.flush()
}

/// Converge a dirty directory that addresses no stored rows at all.
///
/// This is the root a backslash, a control byte or bytes that are not UTF-8
/// spoil — and each of those spoils every path beneath it too, since a
/// descendant's spelling carries the root's own segments. **So there is nothing
/// here to prune**: no document under such a root is storable, and a row under
/// it is one the store never held. What is left is to read what the vault holds,
/// through the seam every walked file's identity is read through, so a spelling
/// the grammar does admit is derived rather than special-cased.
fn quarantine_subtree(
    store: &mut Store,
    vault_root: &Path,
    relative_root: &Path,
    policy: ProductionPolicy,
    progress: &Healing<'_, ProductionAttachment>,
    exclusions: &[PathBuf],
) -> Result<(), JobFailure> {
    let walk = walk_subtree(vault_root, relative_root, exclusions).map_err(effect)?;
    let mut pending = Pending::new(store, policy.changeset_size);
    let mut healed = 0;
    for fact in walk {
        let norn_fs::WalkFact::File(file) = fact.map_err(effect)? else {
            continue;
        };
        let path = file.path().as_path().to_owned();
        if !is_markdown(&path) {
            continue;
        }
        match document_path(&path) {
            Ok(document) => {
                let read = file.read().map_err(effect)?;
                let standing = pending
                    .store
                    .begin_request()
                    .stored_document(&document)
                    .map_err(store_effect)?;
                pending.rederive(
                    &path,
                    document.as_str(),
                    read.bytes(),
                    read.content_hash().to_string(),
                    standing.as_ref().map(|_| &document),
                );
            }
            Err(quarantine) => pending.quarantine(&path, quarantine),
        }
        healed += 1;
        if pending.is_full() {
            pending.flush()?;
        }
        progress.report(healed, None);
    }
    pending.flush()
}

/// Converge a dirty directory the store can range, by merging its walk against
/// the rows it addresses.
///
/// The merge is the vault heal's, narrowed to one root: a walked file the store
/// has no row for is derived, a row the walk no longer reaches is pruned, and a
/// spelling the grammar refuses is quarantined as it is anywhere else. The scope
/// is what makes the prune half reachable for a directory whose own leaf reduces
/// to a refused stem — it names no document and still addresses every row
/// beneath it.
fn heal_subtree(
    store: &mut Store,
    vault_root: &Path,
    scope: SubtreeScope<'_>,
    policy: ProductionPolicy,
    progress: &Healing<'_, ProductionAttachment>,
    exclusions: &[PathBuf],
) -> Result<(), JobFailure> {
    let prefix = scope.as_str();
    let relative_root = Path::new(prefix);
    let walk = walk_subtree(vault_root, relative_root, exclusions).map_err(effect)?;
    let sensitivity = walk.case_sensitivity();
    let order = store_order(sensitivity);
    let mut files = walk
        .filter_map(|fact| match fact {
            Ok(norn_fs::WalkFact::File(file)) => {
                is_markdown(file.path().as_path()).then_some(Ok(file))
            }
            Ok(_) => None,
            Err(error) => Some(Err(error)),
        })
        .peekable();
    let mut after = None;
    let mut stored = Vec::new();
    let mut index = 0;
    let mut exhausted = false;
    let mut pending = Pending::new(store, policy.changeset_size);
    let mut healed = 0;
    loop {
        if index == stored.len() && !exhausted {
            stored = scope.page(pending.store, after.as_ref(), policy, order)?;
            index = 0;
            exhausted = stored.is_empty();
        }
        let fs_path = next_nameable(&mut files, &mut pending)?;
        let db_path = stored.get(index).map(|row| row.path.as_str().to_owned());
        match (fs_path, db_path) {
            (None, None) => break,
            (Some(fp), Some(dp)) if sensitivity.compare(&fp, &dp).is_eq() => {
                let read = files
                    .next()
                    .expect("peeked")
                    .map_err(effect)?
                    .read()
                    .map_err(effect)?;
                if read.content_hash().to_string() != stored[index].content_hash {
                    pending.rederive(
                        Path::new(&fp),
                        &fp,
                        read.bytes(),
                        read.content_hash().to_string(),
                        Some(&stored[index].path),
                    );
                }
                after = Some(stored[index].path.clone());
                index += 1;
            }
            (Some(fp), Some(dp)) if sensitivity.compare(&fp, &dp).is_lt() => {
                let read = files
                    .next()
                    .expect("peeked")
                    .map_err(effect)?
                    .read()
                    .map_err(effect)?;
                pending.derive(&fp, read.bytes(), read.content_hash().to_string());
            }
            (_, Some(_)) => {
                let path = stored[index].path.clone();
                after = Some(path.clone());
                index += 1;
                pending.push(Change::Death {
                    path,
                    provenance: Provenance::HealPrune,
                });
            }
            (Some(fp), None) => {
                let read = files
                    .next()
                    .expect("peeked")
                    .map_err(effect)?
                    .read()
                    .map_err(effect)?;
                pending.derive(&fp, read.bytes(), read.content_hash().to_string());
            }
        }
        if pending.is_full() {
            pending.flush()?;
        }
        healed += 1;
        progress.report(healed, None);
    }
    pending.flush()
}

/// The stored rows a dirty directory addresses, which is what a scoped heal
/// merges against and what a scoped prune ranges over.
///
/// A root the grammar admits carries its own row and its descendants; a
/// directory that names no document carries only descendants, and reaching them
/// is the whole reason the second variant exists. A spelling neither admits
/// addresses nothing, and is why callers take an `Option` of this.
#[derive(Clone, Copy)]
enum SubtreeScope<'a> {
    Subtree(&'a DocumentPath),
    Prefix(&'a DirectoryPrefix),
}

impl SubtreeScope<'_> {
    /// The vault-relative spelling the walk is rooted at and the stored paths
    /// beneath it open with.
    fn as_str(&self) -> &str {
        match self {
            SubtreeScope::Subtree(root) => root.as_str(),
            SubtreeScope::Prefix(prefix) => prefix.as_str(),
        }
    }

    /// The next bounded page of stored rows in this scope, `after` exclusive.
    fn page(
        self,
        store: &mut Store,
        after: Option<&DocumentPath>,
        policy: ProductionPolicy,
        order: StoredPathOrder,
    ) -> Result<Vec<StoredDocument>, JobFailure> {
        let request = store.begin_request();
        match self {
            SubtreeScope::Subtree(root) => request.stored_documents_in_subtree_after_ordered(
                root,
                after,
                policy.store_page_size,
                order,
            ),
            SubtreeScope::Prefix(prefix) => request.stored_documents_under_after_ordered(
                prefix,
                after,
                policy.store_page_size,
                order,
            ),
        }
        .map_err(store_effect)
    }
}

fn prune_subtree_ordered(
    store: &mut Store,
    scope: SubtreeScope<'_>,
    policy: ProductionPolicy,
    progress: &Healing<'_, ProductionAttachment>,
    order: StoredPathOrder,
) -> Result<(), JobFailure> {
    let mut after = None;
    let mut healed = 0;
    let mut pending = Pending::new(store, policy.changeset_size);
    loop {
        let page = scope.page(pending.store, after.as_ref(), policy, order)?;
        if page.is_empty() {
            break;
        }
        after = page.last().map(|row| row.path.clone());
        for row in page {
            pending.push(Change::Death {
                path: row.path,
                provenance: Provenance::WatcherRemoval,
            });
            healed += 1;
            if pending.is_full() {
                pending.flush()?;
            }
        }
        pending.flush()?;
        progress.report(healed, None);
    }
    Ok(())
}

fn prune_descendants_and_aliases(
    store: &mut Store,
    root: &DocumentPath,
    policy: ProductionPolicy,
    progress: &Healing<'_, ProductionAttachment>,
    sensitivity: norn_fs::CaseSensitivity,
) -> Result<(), JobFailure> {
    let mut after = None;
    let mut pruned = 0;
    let mut pending = Pending::new(store, policy.changeset_size);
    loop {
        let page = pending
            .store
            .begin_request()
            .stored_documents_in_subtree_after_ordered(
                root,
                after.as_ref(),
                policy.store_page_size,
                store_order(sensitivity),
            )
            .map_err(store_effect)?;
        if page.is_empty() {
            break;
        }
        after = page.last().map(|row| row.path.clone());
        for row in page {
            if &row.path == root {
                continue;
            }
            pending.push(Change::Death {
                path: row.path,
                provenance: Provenance::WatcherRemoval,
            });
            pruned += 1;
            if pending.is_full() {
                pending.flush()?;
            }
        }
        pending.flush()?;
        progress.report(pruned, None);
    }
    Ok(())
}

/// The next walked file's vault-relative spelling, with every entry whose path
/// bytes name no document quarantined and passed over.
///
/// The iterator is left standing on the file whose spelling comes back, so the
/// caller reads it after deciding what the merge does with it. Every walk names
/// its files from the vault root, so a scope needs no repair here.
fn next_nameable<I>(
    files: &mut Peekable<I>,
    pending: &mut Pending<'_>,
) -> Result<Option<String>, JobFailure>
where
    I: Iterator<Item = Result<norn_fs::FileFact, norn_fs::WalkError>>,
{
    loop {
        let spelling = match files.peek() {
            Some(Ok(file)) => file.path().as_path().to_str().map(str::to_owned),
            Some(Err(error)) => return Err(effect(error)),
            None => return Ok(None),
        };
        if let Some(spelling) = spelling {
            return Ok(Some(spelling));
        }
        let file = files.next().expect("peeked").map_err(effect)?;
        let path = file.path().as_path().to_owned();
        pending.quarantine(
            &path,
            Quarantine {
                cause: Undecodable::PathBytes,
                problem: format!("`{}` is not valid UTF-8", path.display()),
            },
        );
        if pending.is_full() {
            pending.flush()?;
        }
    }
}

fn is_markdown(path: &Path) -> bool {
    path.extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("md"))
}

fn store_order(sensitivity: norn_fs::CaseSensitivity) -> StoredPathOrder {
    match sensitivity {
        norn_fs::CaseSensitivity::Sensitive => StoredPathOrder::Sensitive,
        norn_fs::CaseSensitivity::Insensitive => StoredPathOrder::AsciiCaseInsensitive,
    }
}

/// The severity a quarantine finding carries. A document the vault holds and
/// norn cannot decode is a defect in the vault, not an advisory about it.
const QUARANTINE_SEVERITY: Severity = Severity::Error;

/// Why a path the vault holds produces no document facts.
///
/// One variant per finding kind, which is how a reader tells a name the store
/// cannot hold from bytes the parser cannot read.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Undecodable {
    /// The path bytes are not UTF-8.
    PathBytes,
    /// The path is UTF-8 and is not a document path.
    PathSpelling,
    /// The document's bytes are not UTF-8.
    BodyBytes,
}

impl Undecodable {
    /// The finding kind, which is the cause class a reader dispatches on.
    ///
    /// The vocabulary is the wire's, so a kind recorded in the findings table
    /// is the same string every surface advertises and filters by.
    const fn kind(self) -> FindingKind {
        match self {
            Undecodable::PathBytes => FindingKind::PathBytesNotUtf8,
            Undecodable::PathSpelling => FindingKind::PathNamesNoDocument,
            Undecodable::BodyBytes => FindingKind::BodyBytesNotUtf8,
        }
    }

    /// The cause as the finding's message states it.
    const fn statement(self) -> &'static str {
        match self {
            Undecodable::PathBytes => "its path bytes are not UTF-8",
            Undecodable::PathSpelling => "its path names no document",
            Undecodable::BodyBytes => "its bytes are not UTF-8",
        }
    }
}

/// One document held out of derived state, and why.
#[derive(Clone, Debug)]
struct Quarantine {
    cause: Undecodable,
    /// The decoder's own account of the refusal, which the finding carries in
    /// its detail beside the spelling it was read from.
    problem: String,
}

/// What one heal scope has derived and not yet committed: the changeset being
/// filled, and the findings its quarantined documents record.
///
/// One flush call applies the increment first and records the findings after
/// it, each in its own transaction: a flush torn between the two leaves the
/// quarantine recorded — a tombstone where the path had a row, nothing where it
/// did not — with no finding until the next heal re-derives the path and
/// records it. The order matters because a changeset entry discards the
/// findings recorded about the path it names, so a finding written ahead of the
/// increment is a finding the increment takes — and a quarantined document that
/// reads again is a plain upsert whose own discard clears the finding with no
/// second mechanism.
struct Pending<'s> {
    store: &'s mut Store,
    changes: Vec<Change>,
    quarantined: Vec<FindingFacts>,
    /// The subjects this scope has already re-derived, so a second finding at
    /// one **appends** rather than replacing the first: two spellings can render
    /// to one place, and each of them is a document somebody has to fix.
    ///
    /// It holds one path per quarantined document, which is the vault's defect
    /// count rather than its size, and it is strictly smaller than what those
    /// same defects put in the findings table.
    replaced: BTreeSet<DocumentPath>,
    /// The bound on the changeset and on the findings waiting beside it, which
    /// is what holds a scope's residency independent of how much of the vault it
    /// covers.
    bound: usize,
}

impl<'s> Pending<'s> {
    fn new(store: &'s mut Store, bound: usize) -> Self {
        Pending {
            store,
            changes: Vec::with_capacity(bound),
            quarantined: Vec::new(),
            replaced: BTreeSet::new(),
            bound,
        }
    }

    fn push(&mut self, change: Change) {
        self.changes.push(change);
    }

    /// Derive a document the store holds no row for: its facts where the bytes
    /// yield them, and a finding where they do not.
    fn derive(&mut self, spelling: &str, bytes: &[u8], hash: String) {
        self.rederive(Path::new(spelling), spelling, bytes, hash, None);
    }

    /// Derive a document, taking with it the row it can no longer account for.
    ///
    /// `stored` is the row standing at this path, which every caller already
    /// knows: the merge reads it off the page it is walking and the scoped paths
    /// read it by key. The store holds only what it can represent, so a document
    /// that stops decoding leaves nothing behind but the finding — and the row's
    /// death is a **quarantine**, because the file is still there.
    fn rederive(
        &mut self,
        path: &Path,
        spelling: &str,
        bytes: &[u8],
        hash: String,
        stored: Option<&DocumentPath>,
    ) {
        match map_document(spelling, bytes, hash) {
            Ok(facts) => self.push(Change::Upsert(facts)),
            Err(quarantine) => {
                if let Some(row) = stored {
                    self.push(Change::Death {
                        path: row.clone(),
                        provenance: Provenance::Quarantine,
                    });
                }
                self.quarantine(path, quarantine);
            }
        }
    }

    /// File the finding that says why a path contributes no facts.
    ///
    /// The subject is the place the path occupies — its own spelling where the
    /// grammar admits one, and a rendering of it where the grammar does not.
    /// Since a rendering is not injective, the detail carries the spelling this
    /// finding was read from, escaped, so two paths filed at one place stay
    /// tellable apart.
    fn quarantine(&mut self, path: &Path, quarantine: Quarantine) {
        let subject = DocumentPath::rendered(path);
        self.quarantined.push(FindingFacts {
            kind: quarantine.cause.kind(),
            severity: QUARANTINE_SEVERITY,
            message: format!(
                "`{}` is quarantined: {}",
                subject.as_str(),
                quarantine.cause.statement()
            ),
            path: subject,
            // Quarantine is not a reading of a resolution target, so the
            // finding belongs to no ambiguity class and no class-scoped
            // maintenance owns it.
            class_keys: BTreeSet::new(),
            target: None,
            span: None,
            candidates: Vec::new(),
            candidates_total: 0,
            detail: Some(format!("{path:?}: {}", quarantine.problem)),
        });
    }

    /// Whether the changeset or the findings beside it have reached the bound
    /// one flush may hold.
    fn is_full(&self) -> bool {
        self.changes.len() >= self.bound || self.quarantined.len() >= self.bound
    }

    /// Apply the changeset, then re-derive what its quarantines say.
    ///
    /// Findings go after the increment because the increment's own subject
    /// discard would otherwise take them, and because a document that derived is
    /// then readable: a place a real document occupies is that document's, and a
    /// finding there would call a document that just derived unreadable.
    fn flush(&mut self) -> Result<(), JobFailure> {
        if !self.changes.is_empty() {
            self.store
                .begin_request()
                .apply_increment(IncrementProvenance::Derived, self.changes.drain(..))
                .map_err(store_effect)?;
        }
        for finding in self.quarantined.drain(..) {
            // The first finding this scope files at a subject replaces what
            // stood there, which is how a cause that changed stops being
            // reported twice; the ones after it append.
            let replace = self.replaced.insert(finding.path.clone());
            let mut request = self.store.begin_request();
            if request
                .stored_document(&finding.path)
                .map_err(store_effect)?
                .is_some()
            {
                continue;
            }
            if replace {
                request
                    .discard_findings_about(&finding.path)
                    .map_err(store_effect)?;
            }
            request.record_finding(&finding).map_err(store_effect)?;
        }
        Ok(())
    }
}

/// The document path a vault-relative spelling names, or why it names none.
///
/// This is the one place a walked or watched path becomes a document identity,
/// so the two ways a spelling fails to be one are told apart here rather than
/// at each caller.
fn document_path(path: &Path) -> Result<DocumentPath, Quarantine> {
    let Some(spelling) = path.to_str() else {
        return Err(Quarantine {
            cause: Undecodable::PathBytes,
            problem: "the path bytes are not valid UTF-8".to_string(),
        });
    };
    DocumentPath::new(spelling).map_err(|problem| Quarantine {
        cause: Undecodable::PathSpelling,
        problem: problem.to_string(),
    })
}

fn map_document(path: &str, bytes: &[u8], hash: String) -> Result<DocumentFacts, Quarantine> {
    // Identity before content: a path that names no document has nothing to
    // say about its own bytes.
    let document_path = document_path(Path::new(path))?;
    let source = std::str::from_utf8(bytes).map_err(|problem| Quarantine {
        cause: Undecodable::BodyBytes,
        problem: problem.to_string(),
    })?;
    let document = Document::parse(source);
    let scan = document.scan_body();
    let mut facts = DocumentFacts::new(document_path, hash, document.body(), bytes.len() as u64);
    facts.body_offset = document.body_start() as u64;
    facts.frontmatter = document.frontmatter().map(map_value);
    facts.frontmatter_diagnostic_count = document
        .diagnostics()
        .iter()
        .filter(|d| d.code.frontmatter_scoped())
        .count() as u32;
    facts.links = document
        .frontmatter_wikilinks()
        .into_iter()
        .chain(scan.links())
        .map(map_link)
        .collect();
    facts.headings = scan
        .headings()
        .iter()
        .map(|h| HeadingFact {
            level: h.level,
            text: h.text.clone(),
            slug: h.slug.clone(),
            span: span(h.span),
            body_offset: h.body_offset as u64,
            inside_container: h.inside_container,
        })
        .collect();
    facts.blocks = scan
        .block_ids()
        .into_iter()
        .map(|b| BlockFact {
            block_id: b.id,
            span: Some(span(b.span)),
        })
        .collect();
    facts.tags = scan
        .tags()
        .into_iter()
        .map(|t| TagFact {
            name: t.name,
            source: TagSource::Body,
            span: t.span.map(span),
        })
        .chain(document.frontmatter_tags().into_iter().map(|t| TagFact {
            name: t.name,
            source: TagSource::Frontmatter,
            span: t.span.map(span),
        }))
        .collect();
    Ok(facts)
}

fn map_link(link: norn_text::Link) -> LinkFact {
    LinkFact {
        family: match link.family {
            norn_text::LinkFamily::Wikilink => LinkFamily::Wikilink,
            norn_text::LinkFamily::Markdown => LinkFamily::Markdown,
        },
        embed: link.embed,
        protocol: link.protocol,
        target: link.target,
        title: link.title,
        anchor: link.anchor,
        block_ref: link.block_ref,
        span: span(link.span),
    }
}
fn span(value: SourceSpan) -> Span {
    Span {
        line: value.line as u64,
        column: value.column as u64,
        byte_offset: value.byte_offset as u64,
    }
}
fn map_value(value: &Value) -> FrontmatterValue {
    match value {
        Value::Null => FrontmatterValue::Null,
        Value::Bool(v) => FrontmatterValue::Bool(*v),
        Value::Int(v) => FrontmatterValue::Int(*v),
        Value::Float(v) => FrontmatterValue::Float(*v),
        Value::String(v) => FrontmatterValue::String(v.clone()),
        Value::Sequence(v) => FrontmatterValue::Sequence(v.iter().map(map_value).collect()),
        Value::Map(v) => FrontmatterValue::Map(
            v.iter()
                .map(|(k, v)| (k.to_owned(), map_value(v)))
                .collect(),
        ),
    }
}
fn environmental(message: impl Into<String>) -> JobFailure {
    JobFailure::Environmental(message.into())
}

/// A refusal that is environmental by construction: the vault's filesystem, or
/// the machine under it, said no.
///
/// **The bound is the seam.** [`effect`] takes only errors carrying this trait,
/// so a store refusal cannot reach [`JobFailure::Environmental`] by being
/// handed to the same helper as a walk that could not list a directory. Store
/// refusals go through [`store_effect`], which is the one place a damaged
/// database is told apart from a broken environment.
///
/// The trait is sealed on [`sealed::Sealed`], so the environmental error types
/// are the list below and every addition to it is written at the list: a
/// `StoreError` handed to `effect` does not compile, and granting it that
/// status takes a deliberate pair of implementations here rather than one line
/// anywhere the trait is in scope.
trait EnvironmentalFailure: sealed::Sealed + fmt::Display {}

mod sealed {
    /// The seal on [`super::EnvironmentalFailure`]. It is implemented beside
    /// each implementation of that trait and nowhere else, which is what keeps
    /// the two lists one list.
    pub(super) trait Sealed {}
}

impl sealed::Sealed for std::io::Error {}
impl EnvironmentalFailure for std::io::Error {}
impl sealed::Sealed for norn_fs::Refusal {}
impl EnvironmentalFailure for norn_fs::Refusal {}
impl sealed::Sealed for norn_fs::WalkError {}
impl EnvironmentalFailure for norn_fs::WalkError {}
impl sealed::Sealed for &norn_fs::WalkError {}
impl EnvironmentalFailure for &norn_fs::WalkError {}
impl sealed::Sealed for norn_fs::PathError {}
impl EnvironmentalFailure for norn_fs::PathError {}
impl sealed::Sealed for norn_fs::NormalizerError {}
impl EnvironmentalFailure for norn_fs::NormalizerError {}
impl sealed::Sealed for norn_fs::ExclusionError {}
impl EnvironmentalFailure for norn_fs::ExclusionError {}

fn effect(error: impl EnvironmentalFailure) -> JobFailure {
    environmental(error.to_string())
}

/// The failure class a store refusal belongs to.
///
/// Damaged derived state and a refused operation are two failures with two
/// resolutions, and this is where the store's verdict becomes the lifecycle's.
/// Damage reaches the database-side heal rung; everything else is the
/// environment refusing, which the entry answers by staying untrusted and
/// saying so. Flattening the first onto the second is a loop: a corrupt page
/// answers a retry exactly as it answered the operation before it.
fn store_effect(error: StoreError) -> JobFailure {
    match error.damage() {
        Some(damage) => JobFailure::StoreDamaged(damage.to_string()),
        None => environmental(error.to_string()),
    }
}
fn watcher(error: WatchError) -> JobFailure {
    JobFailure::WatcherTerminal(error)
}

fn map_incumbent(incumbent: norn_fs::Incumbent) -> MaintainerIdentity {
    match incumbent {
        norn_fs::Incumbent::Named {
            pid,
            version,
            started,
        } => MaintainerIdentity::named(
            pid,
            version,
            started
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
        ),
        norn_fs::Incumbent::Unknown => MaintainerIdentity::unknown(),
    }
}

#[cfg(test)]
#[allow(clippy::disallowed_methods)] // test fixtures impersonate external editors and cleanup.
mod tests {
    use super::*;
    use crate::AttachMode;
    use norn_config::registry::{SchemaSource, VaultRoot};
    use norn_store::OpenOutcome;
    use norn_testkit::wait::{Budget, Observed, wait_until};
    use std::fs;
    use std::thread;
    use std::time::{SystemTime, UNIX_EPOCH};

    /// **The bar on what a walk is told about shadows.** A fallback home is
    /// excluded by the fallback root, so the entry cuts every maintainership's
    /// home at one node, and the root is vault-relative so it cuts the same node
    /// for a walk narrowed to any subtree.
    ///
    /// The forbidden shape is excluding the home itself. A watcher event on
    /// `<vault>/.norn` is not mechanism-suppressed, so the subtree heal it
    /// schedules walks from there; an entry naming one home leaves that walk
    /// free to descend into every other maintainership's home over this root and
    /// read staged bytes as documents.
    #[test]
    fn a_fallback_home_is_excluded_by_its_root_and_a_data_root_home_by_nothing() {
        let root = Path::new("/vaults/notes");
        let home = root.join(norn_fs::FALLBACK).join("norn-dev/notes/0f0f0f0f");

        let fallback = shadow_exclusion(Placement::VaultFallback, &home, root)
            .expect("a fallback home is inside the vault");
        assert_eq!(fallback, Path::new(norn_fs::FALLBACK));
        assert!(
            home.strip_prefix(root)
                .expect("a home inside the vault")
                .starts_with(&fallback),
            "the excluded root does not cut this placement's home"
        );

        assert_eq!(
            shadow_exclusion(
                Placement::DataRoot,
                Path::new("/data/norn-dev/vaults/notes/tmp"),
                root
            ),
            None,
            "a home outside the vault was named to a walk of the vault"
        );
        // The one arrangement that puts a data-root home inside the vault: a
        // vault root that contains the machine's data directory.
        assert_eq!(
            shadow_exclusion(
                Placement::DataRoot,
                &root.join("share/norn-dev/vaults/notes/tmp"),
                root
            ),
            Some(PathBuf::from("share/norn-dev/vaults/notes/tmp")),
            "a home the vault root contains was not excluded"
        );
    }

    /// **The bar on the join.** The key both mechanisms are taken under carries
    /// the derived directory's own coordinates, at the positions that directory
    /// puts them: the channel is the component under the data base, the vault
    /// name is the directory's last component, and the third part is the digest
    /// of the data base the whole thing hangs from.
    ///
    /// The forbidden shape is a key of fewer parts than the lock's identity has.
    /// The lock is a file inside the derived directory, so its identity is that
    /// directory's whole path; a key that dropped the data base would have two
    /// hosts running out of two data bases take two locks and resolve one
    /// fallback home — the exact sharing the key exists to prevent.
    #[test]
    fn the_maintainership_key_carries_the_derived_directory_s_own_coordinates() {
        let dirs = ConfigDirs::new("/config", "/data/base").expect("two bases");
        let name = VaultName::new("notes").expect("a name");
        let derived = dirs.derived_dir(&name);
        let expected = dirs.derived_key(&name);

        let key = maintainership_key(&dirs, &name);
        let parts: Vec<_> = key
            .as_path()
            .components()
            .map(|part| part.as_os_str().to_string_lossy().into_owned())
            .collect();

        assert_eq!(parts.len(), 3, "the key is not three components: {parts:?}");
        assert!(
            derived.starts_with(dirs.data_dir()),
            "{} does not hang from the data directory",
            derived.display()
        );
        assert_eq!(
            parts[0],
            dirs.data_dir()
                .file_name()
                .expect("the channel component")
                .to_string_lossy(),
            "the first part is not the channel component {} hangs from",
            derived.display()
        );
        assert_eq!(
            parts[1],
            derived
                .file_name()
                .expect("a last component")
                .to_string_lossy(),
            "the second part is not the last component of {}",
            derived.display()
        );
        assert_eq!(
            parts[2],
            expected.data_base(),
            "the third part is not the digest of the data base"
        );
        assert_ne!(
            key,
            maintainership_key(
                &ConfigDirs::new("/config", "/other/base").expect("two bases"),
                &name
            ),
            "two data bases spell one key"
        );
    }

    #[test]
    fn production_policy_enforces_the_store_page_bound() {
        assert!(matches!(
            ProductionPolicy::new(0, 1),
            Err(ProductionPolicyError::StorePageSize(0))
        ));
        assert!(ProductionPolicy::new(norn_store::MAX_STORED_DOCUMENT_PAGE, 1).is_ok());
        assert!(matches!(
            ProductionPolicy::new(norn_store::MAX_STORED_DOCUMENT_PAGE + 1, 1),
            Err(ProductionPolicyError::StorePageSize(1025))
        ));
    }

    #[test]
    fn production_policy_enforces_the_changeset_allocation_bound() {
        assert!(matches!(
            ProductionPolicy::new(1, 0),
            Err(ProductionPolicyError::ChangesetSize(0))
        ));
        assert!(ProductionPolicy::new(1, MAX_CHANGESET_SIZE).is_ok());
        assert!(matches!(
            ProductionPolicy::new(1, MAX_CHANGESET_SIZE + 1),
            Err(ProductionPolicyError::ChangesetSize(given))
                if given == MAX_CHANGESET_SIZE + 1
        ));
    }

    /// The bound on taking the real-watcher lease, derived from the window a
    /// case here holds it for.
    ///
    /// The queue depth and the wall it is capped against are the isolation
    /// module's, because they are one machine's and not this suite's: the
    /// cases queued ahead of one here include another crate's.
    fn lease_budget() -> Budget {
        norn_testkit::isolation::acquisition_budget(lifecycle_budget())
    }

    struct Fixture {
        root: PathBuf,
        // A case that attaches a real vault installs a real platform watcher.
        // The lease makes this process's watcher the only live one on the
        // machine, and it is held for the fixture's whole life because the
        // attachment's is inside it. A case that stands its own reports up
        // installs no watcher and takes no lease, so the machine's one live
        // watcher stays available to the cases that need it.
        _watcher_lease: Option<norn_testkit::isolation::Lease>,
    }
    impl Fixture {
        fn new(label: &str) -> Self {
            Self::rooted(
                label,
                Some(norn_testkit::isolation::Lease::hold(
                    norn_testkit::isolation::REAL_WATCHER,
                    lease_budget(),
                )),
            )
        }

        /// A fixture for a case that installs no platform watcher.
        ///
        /// The tree and its removal are what such a case wants — a vault
        /// directory to detect case behavior against, gone when the case ends
        /// however it ends — and the machine-wide watcher lease is what it
        /// does not.
        fn watcherless(label: &str) -> Self {
            Self::rooted(label, None)
        }

        fn rooted(label: &str, watcher_lease: Option<norn_testkit::isolation::Lease>) -> Self {
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let root = std::env::temp_dir()
                .join(format!("norn-host-{label}-{}-{nonce}", std::process::id()));
            fs::create_dir_all(root.join("vault/.norn")).unwrap();
            fs::write(root.join("vault/.norn/schema.yaml"), "version: 1\n").unwrap();
            Self {
                root,
                _watcher_lease: watcher_lease,
            }
        }
        fn vault(&self) -> PathBuf {
            self.root.join("vault")
        }
        /// The registration every attach in these cases is handed.
        fn registration(&self) -> Registration {
            Registration::new(
                VaultName::new("notes").unwrap(),
                VaultRoot::new(self.vault()).unwrap(),
            )
        }
        fn ops(&self, page: usize) -> (ProductionEntryOps, VaultName) {
            let dirs = ConfigDirs::new(self.root.join("config"), self.root.join("data")).unwrap();
            (
                ProductionEntryOps::new(dirs, ProductionPolicy::new(page, 2).unwrap()),
                self.registration().name,
            )
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn page_two_merge_inserts_changes_and_prunes_across_boundaries() {
        let f = Fixture::new("page-two");
        for (path, body) in [("a.md", "a"), ("c.md", "c"), ("e.md", "e"), ("g.md", "g")] {
            fs::write(f.vault().join(path), body).unwrap();
        }
        let (ops, name) = f.ops(2);
        let progress = ProgressReporter::disconnected();
        let mut attachment = ops.attach(&f.registration(), &progress).unwrap();
        fs::remove_file(f.vault().join("a.md")).unwrap();
        fs::remove_file(f.vault().join("e.md")).unwrap();
        fs::write(f.vault().join("b.md"), "b").unwrap();
        fs::write(f.vault().join("c.md"), "changed").unwrap();
        fs::write(f.vault().join("h.md"), "h").unwrap();
        ops.reconcile(
            &name,
            &mut attachment,
            ReconcileWork {
                batch: norn_fs::Batch::rescan(RescanScope::Vault),
            },
            &progress,
        )
        .unwrap();
        let rows = attachment
            .store
            .begin_request()
            .stored_documents_after_ordered(None, 20, StoredPathOrder::Sensitive)
            .unwrap();
        assert_eq!(
            rows.iter().map(|r| r.path.as_str()).collect::<Vec<_>>(),
            ["b.md", "c.md", "g.md", "h.md"]
        );
        assert_eq!(
            attachment
                .store
                .begin_request()
                .stored_document(&DocumentPath::new("c.md").unwrap())
                .unwrap()
                .unwrap()
                .byte_length,
            7
        );
        ops.detach(&name, attachment);
    }

    #[test]
    fn case_sensitive_heal_preserves_distinct_mixed_case_prefixes() {
        let f = Fixture::new("mixed-case-prefixes");
        fs::create_dir_all(f.vault().join("A")).unwrap();
        fs::create_dir_all(f.vault().join("a")).unwrap();
        fs::write(f.vault().join("A/z.md"), "upper").unwrap();
        fs::write(f.vault().join("a/b.md"), "lower").unwrap();
        if norn_fs::path_identity(&f.vault().join("A")).unwrap()
            == norn_fs::path_identity(&f.vault().join("a")).unwrap()
        {
            return;
        }

        let (ops, _) = f.ops(1);
        let progress = ProgressReporter::disconnected();
        let mut attachment = ops.attach(&f.registration(), &progress).unwrap();
        let rows = attachment
            .store
            .begin_request()
            .stored_documents_after_ordered(None, 10, StoredPathOrder::Sensitive)
            .unwrap();
        assert_eq!(
            rows.iter().map(|row| row.path.as_str()).collect::<Vec<_>>(),
            ["A/z.md", "a/b.md"]
        );
    }

    #[test]
    fn watcher_observes_an_edit_and_reconcile_converges_to_its_hash() {
        let f = Fixture::new("watch-edit");
        fs::write(f.vault().join("note.md"), "before").unwrap();
        fs::write(f.vault().join("unrelated.md"), "steady").unwrap();
        let (ops, name) = f.ops(2);
        let progress = ProgressReporter::disconnected();
        let mut attachment = ops.attach(&f.registration(), &progress).unwrap();
        let unrelated = DocumentPath::new("unrelated.md").unwrap();
        let generation = attachment
            .store
            .begin_request()
            .stored_document(&unrelated)
            .unwrap()
            .unwrap()
            .generation;
        fs::write(f.vault().join("note.md"), "after").unwrap();
        let after = norn_fs::ContentHash::of(b"after").to_string();
        reconcile_until(
            &ops,
            &name,
            &mut attachment,
            &progress,
            "the edited document to converge on its new hash",
            |attachment, _| match stored(attachment, "note.md") {
                Some(row) if row.content_hash == after => Observed::Met(()),
                row => Observed::pending(format!("note.md is {:?}", row.map(|r| r.content_hash))),
            },
        );
        // The unrelated document is a negative: nothing may touch it, and
        // there is no arrival to wait for. The absorption above is the span it
        // is judged over, and that span ends at the batch that carried the new
        // hash — an absorption stops at the first look its condition holds on,
        // so a later batch the edit also produced may still be settling. What
        // makes the negative sound is not the span but the subject: a reconcile
        // of batches naming note.md has no path to this row, and one that
        // reached it would have reached it inside the span too.
        assert_eq!(
            attachment
                .store
                .begin_request()
                .stored_document(&unrelated)
                .unwrap()
                .unwrap()
                .generation,
            generation,
            "a scoped watcher increment re-derived an unrelated document"
        );
        ops.detach(&name, attachment);
    }

    #[test]
    fn configured_poll_backend_detects_a_same_stat_content_replacement() {
        let f = Fixture::new("poll-watch-edit");
        fs::write(f.vault().join("note.md"), "before").unwrap();
        let mut registration = f.registration();
        registration.poll_backend = Some(norn_config::registry::PollBackend::Poll);
        let (ops, name) = f.ops(2);
        let progress = ProgressReporter::disconnected();
        let mut attachment = ops.attach(&registration, &progress).unwrap();

        let note = f.vault().join("note.md");
        let before = fs::metadata(&note).unwrap();
        let modified = before.modified().unwrap();
        fs::write(&note, "after!").unwrap();
        #[allow(clippy::disallowed_methods, clippy::disallowed_types)]
        fs::File::options()
            .write(true)
            .open(&note)
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(modified))
            .unwrap();
        let replaced = fs::metadata(&note).unwrap();
        assert_eq!(
            replaced.len(),
            before.len(),
            "the replacement changed length"
        );
        assert_eq!(
            replaced.modified().unwrap(),
            modified,
            "the replacement changed mtime"
        );

        let after = norn_fs::ContentHash::of(b"after!").to_string();
        reconcile_until(
            &ops,
            &name,
            &mut attachment,
            &progress,
            "the configured polling backend to hash-detect the same-stat replacement",
            |attachment, _| match stored(attachment, "note.md") {
                Some(row) if row.content_hash == after => Observed::Met(()),
                row => {
                    Observed::pending(format!("note.md is {:?}", row.map(|row| row.content_hash)))
                }
            },
        );
        ops.detach(&name, attachment);
    }

    #[test]
    fn registry_poll_selection_routes_to_the_polling_entrypoint() {
        let f = Fixture::new("poll-watch-route");
        let mut registration = f.registration();

        assert!(std::ptr::fn_addr_eq(
            ProductionEntryOps::watch_entrypoint(&registration),
            watch as WatchEntrypoint,
        ));
        registration.poll_backend = Some(PollBackend::Poll);
        assert!(std::ptr::fn_addr_eq(
            ProductionEntryOps::watch_entrypoint(&registration),
            watch_polling as WatchEntrypoint,
        ));
    }

    #[test]
    fn scoped_missing_markdown_file_prunes_its_stored_document() {
        let f = Fixture::new("missing-markdown-file");
        fs::write(f.vault().join("note.md"), "before").unwrap();
        let (ops, name) = f.ops(2);
        let progress = ProgressReporter::disconnected();
        let mut attachment = ops.attach(&f.registration(), &progress).unwrap();
        fs::remove_file(f.vault().join("note.md")).unwrap();

        scoped_increment(
            &mut attachment.store,
            f.vault().as_path(),
            &dirty_path(f.vault().as_path(), "note.md"),
            ProductionPolicy::new(2, 2).unwrap(),
            &progress.healing(),
            &exclusions(&attachment.registration, &attachment._shadows),
        )
        .unwrap();

        assert!(
            attachment
                .store
                .begin_request()
                .stored_document(&DocumentPath::new("note.md").unwrap())
                .unwrap()
                .is_none()
        );
        ops.detach(&name, attachment);
    }

    #[cfg(unix)]
    #[test]
    fn scoped_nonregular_markdown_path_tombstones_the_stored_document() {
        use std::os::unix::fs::symlink;

        let f = Fixture::new("markdown-to-symlink");
        let note = f.vault().join("note.md");
        fs::write(&note, "before").unwrap();
        let (ops, name) = f.ops(2);
        let progress = ProgressReporter::disconnected();
        let mut attachment = ops.attach(&f.registration(), &progress).unwrap();
        fs::remove_file(&note).unwrap();
        symlink("missing-target", &note).unwrap();
        reconcile_until(
            &ops,
            &name,
            &mut attachment,
            &progress,
            "the document behind the symlink to lose its row",
            |attachment, _| match stored(attachment, "note.md") {
                None => Observed::Met(()),
                Some(row) => Observed::pending(format!("note.md still stands at {row:?}")),
            },
        );
        ops.detach(&name, attachment);
    }

    #[test]
    fn scoped_file_replacing_a_directory_prunes_former_descendants() {
        let f = Fixture::new("directory-to-file");
        fs::create_dir_all(f.vault().join("archive.md")).unwrap();
        fs::write(f.vault().join("archive.md/child.md"), "child").unwrap();
        let (ops, name) = f.ops(2);
        let progress = ProgressReporter::disconnected();
        let mut attachment = ops.attach(&f.registration(), &progress).unwrap();

        fs::remove_dir_all(f.vault().join("archive.md")).unwrap();
        fs::write(f.vault().join("archive.md"), "replacement").unwrap();
        scoped_increment(
            &mut attachment.store,
            f.vault().as_path(),
            &dirty_path(f.vault().as_path(), "archive.md"),
            ProductionPolicy::new(2, 2).unwrap(),
            &progress.healing(),
            &exclusions(&attachment.registration, &attachment._shadows),
        )
        .unwrap();

        let request = attachment.store.begin_request();
        assert!(
            request
                .stored_document(&DocumentPath::new("archive.md/child.md").unwrap())
                .unwrap()
                .is_none()
        );
        assert!(
            request
                .stored_document(&DocumentPath::new("archive.md").unwrap())
                .unwrap()
                .is_some()
        );
        ops.detach(&name, attachment);
    }

    #[cfg(unix)]
    #[test]
    fn document_identity_boundary_quarantines_non_utf8_path_bytes() {
        use std::os::unix::ffi::OsStrExt;

        let path = Path::new(std::ffi::OsStr::from_bytes(b"bad-\xff.md"));
        let quarantine = document_path(path).expect_err("non-UTF-8 path bytes name no document");
        assert_eq!(quarantine.cause, Undecodable::PathBytes);
    }

    #[test]
    fn document_identity_boundary_quarantines_a_spelling_the_grammar_refuses() {
        let path = Path::new("notes/bad\\name.md");
        let quarantine = document_path(path).expect_err("a backslash names no document");
        assert_eq!(quarantine.cause, Undecodable::PathSpelling);
    }

    /// Two spellings can render to one place, and each of them is a document
    /// somebody has to fix — so the second finding appends and the detail is
    /// what tells the two apart.
    #[cfg(unix)]
    #[test]
    fn two_spellings_rendering_to_one_place_each_keep_a_finding() {
        let f = Fixture::new("quarantine-collision");
        let created = write_or_report(&f.vault().join("bad\\name.md"), b"body")
            && write_or_report(&f.vault().join("bad\u{7}name.md"), b"body");
        if !created {
            return;
        }
        let (ops, name) = f.ops(2);
        let progress = ProgressReporter::disconnected();
        let mut attachment = ops.attach(&f.registration(), &progress).unwrap();

        let findings = findings_at(&mut attachment.store, "bad\u{fffd}name.md");
        assert_eq!(findings.len(), 2, "one spelling displaced the other");
        let mut details = findings
            .iter()
            .map(|finding| finding.detail.clone().expect("a detail"))
            .collect::<Vec<_>>();
        details.sort();
        assert_eq!(details.len(), 2);
        assert_ne!(details[0], details[1], "the two findings read the same");

        // And a second heal replaces both rather than doubling them.
        ops.reconcile(
            &name,
            &mut attachment,
            ReconcileWork {
                batch: norn_fs::Batch::rescan(RescanScope::Vault),
            },
            &progress,
        )
        .unwrap();
        assert_eq!(
            findings_at(&mut attachment.store, "bad\u{fffd}name.md").len(),
            2
        );
        ops.detach(&name, attachment);
    }

    /// A cause that changed is re-derived rather than reported twice: the first
    /// finding a scope files at a subject replaces what stood there.
    #[test]
    fn a_quarantine_whose_cause_moves_replaces_the_finding_that_stood() {
        let f = Fixture::new("quarantine-replace");
        fs::write(f.vault().join("bad.md"), b"ok \xff\xfe first\n").unwrap();
        let (ops, name) = f.ops(2);
        let progress = ProgressReporter::disconnected();
        let mut attachment = ops.attach(&f.registration(), &progress).unwrap();
        let first = findings_at(&mut attachment.store, "bad.md");
        assert_eq!(first.len(), 1);

        // The same document, still undecodable, with the first bad byte
        // somewhere else — a different detail for the same subject.
        fs::write(
            f.vault().join("bad.md"),
            b"a much longer prefix \xff\xfe here\n",
        )
        .unwrap();
        ops.reconcile(
            &name,
            &mut attachment,
            ReconcileWork {
                batch: norn_fs::Batch::rescan(RescanScope::Vault),
            },
            &progress,
        )
        .unwrap();

        let second = findings_at(&mut attachment.store, "bad.md");
        assert_eq!(second.len(), 1, "the stale finding outlived its cause");
        assert_ne!(second[0].detail, first[0].detail);
        ops.detach(&name, attachment);
    }

    /// **The bar on the re-derivation leg of a schema change.** A re-pin
    /// discards every finding keyed by the fingerprint it replaced, and the
    /// paths those findings sit at are not paths a schema batch names — so the
    /// increment such a batch would otherwise run reaches none of them. The
    /// reconcile that re-pins is therefore the reconcile that records them
    /// again, which is the whole vault's re-derivation.
    ///
    /// Both envelopes a schema change arrives in carry it: the bare
    /// invalidation an edit to the configured source reports, and the schema
    /// rescan the backend reports when it loses the exact fact.
    ///
    /// The forbidden shape is a hand edit to the schema file leaving the
    /// findings table empty until unrelated work happens to touch those paths.
    #[test]
    fn a_schema_edit_re_derives_the_findings_its_re_pin_discarded() {
        let f = Fixture::new("schema-repin-heal");
        fs::write(f.vault().join("bad.md"), UNDECODABLE).unwrap();
        let (ops, name) = f.ops(2);
        let progress = ProgressReporter::disconnected();
        let mut attachment = ops.attach(&f.registration(), &progress).unwrap();
        let before = findings_at(&mut attachment.store, "bad.md");
        assert_eq!(before.len(), 1, "the quarantine was not recorded at attach");

        // A hand edit to the schema file, reconciled under the envelope the
        // caller names.
        let edit = |attachment: &mut ProductionAttachment, bytes: &str, batch: norn_fs::Batch| {
            fs::write(f.vault().join(".norn/schema.yaml"), bytes).unwrap();
            ops.reconcile(&name, attachment, ReconcileWork { batch }, &progress)
                .unwrap();
        };

        // The envelope an edit to the in-vault schema reports: the schema is
        // invalidated, and the vault paths such a batch also names are not
        // paths a finding sits at.
        edit(
            &mut attachment,
            "version: 1\n# edited by hand\n",
            norn_fs::Batch::schema_change(),
        );

        let after = findings_at(&mut attachment.store, "bad.md");
        assert_eq!(after.len(), 1, "the schema edit discarded a live finding");
        assert_eq!(finding_total(&mut attachment.store), 1);
        assert_ne!(
            after[0].vault_schema_fingerprint, before[0].vault_schema_fingerprint,
            "the edit did not move the pin, so this proves nothing"
        );
        assert!(
            after[0].generation > before[0].generation,
            "the finding standing here was never re-derived"
        );
        assert_eq!(after[0].detail, before[0].detail);

        // The other envelope: the backend lost the schema's own facts and says
        // only that the source partition is dirty.
        edit(
            &mut attachment,
            "version: 1\n# edited again\n",
            norn_fs::Batch::rescan(RescanScope::Schema),
        );

        let rescanned = findings_at(&mut attachment.store, "bad.md");
        assert_eq!(
            rescanned.len(),
            1,
            "the schema rescan discarded a live finding"
        );
        assert_eq!(finding_total(&mut attachment.store), 1);
        assert_ne!(
            rescanned[0].vault_schema_fingerprint, after[0].vault_schema_fingerprint,
            "the edit did not move the pin, so this proves nothing"
        );
        assert!(
            rescanned[0].generation > after[0].generation,
            "the finding standing here was never re-derived"
        );

        // The same bytes written again are a schema event and not a schema
        // change: the re-pin discards nothing, so nothing is re-derived.
        edit(
            &mut attachment,
            "version: 1\n# edited again\n",
            norn_fs::Batch::schema_change(),
        );

        let again = findings_at(&mut attachment.store, "bad.md");
        assert_eq!(again.len(), 1);
        assert_eq!(
            again[0].generation, rescanned[0].generation,
            "a re-pin that changed nothing re-derived the vault"
        );
        ops.detach(&name, attachment);
    }

    /// A place a real document occupies is that document's. A rendering that
    /// lands on one is a collision, and the document wins: a finding there would
    /// call a document that just derived unreadable.
    #[cfg(unix)]
    #[test]
    fn a_rendering_that_collides_with_a_real_document_files_no_finding() {
        let f = Fixture::new("quarantine-marker-collision");
        fs::write(f.vault().join("bad\u{fffd}name.md"), "a real document").unwrap();
        if !write_or_report(&f.vault().join("bad\\name.md"), b"body") {
            return;
        }
        let (ops, name) = f.ops(2);
        let progress = ProgressReporter::disconnected();
        let mut attachment = ops.attach(&f.registration(), &progress).unwrap();

        assert_eq!(
            stored_paths(&mut attachment.store),
            ["bad\u{fffd}name.md"],
            "the real document is served"
        );
        assert!(
            findings_at(&mut attachment.store, "bad\u{fffd}name.md").is_empty(),
            "a document that derived was called unreadable"
        );
        ops.detach(&name, attachment);
    }

    /// Bytes no Markdown document can be read from.
    const UNDECODABLE: &[u8] = b"ok \xff\xfe not utf8\n";

    fn stored_paths(store: &mut Store) -> Vec<String> {
        store
            .begin_request()
            .stored_documents_after_ordered(None, 50, StoredPathOrder::Sensitive)
            .unwrap()
            .iter()
            .map(|row| row.path.as_str().to_owned())
            .collect()
    }

    /// **A store whose pages are corrupt reaches rung 3 at the attach that meets
    /// them, and what it derives is a from-scratch build.**
    ///
    /// The open resolves damage it can see in the store schema, and this
    /// corruption is not in the store schema: the file opens as the shape this
    /// build writes, and the first read of a document page is where SQLite
    /// refuses. Reporting that would send the entry back through an attach that
    /// opens the same file and meets the same page.
    #[test]
    fn a_corrupt_store_at_attach_reaches_rung_three_and_derives_the_vault_again() {
        let f = Fixture::new("attach-damage");
        write_a_vault_of_documents(&f, 120);
        // A document no derivation reads facts out of, so the finding a heal
        // files for it is on both sides of the equality below.
        fs::write(f.vault().join("unreadable.md"), UNDECODABLE).unwrap();
        let (ops, name) = f.ops(64);
        let policy = ProductionPolicy::new(64, 2).unwrap();
        let progress = ProgressReporter::disconnected();

        let attachment = ops.attach(&f.registration(), &progress).unwrap();
        let database = attachment.store.path().to_path_buf();
        ops.detach(&name, attachment);
        corrupt_the_document_pages(&database);

        let mut attachment = ops.attach(&f.registration(), &progress).unwrap();
        assert_eq!(
            *attachment.store.open_outcome(),
            OpenOutcome::Created,
            "the attach served the corrupt file rather than discarding it"
        );
        attachment
            .store
            .verify_integrity()
            .expect("a store rung 3 rebuilt");
        assert_eq!(
            derived_vault(&mut attachment.store, f.vault().as_path()),
            from_scratch(&f, "attach-damage-oracle", policy),
            "the rebuild derived something a from-scratch build does not"
        );
        assert_eq!(
            findings_at(&mut attachment.store, "unreadable.md").len(),
            1,
            "the rebuild derived no finding, so the equality above compared none"
        );
        ops.detach(&name, attachment);
    }

    /// **Logical damage is silent, so the scheduled verification is what meets
    /// it — and the verdict is not swallowed the way a failed sweep is.**
    ///
    /// A full-text index that stopped agreeing with the column it indexes
    /// answers reads about text no document holds. No read fails, so nothing
    /// else in the warm cycle can report it.
    ///
    /// The maintenance leg serves two cadences, so this also pins that the
    /// store is asked only on its own: a leg the shadow clock brought round
    /// inside the store interval reads no page of the database.
    #[test]
    fn scheduled_maintenance_reports_a_full_text_index_that_stopped_agreeing_as_damage() {
        let f = Fixture::new("silent-damage");
        write_a_vault_of_documents(&f, 8);
        // A document no derivation reads facts out of, so the finding a heal
        // files for it is on both sides of the equality below.
        fs::write(f.vault().join("unreadable.md"), UNDECODABLE).unwrap();
        let (ops, name) = f.ops(64);
        let policy = ProductionPolicy::new(64, 2).unwrap();
        let progress = ProgressReporter::disconnected();
        let mut attachment = ops.attach(&f.registration(), &progress).unwrap();
        attachment.store_verification_due = Instant::now();
        ops.maintain(&name, &mut attachment)
            .expect("a store nothing has damaged");

        // The triggers are the only thing that writes the index, so a body edit
        // behind their back is a disagreement nothing else notices.
        norn_store::induced_failure::execute_out_of_band(
            &mut attachment.store,
            "DROP TRIGGER documents_fts_update;
             UPDATE documents SET body = 'an entirely different body'",
        )
        .unwrap();

        ops.maintain(&name, &mut attachment).expect(
            "a maintenance leg inside the store interval asked the database about damage \
             the verification above had already cleared it of",
        );

        attachment.store_verification_due = Instant::now();
        let failure = ops
            .maintain(&name, &mut attachment)
            .expect_err("a full-text index that stopped agreeing with its column");
        let JobFailure::StoreDamaged(detail) = &failure else {
            panic!("the damage was reported as {failure:?} rather than as damaged state");
        };
        assert!(!detail.is_empty(), "the damage was not named");

        // Rung 3, run as the lifecycle runs it, over coverage that stands.
        let mut attachment = ops
            .rebuild(&name, attachment, &progress)
            .expect("rung 3 over a damaged store");
        assert_eq!(*attachment.store.open_outcome(), OpenOutcome::Created);
        assert_eq!(
            derived_vault(&mut attachment.store, f.vault().as_path()),
            from_scratch(&f, "silent-damage-oracle", policy),
            "the rebuild derived something a from-scratch build does not"
        );
        attachment.store_verification_due = Instant::now();
        ops.maintain(&name, &mut attachment)
            .expect("a store rung 3 rebuilt");
        ops.detach(&name, attachment);
    }

    /// **Rung 3 keeps the coverage it runs under.** The watcher and the
    /// maintainer lock are not what was damaged, so a rebuild is not a
    /// re-attach: what it replaces is the derived state between them, and the
    /// facts the watcher reports while it runs are kept exactly as an attach
    /// keeps them.
    #[test]
    fn rung_three_replaces_the_store_and_keeps_the_maintainership_around_it() {
        let f = Fixture::new("rebuild-keeps-coverage");
        write_a_vault_of_documents(&f, 4);
        let (ops, name) = f.ops(64);
        let progress = ProgressReporter::disconnected();
        let mut attachment = ops.attach(&f.registration(), &progress).unwrap();
        let database = attachment.store.path().to_path_buf();
        assert!(
            attachment.maintainership.still_current().unwrap(),
            "the attach did not take maintainership"
        );

        // A document the damaged store never held, so a rebuild that derived
        // nothing would be visible in the rows rather than only in the file.
        fs::write(f.vault().join("arrived.md"), "# Arrived\n").unwrap();
        norn_store::induced_failure::execute_out_of_band(
            &mut attachment.store,
            "DROP TRIGGER documents_fts_delete; DELETE FROM documents",
        )
        .unwrap();

        let mut attachment = ops
            .rebuild(&name, attachment, &progress)
            .expect("rung 3 over a damaged store");
        assert_eq!(attachment.store.path(), database.as_path());
        assert!(
            attachment.maintainership.still_current().unwrap(),
            "rung 3 gave up the maintainer lock it was not asked to give up"
        );
        assert!(
            attachment.subscription.is_some(),
            "rung 3 gave up the watcher coverage it was not asked to give up"
        );
        assert!(
            stored_paths(&mut attachment.store).contains(&"arrived.md".to_string()),
            "the rebuild derived the vault as it stands rather than the vault the damaged store held"
        );
        ops.detach(&name, attachment);
    }

    /// Everything one derivation of a vault produced, in the shape two
    /// derivations are compared as.
    ///
    /// Four readings, because a derivation puts its answer in four places: the
    /// document rows and every fact row under them, the findings that sit where
    /// no document row does, how much each pillar holds — tombstones and
    /// finding candidates included — and the vault schema the whole derivation
    /// was keyed by. A rebuild that lost a finding, kept a tombstone or stopped
    /// re-pinning the schema disagrees here.
    ///
    /// The oracle for a rung-3 rebuild is a from-scratch run of the same heal,
    /// so what an equality over this proves is that rung 3 derives what a first
    /// attach derives. A defect inside the shared walk is invisible to it, and
    /// is what the heal's own cases cover.
    #[derive(Debug, Eq, PartialEq)]
    struct DerivedVault {
        documents: Vec<DerivedDocument>,
        findings: Vec<norn_store::StoredFinding>,
        pillars: norn_store::PillarReport,
        /// The pinned schema's bytes and fingerprint. The generation beside
        /// them is not compared: it counts writes to the database rather than
        /// describing the schema the derivation ran under.
        vault_schema: Option<(Vec<u8>, String)>,
    }

    /// One document's row and every fact row under it, without the timestamp
    /// the row was written at.
    ///
    /// `derived_at` is a whole-second wall clock recording when a row was
    /// written, so two derivations of one vault differ there whenever they land
    /// in different seconds — it is bookkeeping about the writing rather than a
    /// fact read out of the vault, and no claim about equal derivations rests
    /// on it. Everything else stays, `generation` among it: both sides pin and
    /// then walk the same documents under the same policy, so the batching that
    /// hands out generations is part of what the equality claims.
    #[derive(Debug, Eq, PartialEq)]
    struct DerivedDocument {
        path: DocumentPath,
        content_hash: String,
        byte_length: u64,
        body_offset: u64,
        frontmatter: Option<String>,
        frontmatter_diagnostic_count: u32,
        generation: i64,
        body: String,
        links: Vec<LinkFact>,
        headings: Vec<HeadingFact>,
        blocks: Vec<BlockFact>,
        tags: Vec<TagFact>,
    }

    impl From<norn_store::StoredFacts> for DerivedDocument {
        fn from(facts: norn_store::StoredFacts) -> Self {
            let norn_store::StoredFacts {
                document,
                body,
                links,
                headings,
                blocks,
                tags,
            } = facts;
            Self {
                path: document.path,
                content_hash: document.content_hash,
                byte_length: document.byte_length,
                body_offset: document.body_offset,
                frontmatter: document.frontmatter,
                frontmatter_diagnostic_count: document.frontmatter_diagnostic_count,
                generation: document.generation,
                body,
                links,
                headings,
                blocks,
                tags,
            }
        }
    }

    fn derived_vault(store: &mut Store, vault: &Path) -> DerivedVault {
        let documents = every_stored_fact(store)
            .into_iter()
            .map(DerivedDocument::from)
            .collect();
        // A finding sits only where no document row does, so the paths it is
        // read at come from the vault rather than from the store's own list of
        // documents.
        let findings = vault_relative_paths(vault)
            .iter()
            .flat_map(|path| findings_at(store, path))
            .collect();
        let pillars = store.begin_request().pillars().unwrap();
        let vault_schema = store
            .begin_request()
            .vault_schema_pin()
            .unwrap()
            .map(|pin| (pin.bytes, pin.fingerprint));
        DerivedVault {
            documents,
            findings,
            pillars,
            vault_schema,
        }
    }

    /// Every name directly under a fixture vault, in order, as the document
    /// grammar spells it.
    fn vault_relative_paths(vault: &Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(vault)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|name| DocumentPath::new(name).is_ok())
            .collect();
        names.sort();
        names
    }

    /// Every fact the store holds about every document it holds, in path order.
    ///
    /// It reads the rows a read answers from rather than a summary of them, so
    /// two stores agreeing here agree about content hashes, links, tags,
    /// headings, blocks and the frontmatter projection alike.
    fn every_stored_fact(store: &mut Store) -> Vec<norn_store::StoredFacts> {
        let paths: Vec<DocumentPath> = store
            .begin_request()
            .stored_documents_after_ordered(None, 500, StoredPathOrder::Sensitive)
            .unwrap()
            .into_iter()
            .map(|row| row.path)
            .collect();
        paths
            .iter()
            .map(|path| {
                store
                    .begin_request()
                    .stored_facts(path)
                    .unwrap()
                    .expect("a document the store just listed")
            })
            .collect()
    }

    /// The derivation a first attach over this vault produces, built by the
    /// same heal against a database of its own.
    ///
    /// It is the oracle every rung-3 case is judged against, and it is the heal
    /// rather than a restatement of what the heal ought to find: a rebuild that
    /// stopped agreeing with a from-scratch derivation would have to disagree
    /// with this.
    fn from_scratch(f: &Fixture, label: &str, policy: ProductionPolicy) -> DerivedVault {
        let mut store = Store::open(f.root.join(format!("{label}.sqlite3"))).unwrap();
        let registration = f.registration();
        let progress = ProgressReporter::disconnected();
        ProductionEntryOps::pin_schema(&mut store, &registration).unwrap();
        let shadows = ShadowHome::resolve(
            registration.root.as_path(),
            &f.root.join("scratch-tmp"),
            &maintainership_key(
                &ConfigDirs::new(f.root.join("config"), f.root.join("data")).unwrap(),
                &registration.name,
            ),
        )
        .unwrap();
        heal_documents(
            &mut store,
            registration.root.as_path(),
            &exclusions(&registration, &shadows),
            policy,
            &progress.healing(),
        )
        .unwrap();
        derived_vault(&mut store, registration.root.as_path())
    }

    /// Overwrite the pages a database keeps its documents in, leaving the store
    /// schema an open reads intact.
    ///
    /// A database is created holding its store schema, and every page written
    /// after that is appended past it, so the tail is documents and the head is
    /// the schema. This is real corruption rather than an injected verdict: what
    /// the store meets is a page SQLite refuses to read.
    fn corrupt_the_document_pages(database: &Path) {
        let mut bytes = fs::read(database).unwrap();
        let head = bytes.len() / 2;
        assert!(head > 0, "the database is empty");
        for byte in bytes.iter_mut().skip(head) {
            *byte = 0x5a;
        }
        fs::write(database, &bytes).unwrap();
    }

    /// A vault with enough documents that its store outgrows the pages it was
    /// created holding.
    fn write_a_vault_of_documents(f: &Fixture, count: usize) {
        for index in 0..count {
            fs::write(
                f.vault().join(format!("note-{index:03}.md")),
                format!("# Note {index}\n\nA paragraph about [[note-000]] and #tagging.\n"),
            )
            .unwrap();
        }
    }

    fn findings_at(store: &mut Store, at: &str) -> Vec<norn_store::StoredFinding> {
        store
            .begin_request()
            .stored_findings(&DocumentPath::new(at).unwrap())
            .unwrap()
    }

    fn finding_total(store: &mut Store) -> u64 {
        store.begin_request().pillars().unwrap().findings
    }

    /// Writes a name the vault holds and the document-path grammar refuses, or
    /// reports that this filesystem will not hold it.
    fn write_or_report(path: &Path, bytes: &[u8]) -> bool {
        match fs::write(path, bytes) {
            Ok(()) => true,
            Err(error) => {
                eprintln!(
                    "skipped: this filesystem does not create `{}`: {error}",
                    path.display()
                );
                false
            }
        }
    }

    #[test]
    fn heal_quarantines_an_undecodable_body_and_indexes_every_other_document() {
        let f = Fixture::new("quarantine-body");
        fs::write(f.vault().join("alpha.md"), "alpha").unwrap();
        fs::write(f.vault().join("bad.md"), UNDECODABLE).unwrap();
        fs::write(f.vault().join("gamma.md"), "gamma").unwrap();
        let (ops, name) = f.ops(2);
        let progress = ProgressReporter::disconnected();
        let mut attachment = ops.attach(&f.registration(), &progress).unwrap();

        assert_eq!(
            stored_paths(&mut attachment.store),
            ["alpha.md", "gamma.md"]
        );
        let findings = findings_at(&mut attachment.store, "bad.md");
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].kind, "document/body-bytes-not-utf8");
        assert_eq!(findings[0].severity, "error");
        assert!(
            findings[0].message.contains("bad.md"),
            "the finding does not name the document: {}",
            findings[0].message
        );
        assert!(findings[0].detail.is_some());
        assert_eq!(finding_total(&mut attachment.store), 1);
        ops.detach(&name, attachment);
    }

    #[test]
    fn a_second_heal_of_a_quarantined_document_records_one_finding() {
        let f = Fixture::new("quarantine-idempotent");
        fs::write(f.vault().join("bad.md"), UNDECODABLE).unwrap();
        fs::write(f.vault().join("ok.md"), "ok").unwrap();
        let (ops, name) = f.ops(2);
        let progress = ProgressReporter::disconnected();
        let mut attachment = ops.attach(&f.registration(), &progress).unwrap();
        ops.reconcile(
            &name,
            &mut attachment,
            ReconcileWork {
                batch: norn_fs::Batch::rescan(RescanScope::Vault),
            },
            &progress,
        )
        .unwrap();

        assert_eq!(findings_at(&mut attachment.store, "bad.md").len(), 1);
        assert_eq!(finding_total(&mut attachment.store), 1);
        ops.detach(&name, attachment);
    }

    #[cfg(unix)]
    #[test]
    fn heal_quarantines_a_path_spelling_the_document_grammar_refuses() {
        let f = Fixture::new("quarantine-spelling");
        fs::write(f.vault().join("ok.md"), "ok").unwrap();
        if !write_or_report(&f.vault().join("bad\\name.md"), b"body") {
            return;
        }
        let (ops, name) = f.ops(2);
        let progress = ProgressReporter::disconnected();
        let mut attachment = ops.attach(&f.registration(), &progress).unwrap();

        assert_eq!(stored_paths(&mut attachment.store), ["ok.md"]);
        let findings = findings_at(&mut attachment.store, "bad\u{fffd}name.md");
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].kind, "document/path-names-no-document");
        assert!(
            findings[0].message.contains("bad\u{fffd}name.md"),
            "the finding does not name the document: {}",
            findings[0].message
        );
        ops.detach(&name, attachment);
    }

    #[cfg(unix)]
    #[test]
    fn heal_quarantines_a_control_byte_in_a_document_name() {
        let f = Fixture::new("quarantine-control-byte");
        fs::write(f.vault().join("ok.md"), "ok").unwrap();
        if !write_or_report(&f.vault().join("bad\nname.md"), b"body") {
            return;
        }
        let (ops, name) = f.ops(2);
        let progress = ProgressReporter::disconnected();
        let mut attachment = ops.attach(&f.registration(), &progress).unwrap();

        assert_eq!(stored_paths(&mut attachment.store), ["ok.md"]);
        assert_eq!(
            findings_at(&mut attachment.store, "bad\u{fffd}name.md")
                .iter()
                .map(|finding| finding.kind.as_str())
                .collect::<Vec<_>>(),
            ["document/path-names-no-document"]
        );
        ops.detach(&name, attachment);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn heal_quarantines_a_non_utf8_document_name_instead_of_aliasing_it_lossily() {
        use std::os::unix::ffi::OsStringExt;

        let f = Fixture::new("quarantine-non-utf8-name");
        fs::write(f.vault().join("ok.md"), "ok").unwrap();
        let bad = std::ffi::OsString::from_vec(b"bad-\xff.md".to_vec());
        if !write_or_report(&f.vault().join(bad), b"body") {
            return;
        }
        let (ops, name) = f.ops(2);
        let progress = ProgressReporter::disconnected();
        let mut attachment = ops.attach(&f.registration(), &progress).unwrap();

        assert_eq!(stored_paths(&mut attachment.store), ["ok.md"]);
        assert_eq!(
            findings_at(&mut attachment.store, "bad-\u{fffd}.md")
                .iter()
                .map(|finding| finding.kind.as_str())
                .collect::<Vec<_>>(),
            ["document/path-bytes-not-utf8"]
        );
        ops.detach(&name, attachment);
    }

    #[test]
    fn a_document_that_stops_decoding_loses_its_row_to_the_finding_that_replaces_it() {
        let f = Fixture::new("quarantine-prune");
        fs::write(f.vault().join("note.md"), "before").unwrap();
        fs::write(f.vault().join("steady.md"), "steady").unwrap();
        let (ops, name) = f.ops(2);
        let progress = ProgressReporter::disconnected();
        let mut attachment = ops.attach(&f.registration(), &progress).unwrap();
        assert_eq!(
            stored_paths(&mut attachment.store),
            ["note.md", "steady.md"]
        );

        fs::write(f.vault().join("note.md"), UNDECODABLE).unwrap();
        ops.reconcile(
            &name,
            &mut attachment,
            ReconcileWork {
                batch: norn_fs::Batch::rescan(RescanScope::Vault),
            },
            &progress,
        )
        .unwrap();

        assert_eq!(stored_paths(&mut attachment.store), ["steady.md"]);
        assert_eq!(
            findings_at(&mut attachment.store, "note.md")
                .iter()
                .map(|finding| finding.kind.as_str())
                .collect::<Vec<_>>(),
            ["document/body-bytes-not-utf8"]
        );
        // The row died and the file did not, and the tombstone says which:
        // reading this back as a prune or a removal would say the document left
        // the vault.
        assert_eq!(
            attachment
                .store
                .begin_request()
                .stored_tombstone(&DocumentPath::new("note.md").unwrap())
                .unwrap()
                .expect("a tombstone")
                .provenance,
            Provenance::Quarantine
        );
        assert!(f.vault().join("note.md").exists());
        ops.detach(&name, attachment);
    }

    #[test]
    fn a_quarantined_document_that_reads_again_clears_its_finding_and_lands_its_facts() {
        let f = Fixture::new("quarantine-recovery");
        fs::write(f.vault().join("note.md"), UNDECODABLE).unwrap();
        let (ops, name) = f.ops(2);
        let progress = ProgressReporter::disconnected();
        let mut attachment = ops.attach(&f.registration(), &progress).unwrap();
        assert_eq!(findings_at(&mut attachment.store, "note.md").len(), 1);

        fs::write(f.vault().join("note.md"), "# readable\n").unwrap();
        scoped_increment(
            &mut attachment.store,
            f.vault().as_path(),
            &dirty_path(f.vault().as_path(), "note.md"),
            ProductionPolicy::new(2, 2).unwrap(),
            &progress.healing(),
            &exclusions(&attachment.registration, &attachment._shadows),
        )
        .unwrap();

        assert_eq!(stored_paths(&mut attachment.store), ["note.md"]);
        assert!(findings_at(&mut attachment.store, "note.md").is_empty());
        assert_eq!(finding_total(&mut attachment.store), 0);
        ops.detach(&name, attachment);
    }

    #[test]
    fn a_scoped_increment_quarantines_an_undecodable_document_and_derives_the_rest() {
        let f = Fixture::new("quarantine-scoped");
        fs::write(f.vault().join("steady.md"), "steady").unwrap();
        let (ops, name) = f.ops(2);
        let progress = ProgressReporter::disconnected();
        let mut attachment = ops.attach(&f.registration(), &progress).unwrap();

        fs::write(f.vault().join("bad.md"), UNDECODABLE).unwrap();
        fs::write(f.vault().join("fresh.md"), "fresh").unwrap();
        let mut dirty = dirty_path(f.vault().as_path(), "bad.md");
        dirty.extend(dirty_path(f.vault().as_path(), "fresh.md"));
        scoped_increment(
            &mut attachment.store,
            f.vault().as_path(),
            &dirty,
            ProductionPolicy::new(2, 2).unwrap(),
            &progress.healing(),
            &exclusions(&attachment.registration, &attachment._shadows),
        )
        .unwrap();

        assert_eq!(
            stored_paths(&mut attachment.store),
            ["fresh.md", "steady.md"]
        );
        assert_eq!(
            findings_at(&mut attachment.store, "bad.md")
                .iter()
                .map(|finding| finding.kind.as_str())
                .collect::<Vec<_>>(),
            ["document/body-bytes-not-utf8"]
        );
        ops.detach(&name, attachment);
    }

    #[test]
    fn a_scoped_subtree_heal_quarantines_an_undecodable_document_and_derives_the_rest() {
        let f = Fixture::new("quarantine-subtree");
        fs::write(f.vault().join("steady.md"), "steady").unwrap();
        let (ops, name) = f.ops(2);
        let progress = ProgressReporter::disconnected();
        let mut attachment = ops.attach(&f.registration(), &progress).unwrap();

        fs::create_dir_all(f.vault().join("folder")).unwrap();
        fs::write(f.vault().join("folder/ok.md"), "ok").unwrap();
        fs::write(f.vault().join("folder/bad.md"), UNDECODABLE).unwrap();
        scoped_increment(
            &mut attachment.store,
            f.vault().as_path(),
            &dirty_path(f.vault().as_path(), "folder"),
            ProductionPolicy::new(2, 2).unwrap(),
            &progress.healing(),
            &exclusions(&attachment.registration, &attachment._shadows),
        )
        .unwrap();

        assert_eq!(
            stored_paths(&mut attachment.store),
            ["folder/ok.md", "steady.md"]
        );
        assert_eq!(
            findings_at(&mut attachment.store, "folder/bad.md")
                .iter()
                .map(|finding| finding.kind.as_str())
                .collect::<Vec<_>>(),
            ["document/body-bytes-not-utf8"]
        );
        ops.detach(&name, attachment);
    }

    /// **The bar on what a scoped heal reads.** Exclusion is a fact about the
    /// vault, so a heal narrowed to a subtree reads exactly the documents the
    /// vault heal reads under that subtree.
    ///
    /// The forbidden shape is answering membership against the scope. A vault
    /// directory holding a `.norn/tmp` of its own is ordinary content: the vault
    /// heal derives it and the watcher reports on it, so a scoped heal that read
    /// it as the mechanism root would prune live rows the next watcher event
    /// puts straight back, and the two would trade the vault back and forth.
    #[test]
    fn a_scoped_subtree_heal_keeps_the_documents_the_vault_heal_holds_under_it() {
        let f = Fixture::new("scoped-subtree-mechanism-lookalike");
        fs::create_dir_all(f.vault().join("folder/.norn/tmp")).unwrap();
        fs::write(f.vault().join("folder/.norn/tmp/live.md"), "live").unwrap();
        fs::write(f.vault().join("folder/ok.md"), "ok").unwrap();
        let (ops, name) = f.ops(2);
        let progress = ProgressReporter::disconnected();
        let mut attachment = ops.attach(&f.registration(), &progress).unwrap();
        let vault_heal = stored_paths(&mut attachment.store);
        assert_eq!(vault_heal, ["folder/.norn/tmp/live.md", "folder/ok.md"]);

        scoped_increment(
            &mut attachment.store,
            f.vault().as_path(),
            &dirty_path(f.vault().as_path(), "folder"),
            ProductionPolicy::new(2, 2).unwrap(),
            &progress.healing(),
            &exclusions(&attachment.registration, &attachment._shadows),
        )
        .unwrap();

        assert_eq!(stored_paths(&mut attachment.store), vault_heal);
        ops.detach(&name, attachment);
    }

    /// **The bar on the other direction.** The vault's own fallback root is cut
    /// for a scoped heal rooted at its parent, without the host naming it: this
    /// placement stages outside the vault and still excludes nothing here.
    ///
    /// The forbidden shape is a scoped heal reading staged bytes as documents.
    /// A watcher event on `<vault>/.norn` schedules a heal rooted there, and the
    /// vault heal holds no row for anything under `.norn/tmp`.
    #[test]
    fn a_scoped_subtree_heal_rooted_at_the_mechanism_parent_still_cuts_it() {
        let f = Fixture::new("scoped-subtree-mechanism-root");
        let staged = f.vault().join(norn_fs::FALLBACK).join("norn-dev/notes");
        fs::create_dir_all(&staged).unwrap();
        fs::write(staged.join("shadow.md"), "staged bytes").unwrap();
        fs::write(f.vault().join(".norn/notes.md"), "a document").unwrap();
        let (ops, name) = f.ops(2);
        let progress = ProgressReporter::disconnected();
        let mut attachment = ops.attach(&f.registration(), &progress).unwrap();
        assert_eq!(
            attachment._shadows.placement(),
            Placement::DataRoot,
            "this fixture stages outside the vault, so no host root names `.norn/tmp`"
        );
        let vault_heal = stored_paths(&mut attachment.store);
        assert_eq!(vault_heal, [".norn/notes.md"]);

        scoped_increment(
            &mut attachment.store,
            f.vault().as_path(),
            &dirty_path(f.vault().as_path(), ".norn"),
            ProductionPolicy::new(2, 2).unwrap(),
            &progress.healing(),
            &exclusions(&attachment.registration, &attachment._shadows),
        )
        .unwrap();

        assert_eq!(stored_paths(&mut attachment.store), vault_heal);
        ops.detach(&name, attachment);
    }

    /// **The bar on a dirty root named through a link.** A watcher backend that
    /// follows links reports paths through one, and a directory it names is
    /// still a directory to `path_kind`. The heal it schedules converges on the
    /// rows the vault heal holds, which under a link is none.
    ///
    /// The forbidden shape is deriving documents there. The vault walk enters no
    /// link, so every such document would be a row the next vault heal prunes,
    /// and its bytes cannot be read back through the vault root at all — the
    /// increment would fail the reconcile again on every event for that root.
    #[cfg(unix)]
    #[test]
    fn a_scoped_subtree_heal_named_through_a_link_reads_what_the_vault_heal_reads() {
        use std::os::unix::fs::symlink;

        let f = Fixture::new("scoped-subtree-linked-ancestor");
        fs::create_dir_all(f.vault().join("real/sub")).unwrap();
        fs::write(f.vault().join("real/sub/doc.md"), "doc").unwrap();
        symlink("real", f.vault().join("link")).unwrap();
        let (ops, name) = f.ops(2);
        let progress = ProgressReporter::disconnected();
        let mut attachment = ops.attach(&f.registration(), &progress).unwrap();
        let vault_heal = stored_paths(&mut attachment.store);
        assert_eq!(vault_heal, ["real/sub/doc.md"]);

        scoped_increment(
            &mut attachment.store,
            f.vault().as_path(),
            &dirty_path(f.vault().as_path(), "link/sub"),
            ProductionPolicy::new(2, 2).unwrap(),
            &progress.healing(),
            &exclusions(&attachment.registration, &attachment._shadows),
        )
        .expect("a dirty root behind a link to converge rather than fail the reconcile");

        assert_eq!(stored_paths(&mut attachment.store), vault_heal);
        ops.detach(&name, attachment);
    }

    /// **The bar on a dirty file named through a link.** The file case of the
    /// same rule: the warm read resolves a dirty path from the vault root one
    /// component at a time, so a spelling that only resolves through a link
    /// reaches nothing and the increment converges on absence.
    ///
    /// The forbidden shape is an absolute join handed to the kernel. That
    /// follows every intermediate name, reads the file the link points at, and
    /// derives a row at a spelling the vault walk never yields — which the next
    /// vault heal prunes, so the two halves oscillate for as long as the link
    /// and the events naming it are there.
    ///
    /// Absence is the whole answer, and a tombstone is asserted against for
    /// that reason: a watcher that resolves links reports every edit under one
    /// through it, and a death per edit would record the removal of documents
    /// this vault never held, at paths no heal ever reaches to prune.
    #[cfg(unix)]
    #[test]
    fn a_scoped_increment_of_a_file_named_through_a_link_derives_no_row() {
        use std::os::unix::fs::symlink;

        let f = Fixture::new("scoped-file-linked-ancestor");
        fs::create_dir_all(f.vault().join("real/sub")).unwrap();
        fs::write(f.vault().join("real/sub/doc.md"), "doc").unwrap();
        symlink("real", f.vault().join("link")).unwrap();
        let (ops, name) = f.ops(2);
        let progress = ProgressReporter::disconnected();
        let mut attachment = ops.attach(&f.registration(), &progress).unwrap();
        let vault_heal = stored_paths(&mut attachment.store);
        assert_eq!(vault_heal, ["real/sub/doc.md"]);

        scoped_increment(
            &mut attachment.store,
            f.vault().as_path(),
            &dirty_path(f.vault().as_path(), "link/sub/doc.md"),
            ProductionPolicy::new(2, 2).unwrap(),
            &progress.healing(),
            &exclusions(&attachment.registration, &attachment._shadows),
        )
        .expect("a dirty file behind a link to converge rather than fail the reconcile");

        assert_eq!(stored_paths(&mut attachment.store), vault_heal);
        assert!(
            attachment
                .store
                .begin_request()
                .stored_tombstone(&DocumentPath::new("link/sub/doc.md").unwrap())
                .unwrap()
                .is_none(),
            "a death was recorded at a spelling that never held a document"
        );
        ops.detach(&name, attachment);
    }

    /// **The bar on the schema open.** The schema is read through the same
    /// contained open documents are, so a name that is not a regular file is
    /// refused rather than waited on. A FIFO holds an ordinary `open` until
    /// somebody writes to the pipe, and the worker that reaches it is the
    /// lifecycle's own: an attach that never returns is an entry that never
    /// becomes ready and a host that cannot be asked why.
    ///
    /// The case is bounded rather than assertion-only, because the failure it
    /// guards against is not a wrong answer but no answer at all.
    #[cfg(unix)]
    #[test]
    fn an_attach_whose_schema_name_is_a_pipe_refuses_instead_of_waiting() {
        let f = Fixture::new("schema-pipe");
        let schema = f.vault().join(IN_VAULT_SCHEMA_PATH);
        fs::remove_file(&schema).unwrap();
        let made = std::process::Command::new("mkfifo")
            .arg(&schema)
            .status()
            .unwrap();
        assert!(made.success(), "mkfifo failed");

        let (sender, receiver) = std::sync::mpsc::channel();
        let root = f.root.clone();
        let vault = f.vault();
        thread::spawn(move || {
            let dirs = ConfigDirs::new(root.join("config"), root.join("data")).unwrap();
            let ops = ProductionEntryOps::new(dirs, ProductionPolicy::new(2, 2).unwrap());
            let registration = Registration::new(
                VaultName::new("notes").unwrap(),
                VaultRoot::new(vault).unwrap(),
            );
            let attached = ops.attach(&registration, &ProgressReporter::disconnected());
            let outcome = match attached {
                Ok(attachment) => {
                    ops.detach(&registration.name, attachment);
                    Err("a pipe was accepted as schema bytes".to_owned())
                }
                Err(failure) => Ok(format!("{failure:?}")),
            };
            let _ = sender.send(outcome);
        });

        let outcome = receiver
            .recv_timeout(lifecycle_budget().work())
            .expect("the attach never returned: a pipe at the schema name held it inside open")
            .expect("a pipe is not schema bytes");
        assert!(
            outcome.contains("regular file"),
            "the refusal does not name what is wrong with the schema: {outcome}"
        );
    }

    /// **The bar on a link at a name norn appends.** The default schema is the
    /// vault root plus two names this crate chose, and both of those are read
    /// through the vault's own directories: a link at either is refused, and the
    /// refusal names which one, because that is the name an operator has to fix.
    ///
    /// This is a statement about the vault rather than about the operator's
    /// registry, and the two differ deliberately — a shared schema is spelled as
    /// a `schema_source`, where the whole path is the operator's own and is read
    /// as written. The forbidden shape is following the link: a file inside a
    /// synced, multi-writer vault would then decide what every document in it
    /// means.
    #[cfg(unix)]
    #[test]
    fn an_attach_whose_default_schema_name_is_a_link_refuses_and_names_it() {
        use std::os::unix::fs::symlink;

        let f = Fixture::new("schema-name-link");
        fs::create_dir_all(f.root.join("shared")).unwrap();
        fs::write(f.root.join("shared/schema.yaml"), "version: 1\n").unwrap();
        let schema = f.vault().join(IN_VAULT_SCHEMA_PATH);
        fs::remove_file(&schema).unwrap();
        symlink(f.root.join("shared/schema.yaml"), &schema).unwrap();

        let (ops, _) = f.ops(2);
        let failure = ops
            .attach(&f.registration(), &ProgressReporter::disconnected())
            .err()
            .expect("a link at the schema name is not schema bytes");
        let stated = format!("{failure:?}");
        assert!(
            stated.contains("symbolic link: schema.yaml"),
            "the refusal does not name the link or the name it is at: {stated}"
        );
    }

    /// **The bar on a link at the directory norn appends.** The same rule one
    /// component higher: `.norn` is a name this crate appends to the operator's
    /// root, so it is a directory inside the vault or the attach refuses. A
    /// descent that followed it would read a schema from a tree the vault root
    /// does not contain.
    #[cfg(unix)]
    #[test]
    fn an_attach_whose_default_schema_directory_is_a_link_refuses_and_names_it() {
        use std::os::unix::fs::symlink;

        let f = Fixture::new("schema-directory-link");
        fs::create_dir_all(f.root.join("shared")).unwrap();
        fs::write(f.root.join("shared/schema.yaml"), "version: 1\n").unwrap();
        fs::remove_dir_all(f.vault().join(".norn")).unwrap();
        symlink(f.root.join("shared"), f.vault().join(".norn")).unwrap();

        let (ops, _) = f.ops(2);
        let failure = ops
            .attach(&f.registration(), &ProgressReporter::disconnected())
            .err()
            .expect("a link at the schema directory is not a way into the vault");
        let stated = format!("{failure:?}");
        assert!(
            stated.contains("symbolic link: .norn"),
            "the refusal does not name the link or the name it is at: {stated}"
        );
    }

    /// **The bar on a configured source that is a link.** The registry's path is
    /// the operator's, so the directory holding it is the anchor — and the
    /// schema is the file at the name they wrote, never what a link at that name
    /// points to. A link there has this process read one file while the watcher
    /// covering the configured name reports on another.
    ///
    /// The refusal is the seam's, and this is the case that keeps it reachable:
    /// a later change that canonicalized the source before splitting it would
    /// restore link-following with every other case still green.
    #[cfg(unix)]
    #[test]
    fn an_attach_whose_configured_schema_source_is_a_link_refuses_and_names_it() {
        use std::os::unix::fs::symlink;

        let f = Fixture::new("schema-source-link");
        fs::create_dir_all(f.root.join("shared")).unwrap();
        fs::write(f.root.join("shared/schema.yaml"), "version: 1\n").unwrap();
        let configured = f.root.join("linked-schema.yaml");
        symlink(f.root.join("shared/schema.yaml"), &configured).unwrap();

        let mut registration = f.registration();
        registration.schema_source = Some(SchemaSource::new(configured).unwrap());
        let (ops, _) = f.ops(2);
        let failure = ops
            .attach(&registration, &ProgressReporter::disconnected())
            .err()
            .expect("a link at the configured schema name is not schema bytes");
        let stated = format!("{failure:?}");
        assert!(
            stated.contains("symbolic link: linked-schema.yaml"),
            "the refusal does not name the link or the name it is at: {stated}"
        );
    }

    /// The subtree heal reads paths through the same seam the vault heal does,
    /// so a spelling the grammar refuses inside a scope is quarantined there
    /// too.
    #[cfg(unix)]
    #[test]
    fn a_scoped_subtree_heal_quarantines_a_path_spelling_the_grammar_refuses() {
        let f = Fixture::new("quarantine-subtree-spelling");
        let (ops, name) = f.ops(2);
        let progress = ProgressReporter::disconnected();
        let mut attachment = ops.attach(&f.registration(), &progress).unwrap();

        fs::create_dir_all(f.vault().join("folder")).unwrap();
        fs::write(f.vault().join("folder/ok.md"), "ok").unwrap();
        if !write_or_report(&f.vault().join("folder/bad\\name.md"), b"body") {
            return;
        }
        scoped_increment(
            &mut attachment.store,
            f.vault().as_path(),
            &dirty_path(f.vault().as_path(), "folder"),
            ProductionPolicy::new(2, 2).unwrap(),
            &progress.healing(),
            &exclusions(&attachment.registration, &attachment._shadows),
        )
        .unwrap();

        assert_eq!(stored_paths(&mut attachment.store), ["folder/ok.md"]);
        assert_eq!(
            findings_at(&mut attachment.store, "folder/bad\u{fffd}name.md")
                .iter()
                .map(|finding| finding.kind.as_str())
                .collect::<Vec<_>>(),
            ["document/path-names-no-document"]
        );
        ops.detach(&name, attachment);
    }

    /// The warm path reads a dirty file's identity through the same seam, and a
    /// file that is on disk under a spelling the grammar refuses is a document
    /// to quarantine.
    #[cfg(unix)]
    #[test]
    fn a_scoped_increment_quarantines_a_path_spelling_the_grammar_refuses() {
        let f = Fixture::new("quarantine-scoped-spelling");
        fs::write(f.vault().join("steady.md"), "steady").unwrap();
        let (ops, name) = f.ops(2);
        let progress = ProgressReporter::disconnected();
        let mut attachment = ops.attach(&f.registration(), &progress).unwrap();

        if !write_or_report(&f.vault().join("bad\\name.md"), b"body") {
            return;
        }
        scoped_increment(
            &mut attachment.store,
            f.vault().as_path(),
            &dirty_path(f.vault().as_path(), "bad\\name.md"),
            ProductionPolicy::new(2, 2).unwrap(),
            &progress.healing(),
            &exclusions(&attachment.registration, &attachment._shadows),
        )
        .unwrap();

        assert_eq!(stored_paths(&mut attachment.store), ["steady.md"]);
        assert_eq!(
            findings_at(&mut attachment.store, "bad\u{fffd}name.md")
                .iter()
                .map(|finding| finding.kind.as_str())
                .collect::<Vec<_>>(),
            ["document/path-names-no-document"]
        );
        ops.detach(&name, attachment);
    }

    /// A finding is about a document, and a path that is not on disk is not
    /// one: a created-then-deleted spelling the grammar refuses leaves nothing
    /// behind for somebody to go looking for.
    #[test]
    fn a_watcher_event_for_a_refused_spelling_that_is_gone_files_no_finding() {
        let f = Fixture::new("quarantine-ghost");
        fs::write(f.vault().join("steady.md"), "steady").unwrap();
        let (ops, name) = f.ops(2);
        let progress = ProgressReporter::disconnected();
        let mut attachment = ops.attach(&f.registration(), &progress).unwrap();

        scoped_increment(
            &mut attachment.store,
            f.vault().as_path(),
            &dirty_path(f.vault().as_path(), "bad\\name.md"),
            ProductionPolicy::new(2, 2).unwrap(),
            &progress.healing(),
            &exclusions(&attachment.registration, &attachment._shadows),
        )
        .unwrap();

        assert_eq!(stored_paths(&mut attachment.store), ["steady.md"]);
        assert_eq!(finding_total(&mut attachment.store), 0);
        ops.detach(&name, attachment);
    }

    /// A directory the grammar refuses addresses no subtree the store can
    /// range, so the warm path converges it by reading what is under it: every
    /// document beneath is derived or quarantined, exactly as the vault heal
    /// reaches the same files.
    #[cfg(unix)]
    #[test]
    fn a_scoped_increment_converges_a_directory_the_grammar_refuses() {
        let f = Fixture::new("quarantine-refused-directory");
        let (ops, name) = f.ops(2);
        let progress = ProgressReporter::disconnected();
        let mut attachment = ops.attach(&f.registration(), &progress).unwrap();

        if fs::create_dir_all(f.vault().join("bad\\dir")).is_err() {
            eprintln!("skipped: this filesystem does not create a directory named `bad\\dir`");
            return;
        }
        fs::write(f.vault().join("bad\\dir/note.md"), "body").unwrap();
        scoped_increment(
            &mut attachment.store,
            f.vault().as_path(),
            &dirty_path(f.vault().as_path(), "bad\\dir"),
            ProductionPolicy::new(2, 2).unwrap(),
            &progress.healing(),
            &exclusions(&attachment.registration, &attachment._shadows),
        )
        .unwrap();

        assert!(stored_paths(&mut attachment.store).is_empty());
        assert_eq!(
            findings_at(&mut attachment.store, "bad\u{fffd}dir/note.md")
                .iter()
                .map(|finding| finding.kind.as_str())
                .collect::<Vec<_>>(),
            ["document/path-names-no-document"]
        );

        // And the vault heal reaches the same document with the same finding,
        // which is the parity a scope is supposed to hold.
        ops.reconcile(
            &name,
            &mut attachment,
            ReconcileWork {
                batch: norn_fs::Batch::rescan(RescanScope::Vault),
            },
            &progress,
        )
        .unwrap();
        assert_eq!(
            findings_at(&mut attachment.store, "bad\u{fffd}dir/note.md").len(),
            1
        );
        ops.detach(&name, attachment);
    }

    /// A directory whose own leaf reduces to a stem the grammar refuses still
    /// holds documents the grammar admits, and the warm path derives them.
    #[test]
    fn a_scoped_increment_derives_documents_under_a_directory_with_a_refused_stem() {
        let f = Fixture::new("quarantine-refused-stem-directory");
        let (ops, name) = f.ops(2);
        let progress = ProgressReporter::disconnected();
        let mut attachment = ops.attach(&f.registration(), &progress).unwrap();

        fs::create_dir_all(f.vault().join("..md")).unwrap();
        fs::write(f.vault().join("..md/note.md"), "body").unwrap();
        scoped_increment(
            &mut attachment.store,
            f.vault().as_path(),
            &dirty_path(f.vault().as_path(), "..md"),
            ProductionPolicy::new(2, 2).unwrap(),
            &progress.healing(),
            &exclusions(&attachment.registration, &attachment._shadows),
        )
        .unwrap();

        assert_eq!(stored_paths(&mut attachment.store), ["..md/note.md"]);
        assert_eq!(finding_total(&mut attachment.store), 0);

        // And one of them that stops decoding loses its row here as it would
        // anywhere else: the scope has no merge, so the row it prunes is the one
        // it looked up.
        fs::write(f.vault().join("..md/note.md"), UNDECODABLE).unwrap();
        scoped_increment(
            &mut attachment.store,
            f.vault().as_path(),
            &dirty_path(f.vault().as_path(), "..md"),
            ProductionPolicy::new(2, 2).unwrap(),
            &progress.healing(),
            &exclusions(&attachment.registration, &attachment._shadows),
        )
        .unwrap();
        assert!(stored_paths(&mut attachment.store).is_empty());
        assert_eq!(
            findings_at(&mut attachment.store, "..md/note.md")
                .iter()
                .map(|finding| finding.kind.as_str())
                .collect::<Vec<_>>(),
            ["document/body-bytes-not-utf8"]
        );
        ops.detach(&name, attachment);
    }

    /// **A live directory that names no document converges its deletions too.**
    /// The scoped heal of `..md` merges its walk against the rows the prefix
    /// addresses, so a child deleted under it loses its row there rather than
    /// waiting for a vault-wide heal — whether the dirty path is the child or
    /// the directory that held it.
    #[test]
    fn a_scoped_increment_prunes_a_child_deleted_under_a_live_refused_stem_directory() {
        let f = Fixture::new("quarantine-refused-stem-child-deletion");
        let (ops, name) = f.ops(2);
        let progress = ProgressReporter::disconnected();
        let mut attachment = ops.attach(&f.registration(), &progress).unwrap();

        fs::create_dir_all(f.vault().join("..md")).unwrap();
        fs::write(f.vault().join("..md/note.md"), "body").unwrap();
        fs::write(f.vault().join("..md/gone.md"), "gone").unwrap();
        fs::write(f.vault().join("keep.md"), "keep").unwrap();
        ops.reconcile(
            &name,
            &mut attachment,
            ReconcileWork {
                batch: norn_fs::Batch::rescan(RescanScope::Vault),
            },
            &progress,
        )
        .unwrap();
        assert_eq!(
            stored_paths(&mut attachment.store),
            ["..md/gone.md", "..md/note.md", "keep.md"]
        );

        // The directory is still there; only the child left. The dirty path the
        // watcher reports for that is the directory that held it.
        fs::remove_file(f.vault().join("..md/gone.md")).unwrap();
        scoped_increment(
            &mut attachment.store,
            f.vault().as_path(),
            &dirty_path(f.vault().as_path(), "..md"),
            ProductionPolicy::new(2, 2).unwrap(),
            &progress.healing(),
            &exclusions(&attachment.registration, &attachment._shadows),
        )
        .unwrap();

        assert_eq!(
            stored_paths(&mut attachment.store),
            ["..md/note.md", "keep.md"]
        );
        assert_eq!(finding_total(&mut attachment.store), 0);

        // And naming the child itself converges the same row through the scope
        // its own spelling addresses.
        fs::write(f.vault().join("..md/second.md"), "second").unwrap();
        scoped_increment(
            &mut attachment.store,
            f.vault().as_path(),
            &dirty_path(f.vault().as_path(), "..md/second.md"),
            ProductionPolicy::new(2, 2).unwrap(),
            &progress.healing(),
            &exclusions(&attachment.registration, &attachment._shadows),
        )
        .unwrap();
        assert_eq!(
            stored_paths(&mut attachment.store),
            ["..md/note.md", "..md/second.md", "keep.md"]
        );

        fs::remove_file(f.vault().join("..md/second.md")).unwrap();
        scoped_increment(
            &mut attachment.store,
            f.vault().as_path(),
            &dirty_path(f.vault().as_path(), "..md/second.md"),
            ProductionPolicy::new(2, 2).unwrap(),
            &progress.healing(),
            &exclusions(&attachment.registration, &attachment._shadows),
        )
        .unwrap();
        assert_eq!(
            stored_paths(&mut attachment.store),
            ["..md/note.md", "keep.md"]
        );
        assert_eq!(finding_total(&mut attachment.store), 0);
        ops.detach(&name, attachment);
    }

    /// **A directory that names no document is still where documents live, so
    /// its deletion has rows to take.** `..md` reduces to a refused stem while
    /// `..md/note.md` is an ordinary stored path, and leaving those rows behind
    /// would serve a deleted document until the next vault-wide heal. The warm
    /// path prunes them through the prefix instead.
    #[test]
    fn a_scoped_increment_prunes_a_deleted_directory_with_a_refused_stem() {
        let f = Fixture::new("quarantine-refused-stem-deletion");
        let (ops, name) = f.ops(2);
        let progress = ProgressReporter::disconnected();
        let mut attachment = ops.attach(&f.registration(), &progress).unwrap();

        fs::create_dir_all(f.vault().join("..md")).unwrap();
        fs::write(f.vault().join("..md/note.md"), "body").unwrap();
        fs::write(f.vault().join("keep.md"), "keep").unwrap();
        ops.reconcile(
            &name,
            &mut attachment,
            ReconcileWork {
                batch: norn_fs::Batch::rescan(RescanScope::Vault),
            },
            &progress,
        )
        .unwrap();
        assert_eq!(
            stored_paths(&mut attachment.store),
            ["..md/note.md", "keep.md"]
        );

        fs::remove_dir_all(f.vault().join("..md")).unwrap();
        scoped_increment(
            &mut attachment.store,
            f.vault().as_path(),
            &dirty_path(f.vault().as_path(), "..md"),
            ProductionPolicy::new(2, 2).unwrap(),
            &progress.healing(),
            &exclusions(&attachment.registration, &attachment._shadows),
        )
        .unwrap();

        assert_eq!(stored_paths(&mut attachment.store), ["keep.md"]);
        assert_eq!(finding_total(&mut attachment.store), 0);
        ops.detach(&name, attachment);
    }

    struct CountedAttach {
        inner: ProductionEntryOps,
        attaches: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    }

    impl EntryOps for CountedAttach {
        type Attachment = ProductionAttachment;

        fn rebuild(
            &self,
            name: &VaultName,
            attachment: Self::Attachment,
            progress: &ProgressReporter<Self::Attachment>,
        ) -> Result<Self::Attachment, JobFailure> {
            self.inner.rebuild(name, attachment, progress)
        }

        fn attach(
            &self,
            registration: &Registration,
            progress: &ProgressReporter<Self::Attachment>,
        ) -> Result<Self::Attachment, JobFailure> {
            self.attaches
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            self.inner.attach(registration, progress)
        }

        fn reconcile(
            &self,
            name: &VaultName,
            attachment: &mut Self::Attachment,
            work: ReconcileWork,
            progress: &ProgressReporter<Self::Attachment>,
        ) -> Result<(), JobFailure> {
            self.inner.reconcile(name, attachment, work, progress)
        }

        fn recover(
            &self,
            name: &VaultName,
            attachment: &mut Self::Attachment,
            progress: &ProgressReporter<Self::Attachment>,
        ) -> Result<(), JobFailure> {
            self.inner.recover(name, attachment, progress)
        }

        fn poll(
            &self,
            name: &VaultName,
            attachment: &mut Self::Attachment,
        ) -> Result<Option<norn_fs::Batch>, JobFailure> {
            self.inner.poll(name, attachment)
        }

        fn detach(&self, name: &VaultName, attachment: Self::Attachment) {
            self.inner.detach(name, attachment);
        }
    }

    #[test]
    fn a_vault_holding_undecodable_documents_reaches_ready_and_re_attaches_for_none_of_them() {
        let f = Fixture::new("quarantine-ready");
        for index in 0..3 {
            fs::write(f.vault().join(format!("bad-{index}.md")), UNDECODABLE).unwrap();
            fs::write(f.vault().join(format!("ok-{index}.md")), "ok").unwrap();
        }
        let name = VaultName::new("notes").unwrap();
        let entry = Registration::new(name.clone(), VaultRoot::new(f.vault()).unwrap());
        let registry = crate::RegistryRead::from_entries([entry.clone()]);
        let dirs = ConfigDirs::new(f.root.join("config"), f.root.join("data")).unwrap();
        let derived = dirs.derived_dir(&name);
        let attaches = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let host = crate::Host::new(
            registry,
            CountedAttach {
                inner: ProductionEntryOps::new(dirs, ProductionPolicy::new(2, 2).unwrap()),
                attaches: std::sync::Arc::clone(&attaches),
            },
            crate::LifecyclePolicy {
                idle_after: Duration::from_secs(60),
                worker_slots: 1,
                watch_poll_interval: Duration::from_millis(2),
            },
        )
        .unwrap();

        drop(host.demand(&name, AttachMode::Durable).unwrap());
        wait_state(&host, &name, norn_wire::TrustState::Ready);
        // A demand against a ready entry schedules nothing, which is the whole
        // difference from an entry left untrusted by one bad document.
        let second = host.demand(&name, AttachMode::Durable).unwrap();
        assert!(matches!(
            second.outcome(),
            crate::Demand::State(norn_wire::TrustState::Ready)
        ));
        drop(second);
        wait_state(&host, &name, norn_wire::TrustState::Ready);
        assert_eq!(attaches.load(std::sync::atomic::Ordering::SeqCst), 1);
        drop(host);

        let mut store = Store::open(derived.join("store.sqlite3")).unwrap();
        assert_eq!(
            stored_paths(&mut store),
            ["ok-0.md", "ok-1.md", "ok-2.md"],
            "every readable document is served"
        );
        assert_eq!(finding_total(&mut store), 3);
    }

    #[test]
    fn scoped_case_only_file_rename_replaces_the_stored_spelling() {
        let f = Fixture::new("case-only-file-rename");
        if norn_fs::PathNormalizer::detect(&f.vault())
            .unwrap()
            .case_sensitivity()
            != norn_fs::CaseSensitivity::Insensitive
        {
            return;
        }
        fs::write(f.vault().join("note.md"), "body").unwrap();
        let (ops, name) = f.ops(2);
        let progress = ProgressReporter::disconnected();
        let mut attachment = ops.attach(&f.registration(), &progress).unwrap();
        fs::rename(f.vault().join("note.md"), f.vault().join("NOTE.md")).unwrap();

        scoped_increment(
            &mut attachment.store,
            f.vault().as_path(),
            &dirty_path(f.vault().as_path(), "NOTE.md"),
            ProductionPolicy::new(2, 2).unwrap(),
            &progress.healing(),
            &exclusions(&attachment.registration, &attachment._shadows),
        )
        .unwrap();

        let rows = attachment
            .store
            .begin_request()
            .stored_documents_after_ordered(None, 10, StoredPathOrder::Sensitive)
            .unwrap();
        assert_eq!(
            rows.iter().map(|row| row.path.as_str()).collect::<Vec<_>>(),
            ["NOTE.md"]
        );
        ops.detach(&name, attachment);
    }

    #[test]
    fn scoped_case_only_directory_rename_replaces_descendant_spellings() {
        let f = Fixture::new("case-only-directory-rename");
        if norn_fs::PathNormalizer::detect(&f.vault())
            .unwrap()
            .case_sensitivity()
            != norn_fs::CaseSensitivity::Insensitive
        {
            return;
        }
        fs::create_dir(f.vault().join("folder")).unwrap();
        fs::write(f.vault().join("folder/note.md"), "body").unwrap();
        let (ops, name) = f.ops(2);
        let progress = ProgressReporter::disconnected();
        let mut attachment = ops.attach(&f.registration(), &progress).unwrap();
        fs::rename(f.vault().join("folder"), f.vault().join("FOLDER")).unwrap();

        scoped_increment(
            &mut attachment.store,
            f.vault().as_path(),
            &dirty_path(f.vault().as_path(), "FOLDER"),
            ProductionPolicy::new(2, 2).unwrap(),
            &progress.healing(),
            &exclusions(&attachment.registration, &attachment._shadows),
        )
        .unwrap();

        let rows = attachment
            .store
            .begin_request()
            .stored_documents_after_ordered(None, 10, StoredPathOrder::Sensitive)
            .unwrap();
        assert_eq!(
            rows.iter().map(|row| row.path.as_str()).collect::<Vec<_>>(),
            ["FOLDER/note.md"]
        );
        ops.detach(&name, attachment);
    }

    #[test]
    fn attach_indexes_only_markdown_and_ignores_binary_clutter() {
        let f = Fixture::new("markdown-only");
        fs::write(f.vault().join("note.md"), "a note").unwrap();
        fs::write(f.vault().join("UPPER.MD"), "another note").unwrap();
        fs::write(f.vault().join("Mixed.Md"), "mixed-case note").unwrap();
        fs::write(f.vault().join("readme.txt"), "text clutter").unwrap();
        fs::write(f.vault().join("image.bin"), [0xff, 0x00, 0xfe]).unwrap();
        let (ops, name) = f.ops(2);
        let progress = ProgressReporter::disconnected();
        let mut attachment = ops.attach(&f.registration(), &progress).unwrap();
        let rows = attachment
            .store
            .begin_request()
            .stored_documents_after_ordered(None, 10, StoredPathOrder::Sensitive)
            .unwrap();
        assert_eq!(
            rows.iter().map(|row| row.path.as_str()).collect::<Vec<_>>(),
            ["Mixed.Md", "UPPER.MD", "note.md"]
        );

        fs::remove_file(f.vault().join("UPPER.MD")).unwrap();
        scoped_increment(
            &mut attachment.store,
            f.vault().as_path(),
            &dirty_path(f.vault().as_path(), "UPPER.MD"),
            ProductionPolicy::new(2, 2).unwrap(),
            &progress.healing(),
            &exclusions(&attachment.registration, &attachment._shadows),
        )
        .unwrap();
        assert!(
            attachment
                .store
                .begin_request()
                .stored_document(&DocumentPath::new("UPPER.MD").unwrap())
                .unwrap()
                .is_none()
        );
        ops.detach(&name, attachment);
    }

    #[test]
    fn subtree_prune_partitions_store_pages_by_changeset_bound() {
        let f = Fixture::new("bounded-subtree-prune");
        fs::create_dir_all(f.vault().join("folder")).unwrap();
        for index in 0..5 {
            fs::write(f.vault().join(format!("folder/{index}.md")), "body").unwrap();
        }
        let (ops, name) = f.ops(8);
        let progress = ProgressReporter::disconnected();
        let mut attachment = ops.attach(&f.registration(), &progress).unwrap();
        fs::write(f.vault().join("before.md"), "before").unwrap();
        scoped_increment(
            &mut attachment.store,
            f.vault().as_path(),
            &dirty_path(f.vault().as_path(), "before.md"),
            ProductionPolicy::new(8, 2).unwrap(),
            &progress.healing(),
            &exclusions(&attachment.registration, &attachment._shadows),
        )
        .unwrap();
        let generation_before_prune = attachment
            .store
            .begin_request()
            .stored_document(&DocumentPath::new("before.md").unwrap())
            .unwrap()
            .unwrap()
            .generation;

        fs::remove_dir_all(f.vault().join("folder")).unwrap();
        let pruned_root = DocumentPath::new("folder").unwrap();
        prune_subtree_ordered(
            &mut attachment.store,
            SubtreeScope::Subtree(&pruned_root),
            ProductionPolicy::new(8, 2).unwrap(),
            &progress.healing(),
            StoredPathOrder::Sensitive,
        )
        .unwrap();
        fs::write(f.vault().join("after.md"), "after").unwrap();
        scoped_increment(
            &mut attachment.store,
            f.vault().as_path(),
            &dirty_path(f.vault().as_path(), "after.md"),
            ProductionPolicy::new(8, 2).unwrap(),
            &progress.healing(),
            &exclusions(&attachment.registration, &attachment._shadows),
        )
        .unwrap();

        let after = attachment
            .store
            .begin_request()
            .stored_document(&DocumentPath::new("after.md").unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(
            after.generation,
            generation_before_prune + 4,
            "five deaths must use three bounded increments before the upsert"
        );
        ops.detach(&name, attachment);
    }

    #[test]
    fn directory_roots_merge_add_remove_and_rename_without_touching_siblings() {
        let f = Fixture::new("directory-roots");
        fs::create_dir_all(f.vault().join("folder")).unwrap();
        fs::write(f.vault().join("folder/a.md"), "a").unwrap();
        fs::write(f.vault().join("unrelated.md"), "steady").unwrap();
        let (ops, name) = f.ops(2);
        let progress = ProgressReporter::disconnected();
        let mut attachment = ops.attach(&f.registration(), &progress).unwrap();
        let unrelated = DocumentPath::new("unrelated.md").unwrap();
        let generation = attachment
            .store
            .begin_request()
            .stored_document(&unrelated)
            .unwrap()
            .unwrap()
            .generation;

        fs::remove_dir_all(f.vault().join("folder")).unwrap();
        reconcile_until(
            &ops,
            &name,
            &mut attachment,
            &progress,
            "the removed directory's document to be pruned",
            |attachment, _| match stored(attachment, "folder/a.md") {
                None => Observed::Met(()),
                Some(row) => Observed::pending(format!("folder/a.md still stands at {row:?}")),
            },
        );

        fs::create_dir_all(f.vault().join("source")).unwrap();
        fs::write(f.vault().join("source/b.md"), "b").unwrap();
        reconcile_until(
            &ops,
            &name,
            &mut attachment,
            &progress,
            "the added directory's document to be indexed",
            |attachment, _| match stored(attachment, "source/b.md") {
                Some(_) => Observed::Met(()),
                None => Observed::pending("source/b.md is not stored"),
            },
        );

        // A rename reaches the watcher as two facts about two paths, and the
        // platform is free to report them in two batches in either order. Both
        // halves are therefore one condition: a wait that took the destination
        // and asserted the source outside it fails whenever the source's half
        // lands in the next batch, and a wait that took the source and
        // asserted the destination fails on the other split.
        fs::rename(f.vault().join("source"), f.vault().join("renamed")).unwrap();
        reconcile_until(
            &ops,
            &name,
            &mut attachment,
            &progress,
            "both halves of the rename to land: the source pruned and the destination indexed",
            |attachment, _| {
                let source = stored(attachment, "source/b.md");
                let destination = stored(attachment, "renamed/b.md");
                if source.is_none() && destination.is_some() {
                    Observed::Met(())
                } else {
                    Observed::pending(format!(
                        "source/b.md is {}, renamed/b.md is {}",
                        if source.is_some() {
                            "still stored"
                        } else {
                            "pruned"
                        },
                        if destination.is_some() {
                            "stored"
                        } else {
                            "not stored yet"
                        }
                    ))
                }
            },
        );
        assert_eq!(
            attachment
                .store
                .begin_request()
                .stored_document(&unrelated)
                .unwrap()
                .unwrap()
                .generation,
            generation
        );
        ops.detach(&name, attachment);
    }

    #[test]
    fn missing_markdown_suffixed_root_prunes_its_former_descendants() {
        let f = Fixture::new("markdown-directory");
        fs::create_dir_all(f.vault().join("archive.md")).unwrap();
        fs::write(f.vault().join("archive.md/child.md"), "child").unwrap();
        let (ops, name) = f.ops(2);
        let progress = ProgressReporter::disconnected();
        let mut attachment = ops.attach(&f.registration(), &progress).unwrap();
        fs::remove_dir_all(f.vault().join("archive.md")).unwrap();
        reconcile_until(
            &ops,
            &name,
            &mut attachment,
            &progress,
            "the removed markdown-suffixed directory's descendant to be pruned",
            |attachment, _| match stored(attachment, "archive.md/child.md") {
                None => Observed::Met(()),
                Some(row) => {
                    Observed::pending(format!("archive.md/child.md still stands at {row:?}"))
                }
            },
        );
        ops.detach(&name, attachment);
    }

    #[test]
    fn scoped_schema_markdown_invalidation_never_indexes_the_schema() {
        let f = Fixture::new("schema-markdown");
        let schema = f.vault().join(".norn/schema.md");
        fs::write(&schema, "version: one").unwrap();
        fs::write(f.vault().join("note.md"), "note").unwrap();
        let name = VaultName::new("notes").unwrap();
        let mut registration = Registration::new(name.clone(), VaultRoot::new(f.vault()).unwrap());
        registration.schema_source = Some(SchemaSource::new(&schema).unwrap());
        let dirs = ConfigDirs::new(f.root.join("config"), f.root.join("data")).unwrap();
        let ops = ProductionEntryOps::new(dirs, ProductionPolicy::new(2, 2).unwrap());
        let progress = ProgressReporter::disconnected();
        let mut attachment = ops.attach(&registration, &progress).unwrap();
        fs::write(&schema, "version: two").unwrap();
        // The outcome here is that nothing arrives, so what the wait is for is
        // the invalidation itself: the schema edit has been reported and
        // reconciled by the time the row is read. A wait for the store to stay
        // empty would be met by the first look, before the watcher had
        // reported anything at all.
        reconcile_until(
            &ops,
            &name,
            &mut attachment,
            &progress,
            "the schema invalidation to be reported and reconciled",
            |_, absorbed| {
                if absorbed.saw(SCHEMA_INVALIDATED) {
                    Observed::Met(())
                } else {
                    Observed::pending("no batch has carried the schema invalidation")
                }
            },
        );
        assert!(stored(&mut attachment, ".norn/schema.md").is_none());
        ops.detach(&name, attachment);
    }

    #[test]
    fn one_pass_attach_completes_while_watcher_events_continue() {
        let f = Fixture::new("continuous-attach");
        for index in 0..200 {
            fs::write(f.vault().join(format!("note-{index:03}.md")), "body").unwrap();
        }
        let running = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
        let writer_running = std::sync::Arc::clone(&running);
        let hot = f.vault().join("hot.md");
        let writer = thread::spawn(move || {
            let mut generation = 0;
            while writer_running.load(std::sync::atomic::Ordering::SeqCst) {
                fs::write(&hot, generation.to_string()).unwrap();
                generation += 1;
            }
        });
        let (ops, name) = f.ops(2);
        let attachment = ops
            .attach(&f.registration(), &ProgressReporter::disconnected())
            .unwrap();
        running.store(false, std::sync::atomic::Ordering::SeqCst);
        writer.join().unwrap();
        ops.detach(&name, attachment);
    }

    /// Production ops whose reconcile can be held at its entry.
    ///
    /// A reconcile job publishes Warming before it calls in here, so a reconcile
    /// held at its entry pins the entry in that state for as long as a case wants
    /// it. **This is what makes the warming leg an observation rather than a
    /// race.** A slow reconcile in its place leaves a transient the case has to
    /// catch with a poll cadence that has no relationship to how long it lasts:
    /// the leg is published either way, and a case that misses it reports a
    /// warming state that never arrived on a path that behaved correctly.
    ///
    /// The hold is armed rather than always on, because the reconcile that
    /// follows an attach is what carries an entry to Ready in the first place,
    /// and a case has nothing to observe until it has.
    struct HeldReconcile {
        inner: ProductionEntryOps,
        armed: std::sync::Arc<std::sync::atomic::AtomicBool>,
        released: std::sync::Arc<std::sync::atomic::AtomicBool>,
    }

    impl EntryOps for HeldReconcile {
        type Attachment = ProductionAttachment;
        fn rebuild(
            &self,
            name: &VaultName,
            attachment: Self::Attachment,
            progress: &ProgressReporter<Self::Attachment>,
        ) -> Result<Self::Attachment, JobFailure> {
            self.inner.rebuild(name, attachment, progress)
        }

        fn attach(
            &self,
            registration: &Registration,
            progress: &ProgressReporter<Self::Attachment>,
        ) -> Result<Self::Attachment, JobFailure> {
            self.inner.attach(registration, progress)
        }
        fn reconcile(
            &self,
            name: &VaultName,
            attachment: &mut Self::Attachment,
            work: ReconcileWork,
            progress: &ProgressReporter<Self::Attachment>,
        ) -> Result<(), JobFailure> {
            if self.armed.load(std::sync::atomic::Ordering::SeqCst) {
                wait_until(
                    "the case to release the held reconcile",
                    lifecycle_budget(),
                    || {
                        if self.released.load(std::sync::atomic::Ordering::SeqCst) {
                            Observed::Met(())
                        } else {
                            Observed::Pending("the case has not released it".to_owned())
                        }
                    },
                )
                .map_err(|failure| environmental(failure.to_string()))?;
            }
            self.inner.reconcile(name, attachment, work, progress)
        }
        fn recover(
            &self,
            name: &VaultName,
            attachment: &mut Self::Attachment,
            progress: &ProgressReporter<Self::Attachment>,
        ) -> Result<(), JobFailure> {
            self.inner.recover(name, attachment, progress)
        }
        fn poll(
            &self,
            name: &VaultName,
            attachment: &mut Self::Attachment,
        ) -> Result<Option<norn_fs::Batch>, JobFailure> {
            self.inner.poll(name, attachment)
        }
        fn detach(&self, name: &VaultName, attachment: Self::Attachment) {
            self.inner.detach(name, attachment)
        }
    }

    #[test]
    fn external_edit_is_autonomously_pumped_through_warming_to_ready() {
        let f = Fixture::new("host-watch-edit");
        fs::write(f.vault().join("note.md"), "before").unwrap();
        let name = VaultName::new("notes").unwrap();
        let entry = Registration::new(name.clone(), VaultRoot::new(f.vault()).unwrap());
        let registry = crate::RegistryRead::from_entries([entry.clone()]);
        let dirs = ConfigDirs::new(f.root.join("config"), f.root.join("data")).unwrap();
        let derived = dirs.derived_dir(&name);
        let armed = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let released = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let host = crate::Host::new(
            registry,
            HeldReconcile {
                inner: ProductionEntryOps::new(dirs, ProductionPolicy::new(2, 2).unwrap()),
                armed: std::sync::Arc::clone(&armed),
                released: std::sync::Arc::clone(&released),
            },
            crate::LifecyclePolicy {
                idle_after: Duration::from_secs(60),
                worker_slots: 1,
                watch_poll_interval: Duration::from_millis(2),
            },
        )
        .unwrap();
        let _ = host.demand(&name, AttachMode::Durable).unwrap();
        wait_state(&host, &name, norn_wire::TrustState::Ready);
        // Armed before the edit, so the reconcile the watcher schedules for it
        // is held at its entry and the in-flight leg it published on the way in
        // stays there to be read: Warming for a dirty-path batch, or the
        // watcher-overflow refusal when the platform coalesces the edit into a
        // rescan scope.
        armed.store(true, std::sync::atomic::Ordering::SeqCst);
        fs::write(f.vault().join("note.md"), "after").unwrap();
        wait_pump_in_flight(&host, &name);
        released.store(true, std::sync::atomic::Ordering::SeqCst);
        wait_state(&host, &name, norn_wire::TrustState::Ready);
        drop(host);
        let mut store = Store::open(derived.join("store.sqlite3")).unwrap();
        let row = store
            .begin_request()
            .stored_document(&DocumentPath::new("note.md").unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(
            row.content_hash,
            norn_fs::ContentHash::of(b"after").to_string()
        );
    }

    /// Coverage that ended takes the subscription with it and publishes its
    /// cause once. A poll of the attachment that remains reports no facts, so
    /// nothing lands on top of the cause a client is reading.
    #[test]
    fn an_attachment_without_a_subscription_polls_to_no_facts() {
        let f = Fixture::new("absent-subscription");
        fs::write(f.vault().join("note.md"), "body").unwrap();
        let (ops, name) = f.ops(2);
        let progress = ProgressReporter::disconnected();
        let mut attachment = ops.attach(&f.registration(), &progress).unwrap();
        attachment.subscription.take();
        assert!(matches!(poll_subscription(&mut attachment), Ok(None)));
        // Poll drains the heal batch before consulting the subscription, so any
        // heal observation of this setup write is not this assertion's subject.
        attachment.heal_observed = norn_fs::Batch::default();
        assert!(matches!(ops.poll(&name, &mut attachment), Ok(None)));
    }

    /// A poll drains whatever the heal batch holds before it ever consults the
    /// subscription, and draining takes the batch: the next poll reports no
    /// facts.
    #[test]
    fn a_poll_reports_the_heal_batch_and_drains_it_exactly_once() {
        let f = Fixture::new("heal-batch-handoff");
        let (ops, name) = f.ops(2);
        let progress = ProgressReporter::disconnected();
        let mut attachment = ops.attach(&f.registration(), &progress).unwrap();
        // The live subscription is not this test's subject: the heal batch is
        // seeded by hand, and a real watcher event would race the final
        // no-facts assertion.
        attachment.subscription.take();
        attachment.heal_observed = norn_fs::Batch::rescan(RescanScope::Vault);

        let batch = ops.poll(&name, &mut attachment).unwrap();
        assert_eq!(batch, Some(norn_fs::Batch::rescan(RescanScope::Vault)));
        assert!(attachment.heal_observed.is_empty());

        assert!(matches!(ops.poll(&name, &mut attachment), Ok(None)));
    }

    #[test]
    fn dispatcher_poll_refuses_a_replaced_maintainer_lock() {
        let f = Fixture::new("poll-maintainership");
        fs::write(f.vault().join("note.md"), "body").unwrap();
        let name = VaultName::new("notes").unwrap();
        let entry = Registration::new(name.clone(), VaultRoot::new(f.vault()).unwrap());
        let registry = crate::RegistryRead::from_entries([entry.clone()]);
        let dirs = ConfigDirs::new(f.root.join("config"), f.root.join("data")).unwrap();
        let lock = dirs.derived_dir(&name).join("maintainer.lock");
        let host = crate::Host::new(
            registry,
            ProductionEntryOps::new(dirs, ProductionPolicy::new(2, 2).unwrap()),
            crate::LifecyclePolicy {
                idle_after: Duration::from_secs(60),
                worker_slots: 1,
                watch_poll_interval: Duration::from_millis(2),
            },
        )
        .unwrap();
        drop(host.demand(&name, AttachMode::Durable).unwrap());
        wait_state(&host, &name, norn_wire::TrustState::Ready);
        fs::remove_file(&lock).unwrap();
        fs::write(&lock, "replacement").unwrap();
        wait_state(&host, &name, norn_wire::TrustState::Unattached);
    }

    #[test]
    fn scheduled_maintenance_sweeps_aged_shadow_residue() {
        let f = Fixture::new("scheduled-shadow-sweep");
        let (ops, name) = f.ops(2);
        let progress = ProgressReporter::disconnected();
        let mut attachment = ops.attach(&f.registration(), &progress).unwrap();
        let residue = attachment._shadows.directory().join("norn-shadow-77-3");
        fs::write(&residue, "residue").unwrap();
        let aged =
            std::time::SystemTime::now() - norn_fs::SHADOW_AGE_THRESHOLD - Duration::from_secs(1);
        #[allow(clippy::disallowed_methods, clippy::disallowed_types)]
        fs::File::open(&residue)
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(aged))
            .unwrap();
        attachment.last_shadow_sweep =
            Instant::now() - norn_fs::SHADOW_AGE_THRESHOLD - Duration::from_secs(1);

        assert!(ops.maintenance_due(&name, &attachment));
        ops.maintain(&name, &mut attachment).unwrap();
        assert!(!residue.exists());
        assert!(!ops.maintenance_due(&name, &attachment));
        ops.detach(&name, attachment);
    }

    #[test]
    fn failed_scheduled_shadow_sweep_keeps_the_attachment_available_until_the_next_cadence() {
        let f = Fixture::new("failed-scheduled-shadow-sweep");
        let (ops, name) = f.ops(2);
        let progress = ProgressReporter::disconnected();
        let mut attachment = ops.attach(&f.registration(), &progress).unwrap();
        let shadow_home = attachment._shadows.directory().to_owned();
        let displaced = shadow_home.with_extension("displaced");
        fs::rename(&shadow_home, &displaced).unwrap();
        fs::write(&shadow_home, "not a directory").unwrap();
        attachment.last_shadow_sweep =
            Instant::now() - norn_fs::SHADOW_AGE_THRESHOLD - Duration::from_secs(1);

        assert!(ops.maintenance_due(&name, &attachment));
        ops.maintain(&name, &mut attachment)
            .expect("cleanup availability must not withdraw serving availability");
        assert!(attachment.maintainership.still_current().expect("a stat"));
        assert!(!ops.maintenance_due(&name, &attachment));

        ops.detach(&name, attachment);
    }

    struct BlockedRecovery {
        inner: ProductionEntryOps,
        lose_coverage: std::sync::Arc<std::sync::atomic::AtomicBool>,
        started: std::sync::Arc<std::sync::atomic::AtomicBool>,
        release: std::sync::Arc<std::sync::atomic::AtomicBool>,
        /// The document the case edits during the recovery, which is what the
        /// recovery absorbs for and what every reconcile reports on.
        subject: DocumentPath,
        /// The batches the recovery absorbed, waiting to be handed to the
        /// dispatcher one poll at a time.
        observed: std::sync::Mutex<std::collections::VecDeque<norn_fs::Batch>>,
        /// The subject's content hash as of the last reconcile this fake
        /// delegated, or `None` where the store had no row for it.
        ///
        /// **This is how the case watches the store while the host still owns
        /// it.** An entry reports ready when its recovery returns, and the
        /// handed-off batches are reconciled by the polls that follow — so a
        /// case that reads the store the moment the entry says ready is reading
        /// it before the handoff has landed. A reconcile is where a batch
        /// becomes a row, and this is written after one.
        stored: std::sync::Arc<std::sync::Mutex<Option<String>>>,
    }

    impl EntryOps for BlockedRecovery {
        type Attachment = ProductionAttachment;

        fn rebuild(
            &self,
            name: &VaultName,
            attachment: Self::Attachment,
            progress: &ProgressReporter<Self::Attachment>,
        ) -> Result<Self::Attachment, JobFailure> {
            self.inner.rebuild(name, attachment, progress)
        }

        fn attach(
            &self,
            registration: &Registration,
            progress: &ProgressReporter<Self::Attachment>,
        ) -> Result<Self::Attachment, JobFailure> {
            self.inner.attach(registration, progress)
        }

        fn reconcile(
            &self,
            name: &VaultName,
            attachment: &mut Self::Attachment,
            work: ReconcileWork,
            progress: &ProgressReporter<Self::Attachment>,
        ) -> Result<(), JobFailure> {
            self.inner.reconcile(name, attachment, work, progress)?;
            *self.stored.lock().unwrap() = attachment
                .store
                .begin_request()
                .stored_document(&self.subject)
                .expect("reading the subject after a reconcile")
                .map(|row| row.content_hash);
            Ok(())
        }

        fn recover(
            &self,
            _: &VaultName,
            attachment: &mut Self::Attachment,
            progress: &ProgressReporter<Self::Attachment>,
        ) -> Result<(), JobFailure> {
            if !attachment.maintainership.still_current().map_err(effect)? {
                return Err(JobFailure::LostMaintainership);
            }
            let schema = ProductionEntryOps::schema_path(&attachment.registration);
            let (subscription, own_writes) =
                ProductionEntryOps::start_watch(&attachment.registration, &schema)
                    .map_err(watcher)?;
            attachment.subscription = Some(subscription);
            attachment._own_writes = own_writes;
            self.inner.heal(attachment, progress)?;
            self.started
                .store(true, std::sync::atomic::Ordering::SeqCst);
            wait_until(
                "the test to release the blocked recovery",
                lifecycle_budget(),
                || {
                    if self.release.load(std::sync::atomic::Ordering::SeqCst) {
                        Observed::Met(())
                    } else {
                        Observed::Pending("the release flag is unset".to_owned())
                    }
                },
            )
            .map_err(|failure| environmental(failure.to_string()))?;

            // The edit is one change and the platform reports it in as many
            // batches as it likes — and, where it lost the path set, as a
            // rescan naming none of them. So what the recovery waits for is the
            // subject being invalidated rather than a batch arriving. A wait
            // that took the first batch would hand off whichever fragment came
            // first, and the ones behind it would reach the dispatcher only as
            // ordinary warm polls — which is the handoff this case exists to
            // show, quietly not happening.
            //
            // Nothing is reconciled here: the batches are queued for the
            // dispatcher, because a recovery that reconciled its own handoff
            // would leave the handoff untested.
            let subject = PathBuf::from(self.subject.as_str());
            let mut queued = std::collections::VecDeque::new();
            norn_testkit::wait::absorb_until(
                "the edit made during the recovery to be reported",
                absorbing_budget(),
                attachment,
                |attachment, absorbed| match poll_subscription(attachment) {
                    Ok(Some(batch)) => {
                        note_batch(&batch, absorbed);
                        Some(batch)
                    }
                    Ok(None) => None,
                    Err(failure) => panic!("the watcher reported {failure:?}"),
                },
                |_, batch| queued.push_back(batch),
                |_, absorbed| {
                    if absorbed_covers(absorbed, &subject) {
                        Observed::Met(())
                    } else {
                        Observed::pending("no batch has invalidated the edited document")
                    }
                },
            );
            *self.observed.lock().unwrap() = queued;
            Ok(())
        }

        fn poll(
            &self,
            name: &VaultName,
            attachment: &mut Self::Attachment,
        ) -> Result<Option<norn_fs::Batch>, JobFailure> {
            // Coverage ends the way the backend ends it: the subscription is
            // given up and the loss is reported once, so what follows is an
            // entry still holding its store and its lock with nothing watching
            // the vault for it.
            if self
                .lose_coverage
                .swap(false, std::sync::atomic::Ordering::SeqCst)
            {
                attachment.subscription.take();
                return Err(watcher(WatchError::Backend("test".into())));
            }
            // Every batch the recovery absorbed is handed over before the
            // ordinary poll resumes, so the handoff carries the whole edit
            // rather than the fragment that happened to arrive first.
            match self.observed.lock().unwrap().pop_front() {
                Some(batch) => Ok(Some(batch)),
                None => self.inner.poll(name, attachment),
            }
        }

        fn detach(&self, name: &VaultName, attachment: Self::Attachment) {
            self.inner.detach(name, attachment);
        }
    }

    /// Coverage lost against a live dispatcher is reinstalled on demand, and an
    /// event the watcher observes while the heal runs is reconciled after the
    /// handoff instead of being lost with the batch the heal drained.
    ///
    /// The demand is held for the whole recovery, so it is answered whether it
    /// reaches an unclaimed entry or one a watcher poll holds. Which of those a
    /// demand lands on is the dispatcher's cadence to decide; the claim window
    /// itself is pinned leg by leg by the lifecycle tests.
    #[test]
    fn recovery_handoff_reconciles_an_event_observed_during_the_heal() {
        let f = Fixture::new("recovery-handoff");
        let note = f.vault().join("note.md");
        fs::write(&note, "before").unwrap();
        let name = VaultName::new("notes").unwrap();
        let entry = Registration::new(name.clone(), VaultRoot::new(f.vault()).unwrap());
        let registry = crate::RegistryRead::from_entries([entry.clone()]);
        let dirs = ConfigDirs::new(f.root.join("config"), f.root.join("data")).unwrap();
        let derived = dirs.derived_dir(&name);
        let lose_coverage = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let started = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let release = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stored = std::sync::Arc::new(std::sync::Mutex::new(None));
        let host = crate::Host::new(
            registry,
            BlockedRecovery {
                inner: ProductionEntryOps::new(dirs, ProductionPolicy::new(2, 2).unwrap()),
                lose_coverage: std::sync::Arc::clone(&lose_coverage),
                started: std::sync::Arc::clone(&started),
                release: std::sync::Arc::clone(&release),
                subject: DocumentPath::new("note.md").unwrap(),
                observed: std::sync::Mutex::new(std::collections::VecDeque::new()),
                stored: std::sync::Arc::clone(&stored),
            },
            crate::LifecyclePolicy {
                idle_after: Duration::from_secs(60),
                worker_slots: 1,
                watch_poll_interval: Duration::from_millis(2),
            },
        )
        .unwrap();
        drop(host.demand(&name, AttachMode::Durable).unwrap());
        wait_state(&host, &name, norn_wire::TrustState::Ready);
        lose_coverage.store(true, std::sync::atomic::Ordering::SeqCst);
        wait_until(
            "coverage to be reported lost",
            lifecycle_budget(),
            || match host.state(&name) {
                Some(norn_wire::TrustState::Untrusted {
                    reason: norn_wire::UntrustedReason::WatcherLost { .. },
                    ..
                }) => Observed::Met(()),
                state => Observed::Pending(format!("the state is {state:?}")),
            },
        )
        .unwrap_or_else(|failure| panic!("{failure}"));
        // The recovery runs for a demand that is still outstanding, so the
        // lease lives until the entry is serving again.
        let demand = host.demand(&name, AttachMode::Durable).unwrap();
        wait_until(
            "the recovery to reach its block",
            lifecycle_budget(),
            || {
                if started.load(std::sync::atomic::Ordering::SeqCst) {
                    Observed::Met(())
                } else {
                    Observed::Pending("the recovery has not started".to_owned())
                }
            },
        )
        .unwrap_or_else(|failure| panic!("{failure}"));
        fs::write(&note, "during recovery").unwrap();
        release.store(true, std::sync::atomic::Ordering::SeqCst);
        wait_state(&host, &name, norn_wire::TrustState::Ready);

        // Ready says the recovery returned; it does not say the batches it
        // handed off have been reconciled, because those reach the store
        // through the polls that follow. So the outcome is waited for where it
        // is visible — after a reconcile, from inside the host that owns the
        // store — rather than read off disk the moment the entry starts
        // serving again.
        let during = norn_fs::ContentHash::of(b"during recovery").to_string();
        wait_until(
            "the store to converge on the content written during the recovery",
            lifecycle_budget(),
            || match stored.lock().unwrap().clone() {
                Some(hash) if hash == during => Observed::Met(()),
                seen => Observed::pending(format!("note.md is {seen:?}")),
            },
        )
        .unwrap_or_else(|failure| panic!("{failure}"));

        drop(demand);
        drop(host);
        let mut store = Store::open(derived.join("store.sqlite3")).unwrap();
        let row = store
            .begin_request()
            .stored_document(&DocumentPath::new("note.md").unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(row.content_hash, during);
    }

    /// The budget every lifecycle condition here is given: long enough that a
    /// loaded machine is not the thing being measured, and short enough that a
    /// state that never arrives is reported rather than waited on.
    fn lifecycle_budget() -> Budget {
        Budget::new(Duration::from_secs(15), Duration::from_millis(250))
    }

    /// The fact a batch carrying a schema invalidation notes about itself.
    ///
    /// A condition that waits for the invalidation asks for it by this name,
    /// which is also the line a failing wait renders — so the state a case
    /// waited for and the state a reader sees are one string.
    const SCHEMA_INVALIDATED: &str = "the schema was invalidated";

    /// The fact a batch carrying a vault-wide rescan notes about itself.
    ///
    /// A rescan is the backend saying the exact path set was lost, so it names
    /// no path and covers every one of them. It is spelled here rather than
    /// derived from the variant's debug rendering, because a condition asks for
    /// it by name.
    const VAULT_RESCAN: &str = "a vault-wide rescan";

    /// How long one take-and-reconcile may take.
    ///
    /// **This is a probe bound over a probe that reconciles**, which is why it
    /// is not the one the conditions here otherwise obey: a lifecycle probe
    /// reads a row, and a probe in an absorbing wait takes a settled batch and
    /// runs a scoped reconcile of every path it named. Sized at the lifecycle
    /// probe bound, reconciling one wide batch reports
    /// [`norn_testkit::wait::FailureKind::ProbeOverran`] — a diagnostic that
    /// says the probe is structurally too expensive, about a probe doing
    /// exactly the work its wait exists to drive.
    ///
    /// It is sized for the slowest plausible reconcile rather than the typical
    /// one, per the probe bound's own rule: the widest of these is a directory
    /// removal reported as a rescan, which walks the vault. What it still
    /// catches is a reconcile whose cost is structurally wrong.
    const ABSORBING_PROBE: Duration = Duration::from_secs(5);

    /// The budget an absorbing wait obeys.
    ///
    /// The work bound is the lifecycle one — how long a change has to come
    /// through the platform and land in the store is the same question every
    /// condition here asks — and the probe bound is [`ABSORBING_PROBE`].
    fn absorbing_budget() -> Budget {
        Budget::new(lifecycle_budget().work(), ABSORBING_PROBE)
    }

    /// Note what a settled batch says about itself, for the conditions and the
    /// failures that read it.
    ///
    /// Every fact here is read twice over. A condition asks for one by name —
    /// [`SCHEMA_INVALIDATED`] where the invalidation itself is the outcome, and
    /// the roots and [`VAULT_RESCAN`] through [`absorbed_covers`] where the
    /// outcome is that a path was invalidated at all. A failure renders all of
    /// them, and that is the other half of why they are noted: a wait that
    /// expires over a change that never arrived reads very differently from one
    /// that expires over a change reported as a rescan, and neither reads at
    /// all from a bare count.
    fn note_batch(batch: &norn_fs::Batch, absorbed: &mut norn_testkit::wait::Absorbed) {
        for root in batch.vault_roots() {
            absorbed.note(root_fact(root.as_path()));
        }
        for rescan in batch.rescans() {
            absorbed.note(match rescan {
                RescanScope::Vault => VAULT_RESCAN.to_owned(),
                RescanScope::Schema => "a schema rescan".to_owned(),
            });
        }
        if batch.schema_dirty() {
            absorbed.note(SCHEMA_INVALIDATED);
        }
    }

    /// The lead-in of the fact a batch's invalidation root is noted as, ahead
    /// of the root itself.
    ///
    /// [`root_fact`] writes it and [`noted_root`] reads the root back out of
    /// it, so the rendering a failure shows and the value a condition judges
    /// are one string with one spelling of its shape.
    const ROOT_FACT: &str = "root ";

    /// The fact one invalidation root notes about itself.
    fn root_fact(root: &Path) -> String {
        format!("{ROOT_FACT}{}", root.display())
    }

    /// The invalidation root a noted fact carries, for the facts that are one.
    ///
    /// What comes back is the [`Path::display`] spelling [`root_fact`] wrote,
    /// which is the root itself for a UTF-8 name and a replacement-character
    /// rendering of any other — and `norn_fs` normalizes a non-UTF-8 name
    /// rather than refusing it, because a Unix path is bytes. So the roots a
    /// condition judges here are exact for UTF-8 spellings, which is what the
    /// awaited paths are: a case awaits a `DocumentPath`, and that type is
    /// UTF-8. A root that is not is judged as its rendering rather than as its
    /// name.
    fn noted_root(fact: &str) -> Option<&Path> {
        fact.strip_prefix(ROOT_FACT).map(Path::new)
    }

    /// Whether what has been absorbed invalidates `path`.
    ///
    /// Two reports answer yes and they are not the same answer. An
    /// invalidation root at or above the path is the backend naming it, and
    /// at-or-above is [`norn_testkit::invalidation::at_or_above`] — the
    /// workspace's one spelling of that containment, which the `norn-fs`
    /// watcher cases ask of the same reports. A vault-wide rescan names
    /// nothing and covers everything, because it is the backend saying the
    /// exact path set was lost — and a case whose outcome is that a reconcile
    /// happened is answered by either, since a reconcile of a rescan reaches
    /// the path too.
    fn absorbed_covers(absorbed: &norn_testkit::wait::Absorbed, path: &Path) -> bool {
        absorbed.saw(VAULT_RESCAN)
            || absorbed.saw_any(|fact| {
                noted_root(fact)
                    .is_some_and(|root| norn_testkit::invalidation::at_or_above(root, path))
            })
    }

    /// The per-path half of [`absorbed_covers`], driven: an absorbed root
    /// above the awaited document answers the condition, and a root that is
    /// only a byte prefix of it does not.
    ///
    /// **The reports here are stood up rather than waited for**, because the
    /// platform does not produce this shape where the condition is used.
    /// [`recovery_handoff_reconciles_an_event_observed_during_the_heal`] is
    /// answered by [`VAULT_RESCAN`]: an edit made while a heal runs is
    /// reported as the path set having been lost, so the batches that case
    /// absorbs carry no per-path root at all. Naming the reports is what makes
    /// the other half of the condition live, and it is the half where the
    /// difference between at-or-above and exact equality is visible.
    #[test]
    fn an_absorbed_root_above_the_awaited_document_covers_it() {
        let f = Fixture::watcherless("absorbed-covers");
        let normalizer = norn_fs::PathNormalizer::detect(&f.vault()).unwrap();
        let normalize = |relative: &str| normalizer.normalize(Path::new(relative)).unwrap();

        // `notes/note` is a byte prefix of the awaited document and no
        // ancestor of it, so it is the report a containment answered on
        // strings would wrongly accept; `notes` is the directory holding the
        // document, which is the report a containment answered on equality
        // would wrongly reject.
        let awaited = PathBuf::from("notes/note.md");
        let mut reports = std::collections::VecDeque::from([
            norn_fs::Batch::vault_change(normalize("notes/note")),
            norn_fs::Batch::vault_change(normalize("notes")),
        ]);
        let mut applied: Vec<norn_fs::Batch> = Vec::new();
        norn_testkit::wait::absorb_until(
            "an absorbed invalidation root at or above the awaited document",
            absorbing_budget(),
            &mut (),
            |_: &mut (), absorbed| {
                let batch = reports.pop_front()?;
                note_batch(&batch, absorbed);
                Some(batch)
            },
            |_: &mut (), batch| applied.push(batch),
            |_: &mut (), absorbed| {
                if absorbed_covers(absorbed, &awaited) {
                    Observed::Met(())
                } else {
                    Observed::pending("no absorbed root is at or above the awaited document")
                }
            },
        );

        assert_eq!(
            applied.len(),
            2,
            "the wait ended on a root that is a byte prefix of the awaited document \
             rather than an ancestor of it"
        );
        assert!(
            applied.iter().all(|batch| batch.rescans().is_empty()),
            "a rescan answered a condition this case drives per-path"
        );
    }

    /// Reconcile settled batches until `condition` holds, under one budget.
    ///
    /// **A watcher batch is not the unit an outcome arrives in.** The platform
    /// splits one change across as many batches as it likes and in either
    /// order — a directory rename arrives as the destination appearing in one
    /// batch and the source disappearing in another — so a case that takes one
    /// batch, reconciles it, and asserts the outcome outside any wait is
    /// asserting that the split did not happen. When it does happen the case
    /// fails against a store the next batch settles, which is a report about
    /// batch granularity wearing the costume of a reconcile defect.
    ///
    /// So the outcome is the condition and the batches are what the wait
    /// pumps. The pump is [`norn_testkit::wait::absorb_until`]; what this adds
    /// is the two ends that are this crate's — a take that is the same
    /// nonblocking one [`poll_subscription`] makes, and an apply that is a real
    /// scoped reconcile.
    ///
    /// A terminal watch error ends the wait rather than being polled for
    /// again, because it takes the subscription with it and every look after
    /// it reports no facts.
    fn reconcile_until(
        ops: &ProductionEntryOps,
        name: &VaultName,
        attachment: &mut ProductionAttachment,
        progress: &ProgressReporter<ProductionAttachment>,
        what: &str,
        condition: impl FnMut(&mut ProductionAttachment, &norn_testkit::wait::Absorbed) -> Observed<()>,
    ) {
        norn_testkit::wait::absorb_until(
            what,
            absorbing_budget(),
            attachment,
            |attachment, absorbed| match poll_subscription(attachment) {
                Ok(Some(batch)) => {
                    note_batch(&batch, absorbed);
                    Some(batch)
                }
                Ok(None) => None,
                Err(failure) => panic!("the watcher reported {failure:?}"),
            },
            |attachment, batch| {
                ops.reconcile(name, attachment, ReconcileWork { batch }, progress)
                    .unwrap();
            },
            condition,
        );
    }

    /// **The bar on the hold window.** A fixture holds the real-watcher lease
    /// for its whole life, which is longer than any attachment built on it.
    ///
    /// The forbidden shape is the lease released early — around the attach
    /// alone, say, rather than around the fixture. It costs nothing visible and
    /// it speeds this suite up, because every case then runs its watcher beside
    /// every sibling's; what it buys is the starvation the lease exists to
    /// prevent, showing up in some other case as changes that never arrived.
    ///
    /// The exclusion is read from this process, which is where a file lock
    /// excludes per open file description rather than per process. The
    /// reacquisition after the drop takes the ordinary queueing bound, because
    /// a sibling case is entitled to be next in line.
    #[test]
    fn a_fixture_holds_the_watcher_lease_for_as_long_as_it_lives() {
        let f = Fixture::new("lease-window");

        let contested = norn_testkit::isolation::Lease::try_hold(
            norn_testkit::isolation::REAL_WATCHER,
            Budget::new(Duration::from_millis(50), Duration::from_millis(250)),
        );
        assert!(
            contested.is_err(),
            "the lease was free while a fixture was alive, so any watcher it attaches runs beside \
             every sibling's"
        );

        drop(f);
        drop(norn_testkit::isolation::Lease::hold(
            norn_testkit::isolation::REAL_WATCHER,
            lease_budget(),
        ));
    }

    /// Whether the store holds a document at `path`, as a condition.
    ///
    /// The two shapes every absorption here waits on are a document that must
    /// arrive and a document that must go, and both read the same row.
    fn stored(attachment: &mut ProductionAttachment, path: &str) -> Option<StoredDocument> {
        attachment
            .store
            .begin_request()
            .stored_document(&DocumentPath::new(path).unwrap())
            .unwrap()
    }

    /// Wait for one exact trust state, reporting the last state observed.
    ///
    /// The observation is the whole state, phase included, because that is
    /// what tells the two ways a wait expires apart: an entry still installing
    /// coverage and an entry whose heal is not advancing both stand at zero
    /// healed against an unknown total.
    fn wait_state<O: EntryOps>(
        host: &crate::Host<O>,
        name: &VaultName,
        expected: norn_wire::TrustState,
    ) {
        wait_until(
            &format!("trust state {expected:?}"),
            lifecycle_budget(),
            || match host.state(name) {
                Some(state) if state == expected => Observed::Met(()),
                state => Observed::Pending(format!("the state is {state:?}")),
            },
        )
        .unwrap_or_else(|failure| panic!("{failure}"));
    }

    /// The pump has left `Ready` with its reconcile leg in flight. The leg
    /// reads as `Warming` when the batch carried the dirty path, and as the
    /// watcher-overflow refusal when the platform delivered a rescan scope
    /// instead — both are the same in-flight fact, and pinning one of them
    /// pins the watcher backend's batch granularity.
    fn wait_pump_in_flight<O: EntryOps>(host: &crate::Host<O>, name: &VaultName) {
        wait_until("an in-flight trust state", lifecycle_budget(), || {
            let state = host.state(name);
            if matches!(
                state,
                Some(norn_wire::TrustState::Warming { .. })
                    | Some(norn_wire::TrustState::Untrusted {
                        reason: norn_wire::UntrustedReason::WatcherOverflow,
                        ..
                    })
            ) {
                Observed::Met(())
            } else {
                Observed::Pending(format!("the state is {state:?}"))
            }
        })
        .unwrap_or_else(|failure| panic!("{failure}"));
    }

    #[test]
    fn mapper_is_the_complete_text_to_store_boundary() {
        let source = b"---\ntags: [front]\nkind: note\n---\n# Heading\n[[target#Part|Title]] #body\nblock ^id\n";
        let facts = map_document(
            "note.md",
            source,
            norn_fs::ContentHash::of(source).to_string(),
        )
        .unwrap();
        assert!(facts.frontmatter.is_some());
        assert_eq!(facts.headings.len(), 1);
        assert_eq!(facts.links.len(), 1);
        assert_eq!(facts.blocks.len(), 1);
        assert_eq!(facts.tags.len(), 2);
    }

    /// The count beside an absent frontmatter projection is what tells a
    /// document with no block apart from one whose block did not read, so what
    /// the mapper counts is the notes the text layer scoped to the block.
    ///
    /// Every code that layer raises is scoped to the block today, so no
    /// document here is one the filter and a count of every note disagree
    /// over. What the filter holds is the seam: a note the text layer raises
    /// about something other than the block leaves this count through it, and
    /// the scope of a code is that layer's own answer rather than a spelling
    /// read here.
    #[test]
    fn the_frontmatter_note_count_separates_no_block_from_a_block_that_did_not_read() {
        for (source, projection, notes) in [
            (b"# Heading\nbody\n".to_vec(), false, 0),
            (b"---\ntitle: note\n---\nbody\n".to_vec(), true, 0),
            (b"---\ntitle: note\nbody\n".to_vec(), false, 1),
        ] {
            let facts = map_document(
                "note.md",
                &source,
                norn_fs::ContentHash::of(&source).to_string(),
            )
            .unwrap();
            let read = String::from_utf8(source).unwrap();
            assert_eq!(
                facts.frontmatter.is_some(),
                projection,
                "the projection of `{read}` is not what the block is"
            );
            assert_eq!(
                facts.frontmatter_diagnostic_count, notes,
                "`{read}` raised another count of block-scoped notes"
            );
        }
    }

    fn dirty_path(
        root: &Path,
        relative: &str,
    ) -> std::collections::BTreeSet<norn_fs::NormalizedPath> {
        let normalizer = norn_fs::PathNormalizer::detect(root).unwrap();
        [normalizer.normalize(Path::new(relative)).unwrap()]
            .into_iter()
            .collect()
    }
}
