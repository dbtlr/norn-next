//! The registry file: which vaults exist on this machine, and where.
//!
//! **This module is host-only.** What is here is the registry's *shape and
//! storage* — the five fields an entry carries, how they are written down, and
//! the locked read-modify-write that changes them. The registry's *semantics*
//! are the orchestrator's: which entries are being served, what attaching one
//! means, whether two entries point at the same tree, what happens when a root
//! disappears. None of those questions is asked here, and none of them can be
//! answered here without reaching a filesystem this crate does not touch.
//!
//! Two absences are deliberate:
//!
//! - **No duplicate-root detection.** Deciding that two roots are the same
//!   directory means resolving symlinks and comparing device and inode, which
//!   is a filesystem read. [`VaultRoot`] refuses a path that is not absolute
//!   UTF-8 — pure syntax, decidable from the string — and stops there.
//! - **No vault is opened.** An entry names a root and possibly a schema
//!   source. Neither path is checked for existence, and neither file is read.
//!
//! # What a rewrite preserves
//!
//! A key this build does not model survives a rewrite, at the top level and
//! inside an entry alike, because the document is held as it was parsed and a
//! write edits the keys the typed view owns. **Comments and key order do
//! not**: the file is re-rendered from the parsed document, so a hand-written
//! comment is gone after the next write and keys come back in the order the
//! renderer emits them. What is preserved is the *state* a newer build wrote,
//! which is what a lost field would cost; the layout of the text is not.
//!
//! # The file
//!
//! ```toml
//! version = 1
//!
//! [vaults.notes]
//! root = "/home/person/notes"
//! semantic_search = true
//!
//! [vaults.work]
//! root = "/home/person/work"
//! schema_source = "/home/person/.config/norn/schemas/work.yaml"
//! poll_backend = "poll"
//! ```
//!
//! The entry's name is the table key rather than a field, because it is the
//! key: a name appearing twice is not a thing TOML can express, which is one
//! duplicate class that cannot reach the reader at all.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use toml::{Table, Value};

use crate::document::{self, Document};
use crate::error::{ConfigError, corrupt};
use crate::file::{Sensitivity, Stored};
use crate::{ConfigDirs, VaultName, absolute_path};

/// The key of the table holding the entries.
const VAULTS_KEY: &str = "vaults";

const ROOT_KEY: &str = "root";
const SCHEMA_SOURCE_KEY: &str = "schema_source";
const SEMANTIC_SEARCH_KEY: &str = "semantic_search";
const POLL_BACKEND_KEY: &str = "poll_backend";

/// A vault's root directory, as recorded.
///
/// Absolute and UTF-8, and nothing more is claimed. Whether it exists, whether
/// it is a directory, whether it is the same directory as another entry's root
/// — all three are filesystem questions, and this crate does not ask them.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VaultRoot(PathBuf);

impl VaultRoot {
    /// The root `path` names, or a refusal if it is not one.
    ///
    /// A relative root resolves against whatever directory the reading process
    /// happens to be in, and machine-local state is read by a service, a CLI
    /// and a shim that are in three different ones. A root that is not UTF-8
    /// cannot be written into the file at all: rendering it would substitute
    /// replacement characters and report a success that recorded a different
    /// directory.
    pub fn new(path: impl Into<PathBuf>) -> Result<Self, ConfigError> {
        Ok(VaultRoot(absolute_path(path.into(), "vault root")?))
    }

    pub fn as_path(&self) -> &Path {
        &self.0
    }

    /// The root as it is written down. Infallible: the constructor refused
    /// anything that is not text.
    fn as_str(&self) -> &str {
        self.0.to_str().expect("a vault root is UTF-8")
    }
}

/// Where a vault's schema is read from, when it is not the in-vault default.
///
/// The same two demands as [`VaultRoot`], for the same two reasons: the path
/// is recorded in a text file and resolved by processes in different working
/// directories. Whether the file exists, and what is in it, belong to whoever
/// resolves the entry.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SchemaSource(PathBuf);

impl SchemaSource {
    /// The schema source `path` names, or a refusal if it is not one.
    pub fn new(path: impl Into<PathBuf>) -> Result<Self, ConfigError> {
        Ok(SchemaSource(absolute_path(path.into(), "schema source")?))
    }

    pub fn as_path(&self) -> &Path {
        &self.0
    }

    /// The source as it is written down. Infallible: the constructor refused
    /// anything that is not text.
    fn as_str(&self) -> &str {
        self.0.to_str().expect("a schema source is UTF-8")
    }
}

/// A filesystem-watch backend an entry pins in place of the platform's native
/// one.
///
/// Absence is the native backend, which is why the field is an option rather
/// than a variant: an entry says nothing about watching unless the native
/// backend does not work for its root — a network mount, a container bind
/// mount — and polling is what does.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PollBackend {
    /// Walk the tree on an interval instead of subscribing to the platform's
    /// notification API.
    Poll,
}

impl PollBackend {
    const POLL: &'static str = "poll";

    fn as_str(self) -> &'static str {
        match self {
            PollBackend::Poll => PollBackend::POLL,
        }
    }

    fn parse(text: &str) -> Option<Self> {
        match text {
            PollBackend::POLL => Some(PollBackend::Poll),
            _ => None,
        }
    }
}

/// One registered vault.
///
/// Five fields and no others. Anything a host learns about a vault by looking
/// at it — its trust state, its store's generation, when it was last derived —
/// is derived state and lives where derived state lives, not in the file a
/// person edits.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Entry {
    /// The name the vault is addressed and keyed by.
    pub name: VaultName,
    /// The vault's root directory.
    pub root: VaultRoot,
    /// Where the vault's schema is read from. Absent means the in-vault
    /// default, [`crate::IN_VAULT_SCHEMA_PATH`], relative to the root.
    pub schema_source: Option<SchemaSource>,
    /// Whether semantic search is enabled for this vault. Off unless somebody
    /// turned it on: it is the one feature that fetches a model.
    pub semantic_search: bool,
    /// The watch backend this entry pins. Absent is the platform's native one.
    pub poll_backend: Option<PollBackend>,
}

impl Entry {
    /// An entry with the two fields that have no default, and the defaults for
    /// the three that do.
    pub fn new(name: VaultName, root: VaultRoot) -> Self {
        Entry {
            name,
            root,
            schema_source: None,
            semantic_search: false,
            poll_backend: None,
        }
    }
}

/// The registry, as read.
///
/// Holds the document it was read from as well as the entries projected out of
/// it, so that a field written by a newer build survives a rewrite by this
/// one.
#[derive(Clone, Debug)]
pub struct Registry {
    document: Document,
    /// The sub-table each entry was read from, keyed the same way as
    /// [`Registry::entries`]. A rewrite edits these rather than building fresh
    /// ones, which is what preserves a key this build does not model.
    raw: BTreeMap<VaultName, Table>,
    entries: BTreeMap<VaultName, Entry>,
}

impl Registry {
    /// The entry registered under `name`.
    pub fn get(&self, name: &VaultName) -> Option<&Entry> {
        self.entries.get(name)
    }

    /// Every entry, in name order.
    pub fn entries(&self) -> impl Iterator<Item = &Entry> {
        self.entries.values()
    }

    /// Register `entry`, replacing whatever was registered under its name.
    ///
    /// Keyed by the entry's own name, so the key and the field cannot disagree.
    pub fn insert(&mut self, entry: Entry) -> Option<Entry> {
        self.entries.insert(entry.name.clone(), entry)
    }

    /// Remove the entry registered under `name`.
    ///
    /// The keys this build does not model go with it. **Unknown-key retention
    /// is scoped to an entry's lifetime, not to its name**: a name registered
    /// again later is a new entry, and grafting the removed one's fields onto
    /// it — a `revoked` flag, a scope a newer build wrote — would resurrect
    /// state nobody asked to keep.
    pub fn remove(&mut self, name: &VaultName) -> Option<Entry> {
        self.raw.remove(name);
        self.entries.remove(name)
    }

    fn load(path: &Path, bytes: Option<&[u8]>) -> Result<Self, ConfigError> {
        let document = Document::read(path, bytes)?;
        let section = document.section(path, VAULTS_KEY)?;

        let mut raw = BTreeMap::new();
        let mut entries = BTreeMap::new();
        for (key, value) in &section {
            let context = format!("vault `{key}`");
            let table = document::entry_table(path, &context, value)?;
            let name = VaultName::new(key)?;
            let root =
                VaultRoot::new(document::required_string(path, &context, &table, ROOT_KEY)?)?;
            let schema_source =
                match document::optional_string(path, &context, &table, SCHEMA_SOURCE_KEY)? {
                    Some(source) => Some(SchemaSource::new(source)?),
                    None => None,
                };
            let semantic_search =
                document::boolean(path, &context, &table, SEMANTIC_SEARCH_KEY, false)?;
            let poll_backend =
                match document::optional_string(path, &context, &table, POLL_BACKEND_KEY)? {
                    Some(text) => Some(PollBackend::parse(&text).ok_or_else(|| {
                        corrupt(
                            path,
                            format!("{context}: `{POLL_BACKEND_KEY}` names no backend: `{text}`"),
                        )
                    })?),
                    None => None,
                };

            raw.insert(name.clone(), table);
            entries.insert(
                name.clone(),
                Entry {
                    name,
                    root,
                    schema_source,
                    semantic_search,
                    poll_backend,
                },
            );
        }
        Ok(Registry {
            document,
            raw,
            entries,
        })
    }

    /// The registry as TOML, with every key this build does not model still in
    /// the place it was read from.
    ///
    /// Takes the registry by mutable reference so that the section lands in
    /// the document rather than in a copy of it: a registry of any size is
    /// rendered without the whole document being cloned twice on the way.
    fn render(&mut self, path: &Path) -> Result<String, ConfigError> {
        let mut section = Table::new();
        for (name, entry) in &self.entries {
            // An entry that was already in the file keeps its own sub-table,
            // unknown keys and all; the known keys are then overwritten on top
            // of it. An entry that is new starts empty.
            let mut table = self.raw.get(name).cloned().unwrap_or_default();
            table.insert(
                ROOT_KEY.to_string(),
                Value::String(entry.root.as_str().to_string()),
            );
            document::set_optional(
                &mut table,
                SCHEMA_SOURCE_KEY,
                entry
                    .schema_source
                    .as_ref()
                    .map(|source| Value::String(source.as_str().to_string())),
            );
            table.insert(
                SEMANTIC_SEARCH_KEY.to_string(),
                Value::Boolean(entry.semantic_search),
            );
            document::set_optional(
                &mut table,
                POLL_BACKEND_KEY,
                entry
                    .poll_backend
                    .map(|backend| Value::String(backend.as_str().to_string())),
            );
            section.insert(name.as_str().to_string(), Value::Table(table));
        }
        self.document.set_section(VAULTS_KEY, section);
        self.document.render(path)
    }
}

/// The registry file, as this crate reads and replaces it.
fn stored(dirs: &ConfigDirs) -> Stored<Registry> {
    Stored::new(
        dirs.registry_file(),
        Sensitivity::Shared,
        Registry::load,
        Registry::render,
    )
}

/// Read the registry. Never writes, whatever it finds.
///
/// A registry file that is not there reads as an empty registry: a first run
/// and a machine with no vaults registered are the same state, and neither is
/// an error. A file written by an older build is migrated in memory and left
/// on disk as it was — the migrated form reaches the file on the next
/// [`mutate`], not on this read.
///
/// The registry file is not a secret and is not held to the token file's mode:
/// it is written at `0644` and read at whatever mode it has. A mode somebody
/// tightened by hand is honoured until the next write, which restores the
/// one this crate writes.
pub fn read(dirs: &ConfigDirs) -> Result<Registry, ConfigError> {
    stored(dirs).read()
}

/// Read the registry, hand it to `apply`, and write back what `apply` leaves.
///
/// The exclusive lock is taken before the read and released after the file is
/// replaced, so no second writer's read can begin inside this one's window.
/// **A lost update is not possible**, rather than unlikely: the whole
/// read-modify-write is one critical section. The wait for the lock is
/// unbounded — the thing being waited for is another writer's rewrite of a
/// small file.
///
/// A refusal from `apply` leaves the file untouched, and so does a mutation
/// that changes nothing: bytes identical to the ones read are not written back.
///
/// **`apply` must not call [`mutate`] again.** The lock is exclusive and held
/// across the whole call, so a nested mutation would wait on its own caller;
/// it is refused with
/// [`NestedMutation`](ConfigError::NestedMutation) rather than allowed to
/// hang.
pub fn mutate<T>(
    dirs: &ConfigDirs,
    apply: impl FnOnce(&mut Registry) -> Result<T, ConfigError>,
) -> Result<T, ConfigError> {
    stored(dirs).mutate(apply)
}
