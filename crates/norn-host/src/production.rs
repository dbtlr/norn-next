use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::iter::Peekable;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use norn_config::registry::Entry;
use norn_config::{ConfigDirs, IN_VAULT_SCHEMA_PATH, VaultName};
use norn_fs::{
    Acquisition, Maintainership, RescanScope, ShadowHome, Subscription, try_acquire, walk, watch,
};
use norn_store::{
    BlockFact, Change, DocumentFacts, DocumentPath, FindingFacts, FrontmatterValue, HeadingFact,
    IncrementProvenance, LinkFact, LinkFamily, Provenance, Span, Store, StoredPathOrder, TagFact,
    TagSource,
};
use norn_text::{Document, SourceSpan, Value};
use norn_wire::MaintainerIdentity;

use crate::{EntryOps, JobFailure, ProgressReporter, ReconcileWork};

/// Maximum number of document changes materialized for one store transaction.
pub const MAX_CHANGESET_SIZE: usize = 1024;

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
pub struct ProductionEntryOps {
    entries: BTreeMap<VaultName, Entry>,
    dirs: ConfigDirs,
    policy: ProductionPolicy,
}

pub struct ProductionAttachment {
    entry: Entry,
    maintainership: Maintainership,
    store: Store,
    subscription: Option<Subscription>,
    _shadows: ShadowHome,
    last_shadow_sweep: Instant,
}

impl ProductionEntryOps {
    pub fn new(
        entries: impl IntoIterator<Item = Entry>,
        dirs: ConfigDirs,
        policy: ProductionPolicy,
    ) -> Self {
        Self {
            entries: entries.into_iter().map(|e| (e.name.clone(), e)).collect(),
            dirs,
            policy,
        }
    }

    fn entry(&self, name: &VaultName) -> Result<Entry, JobFailure> {
        self.entries
            .get(name)
            .cloned()
            .ok_or_else(|| environmental("registry entry disappeared"))
    }

    fn derived(&self, name: &VaultName) -> PathBuf {
        self.dirs.derived_dir(name)
    }

    fn schema_path(entry: &Entry) -> PathBuf {
        entry
            .schema_source
            .as_ref()
            .map(|s| s.as_path().to_owned())
            .unwrap_or_else(|| entry.root.as_path().join(IN_VAULT_SCHEMA_PATH))
    }

    fn pin_schema(store: &mut Store, entry: &Entry) -> Result<(), JobFailure> {
        let observed = norn_fs::read_and_hash(&Self::schema_path(entry)).map_err(effect)?;
        std::str::from_utf8(observed.bytes())
            .map_err(|e| environmental(format!("schema is not UTF-8: {e}")))?;
        store
            .begin_request()
            .pin_vault_schema(observed.bytes(), &observed.content_hash().to_string())
            .map_err(effect)?;
        Ok(())
    }

    fn heal(
        &self,
        attachment: &mut ProductionAttachment,
        progress: &ProgressReporter<ProductionAttachment>,
    ) -> Result<(), JobFailure> {
        if !attachment.maintainership.still_current() {
            return Err(JobFailure::LostMaintainership);
        }
        let exclusions = exclusions(&attachment.entry, &attachment._shadows);
        Self::pin_schema(&mut attachment.store, &attachment.entry)?;
        heal_documents(
            &mut attachment.store,
            attachment.entry.root.as_path(),
            &exclusions,
            self.policy,
            progress,
        )
    }
}

fn exclusions(entry: &Entry, shadows: &ShadowHome) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Ok(relative) = shadows.directory().strip_prefix(entry.root.as_path()) {
        paths.push(relative.to_owned());
    }
    let schema = ProductionEntryOps::schema_path(entry);
    if let Ok(relative) = schema.strip_prefix(entry.root.as_path()) {
        paths.push(relative.to_owned());
    }
    paths
}

impl EntryOps for ProductionEntryOps {
    type Attachment = ProductionAttachment;

    fn attach(
        &self,
        name: &VaultName,
        progress: &ProgressReporter<Self::Attachment>,
    ) -> Result<Self::Attachment, JobFailure> {
        let entry = self.entry(name)?;
        let derived = self.derived(name);
        let maintainership = match try_acquire(&derived.join("maintainer.lock")).map_err(effect)? {
            Acquisition::Acquired(guard) => guard,
            Acquisition::Contended { incumbent } => {
                return Err(JobFailure::MaintainerContended(map_incumbent(incumbent)));
            }
        };
        let shadows =
            ShadowHome::resolve(entry.root.as_path(), &derived.join("tmp")).map_err(effect)?;
        shadows.sweep(Duration::ZERO).map_err(effect)?;
        let schema = Self::schema_path(&entry);
        let (subscription, _) = watch(entry.root.as_path(), &schema).map_err(watcher)?;
        let store = Store::open(derived.join("store.sqlite3")).map_err(effect)?;
        let mut attachment = ProductionAttachment {
            entry,
            maintainership,
            store,
            subscription: Some(subscription),
            _shadows: shadows,
            last_shadow_sweep: Instant::now(),
        };
        self.heal(&mut attachment, progress)?;
        Ok(attachment)
    }

    fn reconcile(
        &self,
        _: &VaultName,
        attachment: &mut Self::Attachment,
        work: ReconcileWork,
        progress: &ProgressReporter<Self::Attachment>,
    ) -> Result<(), JobFailure> {
        if !attachment.maintainership.still_current() {
            return Err(JobFailure::LostMaintainership);
        }
        let schema =
            work.batch.schema_dirty() || work.batch.rescans().contains(&RescanScope::Schema);
        if schema {
            Self::pin_schema(&mut attachment.store, &attachment.entry)?;
        }
        if work.batch.rescans().contains(&RescanScope::Vault) {
            return self.heal(attachment, progress);
        }
        scoped_increment(
            &mut attachment.store,
            attachment.entry.root.as_path(),
            work.batch.vault_roots(),
            self.policy,
            progress,
            &exclusions(&attachment.entry, &attachment._shadows),
        )
    }

    fn recover(
        &self,
        _: &VaultName,
        attachment: &mut Self::Attachment,
        progress: &ProgressReporter<Self::Attachment>,
    ) -> Result<(), JobFailure> {
        if !attachment.maintainership.still_current() {
            return Err(JobFailure::LostMaintainership);
        }
        let schema = Self::schema_path(&attachment.entry);
        let (subscription, _) = watch(attachment.entry.root.as_path(), &schema).map_err(watcher)?;
        attachment.subscription = Some(subscription);
        self.heal(attachment, progress)
    }

    fn poll(
        &self,
        _: &VaultName,
        attachment: &mut Self::Attachment,
    ) -> Result<Option<norn_fs::Batch>, JobFailure> {
        if !attachment.maintainership.still_current() {
            return Err(JobFailure::LostMaintainership);
        }
        poll_subscription(attachment)
    }

    fn maintenance_due(&self, _: &VaultName, attachment: &Self::Attachment) -> bool {
        attachment.last_shadow_sweep.elapsed() >= norn_fs::SHADOW_AGE_THRESHOLD
    }

    fn maintain(&self, _: &VaultName, attachment: &mut Self::Attachment) -> Result<(), JobFailure> {
        if !attachment.maintainership.still_current() {
            return Err(JobFailure::LostMaintainership);
        }
        // Shadow residue is inert, and a sweep is only bounded cleanup. Losing
        // that cleanup opportunity must not withdraw an otherwise healthy
        // attachment or force a full heal; try again at the next normal cadence.
        let _ = attachment._shadows.sweep(norn_fs::SHADOW_AGE_THRESHOLD);
        attachment.last_shadow_sweep = Instant::now();
        Ok(())
    }

    fn detach(&self, _: &VaultName, attachment: Self::Attachment) {
        drop(attachment.subscription);
        let _ = attachment.store.close();
        drop(attachment.maintainership);
    }
}

fn poll_subscription(
    attachment: &mut ProductionAttachment,
) -> Result<Option<norn_fs::Batch>, JobFailure> {
    let result = attachment
        .subscription
        .as_ref()
        .ok_or_else(|| JobFailure::WatcherTerminal("watcher coverage is absent".into()))?
        .try_recv();
    match result {
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
    progress: &ProgressReporter<ProductionAttachment>,
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
                .map_err(effect)?;
            index = 0;
            exhausted = stored.is_empty();
        }
        let fs_path = next_nameable(&mut files, None, &mut pending)?;
        let db_path = stored.get(index).map(|d| d.path.as_str().to_owned());
        match (fs_path, db_path) {
            (None, None) => break,
            (Some(fp), Some(dp)) if sensitivity.compare(&fp, &dp).is_eq() => {
                let file = files.next().expect("peeked").map_err(effect)?;
                let read = file.read().map_err(effect)?;
                if read.content_hash().to_string() != stored[index].content_hash {
                    match map_document(&fp, read.bytes(), read.content_hash().to_string()) {
                        Ok(facts) => pending.push(Change::Upsert(facts)),
                        // The row the document no longer accounts for goes with
                        // the finding that says why: the store holds
                        // representable truth, and a quarantined path is absent
                        // from it.
                        Err(quarantine) => {
                            pending.push(Change::Death {
                                path: stored[index].path.clone(),
                                provenance: Provenance::HealPrune,
                            });
                            pending.quarantine(Path::new(&fp), quarantine);
                        }
                    }
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
    progress: &ProgressReporter<ProductionAttachment>,
    exclusions: &[PathBuf],
) -> Result<(), JobFailure> {
    let sensitivity = norn_fs::PathNormalizer::detect(root)
        .map_err(effect)?
        .case_sensitivity();
    let mut pending = Pending::new(store, policy.changeset_size);
    for (index, relative) in dirty.iter().enumerate() {
        let path = relative.as_path();
        if is_excluded(path, exclusions) {
            continue;
        }
        // A path that names no document names no subtree either, so the
        // identity is taken before anything that would range over it. Only a
        // Markdown spelling is a vault document, and a finding is about a
        // document.
        let document_path = match document_path(path) {
            Ok(document) => document,
            Err(quarantine) => {
                if is_markdown(path) {
                    pending.quarantine(path, quarantine);
                }
                continue;
            }
        };
        match norn_fs::path_kind(&root.join(path)).map_err(effect)? {
            norn_fs::PathKind::Directory => {
                pending.flush()?;
                heal_subtree(
                    pending.store,
                    root,
                    &document_path,
                    policy,
                    progress,
                    exclusions,
                )?;
                continue;
            }
            norn_fs::PathKind::Missing | norn_fs::PathKind::Other => {
                pending.flush()?;
                prune_subtree_ordered(
                    pending.store,
                    &document_path,
                    policy,
                    progress,
                    store_order(sensitivity),
                )?;
                continue;
            }
            norn_fs::PathKind::RegularFile => {}
        }
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
        match norn_fs::read_optional_and_hash(&root.join(path)).map_err(effect)? {
            Some(observed) => {
                let hash = observed.content_hash().to_string();
                let standing = pending
                    .store
                    .begin_request()
                    .stored_document(&document_path)
                    .map_err(effect)?;
                if standing.as_ref().is_none_or(|row| row.content_hash != hash) {
                    match map_document(document_path.as_str(), observed.bytes(), hash) {
                        Ok(facts) => pending.push(Change::Upsert(facts)),
                        Err(quarantine) => {
                            if standing.is_some() {
                                pending.push(Change::Death {
                                    path: document_path.clone(),
                                    provenance: Provenance::WatcherRemoval,
                                });
                            }
                            pending.quarantine(path, quarantine);
                        }
                    }
                }
            }
            None => pending.push(Change::Death {
                path: document_path,
                provenance: Provenance::WatcherRemoval,
            }),
        }
        if pending.is_full() {
            pending.flush()?;
        }
        progress.report((index + 1) as u64, Some(dirty.len() as u64));
    }
    pending.flush()
}

fn heal_subtree(
    store: &mut Store,
    vault_root: &Path,
    subtree: &DocumentPath,
    policy: ProductionPolicy,
    progress: &ProgressReporter<ProductionAttachment>,
    exclusions: &[PathBuf],
) -> Result<(), JobFailure> {
    let prefix = subtree.as_str();
    let relative_root = Path::new(prefix);
    let scoped_exclusions = exclusions
        .iter()
        .filter_map(|excluded| excluded.strip_prefix(relative_root).ok())
        .map(Path::to_owned)
        .collect::<Vec<_>>();
    let walk = walk(&vault_root.join(relative_root), &scoped_exclusions).map_err(effect)?;
    let sensitivity = walk.case_sensitivity();
    let order = store_order(sensitivity);
    let mut files = walk
        .filter_map(|fact| match fact {
            Ok(norn_fs::WalkFact::File(file)) => {
                let full = relative_root.join(file.path().as_path());
                (is_markdown(&full) && !is_excluded(&full, exclusions)).then_some(Ok(file))
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
            stored = pending
                .store
                .begin_request()
                .stored_documents_in_subtree_after_ordered(
                    subtree,
                    after.as_ref(),
                    policy.store_page_size,
                    order,
                )
                .map_err(effect)?;
            index = 0;
            exhausted = stored.is_empty();
        }
        let fs_path = next_nameable(&mut files, Some(prefix), &mut pending)?;
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
                    match map_document(&fp, read.bytes(), read.content_hash().to_string()) {
                        Ok(facts) => pending.push(Change::Upsert(facts)),
                        Err(quarantine) => {
                            pending.push(Change::Death {
                                path: stored[index].path.clone(),
                                provenance: Provenance::HealPrune,
                            });
                            pending.quarantine(Path::new(&fp), quarantine);
                        }
                    }
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

#[cfg(test)]
fn prune_subtree(
    store: &mut Store,
    root: &DocumentPath,
    policy: ProductionPolicy,
    progress: &ProgressReporter<ProductionAttachment>,
) -> Result<(), JobFailure> {
    prune_subtree_ordered(store, root, policy, progress, StoredPathOrder::Sensitive)
}

fn prune_subtree_ordered(
    store: &mut Store,
    root: &DocumentPath,
    policy: ProductionPolicy,
    progress: &ProgressReporter<ProductionAttachment>,
    order: StoredPathOrder,
) -> Result<(), JobFailure> {
    let mut after = None;
    let mut healed = 0;
    let mut pending = Pending::new(store, policy.changeset_size);
    loop {
        let page = pending
            .store
            .begin_request()
            .stored_documents_in_subtree_after_ordered(
                root,
                after.as_ref(),
                policy.store_page_size,
                order,
            )
            .map_err(effect)?;
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
    progress: &ProgressReporter<ProductionAttachment>,
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
            .map_err(effect)?;
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
/// caller reads it after deciding what the merge does with it. `prefix` is the
/// scope the walk is relative to, and `None` is a walk of the vault root.
fn next_nameable<I>(
    files: &mut Peekable<I>,
    prefix: Option<&str>,
    pending: &mut Pending<'_>,
) -> Result<Option<String>, JobFailure>
where
    I: Iterator<Item = Result<norn_fs::FileFact, norn_fs::WalkError>>,
{
    loop {
        let spelling = match files.peek() {
            Some(Ok(file)) => file.path().as_path().to_str().map(|spelling| match prefix {
                Some(prefix) => format!("{prefix}/{spelling}"),
                None => spelling.to_owned(),
            }),
            Some(Err(error)) => return Err(effect(error)),
            None => return Ok(None),
        };
        if let Some(spelling) = spelling {
            return Ok(Some(spelling));
        }
        let file = files.next().expect("peeked").map_err(effect)?;
        let path = match prefix {
            Some(prefix) => Path::new(prefix).join(file.path().as_path()),
            None => file.path().as_path().to_owned(),
        };
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

fn is_excluded(path: &Path, exclusions: &[PathBuf]) -> bool {
    exclusions.iter().any(|excluded| path.starts_with(excluded))
}

/// The severity a quarantine finding carries. A document the vault holds and
/// norn cannot decode is a defect in the vault, not an advisory about it.
const QUARANTINE_SEVERITY: &str = "error";

/// The character a quarantine subject renders every byte the document-path
/// grammar refuses as.
const REPLACEMENT: char = '\u{FFFD}';

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
    const fn kind(self) -> &'static str {
        match self {
            Undecodable::PathBytes => "document/path-bytes-not-utf8",
            Undecodable::PathSpelling => "document/path-names-no-document",
            Undecodable::BodyBytes => "document/body-bytes-not-utf8",
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
    /// The decoder's own account of the refusal, which the finding carries as
    /// its detail.
    problem: String,
}

/// What one heal scope has derived and not yet committed: the changeset being
/// filled, and the findings its quarantined documents record.
///
/// The two flush together and in that order. A changeset entry discards the
/// findings recorded about the path it names, so a finding written ahead of the
/// increment is a finding the increment takes — and a quarantined document that
/// reads again is a plain upsert whose own discard clears the finding with no
/// second mechanism.
struct Pending<'s> {
    store: &'s mut Store,
    changes: Vec<Change>,
    quarantined: Vec<FindingFacts>,
    /// The bound on either list, which is what holds a scope's residency
    /// independent of how much of the vault it covers.
    bound: usize,
}

impl<'s> Pending<'s> {
    fn new(store: &'s mut Store, bound: usize) -> Self {
        Pending {
            store,
            changes: Vec::with_capacity(bound),
            quarantined: Vec::new(),
            bound,
        }
    }

    fn push(&mut self, change: Change) {
        self.changes.push(change);
    }

    /// Derive a document the store holds no row for: its facts where the bytes
    /// yield them, and a finding where they do not.
    ///
    /// There is no row to prune here. The caller that has one prunes it beside
    /// the finding, because the store holds only what it can represent.
    fn derive(&mut self, path: &str, bytes: &[u8], hash: String) {
        match map_document(path, bytes, hash) {
            Ok(facts) => self.push(Change::Upsert(facts)),
            Err(quarantine) => self.quarantine(Path::new(path), quarantine),
        }
    }

    /// File the finding that says why a path contributes no facts.
    ///
    /// The subject is the path as the vault spells it where the document-path
    /// grammar admits that spelling, and a rendering of it where the grammar
    /// does not — see [`quarantine_subject`]. A spelling no rendering reaches
    /// names no subject, and the document is passed over without a finding.
    fn quarantine(&mut self, path: &Path, quarantine: Quarantine) {
        let Some(subject) = quarantine_subject(path) else {
            return;
        };
        self.quarantined.push(FindingFacts {
            kind: quarantine.cause.kind().to_string(),
            severity: QUARANTINE_SEVERITY.to_string(),
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
            detail: Some(quarantine.problem),
        });
    }

    /// Whether either list has reached the bound one flush may hold.
    fn is_full(&self) -> bool {
        self.changes.len() >= self.bound || self.quarantined.len() >= self.bound
    }

    /// Apply the changeset, then record what its quarantines say.
    fn flush(&mut self) -> Result<(), JobFailure> {
        if !self.changes.is_empty() {
            self.store
                .begin_request()
                .apply_increment(IncrementProvenance::Derived, self.changes.drain(..))
                .map_err(effect)?;
        }
        for finding in self.quarantined.drain(..) {
            let mut request = self.store.begin_request();
            let standing = request.stored_findings(&finding.path).map_err(effect)?;
            // A subject the changeset named has no findings left to duplicate.
            // A subject it did not — a path no document row can hold — keeps the
            // finding already at rest rather than gaining a second copy of it
            // on every heal.
            let already = standing.iter().any(|stood| {
                stood.kind == finding.kind
                    && stood.message == finding.message
                    && stood.detail == finding.detail
            });
            if !already {
                request.record_finding(&finding).map_err(effect)?;
            }
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
            problem: format!("`{}` is not valid UTF-8", path.display()),
        });
    };
    DocumentPath::new(spelling).map_err(|problem| Quarantine {
        cause: Undecodable::PathSpelling,
        problem: problem.to_string(),
    })
}

/// The document path a quarantine finding is filed under.
///
/// The vault's own spelling where the document-path grammar admits it, and a
/// rendering of that spelling where it does not: bytes that are not UTF-8 and
/// characters the grammar refuses alike become [`REPLACEMENT`], and a leaf whose
/// stem the grammar refuses carries the marker ahead of it. A rendering names a
/// place a reader can print, never an identity — no document is stored under
/// one, and a finding's subject needs no row.
///
/// `None` is a spelling no rendering reaches, which a vault-relative walk does
/// not produce: an empty path, an absolute one, or one carrying a `.` or `..`
/// segment.
fn quarantine_subject(path: &Path) -> Option<DocumentPath> {
    if let Some(spelling) = path.to_str()
        && let Ok(document) = DocumentPath::new(spelling)
    {
        return Some(document);
    }
    let rendered: String = path
        .to_string_lossy()
        .chars()
        .map(|character| {
            if character == '\\' || character.is_control() {
                REPLACEMENT
            } else {
                character
            }
        })
        .collect();
    if let Ok(document) = DocumentPath::new(&rendered) {
        return Some(document);
    }
    // A leaf whose stem reduces to `.` or `..` names no ambiguity class, and no
    // replacement inside the leaf changes that. The marker goes ahead of the
    // leaf, which is the shortest rendering that leaves the rest of the name
    // readable.
    let (ancestors, leaf) = rendered.rsplit_once('/').unwrap_or(("", rendered.as_str()));
    let marked = if ancestors.is_empty() {
        format!("{REPLACEMENT}{leaf}")
    } else {
        format!("{ancestors}/{REPLACEMENT}{leaf}")
    };
    DocumentPath::new(&marked).ok()
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
        .filter(|d| d.code.starts_with("frontmatter-"))
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
fn effect(error: impl std::fmt::Display) -> JobFailure {
    environmental(error.to_string())
}
fn watcher(error: impl std::fmt::Display) -> JobFailure {
    JobFailure::WatcherTerminal(error.to_string())
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
    use norn_config::registry::{SchemaSource, VaultRoot};
    use norn_testkit::wait::{Budget, Observed, wait_until};
    use std::fs;
    use std::thread;
    use std::time::{SystemTime, UNIX_EPOCH};

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

    struct Fixture {
        root: PathBuf,
    }
    impl Fixture {
        fn new(label: &str) -> Self {
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let root = std::env::temp_dir()
                .join(format!("norn-host-{label}-{}-{nonce}", std::process::id()));
            fs::create_dir_all(root.join("vault/.norn")).unwrap();
            fs::write(root.join("vault/.norn/schema.yaml"), "version: 1\n").unwrap();
            Self { root }
        }
        fn vault(&self) -> PathBuf {
            self.root.join("vault")
        }
        fn ops(&self, page: usize) -> (ProductionEntryOps, VaultName) {
            let name = VaultName::new("notes").unwrap();
            let entry = Entry::new(name.clone(), VaultRoot::new(self.vault()).unwrap());
            let dirs = ConfigDirs::new(self.root.join("config"), self.root.join("data")).unwrap();
            (
                ProductionEntryOps::new([entry], dirs, ProductionPolicy::new(page, 2).unwrap()),
                name,
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
        let mut attachment = ops.attach(&name, &progress).unwrap();
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
            .stored_documents_after(None, 20)
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

        let (ops, name) = f.ops(1);
        let progress = ProgressReporter::disconnected();
        let mut attachment = ops.attach(&name, &progress).unwrap();
        let rows = attachment
            .store
            .begin_request()
            .stored_documents_after(None, 10)
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
        let mut attachment = ops.attach(&name, &progress).unwrap();
        let unrelated = DocumentPath::new("unrelated.md").unwrap();
        let generation = attachment
            .store
            .begin_request()
            .stored_document(&unrelated)
            .unwrap()
            .unwrap()
            .generation;
        fs::write(f.vault().join("note.md"), "after").unwrap();
        let batch = attachment
            .subscription
            .as_ref()
            .unwrap()
            .recv_timeout(Duration::from_secs(5))
            .unwrap()
            .expect("watch batch");
        ops.reconcile(&name, &mut attachment, ReconcileWork { batch }, &progress)
            .unwrap();
        let row = attachment
            .store
            .begin_request()
            .stored_document(&DocumentPath::new("note.md").unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(
            row.content_hash,
            norn_fs::ContentHash::of(b"after").to_string()
        );
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
    fn scoped_missing_markdown_file_prunes_its_stored_document() {
        let f = Fixture::new("missing-markdown-file");
        fs::write(f.vault().join("note.md"), "before").unwrap();
        let (ops, name) = f.ops(2);
        let progress = ProgressReporter::disconnected();
        let mut attachment = ops.attach(&name, &progress).unwrap();
        fs::remove_file(f.vault().join("note.md")).unwrap();

        scoped_increment(
            &mut attachment.store,
            f.vault().as_path(),
            &dirty_path(f.vault().as_path(), "note.md"),
            ProductionPolicy::new(2, 2).unwrap(),
            &progress,
            &exclusions(&attachment.entry, &attachment._shadows),
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
        let mut attachment = ops.attach(&name, &progress).unwrap();
        fs::remove_file(&note).unwrap();
        symlink("missing-target", &note).unwrap();
        let batch = attachment
            .subscription
            .as_ref()
            .unwrap()
            .recv_timeout(Duration::from_secs(5))
            .unwrap()
            .expect("replacement watcher batch");
        ops.reconcile(&name, &mut attachment, ReconcileWork { batch }, &progress)
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

    #[test]
    fn scoped_file_replacing_a_directory_prunes_former_descendants() {
        let f = Fixture::new("directory-to-file");
        fs::create_dir_all(f.vault().join("archive.md")).unwrap();
        fs::write(f.vault().join("archive.md/child.md"), "child").unwrap();
        let (ops, name) = f.ops(2);
        let progress = ProgressReporter::disconnected();
        let mut attachment = ops.attach(&name, &progress).unwrap();

        fs::remove_dir_all(f.vault().join("archive.md")).unwrap();
        fs::write(f.vault().join("archive.md"), "replacement").unwrap();
        scoped_increment(
            &mut attachment.store,
            f.vault().as_path(),
            &dirty_path(f.vault().as_path(), "archive.md"),
            ProductionPolicy::new(2, 2).unwrap(),
            &progress,
            &exclusions(&attachment.entry, &attachment._shadows),
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
        assert_eq!(
            quarantine_subject(path)
                .expect("a rendered subject")
                .as_str(),
            "bad-\u{fffd}.md"
        );
    }

    #[test]
    fn document_identity_boundary_quarantines_a_spelling_the_grammar_refuses() {
        let path = Path::new("notes/bad\\name.md");
        let quarantine = document_path(path).expect_err("a backslash names no document");
        assert_eq!(quarantine.cause, Undecodable::PathSpelling);
        assert_eq!(
            quarantine_subject(path)
                .expect("a rendered subject")
                .as_str(),
            "notes/bad\u{fffd}name.md"
        );
    }

    #[test]
    fn a_quarantine_subject_marks_a_leaf_whose_stem_names_no_class() {
        assert_eq!(
            quarantine_subject(Path::new("notes/..md"))
                .expect("a rendered subject")
                .as_str(),
            "notes/\u{fffd}..md"
        );
        assert_eq!(
            quarantine_subject(Path::new("..md"))
                .expect("a rendered subject")
                .as_str(),
            "\u{fffd}..md"
        );
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
        let mut attachment = ops.attach(&name, &progress).unwrap();
        fs::rename(f.vault().join("note.md"), f.vault().join("NOTE.md")).unwrap();

        scoped_increment(
            &mut attachment.store,
            f.vault().as_path(),
            &dirty_path(f.vault().as_path(), "NOTE.md"),
            ProductionPolicy::new(2, 2).unwrap(),
            &progress,
            &exclusions(&attachment.entry, &attachment._shadows),
        )
        .unwrap();

        let rows = attachment
            .store
            .begin_request()
            .stored_documents_after(None, 10)
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
        let mut attachment = ops.attach(&name, &progress).unwrap();
        fs::rename(f.vault().join("folder"), f.vault().join("FOLDER")).unwrap();

        scoped_increment(
            &mut attachment.store,
            f.vault().as_path(),
            &dirty_path(f.vault().as_path(), "FOLDER"),
            ProductionPolicy::new(2, 2).unwrap(),
            &progress,
            &exclusions(&attachment.entry, &attachment._shadows),
        )
        .unwrap();

        let rows = attachment
            .store
            .begin_request()
            .stored_documents_after(None, 10)
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
        let mut attachment = ops.attach(&name, &progress).unwrap();
        let rows = attachment
            .store
            .begin_request()
            .stored_documents_after(None, 10)
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
            &progress,
            &exclusions(&attachment.entry, &attachment._shadows),
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
        let mut attachment = ops.attach(&name, &progress).unwrap();
        fs::write(f.vault().join("before.md"), "before").unwrap();
        scoped_increment(
            &mut attachment.store,
            f.vault().as_path(),
            &dirty_path(f.vault().as_path(), "before.md"),
            ProductionPolicy::new(8, 2).unwrap(),
            &progress,
            &exclusions(&attachment.entry, &attachment._shadows),
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
        prune_subtree(
            &mut attachment.store,
            &DocumentPath::new("folder").unwrap(),
            ProductionPolicy::new(8, 2).unwrap(),
            &progress,
        )
        .unwrap();
        fs::write(f.vault().join("after.md"), "after").unwrap();
        scoped_increment(
            &mut attachment.store,
            f.vault().as_path(),
            &dirty_path(f.vault().as_path(), "after.md"),
            ProductionPolicy::new(8, 2).unwrap(),
            &progress,
            &exclusions(&attachment.entry, &attachment._shadows),
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
        let mut attachment = ops.attach(&name, &progress).unwrap();
        let unrelated = DocumentPath::new("unrelated.md").unwrap();
        let generation = attachment
            .store
            .begin_request()
            .stored_document(&unrelated)
            .unwrap()
            .unwrap()
            .generation;

        fs::remove_dir_all(f.vault().join("folder")).unwrap();
        let batch = attachment
            .subscription
            .as_ref()
            .unwrap()
            .recv_timeout(Duration::from_secs(5))
            .unwrap()
            .unwrap();
        ops.reconcile(&name, &mut attachment, ReconcileWork { batch }, &progress)
            .unwrap();
        assert!(
            attachment
                .store
                .begin_request()
                .stored_document(&DocumentPath::new("folder/a.md").unwrap())
                .unwrap()
                .is_none()
        );

        fs::create_dir_all(f.vault().join("source")).unwrap();
        fs::write(f.vault().join("source/b.md"), "b").unwrap();
        let batch = attachment
            .subscription
            .as_ref()
            .unwrap()
            .recv_timeout(Duration::from_secs(5))
            .unwrap()
            .unwrap();
        ops.reconcile(&name, &mut attachment, ReconcileWork { batch }, &progress)
            .unwrap();
        fs::rename(f.vault().join("source"), f.vault().join("renamed")).unwrap();
        let batch = attachment
            .subscription
            .as_ref()
            .unwrap()
            .recv_timeout(Duration::from_secs(5))
            .unwrap()
            .unwrap();
        ops.reconcile(&name, &mut attachment, ReconcileWork { batch }, &progress)
            .unwrap();
        assert!(
            attachment
                .store
                .begin_request()
                .stored_document(&DocumentPath::new("source/b.md").unwrap())
                .unwrap()
                .is_none()
        );
        assert!(
            attachment
                .store
                .begin_request()
                .stored_document(&DocumentPath::new("renamed/b.md").unwrap())
                .unwrap()
                .is_some()
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
        let mut attachment = ops.attach(&name, &progress).unwrap();
        fs::remove_dir_all(f.vault().join("archive.md")).unwrap();
        let batch = attachment
            .subscription
            .as_ref()
            .unwrap()
            .recv_timeout(Duration::from_secs(5))
            .unwrap()
            .unwrap();
        ops.reconcile(&name, &mut attachment, ReconcileWork { batch }, &progress)
            .unwrap();
        assert!(
            attachment
                .store
                .begin_request()
                .stored_document(&DocumentPath::new("archive.md/child.md").unwrap())
                .unwrap()
                .is_none()
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
        let mut entry = Entry::new(name.clone(), VaultRoot::new(f.vault()).unwrap());
        entry.schema_source = Some(SchemaSource::new(&schema).unwrap());
        let dirs = ConfigDirs::new(f.root.join("config"), f.root.join("data")).unwrap();
        let ops = ProductionEntryOps::new([entry], dirs, ProductionPolicy::new(2, 2).unwrap());
        let progress = ProgressReporter::disconnected();
        let mut attachment = ops.attach(&name, &progress).unwrap();
        fs::write(&schema, "version: two").unwrap();
        let batch = attachment
            .subscription
            .as_ref()
            .unwrap()
            .recv_timeout(Duration::from_secs(5))
            .unwrap()
            .unwrap();
        ops.reconcile(&name, &mut attachment, ReconcileWork { batch }, &progress)
            .unwrap();
        assert!(
            attachment
                .store
                .begin_request()
                .stored_document(&DocumentPath::new(".norn/schema.md").unwrap())
                .unwrap()
                .is_none()
        );
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
            .attach(&name, &ProgressReporter::disconnected())
            .unwrap();
        running.store(false, std::sync::atomic::Ordering::SeqCst);
        writer.join().unwrap();
        ops.detach(&name, attachment);
    }

    struct SlowProduction(ProductionEntryOps);
    impl EntryOps for SlowProduction {
        type Attachment = ProductionAttachment;
        fn attach(
            &self,
            name: &VaultName,
            progress: &ProgressReporter<Self::Attachment>,
        ) -> Result<Self::Attachment, JobFailure> {
            self.0.attach(name, progress)
        }
        fn reconcile(
            &self,
            name: &VaultName,
            attachment: &mut Self::Attachment,
            work: ReconcileWork,
            progress: &ProgressReporter<Self::Attachment>,
        ) -> Result<(), JobFailure> {
            thread::sleep(Duration::from_millis(30));
            self.0.reconcile(name, attachment, work, progress)
        }
        fn recover(
            &self,
            name: &VaultName,
            attachment: &mut Self::Attachment,
            progress: &ProgressReporter<Self::Attachment>,
        ) -> Result<(), JobFailure> {
            self.0.recover(name, attachment, progress)
        }
        fn poll(
            &self,
            name: &VaultName,
            attachment: &mut Self::Attachment,
        ) -> Result<Option<norn_fs::Batch>, JobFailure> {
            self.0.poll(name, attachment)
        }
        fn detach(&self, name: &VaultName, attachment: Self::Attachment) {
            self.0.detach(name, attachment)
        }
    }

    #[test]
    fn external_edit_is_autonomously_pumped_through_warming_to_ready() {
        let f = Fixture::new("host-watch-edit");
        fs::write(f.vault().join("note.md"), "before").unwrap();
        let name = VaultName::new("notes").unwrap();
        let entry = Entry::new(name.clone(), VaultRoot::new(f.vault()).unwrap());
        let registry = crate::ServingRegistry::from_entries([entry.clone()]).unwrap();
        let dirs = ConfigDirs::new(f.root.join("config"), f.root.join("data")).unwrap();
        let derived = dirs.derived_dir(&name);
        let host = crate::Host::new(
            registry,
            SlowProduction(ProductionEntryOps::new(
                [entry],
                dirs,
                ProductionPolicy::new(2, 2).unwrap(),
            )),
            crate::LifecyclePolicy {
                idle_after: Duration::from_secs(60),
                worker_slots: 1,
                watch_poll_interval: Duration::from_millis(2),
            },
        )
        .unwrap();
        let _ = host.demand(&name).unwrap();
        wait_state(&host, &name, norn_wire::TrustState::Ready);
        fs::write(f.vault().join("note.md"), "after").unwrap();
        wait_state_kind(&host, &name, true);
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

    #[test]
    fn dispatcher_poll_refuses_a_replaced_maintainer_lock() {
        let f = Fixture::new("poll-maintainership");
        fs::write(f.vault().join("note.md"), "body").unwrap();
        let name = VaultName::new("notes").unwrap();
        let entry = Entry::new(name.clone(), VaultRoot::new(f.vault()).unwrap());
        let registry = crate::ServingRegistry::from_entries([entry.clone()]).unwrap();
        let dirs = ConfigDirs::new(f.root.join("config"), f.root.join("data")).unwrap();
        let lock = dirs.derived_dir(&name).join("maintainer.lock");
        let host = crate::Host::new(
            registry,
            ProductionEntryOps::new([entry], dirs, ProductionPolicy::new(2, 2).unwrap()),
            crate::LifecyclePolicy {
                idle_after: Duration::from_secs(60),
                worker_slots: 1,
                watch_poll_interval: Duration::from_millis(2),
            },
        )
        .unwrap();
        drop(host.demand(&name).unwrap());
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
        let mut attachment = ops.attach(&name, &progress).unwrap();
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
        let mut attachment = ops.attach(&name, &progress).unwrap();
        let shadow_home = attachment._shadows.directory().to_owned();
        let displaced = shadow_home.with_extension("displaced");
        fs::rename(&shadow_home, &displaced).unwrap();
        fs::write(&shadow_home, "not a directory").unwrap();
        attachment.last_shadow_sweep =
            Instant::now() - norn_fs::SHADOW_AGE_THRESHOLD - Duration::from_secs(1);

        assert!(ops.maintenance_due(&name, &attachment));
        ops.maintain(&name, &mut attachment)
            .expect("cleanup availability must not withdraw serving availability");
        assert!(attachment.maintainership.still_current());
        assert!(!ops.maintenance_due(&name, &attachment));

        ops.detach(&name, attachment);
    }

    struct BlockedRecovery {
        inner: ProductionEntryOps,
        started: std::sync::Arc<std::sync::atomic::AtomicBool>,
        release: std::sync::Arc<std::sync::atomic::AtomicBool>,
        observed: std::sync::Mutex<Option<norn_fs::Batch>>,
    }

    impl EntryOps for BlockedRecovery {
        type Attachment = ProductionAttachment;

        fn attach(
            &self,
            name: &VaultName,
            progress: &ProgressReporter<Self::Attachment>,
        ) -> Result<Self::Attachment, JobFailure> {
            self.inner.attach(name, progress)
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
            _: &VaultName,
            attachment: &mut Self::Attachment,
            progress: &ProgressReporter<Self::Attachment>,
        ) -> Result<(), JobFailure> {
            if !attachment.maintainership.still_current() {
                return Err(JobFailure::LostMaintainership);
            }
            let schema = ProductionEntryOps::schema_path(&attachment.entry);
            let (subscription, _) =
                watch(attachment.entry.root.as_path(), &schema).map_err(watcher)?;
            attachment.subscription = Some(subscription);
            self.inner.heal(attachment, progress)?;
            self.started
                .store(true, std::sync::atomic::Ordering::SeqCst);
            while !self.release.load(std::sync::atomic::Ordering::SeqCst) {
                thread::yield_now();
            }
            loop {
                if let Some(batch) = poll_subscription(attachment)? {
                    *self.observed.lock().unwrap() = Some(batch);
                    break;
                }
                thread::yield_now();
            }
            Ok(())
        }

        fn poll(
            &self,
            name: &VaultName,
            attachment: &mut Self::Attachment,
        ) -> Result<Option<norn_fs::Batch>, JobFailure> {
            if let Some(batch) = self.observed.lock().unwrap().take() {
                return Ok(Some(batch));
            }
            self.inner.poll(name, attachment)
        }

        fn detach(&self, name: &VaultName, attachment: Self::Attachment) {
            self.inner.detach(name, attachment);
        }
    }

    #[test]
    fn recovery_handoff_reconciles_an_event_observed_during_the_heal() {
        let f = Fixture::new("recovery-handoff");
        let note = f.vault().join("note.md");
        fs::write(&note, "before").unwrap();
        let name = VaultName::new("notes").unwrap();
        let entry = Entry::new(name.clone(), VaultRoot::new(f.vault()).unwrap());
        let registry = crate::ServingRegistry::from_entries([entry.clone()]).unwrap();
        let dirs = ConfigDirs::new(f.root.join("config"), f.root.join("data")).unwrap();
        let derived = dirs.derived_dir(&name);
        let started = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let release = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let host = crate::Host::new(
            registry,
            BlockedRecovery {
                inner: ProductionEntryOps::new([entry], dirs, ProductionPolicy::new(2, 2).unwrap()),
                started: std::sync::Arc::clone(&started),
                release: std::sync::Arc::clone(&release),
                observed: std::sync::Mutex::new(None),
            },
            crate::LifecyclePolicy {
                idle_after: Duration::from_secs(60),
                worker_slots: 1,
                watch_poll_interval: Duration::from_millis(2),
            },
        )
        .unwrap();
        drop(host.demand(&name).unwrap());
        wait_state(&host, &name, norn_wire::TrustState::Ready);
        host.watcher_failed(&name, norn_fs::WatchError::Backend("test".into()));
        drop(host.demand(&name).unwrap());
        while !started.load(std::sync::atomic::Ordering::SeqCst) {
            thread::yield_now();
        }
        fs::write(&note, "during recovery").unwrap();
        release.store(true, std::sync::atomic::Ordering::SeqCst);
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
            norn_fs::ContentHash::of(b"during recovery").to_string()
        );
    }

    /// The budget every lifecycle condition here is given: long enough that a
    /// loaded machine is not the thing being measured, and short enough that a
    /// state that never arrives is reported rather than waited on.
    fn lifecycle_budget() -> Budget {
        Budget::new(Duration::from_secs(15), Duration::from_millis(250))
    }

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

    fn wait_state_kind<O: EntryOps>(host: &crate::Host<O>, name: &VaultName, warming: bool) {
        wait_until(
            if warming {
                "a warming trust state"
            } else {
                "a settled trust state"
            },
            lifecycle_budget(),
            || {
                let state = host.state(name);
                if matches!(state, Some(norn_wire::TrustState::Warming { .. })) == warming {
                    Observed::Met(())
                } else {
                    Observed::Pending(format!("the state is {state:?}"))
                }
            },
        )
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
