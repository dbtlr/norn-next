use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use norn_config::registry::{Entry, Registry};
use norn_fs::{Identity, Refusal, path_identity};
use norn_wire::VaultName;

/// Every registry name that resolves to one filesystem root.
///
/// The names are ascending and each appears once. [`AliasConflict::new`] is
/// the one place that order and that uniqueness are established, so a conflict
/// raised by classifying the whole registry and a conflict raised by one
/// attach meeting another alias's claim are the same fact in the same shape.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AliasConflict {
    aliases: Vec<VaultName>,
}

impl AliasConflict {
    /// The conflict the `aliases` name.
    ///
    /// The names come out deduplicated and ascending, and the refusal that
    /// acts on a conflict is what both of those carry. `refuse_conflict` takes
    /// one entry gate per alias and holds them all at once: a name appearing
    /// twice would have that one thread wait on a lock it is already holding,
    /// and ascending order is what makes two concurrent refusals over
    /// overlapping alias sets take the gates they share in the same order
    /// rather than in opposite ones.
    pub fn new(aliases: impl IntoIterator<Item = VaultName>) -> Self {
        Self {
            aliases: aliases
                .into_iter()
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect(),
        }
    }

    /// Every registered name that reaches the one root, ascending.
    pub fn aliases(&self) -> &[VaultName] {
        &self.aliases
    }
}

/// What one read of the served roots resolved for one registered name.
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

/// The registrations a host is built from: one name's root and schema source
/// per entry, ascending and each name once.
///
/// This is a read, not a collection the host keeps. [`Host::new`] turns each
/// registration here into a served entry and retains no second account of
/// them, so the roots a running host classifies are the roots its serving set
/// holds — including any the set has gained since this read.
///
/// [`Host::new`]: crate::Host::new
#[derive(Clone, Debug)]
pub struct RegistryRead {
    entries: BTreeMap<VaultName, Entry>,
}

impl RegistryRead {
    /// Read the registrations without choosing a winner among duplicate roots.
    /// Duplicates and missing roots are classified where a host acquires
    /// coverage over a root or is told coverage over one ended, against the
    /// roots that host serves at that moment.
    pub fn read(registry: &Registry) -> Self {
        Self::from_entries(registry.entries().cloned())
    }

    /// Take the registrations from an already-read registry projection.
    pub fn from_entries(entries: impl IntoIterator<Item = Entry>) -> Self {
        Self {
            entries: entries
                .into_iter()
                .map(|entry| (entry.name.clone(), entry))
                .collect(),
        }
    }

    /// The registrations themselves, ascending by name.
    pub(crate) fn into_entries(self) -> impl Iterator<Item = Entry> {
        self.entries.into_values()
    }
}

/// Classify every root the host serves against the others, answering for
/// `requested` alone: the identity its root resolved to, and the conflict it
/// stands in where more than one served name reaches that root.
///
/// A refusal over `requested`'s own root is this read's refusal; a refusal over
/// any other root belongs to that name and is left for the read that asks about
/// it. A root the filesystem answers for with nothing is registrable rather
/// than resolved, and classifies against nothing.
pub(crate) fn recheck<'a>(
    roots: impl IntoIterator<Item = (&'a VaultName, &'a Path)>,
    requested: &VaultName,
) -> Result<RootReading, Refusal> {
    let mut identities = BTreeMap::<Identity, BTreeSet<VaultName>>::new();
    let mut resolved = None;
    for (name, root) in roots {
        match path_identity(root) {
            Ok(Some(identity)) => {
                if name == requested {
                    resolved = Some(identity);
                }
                identities.entry(identity).or_default().insert(name.clone());
            }
            Ok(None) => {}
            Err(refusal) if name == requested => return Err(refusal),
            Err(_) => {}
        }
    }
    Ok(RootReading {
        identity: resolved,
        conflict: conflicts_from_identities(identities).remove(requested),
    })
}

fn conflicts_from_identities(
    identities: BTreeMap<Identity, BTreeSet<VaultName>>,
) -> BTreeMap<VaultName, AliasConflict> {
    let mut conflicts = BTreeMap::new();
    for aliases in identities.into_values().filter(|names| names.len() > 1) {
        let conflict = AliasConflict::new(aliases.iter().cloned());
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

    /// Classify `entries` as a host serving exactly those roots would.
    fn recheck_over(entries: &[Entry], requested: &VaultName) -> Result<RootReading, Refusal> {
        recheck(
            entries
                .iter()
                .map(|entry| (&entry.name, entry.root.as_path())),
            requested,
        )
    }

    #[test]
    fn every_alias_is_refused_and_names_the_whole_conflict() {
        let entries = [entry("alpha", "/tmp"), entry("beta", "/tmp/.")];
        let expected = vec![
            VaultName::new("alpha").unwrap(),
            VaultName::new("beta").unwrap(),
        ];
        for requested in &expected {
            assert_eq!(
                recheck_over(&entries, requested)
                    .unwrap()
                    .conflict
                    .unwrap()
                    .aliases(),
                expected
            );
        }
    }

    #[test]
    fn a_missing_root_stays_registrable() {
        let entries = [entry("later", "/tmp/norn-host-root-that-does-not-exist")];
        let reading = recheck_over(&entries, &VaultName::new("later").unwrap()).unwrap();
        assert!(reading.conflict.is_none());
        assert!(reading.identity.is_none());
    }

    /// A recheck classifies every served root, and the identity it answers
    /// with is the requested name's own. The other roots are read for the
    /// conflict alone: a reading that carried one of them would file the
    /// acquisition claim under a root the caller never asked about.
    #[test]
    fn a_recheck_resolves_the_requested_root_among_several() {
        let base = std::env::temp_dir().join(format!(
            "norn-host-registry-requested-identity-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let alpha_root = base.join("alpha");
        let beta_root = base.join("beta");
        std::fs::create_dir_all(&alpha_root).unwrap();
        std::fs::create_dir_all(&beta_root).unwrap();
        let alpha = VaultName::new("alpha").unwrap();
        let beta = VaultName::new("beta").unwrap();
        let entries = [
            Entry::new(alpha.clone(), VaultRoot::new(&alpha_root).unwrap()),
            Entry::new(beta.clone(), VaultRoot::new(&beta_root).unwrap()),
        ];

        let alpha_identity = path_identity(&alpha_root).unwrap();
        let beta_identity = path_identity(&beta_root).unwrap();
        assert!(
            alpha_identity.is_some() && alpha_identity != beta_identity,
            "the two roots are one root, so nothing here can tell them apart"
        );

        for (requested, expected) in [(&alpha, alpha_identity), (&beta, beta_identity)] {
            assert_eq!(
                recheck_over(&entries, requested).unwrap().identity,
                expected,
                "the recheck over {requested:?} resolved another registered root"
            );
        }

        let _ = std::fs::remove_dir_all(base);
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
        let entries = [
            Entry::new(
                VaultName::new("healthy").unwrap(),
                VaultRoot::new(&healthy).unwrap(),
            ),
            Entry::new(
                VaultName::new("refused").unwrap(),
                VaultRoot::new(&refused).unwrap(),
            ),
        ];
        std::fs::remove_dir(&refused).unwrap();
        symlink("refused", &refused).unwrap();

        let healthy_reading = recheck_over(&entries, &VaultName::new("healthy").unwrap())
            .expect("the healthy root reads");
        assert!(healthy_reading.conflict.is_none());
        assert!(
            healthy_reading.identity.is_some(),
            "a recheck that passed resolved the root it classified"
        );
        assert!(recheck_over(&entries, &VaultName::new("refused").unwrap()).is_err());

        let _ = std::fs::remove_dir_all(base);
    }

    #[cfg(unix)]
    #[test]
    fn a_recheck_keeps_healthy_entries_serviceable_when_another_identity_refuses() {
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

        let entries = [
            Entry::new(healthy_name.clone(), VaultRoot::new(&healthy).unwrap()),
            Entry::new(alias_name.clone(), VaultRoot::new(&healthy_alias).unwrap()),
            Entry::new(refused_name.clone(), VaultRoot::new(&refused).unwrap()),
        ];
        assert_eq!(
            recheck_over(&entries, &healthy_name)
                .expect("an unrelated refusal must not reach the healthy root")
                .conflict
                .unwrap()
                .aliases(),
            vec![healthy_name, alias_name]
        );
        assert!(recheck_over(&entries, &refused_name).is_err());

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
        let entries = [
            Entry::new(alpha.clone(), VaultRoot::new(&shared).unwrap()),
            Entry::new(beta.clone(), VaultRoot::new(&alias).unwrap()),
            Entry::new(
                VaultName::new("refused").unwrap(),
                VaultRoot::new(&refused).unwrap(),
            ),
        ];
        std::fs::remove_dir(&alias).unwrap();
        symlink(&shared, &alias).unwrap();
        std::fs::remove_dir(&refused).unwrap();
        symlink("refused", &refused).unwrap();

        assert_eq!(
            recheck_over(&entries, &alpha)
                .unwrap()
                .conflict
                .unwrap()
                .aliases(),
            vec![alpha, beta]
        );

        let _ = std::fs::remove_dir_all(base);
    }
}
