use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

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

use crate::{EntryOps, JobFailure, ReconcileWork};

/// Resource bounds for the concrete filesystem/store adapter.
#[derive(Clone, Copy, Debug)]
pub struct ProductionPolicy {
    /// Maximum stored rows read at once during an ordered heal merge.
    pub store_page_size: usize,
    /// Maximum changes committed by one increment transaction.
    pub changeset_size: usize,
}

impl ProductionPolicy {
    pub fn new(store_page_size: usize, changeset_size: usize) -> Self {
        assert!(store_page_size > 0, "store pages must be non-empty");
        assert!(changeset_size > 0, "changesets must be non-empty");
        Self {
            store_page_size,
            changeset_size,
        }
    }
}

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
    subscription: Subscription,
    _shadows: ShadowHome,
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

    fn heal(&self, attachment: &mut ProductionAttachment) -> Result<(), JobFailure> {
        if !attachment.maintainership.still_current() {
            return Err(JobFailure::LostMaintainership);
        }
        let exclusions = exclusions(&attachment.entry, &attachment._shadows);
        loop {
            Self::pin_schema(&mut attachment.store, &attachment.entry)?;
            heal_documents(
                &mut attachment.store,
                attachment.entry.root.as_path(),
                &exclusions,
                self.policy,
            )?;
            // Coverage was established before the first walk. Repeat until a
            // complete pass has no fact queued behind it; no racing edit is
            // hidden by the heal that happened to overlap it.
            let mut raced = false;
            while attachment
                .subscription
                .try_recv()
                .map_err(watcher)?
                .is_some()
            {
                raced = true;
            }
            if !raced {
                return Ok(());
            }
        }
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

    fn attach(&self, name: &VaultName) -> Result<Self::Attachment, JobFailure> {
        let entry = self.entry(name)?;
        let derived = self.derived(name);
        let maintainership = match try_acquire(&derived.join("maintainer.lock")).map_err(effect)? {
            Acquisition::Acquired(guard) => guard,
            Acquisition::Contended { incumbent } => {
                return Err(JobFailure::MaintainerContended(incumbent.to_string()));
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
            subscription,
            _shadows: shadows,
        };
        self.heal(&mut attachment)?;
        Ok(attachment)
    }

    fn reconcile(
        &self,
        _: &VaultName,
        attachment: &mut Self::Attachment,
        work: ReconcileWork,
    ) -> Result<(), JobFailure> {
        if !attachment.maintainership.still_current() {
            return Err(JobFailure::LostMaintainership);
        }
        let schema =
            work.batch.schema_dirty() || work.batch.rescans().contains(&RescanScope::Schema);
        if schema {
            Self::pin_schema(&mut attachment.store, &attachment.entry)?;
        }
        // A full ordered heal is also a correct widening of every root batch;
        // the bounded merge keeps its resource cost independent of vault size.
        self.heal(attachment)
    }

    fn recover(&self, _: &VaultName, attachment: &mut Self::Attachment) -> Result<(), JobFailure> {
        if !attachment.maintainership.still_current() {
            return Err(JobFailure::LostMaintainership);
        }
        let schema = Self::schema_path(&attachment.entry);
        let (subscription, _) = watch(attachment.entry.root.as_path(), &schema).map_err(watcher)?;
        attachment.subscription = subscription;
        self.heal(attachment)
    }

    fn detach(&self, _: &VaultName, attachment: Self::Attachment) {
        drop(attachment.subscription);
        let _ = attachment.store.close();
        drop(attachment.maintainership);
    }
}

fn heal_documents(
    store: &mut Store,
    root: &Path,
    exclusions: &[PathBuf],
    policy: ProductionPolicy,
) -> Result<(), JobFailure> {
    let mut files = walk(root, exclusions)
        .map_err(effect)?
        .filter_map(|fact| match fact {
            Ok(norn_fs::WalkFact::File(file)) => Some(Ok(file)),
            Ok(norn_fs::WalkFact::Skipped(_)) => None,
            Err(e) => Some(Err(e)),
        })
        .peekable();
    let mut after: Option<DocumentPath> = None;
    let mut stored = Vec::new();
    let mut index = 0usize;
    let mut exhausted = false;
    let mut changes = Vec::with_capacity(policy.changeset_size);
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
    }
    commit(store, &mut changes)
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

#[cfg(test)]
#[allow(clippy::disallowed_methods)] // test fixtures impersonate external editors and cleanup.
mod tests {
    use super::*;
    use norn_config::registry::VaultRoot;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

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
                ProductionEntryOps::new([entry], dirs, ProductionPolicy::new(page, 2)),
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
        let mut attachment = ops.attach(&name).unwrap();
        fs::remove_file(f.vault().join("a.md")).unwrap();
        fs::remove_file(f.vault().join("e.md")).unwrap();
        fs::write(f.vault().join("b.md"), "b").unwrap();
        fs::write(f.vault().join("c.md"), "changed").unwrap();
        fs::write(f.vault().join("h.md"), "h").unwrap();
        ops.reconcile(&name, &mut attachment, ReconcileWork::default())
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
        let (ops, name) = f.ops(2);
        let mut attachment = ops.attach(&name).unwrap();
        fs::write(f.vault().join("note.md"), "after").unwrap();
        let batch = attachment
            .subscription
            .recv_timeout(Duration::from_secs(5))
            .unwrap()
            .expect("watch batch");
        ops.reconcile(&name, &mut attachment, ReconcileWork { batch })
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
        ops.detach(&name, attachment);
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
}
