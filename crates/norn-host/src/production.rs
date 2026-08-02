use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use norn_config::registry::Entry;
use norn_config::{ConfigDirs, IN_VAULT_SCHEMA_PATH, VaultName};
use norn_fs::{
    Acquisition, Maintainership, RescanScope, ShadowHome, Subscription, try_acquire, walk, watch,
};
use norn_store::{
    BlockFact, Change, DocumentFacts, DocumentPath, FrontmatterValue, HeadingFact,
    IncrementProvenance, LinkFact, LinkFamily, Provenance, Span, Store, TagFact, TagSource,
};
use norn_text::{Document, SourceSpan, Value};
use norn_wire::MaintainerIdentity;

use crate::{EntryOps, JobFailure, ProgressReporter, ReconcileWork};

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
        if changeset_size == 0 {
            return Err(ProductionPolicyError::ChangesetSize);
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
    ChangesetSize,
}

impl fmt::Display for ProductionPolicyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::StorePageSize(given) => write!(
                f,
                "store page size must be between 1 and {}, got {given}",
                norn_store::MAX_STORED_DOCUMENT_PAGE
            ),
            Self::ChangesetSize => f.write_str("changesets must be non-empty"),
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
        if attachment.last_shadow_sweep.elapsed() >= norn_fs::SHADOW_AGE_THRESHOLD {
            attachment
                ._shadows
                .sweep(norn_fs::SHADOW_AGE_THRESHOLD)
                .map_err(effect)?;
            attachment.last_shadow_sweep = Instant::now();
        }
        poll_subscription(attachment)
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
    let mut files = walk(root, exclusions)
        .map_err(effect)?
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
    let mut changes = Vec::with_capacity(policy.changeset_size);
    let mut healed = 0;
    loop {
        if index == stored.len() && !exhausted {
            stored = store
                .begin_request()
                .stored_documents_after(after.as_ref(), policy.store_page_size)
                .map_err(effect)?;
            index = 0;
            exhausted = stored.is_empty();
        }
        let fs_path = match files.peek() {
            Some(Ok(file)) => Some(file.path().as_path().to_string_lossy().into_owned()),
            Some(Err(error)) => return Err(effect(error)),
            None => None,
        };
        let db_path = stored.get(index).map(|d| d.path.as_str().to_owned());
        match (fs_path, db_path) {
            (None, None) => break,
            (Some(fp), Some(dp)) if fp == dp => {
                let file = files.next().expect("peeked").map_err(effect)?;
                let read = file.read().map_err(effect)?;
                if read.content_hash().to_string() != stored[index].content_hash {
                    changes.push(Change::Upsert(map_document(
                        &fp,
                        read.bytes(),
                        read.content_hash().to_string(),
                    )?));
                }
                after = Some(stored[index].path.clone());
                index += 1;
            }
            (Some(fp), Some(dp)) if fp < dp => {
                let file = files.next().expect("peeked").map_err(effect)?;
                let read = file.read().map_err(effect)?;
                changes.push(Change::Upsert(map_document(
                    &fp,
                    read.bytes(),
                    read.content_hash().to_string(),
                )?));
            }
            (_, Some(_)) => {
                let path = stored[index].path.clone();
                after = Some(path.clone());
                index += 1;
                changes.push(Change::Death {
                    path,
                    provenance: Provenance::HealPrune,
                });
            }
            (Some(fp), None) => {
                let file = files.next().expect("peeked").map_err(effect)?;
                let read = file.read().map_err(effect)?;
                changes.push(Change::Upsert(map_document(
                    &fp,
                    read.bytes(),
                    read.content_hash().to_string(),
                )?));
            }
        }
        if changes.len() == policy.changeset_size {
            commit(store, &mut changes)?;
        }
        healed += 1;
        progress.report(healed, None);
    }
    commit(store, &mut changes)
}

fn scoped_increment(
    store: &mut Store,
    root: &Path,
    dirty: &std::collections::BTreeSet<norn_fs::NormalizedPath>,
    policy: ProductionPolicy,
    progress: &ProgressReporter<ProductionAttachment>,
    exclusions: &[PathBuf],
) -> Result<(), JobFailure> {
    let mut changes = Vec::with_capacity(policy.changeset_size);
    for (index, relative) in dirty.iter().enumerate() {
        let path = relative.as_path();
        if is_excluded(path, exclusions) {
            continue;
        }
        match norn_fs::path_kind(&root.join(path)).map_err(effect)? {
            norn_fs::PathKind::Directory => {
                commit(store, &mut changes)?;
                heal_subtree(store, root, path, policy, progress, exclusions)?;
                continue;
            }
            norn_fs::PathKind::Missing => {
                commit(store, &mut changes)?;
                prune_subtree(store, path, policy, progress)?;
                continue;
            }
            norn_fs::PathKind::Other => {
                if is_markdown(path) {
                    changes.push(Change::Death {
                        path: DocumentPath::new(&path.to_string_lossy()).map_err(effect)?,
                        provenance: Provenance::WatcherRemoval,
                    });
                    if changes.len() == policy.changeset_size {
                        commit(store, &mut changes)?;
                    }
                }
                continue;
            }
            norn_fs::PathKind::RegularFile => {}
        }
        if !is_markdown(path) {
            continue;
        }
        let document_path = DocumentPath::new(&path.to_string_lossy()).map_err(effect)?;
        match norn_fs::read_optional_and_hash(&root.join(path)).map_err(effect)? {
            Some(observed) => {
                let hash = observed.content_hash().to_string();
                let standing = store
                    .begin_request()
                    .stored_document(&document_path)
                    .map_err(effect)?;
                if standing.as_ref().is_none_or(|row| row.content_hash != hash) {
                    changes.push(Change::Upsert(map_document(
                        document_path.as_str(),
                        observed.bytes(),
                        hash,
                    )?));
                }
            }
            None => changes.push(Change::Death {
                path: document_path,
                provenance: Provenance::WatcherRemoval,
            }),
        }
        if changes.len() == policy.changeset_size {
            commit(store, &mut changes)?;
        }
        progress.report((index + 1) as u64, Some(dirty.len() as u64));
    }
    commit(store, &mut changes)
}

fn heal_subtree(
    store: &mut Store,
    vault_root: &Path,
    relative_root: &Path,
    policy: ProductionPolicy,
    progress: &ProgressReporter<ProductionAttachment>,
    exclusions: &[PathBuf],
) -> Result<(), JobFailure> {
    let subtree = DocumentPath::new(&relative_root.to_string_lossy()).map_err(effect)?;
    let prefix = relative_root.to_string_lossy();
    let mut files = walk(&vault_root.join(relative_root), &[])
        .map_err(effect)?
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
    let mut changes = Vec::with_capacity(policy.changeset_size);
    let mut healed = 0;
    loop {
        if index == stored.len() && !exhausted {
            stored = store
                .begin_request()
                .stored_documents_in_subtree_after(&subtree, after.as_ref(), policy.store_page_size)
                .map_err(effect)?;
            index = 0;
            exhausted = stored.is_empty();
        }
        let fs_path = match files.peek() {
            Some(Ok(file)) => Some(format!(
                "{prefix}/{}",
                file.path().as_path().to_string_lossy()
            )),
            Some(Err(error)) => return Err(effect(error)),
            None => None,
        };
        let db_path = stored.get(index).map(|row| row.path.as_str().to_owned());
        match (fs_path, db_path) {
            (None, None) => break,
            (Some(fp), Some(dp)) if fp == dp => {
                let read = files
                    .next()
                    .expect("peeked")
                    .map_err(effect)?
                    .read()
                    .map_err(effect)?;
                if read.content_hash().to_string() != stored[index].content_hash {
                    changes.push(Change::Upsert(map_document(
                        &fp,
                        read.bytes(),
                        read.content_hash().to_string(),
                    )?));
                }
                after = Some(stored[index].path.clone());
                index += 1;
            }
            (Some(fp), Some(dp)) if fp < dp => {
                let read = files
                    .next()
                    .expect("peeked")
                    .map_err(effect)?
                    .read()
                    .map_err(effect)?;
                changes.push(Change::Upsert(map_document(
                    &fp,
                    read.bytes(),
                    read.content_hash().to_string(),
                )?));
            }
            (_, Some(_)) => {
                let path = stored[index].path.clone();
                after = Some(path.clone());
                index += 1;
                changes.push(Change::Death {
                    path,
                    provenance: Provenance::WatcherRemoval,
                });
            }
            (Some(fp), None) => {
                let read = files
                    .next()
                    .expect("peeked")
                    .map_err(effect)?
                    .read()
                    .map_err(effect)?;
                changes.push(Change::Upsert(map_document(
                    &fp,
                    read.bytes(),
                    read.content_hash().to_string(),
                )?));
            }
        }
        if changes.len() == policy.changeset_size {
            commit(store, &mut changes)?;
        }
        healed += 1;
        progress.report(healed, None);
    }
    commit(store, &mut changes)
}

fn prune_subtree(
    store: &mut Store,
    relative_root: &Path,
    policy: ProductionPolicy,
    progress: &ProgressReporter<ProductionAttachment>,
) -> Result<(), JobFailure> {
    let root = DocumentPath::new(&relative_root.to_string_lossy()).map_err(effect)?;
    let mut after = None;
    let mut healed = 0;
    loop {
        let page = store
            .begin_request()
            .stored_documents_in_subtree_after(&root, after.as_ref(), policy.store_page_size)
            .map_err(effect)?;
        if page.is_empty() {
            break;
        }
        after = page.last().map(|row| row.path.clone());
        let changes = page
            .into_iter()
            .map(|row| Change::Death {
                path: row.path,
                provenance: Provenance::WatcherRemoval,
            })
            .collect::<Vec<_>>();
        healed += changes.len() as u64;
        store
            .begin_request()
            .apply_increment(IncrementProvenance::Derived, changes)
            .map_err(effect)?;
        progress.report(healed, None);
    }
    Ok(())
}

fn is_markdown(path: &Path) -> bool {
    path.extension().is_some_and(|extension| extension == "md")
}

fn is_excluded(path: &Path, exclusions: &[PathBuf]) -> bool {
    exclusions.iter().any(|excluded| path.starts_with(excluded))
}

fn commit(store: &mut Store, changes: &mut Vec<Change>) -> Result<(), JobFailure> {
    if !changes.is_empty() {
        store
            .begin_request()
            .apply_increment(IncrementProvenance::Derived, changes.drain(..))
            .map_err(effect)?;
    }
    Ok(())
}

fn map_document(path: &str, bytes: &[u8], hash: String) -> Result<DocumentFacts, JobFailure> {
    let source = std::str::from_utf8(bytes)
        .map_err(|e| environmental(format!("document {path} is not UTF-8: {e}")))?;
    let document = Document::parse(source);
    let scan = document.scan_body();
    let mut facts = DocumentFacts::new(
        DocumentPath::new(path).map_err(effect)?,
        hash,
        document.body(),
        bytes.len() as u64,
    );
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
    fn attach_indexes_only_markdown_and_ignores_binary_clutter() {
        let f = Fixture::new("markdown-only");
        fs::write(f.vault().join("note.md"), "a note").unwrap();
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
            ["note.md"]
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

    fn wait_state<O: EntryOps>(
        host: &crate::Host<O>,
        name: &VaultName,
        expected: norn_wire::TrustState,
    ) {
        for _ in 0..500 {
            if host.state(name) == Some(expected.clone()) {
                return;
            }
            thread::sleep(Duration::from_millis(2));
        }
        panic!("state did not become {expected:?}");
    }

    fn wait_state_kind<O: EntryOps>(host: &crate::Host<O>, name: &VaultName, warming: bool) {
        for _ in 0..500 {
            if matches!(
                host.state(name),
                Some(norn_wire::TrustState::Warming { .. })
            ) == warming
            {
                return;
            }
            thread::sleep(Duration::from_millis(2));
        }
        panic!("state did not become warming");
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

    const TEAR_ROOT: &str = "NORN_HOST_TEAR_ROOT";

    #[test]
    fn kill_mid_increment_child() {
        let Some(root) = std::env::var_os(TEAR_ROOT) else {
            return;
        };
        let root = PathBuf::from(root);
        let name = VaultName::new("notes").unwrap();
        let entry = Entry::new(name.clone(), VaultRoot::new(root.join("vault")).unwrap());
        let dirs = ConfigDirs::new(root.join("config"), root.join("data")).unwrap();
        let ops = ProductionEntryOps::new([entry], dirs, ProductionPolicy::new(2, 10).unwrap());
        norn_store::induced_failure::abort_after_changeset_entries(1);
        let _never_returns = ops.attach(&name, &ProgressReporter::disconnected());
        panic!("induced process tear did not abort");
    }

    #[test]
    fn attach_recovers_after_process_dies_mid_increment() {
        let f = Fixture::new("tear-recovery");
        fs::write(f.vault().join("a.md"), "old-a").unwrap();
        fs::write(f.vault().join("b.md"), "old-b").unwrap();
        let (ops, name) = f.ops(2);
        let attachment = ops
            .attach(&name, &ProgressReporter::disconnected())
            .unwrap();
        ops.detach(&name, attachment);
        fs::write(f.vault().join("a.md"), "new-a").unwrap();
        fs::write(f.vault().join("b.md"), "new-b").unwrap();

        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "production::tests::kill_mid_increment_child",
                "--nocapture",
            ])
            .env(TEAR_ROOT, &f.root)
            .status()
            .unwrap();
        assert!(!status.success(), "tear child unexpectedly survived");

        let (ops, name) = f.ops(2);
        let mut attachment = ops
            .attach(&name, &ProgressReporter::disconnected())
            .unwrap();
        for (path, expected) in [("a.md", b"new-a".as_slice()), ("b.md", b"new-b".as_slice())] {
            let row = attachment
                .store
                .begin_request()
                .stored_document(&DocumentPath::new(path).unwrap())
                .unwrap()
                .unwrap();
            assert_eq!(
                row.content_hash,
                norn_fs::ContentHash::of(expected).to_string()
            );
        }
        attachment.store.verify_integrity().unwrap();
        ops.detach(&name, attachment);
    }
}
