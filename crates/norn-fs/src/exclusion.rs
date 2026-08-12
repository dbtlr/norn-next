#![forbid(unsafe_code)]
//! The roots inside a vault Norn does not read, and the one answer to whether a
//! path lies in one.
//!
//! Every root is vault-relative and is resolved through the vault root's own
//! [`PathNormalizer`], so membership is decided on normalized identity: the case
//! behavior the root proved decides it, and only whole components match. A walk
//! rooted at a subdirectory therefore excludes the same paths a walk of the
//! whole vault does — the set is a property of the vault, never of where a
//! traversal happens to start.

use std::collections::BTreeSet;
use std::fmt;
use std::path::{Path, PathBuf};

use crate::path::{NormalizedPath, PathError, PathNormalizer};
use crate::shadow::FALLBACK;

/// Why a path lies inside a root Norn does not read.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Excluded {
    /// A root supplied by the host.
    Host,
    /// Norn's own `.norn/tmp` fallback subtree.
    Mechanism,
}

/// A supplied exclusion root with no vault-relative identity.
#[derive(Clone, Debug)]
pub struct ExclusionError {
    /// The root as it was supplied.
    pub path: PathBuf,
    /// Why it names nothing inside the vault.
    pub source: PathError,
}

impl fmt::Display for ExclusionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "cannot normalize exclusion {}: {}",
            self.path.display(),
            self.source
        )
    }
}

impl std::error::Error for ExclusionError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

/// The excluded roots of one vault, resolved against that vault's identity.
#[derive(Clone, Debug)]
pub struct Exclusions {
    hosts: BTreeSet<NormalizedPath>,
    mechanism: NormalizedPath,
}

impl Exclusions {
    /// Resolves host-supplied roots against `normalizer`, alongside Norn's own
    /// built-in fallback root.
    pub fn new(normalizer: &PathNormalizer, hosts: &[PathBuf]) -> Result<Self, ExclusionError> {
        let hosts = hosts
            .iter()
            .map(|path| {
                normalizer.normalize(path).map_err(|source| ExclusionError {
                    path: path.clone(),
                    source,
                })
            })
            .collect::<Result<BTreeSet<_>, _>>()?;
        let mechanism = normalizer
            .normalize(Path::new(FALLBACK))
            .expect("the fixed mechanism root is relative and normalized");
        Ok(Self { hosts, mechanism })
    }

    /// Why `path` is excluded, or `None` when Norn reads it.
    ///
    /// A path under an excluded root is excluded with its root's reason, so the
    /// answer holds for a path a traversal reaches without passing through the
    /// root itself.
    pub fn reason(&self, path: &NormalizedPath) -> Option<Excluded> {
        if self.hosts.iter().any(|root| path.starts_with(root)) {
            Some(Excluded::Host)
        } else if path.starts_with(&self.mechanism) {
            Some(Excluded::Mechanism)
        } else {
            None
        }
    }

    /// Whether `path` lies in any excluded root.
    pub fn excludes(&self, path: &NormalizedPath) -> bool {
        self.reason(path).is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::path::CaseSensitivity;

    fn exclusions(sensitivity: CaseSensitivity, hosts: &[&str]) -> (PathNormalizer, Exclusions) {
        let normalizer = PathNormalizer::for_sensitivity(sensitivity);
        let hosts = hosts.iter().map(PathBuf::from).collect::<Vec<_>>();
        let set = Exclusions::new(&normalizer, &hosts).expect("vault-relative roots");
        (normalizer, set)
    }

    /// **The bar on membership.** A root excludes itself and everything beneath
    /// it, and nothing whose spelling merely opens with the root's characters.
    #[test]
    fn a_root_excludes_itself_and_its_descendants_only() {
        let (paths, excluded) = exclusions(CaseSensitivity::Sensitive, &["staging"]);
        let member = |spelling| excluded.reason(&paths.normalize(Path::new(spelling)).unwrap());
        assert_eq!(member("staging"), Some(Excluded::Host));
        assert_eq!(member("staging/deep/note.md"), Some(Excluded::Host));
        assert_eq!(member("staging-old/note.md"), None);
        assert_eq!(member("note.md"), None);
    }

    /// **The bar on the built-in root.** The fallback subtree is excluded
    /// without being supplied, and it is the vault's own `.norn/tmp` — a
    /// directory of that name anywhere else is a vault directory Norn reads.
    #[test]
    fn the_fallback_subtree_is_excluded_by_the_vault_root_it_hangs_from() {
        let (paths, excluded) = exclusions(CaseSensitivity::Sensitive, &[]);
        let member = |spelling| excluded.reason(&paths.normalize(Path::new(spelling)).unwrap());
        assert_eq!(member(FALLBACK), Some(Excluded::Mechanism));
        assert_eq!(
            member(".norn/tmp/norn-dev/notes/0f0f0f0f/staged"),
            Some(Excluded::Mechanism)
        );
        assert_eq!(member(".norn"), None);
        assert_eq!(member(".norn/notes.md"), None);
        assert_eq!(member("folder/.norn/tmp/live.md"), None);
    }

    /// **The bar on case.** Membership is decided by the identity the vault root
    /// proved, so an alternate-case spelling of an excluded root is excluded
    /// exactly where an alternate-case spelling of a document is the same
    /// document.
    #[test]
    fn membership_follows_the_case_behavior_the_root_proved() {
        for sensitivity in [CaseSensitivity::Sensitive, CaseSensitivity::Insensitive] {
            let (paths, excluded) = exclusions(sensitivity, &["Staging"]);
            let member = |spelling| excluded.reason(&paths.normalize(Path::new(spelling)).unwrap());
            let folded = sensitivity == CaseSensitivity::Insensitive;
            assert_eq!(
                member("staging/note.md").is_some(),
                folded,
                "host root under {sensitivity:?}"
            );
            assert_eq!(
                member(".NORN/TMP/staged").is_some(),
                folded,
                "mechanism root under {sensitivity:?}"
            );
        }
    }

    #[test]
    fn a_root_with_no_vault_relative_identity_is_refused() {
        let normalizer = PathNormalizer::for_sensitivity(CaseSensitivity::Sensitive);
        let error = Exclusions::new(&normalizer, &[PathBuf::from("/absolute")])
            .expect_err("an absolute root names nothing inside the vault");
        assert_eq!(error.source, PathError::Absolute);
        assert_eq!(error.path, Path::new("/absolute"));
    }
}
