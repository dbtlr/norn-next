use std::collections::{BTreeMap, BTreeSet};

use norn_config::VaultName;
use norn_config::registry::{Entry, Registry};
use norn_fs::{Identity, Refusal, path_identity};

/// Every registry name that resolves to one filesystem root.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AliasConflict {
    pub aliases: Vec<VaultName>,
}

/// The serving set after filesystem aliases have been classified.
#[derive(Clone, Debug)]
pub struct ServingRegistry {
    entries: BTreeMap<VaultName, Entry>,
    conflicts: BTreeMap<VaultName, AliasConflict>,
}

impl ServingRegistry {
    /// Read the serving set without choosing a winner among duplicate roots.
    /// Missing roots remain registrable and are rechecked on demand.
    pub fn read(registry: &Registry) -> Result<Self, Refusal> {
        Self::from_entries(registry.entries().cloned())
    }

    /// Build a serving set from an already-read registry projection.
    pub fn from_entries(entries: impl IntoIterator<Item = Entry>) -> Result<Self, Refusal> {
        let entries = entries
            .into_iter()
            .map(|entry| (entry.name.clone(), entry))
            .collect::<BTreeMap<_, _>>();
        let conflicts = classify(&entries)?;
        Ok(Self { entries, conflicts })
    }

    pub fn entry(&self, name: &VaultName) -> Option<&Entry> {
        self.entries.get(name)
    }

    pub fn conflict(&self, name: &VaultName) -> Option<&AliasConflict> {
        self.conflicts.get(name)
    }

    /// Re-evaluate aliases at attach time so roots that appeared since the
    /// registry read cannot bypass the maintainer singleton.
    pub fn recheck(&self, name: &VaultName) -> Result<Option<AliasConflict>, Refusal> {
        if !self.entries.contains_key(name) {
            return Ok(None);
        }
        Ok(classify(&self.entries)?.remove(name))
    }

    /// Resolve one registered root for an attach-time acquisition claim.
    pub(crate) fn identity(&self, name: &VaultName) -> Result<Option<Identity>, Refusal> {
        self.entries
            .get(name)
            .map(|entry| path_identity(entry.root.as_path()))
            .transpose()
            .map(Option::flatten)
    }

    pub fn entries(&self) -> impl Iterator<Item = &Entry> {
        self.entries.values()
    }
}

fn classify(
    entries: &BTreeMap<VaultName, Entry>,
) -> Result<BTreeMap<VaultName, AliasConflict>, Refusal> {
    let mut identities = BTreeMap::<Identity, BTreeSet<VaultName>>::new();
    for entry in entries.values() {
        if let Some(identity) = path_identity(entry.root.as_path())? {
            identities
                .entry(identity)
                .or_default()
                .insert(entry.name.clone());
        }
    }

    let mut conflicts = BTreeMap::new();
    for aliases in identities.into_values().filter(|names| names.len() > 1) {
        let aliases = aliases.into_iter().collect::<Vec<_>>();
        let conflict = AliasConflict {
            aliases: aliases.clone(),
        };
        for alias in aliases {
            conflicts.insert(alias, conflict.clone());
        }
    }
    Ok(conflicts)
}

#[cfg(test)]
mod tests {
    use super::*;
    use norn_config::registry::VaultRoot;

    fn entry(name: &str, root: &str) -> Entry {
        Entry::new(VaultName::new(name).unwrap(), VaultRoot::new(root).unwrap())
    }

    #[test]
    fn every_alias_is_refused_and_names_the_whole_conflict() {
        let registry =
            ServingRegistry::from_entries([entry("alpha", "/tmp"), entry("beta", "/tmp/.")])
                .unwrap();
        let expected = vec![
            VaultName::new("alpha").unwrap(),
            VaultName::new("beta").unwrap(),
        ];
        assert_eq!(registry.conflict(&expected[0]).unwrap().aliases, expected);
        assert_eq!(registry.conflict(&expected[1]).unwrap().aliases, expected);
    }

    #[test]
    fn a_missing_root_stays_registrable() {
        let registry = ServingRegistry::from_entries([entry(
            "later",
            "/tmp/norn-host-root-that-does-not-exist",
        )])
        .unwrap();
        assert!(
            registry
                .conflict(&VaultName::new("later").unwrap())
                .is_none()
        );
    }
}
