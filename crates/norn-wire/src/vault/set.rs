//! `vault set`: change a field of one registration.
//!
//! **An edit says what to do with a field, not what the field is.** A request
//! that carried the new value alone could not tell "leave the schema source
//! alone" apart from "clear it back to the in-vault default", because both are
//! spelled by the absence of a value. [`VaultChange`] makes the three an edit has —
//! keep, set, clear — three shapes, so an unmentioned field is kept and a
//! cleared field is cleared, and neither is inferred from a `null`.
//!
//! **A field with no default cannot be cleared.** A registration without a
//! root is not a registration, so the root's edit is a [`VaultReplace`], which
//! holds keep and set and has no clear to spell. A replacement is tagged
//! `change` exactly as a [`VaultChange`] is, and holds two of the same three
//! members, so a client reads both the same way. Dropping `clear` is a
//! refusal at the read path rather than a rule a handler enforces:
//! `{"change":"clear"}` is not a `VaultReplace` a reader accepts.
//!
//! **An edit under a standing park is refused under the park's own code**, so
//! a `vault set` never silently withdraws a park. A vault parked
//! `host/duplicate-root` therefore cannot be moved off the shared root by a
//! `vault set`; `vault unregister` of one of the names is the remedy.
//!
//! **The registry file changes first, and the serving set after.** From the
//! moment the edit finds the vault idle until it commits or is refused, every
//! request that asks the vault for anything is refused `host/entry-held`, while
//! `vault list` and `vault resolve` go on naming the registration as it stood.
//! Once the file records the edit, the vault is served under the registration
//! as edited, and the next attach reads its root, its schema source and its
//! watch backend. An edit that changes nothing answers the registration as it
//! stands and writes nothing.
//!
//! **A root is compared as the registry records it.** A root whose canonical
//! spelling differs from the recorded root is a root move to the canonical
//! spelling, even where the recorded spelling (a link, a trailing `/`)
//! reaches the same directory: the store does not record the directory it
//! was derived from, so the host re-derives rather than trust it. A root
//! whose canonical spelling is the recorded root is no move.
//!
//! **The host's served registrations are authoritative over a hand edit of
//! the registry file.** The edit is made to the registration the host serves,
//! and the file is written with that registration as edited: over a root or a
//! field a hand edit changed, and under a served name a hand edit took out. A
//! hand edit takes effect when the host next starts.
//!
//! **A root move discards the derived state the old root left** — the derived
//! database, its sidecars, the semantic sidecar and the shadow homes, as
//! `vault unregister` discards them — so nothing the old root held is answered
//! from the new one, and the next attach derives the new root under a new
//! store epoch. The vault's own documents, at either root, are never touched.
//!
//! The other refusals are `host/unknown-vault` where no such registration
//! exists, `host/entry-held` where the entry is in use,
//! `host/registry-unwritable` where the registry file could not be read or
//! replaced, and the pre-check codes a register meets where the edit moves the
//! root: `host/duplicate-root` where another registration already reaches the
//! new root, `host/shared-schema` where an edit of the root or the schema
//! source would serve the vault under a schema file another registration
//! uses, and `host/entry-untrusted` where the new root itself could not be
//! read — carrying the environmental-refusal reason, which is the rendering
//! the registry recheck gives such a root. A root move is also refused as
//! `vault unregister` is refused over the derived state it discards:
//! `host/maintainer-contended` where another process maintains it, and
//! `host/entry-untrusted` carrying the environmental-refusal reason where the
//! data directory refused the maintainer lock or the discard. After any
//! refusal the registration that stood before still stands.

use schemars::JsonSchema;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::address::{PollBackend, SchemaSource, VaultRoot};
use crate::name::VaultName;
use crate::status::{Published, Registration};

/// What an edit does to a field that has a default to fall back to.
///
/// On the wire a change is an object tagged `change`: `{"change":"keep"}`,
/// `{"change":"set","value":…}`, `{"change":"clear"}`.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(tag = "change", rename_all = "snake_case")]
#[serde(bound(serialize = "T: Serialize", deserialize = "T: DeserializeOwned"))]
#[non_exhaustive]
pub enum VaultChange<T: JsonSchema + Serialize + DeserializeOwned> {
    /// Leave the field as it stands.
    Keep {},
    /// Put this value in the field.
    #[non_exhaustive]
    Set {
        /// The value to put there.
        value: T,
    },
    /// Empty the field, so it falls back to its default.
    Clear {},
}

impl<T: JsonSchema + Serialize + DeserializeOwned> VaultChange<T> {
    /// Leave the field as it stands.
    pub const fn keep() -> Self {
        VaultChange::Keep {}
    }

    /// Put `value` in the field.
    pub const fn set(value: T) -> Self {
        VaultChange::Set { value }
    }

    /// Empty the field.
    pub const fn clear() -> Self {
        VaultChange::Clear {}
    }

    /// What the field holds once this edit is made to `field`: `field` as it
    /// stands for a keep, the value for a set, and nothing for a clear.
    pub fn applied_to(&self, field: Option<T>) -> Option<T>
    where
        T: Clone,
    {
        match self {
            VaultChange::Keep {} => field,
            VaultChange::Set { value } => Some(value.clone()),
            VaultChange::Clear {} => None,
        }
    }
}

impl<T: JsonSchema + Serialize + DeserializeOwned> Default for VaultChange<T> {
    /// An edit that names nothing leaves the field as it stands.
    fn default() -> Self {
        VaultChange::keep()
    }
}

/// What an edit does to a field that has no default to fall back to.
///
/// On the wire a replacement is an object tagged `change`:
/// `{"change":"keep"}`, `{"change":"set","value":…}`. There is no `clear`,
/// because a field spelled this way is one a registration cannot be
/// without.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(tag = "change", rename_all = "snake_case")]
#[serde(bound(serialize = "T: Serialize", deserialize = "T: DeserializeOwned"))]
#[non_exhaustive]
pub enum VaultReplace<T: JsonSchema + Serialize + DeserializeOwned> {
    /// Leave the field as it stands.
    Keep {},
    /// Put this value in the field.
    #[non_exhaustive]
    Set {
        /// The value to put there.
        value: T,
    },
}

impl<T: JsonSchema + Serialize + DeserializeOwned> VaultReplace<T> {
    /// Leave the field as it stands.
    pub const fn keep() -> Self {
        VaultReplace::Keep {}
    }

    /// Put `value` in the field.
    pub const fn set(value: T) -> Self {
        VaultReplace::Set { value }
    }

    /// The value this edit puts in the field, and nothing where it keeps the
    /// field as it stands.
    pub const fn value(&self) -> Option<&T> {
        match self {
            VaultReplace::Keep {} => None,
            VaultReplace::Set { value } => Some(value),
        }
    }
}

impl<T: JsonSchema + Serialize + DeserializeOwned> Default for VaultReplace<T> {
    /// An edit that names nothing leaves the field as it stands.
    fn default() -> Self {
        VaultReplace::keep()
    }
}

/// What a `vault set` request carries.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[non_exhaustive]
pub struct VaultSetParams {
    /// The registration to edit.
    pub name: VaultName,
    /// What to do with the root. `keep` leaves it as registered; `set` moves
    /// the registration to the given root. There is no `clear`: a
    /// registration cannot be without a root.
    pub root: VaultReplace<VaultRoot>,
    /// What to do with the schema source. `keep` leaves it as registered;
    /// `set` reads the schema from the given path; `clear` returns the vault
    /// to the in-vault default.
    pub schema_source: VaultChange<SchemaSource>,
    /// What to do with the watch backend. `keep` leaves it as registered;
    /// `set` pins the given backend; `clear` returns the vault to the
    /// platform's native one.
    pub poll_backend: VaultChange<PollBackend>,
}

impl VaultSetParams {
    /// An edit of `name` that changes nothing.
    pub const fn new(name: VaultName) -> Self {
        VaultSetParams {
            name,
            root: VaultReplace::keep(),
            schema_source: VaultChange::keep(),
            poll_backend: VaultChange::keep(),
        }
    }

    /// The edit doing `root` to the root.
    #[must_use]
    pub fn with_root(mut self, root: VaultReplace<VaultRoot>) -> Self {
        self.root = root;
        self
    }

    /// The edit doing `schema_source` to the schema source.
    #[must_use]
    pub fn with_schema_source(mut self, schema_source: VaultChange<SchemaSource>) -> Self {
        self.schema_source = schema_source;
        self
    }

    /// The edit doing `poll_backend` to the watch backend.
    #[must_use]
    pub fn with_poll_backend(mut self, poll_backend: VaultChange<PollBackend>) -> Self {
        self.poll_backend = poll_backend;
        self
    }
}

/// What `vault set` answers with: the registration as it now stands, and what
/// its entry publishes after the edit.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[non_exhaustive]
pub struct VaultSetReport {
    /// The registration as it now stands.
    pub registration: Registration,
    /// What the edited entry publishes.
    pub published: Published,
}

impl VaultSetReport {
    /// The edit left `registration`, whose entry publishes `published`.
    pub const fn new(registration: Registration, published: Published) -> Self {
        VaultSetReport {
            registration,
            published,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn backend() -> Option<PollBackend> {
        Some(PollBackend::Poll)
    }

    /// A keep leaves the field as it stands, whatever it holds.
    #[test]
    fn a_kept_field_stands_as_it_was() {
        assert_eq!(VaultChange::keep().applied_to(backend()), backend());
        assert_eq!(VaultChange::<PollBackend>::keep().applied_to(None), None);
    }

    /// A set puts its value in the field, over a value or over nothing.
    #[test]
    fn a_set_field_holds_the_value() {
        assert_eq!(
            VaultChange::set(PollBackend::Poll).applied_to(None),
            backend()
        );
    }

    /// A clear empties the field, which falls back to its default.
    #[test]
    fn a_cleared_field_holds_nothing() {
        assert_eq!(
            VaultChange::<PollBackend>::clear().applied_to(backend()),
            None
        );
    }

    /// A replacement names the value it puts in the field, and a keep names
    /// none.
    #[test]
    fn a_replacement_names_its_value_and_a_keep_names_none() {
        let root = VaultRoot::new("/srv/vaults/notes").unwrap();
        assert_eq!(VaultReplace::set(root.clone()).value(), Some(&root));
        assert_eq!(VaultReplace::<VaultRoot>::keep().value(), None);
    }
}
