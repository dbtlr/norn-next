//! The registry file: which vaults exist on this machine, and where.
//!
//! **This module is host-only.** What is here is the registry's *shape and
//! storage* — the four fields an entry carries, how they are written down, and
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
use std::path::Path;

use toml::{Table, Value};

use crate::document::{self, Document};
use crate::error::{ConfigError, corrupt};
use crate::file::{Sensitivity, Stored};
use crate::{ConfigDirs, VaultName};

/// The key of the table holding the entries.
const VAULTS_KEY: &str = "vaults";

const ROOT_KEY: &str = "root";
const SCHEMA_SOURCE_KEY: &str = "schema_source";
const POLL_BACKEND_KEY: &str = "poll_backend";

/// A vault's root directory, as recorded.
///
/// The grammar is `norn-wire`'s and is re-exported here: a root crosses the
/// client/host seam as well as being written into the registry file, so it is
/// checked once, there.
pub use norn_wire::VaultRoot;

/// Where a vault's schema is read from, when it is not the in-vault default.
///
/// The same grammar as [`VaultRoot`], re-exported on the same terms.
pub use norn_wire::SchemaSource;

/// A filesystem-watch backend an entry pins in place of the platform's native
/// one.
///
/// Absence is the native backend, which is why the field is an option rather
/// than a variant: an entry says nothing about watching unless the native
/// backend does not work for its root — a network mount, a container bind
/// mount — and polling is what does. The vocabulary is `norn-wire`'s and is
/// re-exported here.
pub use norn_wire::PollBackend;

/// A path outside the grammar a root or a schema source is held to.
pub use norn_wire::IllegalPath;

/// The registration that crosses the client/host seam, which is the four
/// fields the registry file holds and no others.
///
/// Anything a host learns about a vault by looking at it — its trust state,
/// its store's generation, when it was last derived — is derived state and
/// lives where derived state lives, not in the file a person edits.
pub use norn_wire::Registration;

/// A registry entry.
///
/// The entry and the registration are one reading of a registered vault, so
/// there is one type rather than two shapes and a conversion between them.
/// The name stays because the registry file is what this module is about: an
/// entry is what the file holds, and a registration is what that same value is
/// called at the seam.
pub type Entry = Registration;

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
    /// A displaced entry takes the keys this build does not model with it —
    /// the replacement is a new entry, not a continuation of the old one.
    pub fn insert(&mut self, entry: Entry) -> Option<Entry> {
        self.raw.remove(&entry.name);
        self.entries.insert(entry.name.clone(), entry)
    }

    /// Change the entry registered under `entry`'s name to `entry`, keeping
    /// the keys this build does not model that it was read with.
    ///
    /// An amendment is the entry going on, where [`Registry::insert`] is a
    /// new one arriving: its fields are the ones `entry` holds, and a key a
    /// newer build wrote beside them stays with it. A name nothing is
    /// registered under is registered as new.
    pub fn amend(&mut self, entry: Entry) -> Option<Entry> {
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
            let poll_backend =
                match document::optional_string(path, &context, &table, POLL_BACKEND_KEY)? {
                    Some(text) => Some(PollBackend::try_from(text.as_str()).map_err(|_| {
                        corrupt(
                            path,
                            format!("{context}: `{POLL_BACKEND_KEY}` names no backend: `{text}`"),
                        )
                    })?),
                    None => None,
                };

            raw.insert(name.clone(), table);
            // `Registration` is `#[non_exhaustive]` and belongs to another
            // crate, so a field added to it is filled in here by
            // `Entry::new`'s default rather than refused by the compiler: this
            // parser goes on compiling while projecting nothing for the new
            // key. What catches that is the registry file's round trip —
            // `a_registration_round_trips_through_the_file_over_every_combination`
            // and its siblings compare the read-back registration to the
            // written one whole, so a field a producer sets and this parser
            // does not project comes back unequal.
            let mut entry = Entry::new(name.clone(), root);
            entry.schema_source = schema_source;
            entry.poll_backend = poll_backend;
            entries.insert(name, entry);
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
/// unbounded: a waiter waits for the holder's whole change — its read, its
/// `apply`, whatever that `apply` does while the lock is held, and its write.
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
