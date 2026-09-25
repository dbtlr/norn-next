//! `vault set`: change a field of one registration.
//!
//! **An edit says what to do with a field, not what the field is.** A request
//! that carried the new value alone could not tell "leave the schema source
//! alone" apart from "clear it back to the in-vault default", because both are
//! spelled by the absence of a value. [`Change`] makes the three an edit has —
//! keep, set, clear — three shapes, so an unmentioned field is kept and a
//! cleared field is cleared, and neither is inferred from a `null`.
//!
//! **A field with no default cannot be cleared.** A registration without a
//! root is not a registration, so the root's edit is a [`Replace`], which
//! holds keep and set and has no clear to spell. A replacement is tagged
//! `change` exactly as a [`Change`] is, and holds two of the same three
//! members, so a client reads both the same way. Dropping `clear` is a
//! refusal at the read path rather than a rule a handler enforces:
//! `{"change":"clear"}` is not a `Replace` a reader accepts.
//!
//! **An edit under a standing park is refused under the park's own code**, so
//! a `vault set` never silently withdraws a park.
//!
//! The other refusals are `host/unknown-vault` where no such registration
//! exists, `host/entry-held` where the entry is in use,
//! `host/registry-unwritable` where the registry file could not be replaced,
//! and the pre-check codes a register meets where the edit moves the root:
//! `host/duplicate-root` where another registration already reaches the new
//! root, and `host/entry-untrusted` where the new root itself could not be
//! read — carrying the environmental-refusal reason, which is the rendering
//! the registry recheck gives such a root.

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
pub enum Change<T: JsonSchema + Serialize + DeserializeOwned> {
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

impl<T: JsonSchema + Serialize + DeserializeOwned> Change<T> {
    /// Leave the field as it stands.
    pub const fn keep() -> Self {
        Change::Keep {}
    }

    /// Put `value` in the field.
    pub const fn set(value: T) -> Self {
        Change::Set { value }
    }

    /// Empty the field.
    pub const fn clear() -> Self {
        Change::Clear {}
    }

    /// What the field holds once this edit is made to `field`: `field` as it
    /// stands for a keep, the value for a set, and nothing for a clear.
    pub fn applied_to(&self, field: Option<T>) -> Option<T>
    where
        T: Clone,
    {
        match self {
            Change::Keep {} => field,
            Change::Set { value } => Some(value.clone()),
            Change::Clear {} => None,
        }
    }
}

impl<T: JsonSchema + Serialize + DeserializeOwned> Default for Change<T> {
    /// An edit that names nothing leaves the field as it stands.
    fn default() -> Self {
        Change::keep()
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
pub enum Replace<T: JsonSchema + Serialize + DeserializeOwned> {
    /// Leave the field as it stands.
    Keep {},
    /// Put this value in the field.
    #[non_exhaustive]
    Set {
        /// The value to put there.
        value: T,
    },
}

impl<T: JsonSchema + Serialize + DeserializeOwned> Replace<T> {
    /// Leave the field as it stands.
    pub const fn keep() -> Self {
        Replace::Keep {}
    }

    /// Put `value` in the field.
    pub const fn set(value: T) -> Self {
        Replace::Set { value }
    }

    /// The value this edit puts in the field, and nothing where it keeps the
    /// field as it stands.
    pub const fn value(&self) -> Option<&T> {
        match self {
            Replace::Keep {} => None,
            Replace::Set { value } => Some(value),
        }
    }
}

impl<T: JsonSchema + Serialize + DeserializeOwned> Default for Replace<T> {
    /// An edit that names nothing leaves the field as it stands.
    fn default() -> Self {
        Replace::keep()
    }
}

/// What a `vault set` request carries.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[non_exhaustive]
pub struct SetParams {
    /// The registration to edit.
    pub name: VaultName,
    /// What to do with the root. `keep` leaves it as registered; `set` moves
    /// the registration to the given root. There is no `clear`: a
    /// registration cannot be without a root.
    pub root: Replace<VaultRoot>,
    /// What to do with the schema source. `keep` leaves it as registered;
    /// `set` reads the schema from the given path; `clear` returns the vault
    /// to the in-vault default.
    pub schema_source: Change<SchemaSource>,
    /// What to do with the watch backend. `keep` leaves it as registered;
    /// `set` pins the given backend; `clear` returns the vault to the
    /// platform's native one.
    pub poll_backend: Change<PollBackend>,
}

impl SetParams {
    /// An edit of `name` that changes nothing.
    pub const fn new(name: VaultName) -> Self {
        SetParams {
            name,
            root: Replace::keep(),
            schema_source: Change::keep(),
            poll_backend: Change::keep(),
        }
    }

    /// The edit doing `root` to the root.
    #[must_use]
    pub fn with_root(mut self, root: Replace<VaultRoot>) -> Self {
        self.root = root;
        self
    }

    /// The edit doing `schema_source` to the schema source.
    #[must_use]
    pub fn with_schema_source(mut self, schema_source: Change<SchemaSource>) -> Self {
        self.schema_source = schema_source;
        self
    }

    /// The edit doing `poll_backend` to the watch backend.
    #[must_use]
    pub fn with_poll_backend(mut self, poll_backend: Change<PollBackend>) -> Self {
        self.poll_backend = poll_backend;
        self
    }
}

/// What `vault set` answers with: the registration as it now stands, and what
/// its entry publishes after the edit.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[non_exhaustive]
pub struct SetReport {
    /// The registration as it now stands.
    pub registration: Registration,
    /// What the edited entry publishes.
    pub published: Published,
}

impl SetReport {
    /// The edit left `registration`, whose entry publishes `published`.
    pub const fn new(registration: Registration, published: Published) -> Self {
        SetReport {
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
        assert_eq!(Change::keep().applied_to(backend()), backend());
        assert_eq!(Change::<PollBackend>::keep().applied_to(None), None);
    }

    /// A set puts its value in the field, over a value or over nothing.
    #[test]
    fn a_set_field_holds_the_value() {
        assert_eq!(Change::set(PollBackend::Poll).applied_to(None), backend());
    }

    /// A clear empties the field, which falls back to its default.
    #[test]
    fn a_cleared_field_holds_nothing() {
        assert_eq!(Change::<PollBackend>::clear().applied_to(backend()), None);
    }

    /// A replacement names the value it puts in the field, and a keep names
    /// none.
    #[test]
    fn a_replacement_names_its_value_and_a_keep_names_none() {
        let root = VaultRoot::new("/srv/vaults/notes").unwrap();
        assert_eq!(Replace::set(root.clone()).value(), Some(&root));
        assert_eq!(Replace::<VaultRoot>::keep().value(), None);
    }
}
