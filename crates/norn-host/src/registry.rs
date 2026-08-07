use std::collections::{BTreeMap, BTreeSet};

use norn_config::VaultName;
use norn_config::registry::{Entry, Registry};
use norn_fs::{Identity, Refusal, path_identity};

/// Every registry name that resolves to one filesystem root.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AliasConflict {
    pub aliases: Vec<VaultName>,
}

/// What one read of the registry resolved for one registered name.
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
        let conflicts = classify(&entries);
        Ok(Self { entries, conflicts })
    }

    pub fn entry(&self, name: &VaultName) -> Option<&Entry> {
        self.entries.get(name)
    }

    pub fn conflict(&self, name: &VaultName) -> Option<&AliasConflict> {
        self.conflicts.get(name)
    }

    /// Re-evaluate aliases at attach time so roots that appeared since the
    /// registry read cannot bypass the maintainer singleton, and answer with
    /// the identity that classification resolved for this name.
    pub(crate) fn recheck(&self, name: &VaultName) -> Result<RootReading, Refusal> {
        if !self.entries.contains_key(name) {
            return Ok(RootReading::default());
        }
        let (identity, mut conflicts) = classify_for(&self.entries, name)?;
        Ok(RootReading {
            identity,
            conflict: conflicts.remove(name),
        })
    }

    pub fn entries(&self) -> impl Iterator<Item = &Entry> {
        self.entries.values()
    }
}

/// Classify every registered root against the others, answering with the
/// identity `requested` resolved to and the conflicts the classification
/// raised. A refusal over `requested`'s own root is this read's refusal; a
/// refusal over any other root belongs to that name and is left for the read
/// that asks about it.
fn classify_for(
    entries: &BTreeMap<VaultName, Entry>,
    requested: &VaultName,
) -> Result<(Option<Identity>, BTreeMap<VaultName, AliasConflict>), Refusal> {
    let mut identities = BTreeMap::<Identity, BTreeSet<VaultName>>::new();
    let mut resolved = None;
    for entry in entries.values() {
        match path_identity(entry.root.as_path()) {
            Ok(Some(identity)) => {
                if &entry.name == requested {
                    resolved = Some(identity);
                }
                identities
                    .entry(identity)
                    .or_default()
                    .insert(entry.name.clone());
            }
            Ok(None) => {}
            Err(refusal) if &entry.name == requested => return Err(refusal),
            Err(_) => {}
        }
    }
    Ok((resolved, conflicts_from_identities(identities)))
}

fn classify(entries: &BTreeMap<VaultName, Entry>) -> BTreeMap<VaultName, AliasConflict> {
    let mut identities = BTreeMap::<Identity, BTreeSet<VaultName>>::new();
    for entry in entries.values() {
        // An environmental refusal belongs to this entry, not to the registry
        // as a whole. Demand rechecks it and reports the refusal for that name.
        if let Ok(Some(identity)) = path_identity(entry.root.as_path()) {
            identities
                .entry(identity)
                .or_default()
                .insert(entry.name.clone());
        }
    }

    conflicts_from_identities(identities)
}

fn conflicts_from_identities(
    identities: BTreeMap<Identity, BTreeSet<VaultName>>,
) -> BTreeMap<VaultName, AliasConflict> {
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
    conflicts
}

#[cfg(test)]
#[allow(clippy::disallowed_methods)] // fixtures impersonate external filesystem retargets.
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

    #[cfg(unix)]
    #[test]
    fn recheck_of_a_healthy_entry_ignores_an_unrelated_identity_refusal() {
        use std::os::unix::fs::symlink;

        let base = std::env::temp_dir().join(format!(
            "norn-host-registry-refusal-isolation-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let healthy = base.join("healthy");
        let refused = base.join("refused");
        std::fs::create_dir_all(&healthy).unwrap();
        std::fs::create_dir_all(&refused).unwrap();
        let registry = ServingRegistry::from_entries([
            Entry::new(
                VaultName::new("healthy").unwrap(),
                VaultRoot::new(&healthy).unwrap(),
            ),
            Entry::new(
                VaultName::new("refused").unwrap(),
                VaultRoot::new(&refused).unwrap(),
            ),
        ])
        .unwrap();
        std::fs::remove_dir(&refused).unwrap();
        symlink("refused", &refused).unwrap();

        let healthy_reading = registry
            .recheck(&VaultName::new("healthy").unwrap())
            .expect("the healthy root reads");
        assert!(healthy_reading.conflict.is_none());
        assert!(
            healthy_reading.identity.is_some(),
            "a recheck that passed resolved the root it classified"
        );
        assert!(
            registry
                .recheck(&VaultName::new("refused").unwrap())
                .is_err()
        );

        let _ = std::fs::remove_dir_all(base);
    }

    #[cfg(unix)]
    #[test]
    fn startup_keeps_healthy_entries_serviceable_when_another_identity_refuses() {
        use std::os::unix::fs::symlink;

        let base = std::env::temp_dir().join(format!(
            "norn-host-registry-startup-refusal-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let healthy = base.join("healthy");
        let healthy_alias = healthy.join(".");
        let refused = base.join("refused");
        std::fs::create_dir_all(&healthy).unwrap();
        std::fs::create_dir_all(&base).unwrap();
        symlink("refused", &refused).unwrap();
        let healthy_name = VaultName::new("healthy").unwrap();
        let alias_name = VaultName::new("healthy-alias").unwrap();
        let refused_name = VaultName::new("refused").unwrap();

        let registry = ServingRegistry::from_entries([
            Entry::new(healthy_name.clone(), VaultRoot::new(&healthy).unwrap()),
            Entry::new(alias_name.clone(), VaultRoot::new(&healthy_alias).unwrap()),
            Entry::new(refused_name.clone(), VaultRoot::new(&refused).unwrap()),
        ])
        .expect("an unrelated refusal must not abort the serving registry");
        assert_eq!(
            registry
                .recheck(&healthy_name)
                .unwrap()
                .conflict
                .unwrap()
                .aliases,
            vec![healthy_name, alias_name]
        );
        assert!(registry.recheck(&refused_name).is_err());

        let _ = std::fs::remove_dir_all(base);
    }

    #[cfg(unix)]
    #[test]
    fn recheck_keeps_global_duplicate_classification_while_isolating_a_refusal() {
        use std::os::unix::fs::symlink;

        let base = std::env::temp_dir().join(format!(
            "norn-host-registry-global-aliases-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let shared = base.join("shared");
        let alias = base.join("alias");
        let refused = base.join("refused");
        std::fs::create_dir_all(&shared).unwrap();
        std::fs::create_dir_all(&alias).unwrap();
        std::fs::create_dir_all(&refused).unwrap();
        let alpha = VaultName::new("alpha").unwrap();
        let beta = VaultName::new("beta").unwrap();
        let registry = ServingRegistry::from_entries([
            Entry::new(alpha.clone(), VaultRoot::new(&shared).unwrap()),
            Entry::new(beta.clone(), VaultRoot::new(&alias).unwrap()),
            Entry::new(
                VaultName::new("refused").unwrap(),
                VaultRoot::new(&refused).unwrap(),
            ),
        ])
        .unwrap();
        std::fs::remove_dir(&alias).unwrap();
        symlink(&shared, &alias).unwrap();
        std::fs::remove_dir(&refused).unwrap();
        symlink("refused", &refused).unwrap();

        assert_eq!(
            registry.recheck(&alpha).unwrap().conflict.unwrap().aliases,
            vec![alpha, beta]
        );

        let _ = std::fs::remove_dir_all(base);
    }
}
