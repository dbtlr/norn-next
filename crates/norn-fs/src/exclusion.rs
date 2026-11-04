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

use std::borrow::Cow;
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
    /// Norn's own `.norn/tmp` fallback subtree. It is dot-led as well, and is
    /// named on its own so Norn's staging stays outside the vault on the
    /// strength of its own rule rather than the parity rule beside it.
    Mechanism,
    /// A file or folder whose name leads with a dot, which Obsidian ignores
    /// and the vault therefore does not hold: `.git`, `.obsidian`, `.trash`,
    /// `.norn` beyond its fallback subtree, or a dotfile anywhere.
    DotLed,
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
        } else if path.has_dot_led_segment() {
            Some(Excluded::DotLed)
        } else {
            None
        }
    }

    /// The excluded root `path` lies in, with its reason.
    ///
    /// The root is what a consumer converging derived state acts over: every
    /// place beneath it holds the same nothing, so one reading of the root
    /// answers for every path inside it rather than one reading per path. A
    /// supplied root answers first and the fallback subtree next, so a path
    /// beneath either keeps that root though it may also be dot-led; a
    /// dot-led path otherwise answers with its outermost dot-led segment.
    pub(crate) fn covering_root(
        &self,
        path: &NormalizedPath,
    ) -> Option<(Cow<'_, NormalizedPath>, Excluded)> {
        if let Some(root) = self.hosts.iter().find(|root| path.starts_with(root)) {
            Some((Cow::Borrowed(root), Excluded::Host))
        } else if path.starts_with(&self.mechanism) {
            Some((Cow::Borrowed(&self.mechanism), Excluded::Mechanism))
        } else {
            path.dot_led_root()
                .map(|root| (Cow::Owned(root), Excluded::DotLed))
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
    /// without being supplied, and it is the vault's own `.norn/tmp`. Every
    /// other `.norn` path, here or deeper, is excluded as dot-led instead.
    #[test]
    fn the_fallback_subtree_is_excluded_by_the_vault_root_it_hangs_from() {
        let (paths, excluded) = exclusions(CaseSensitivity::Sensitive, &[]);
        let member = |spelling| excluded.reason(&paths.normalize(Path::new(spelling)).unwrap());
        assert_eq!(member(FALLBACK), Some(Excluded::Mechanism));
        assert_eq!(
            member(".norn/tmp/norn-dev/notes/0f0f0f0f/staged"),
            Some(Excluded::Mechanism)
        );
        assert_eq!(member(".norn"), Some(Excluded::DotLed));
        assert_eq!(member(".norn/notes.md"), Some(Excluded::DotLed));
        assert_eq!(member("folder/.norn/tmp/live.md"), Some(Excluded::DotLed));
    }

    /// **The bar on dot-led names.** Obsidian ignores every file and folder
    /// whose name leads with a dot, so the vault does too: a dot-led segment
    /// excludes itself and everything beneath it, wherever it stands, and the
    /// root it answers with is the outermost dot-led segment. A dot anywhere
    /// but the head of a segment leaves the path in the vault.
    #[test]
    fn a_dot_led_segment_excludes_itself_and_its_descendants() {
        let (paths, excluded) = exclusions(CaseSensitivity::Sensitive, &[]);
        let path = |spelling| paths.normalize(Path::new(spelling)).unwrap();
        let root = |spelling| {
            excluded
                .covering_root(&path(spelling))
                .map(|(root, reason)| (root.as_path().to_path_buf(), reason))
        };
        let dot_led = |spelling: &str| Some((PathBuf::from(spelling), Excluded::DotLed));
        assert_eq!(root(".trash"), dot_led(".trash"));
        assert_eq!(root(".trash/note.md"), dot_led(".trash"));
        assert_eq!(root("notes/.draft.md"), dot_led("notes/.draft.md"));
        assert_eq!(root("a/.hidden/b/.deeper/c.md"), dot_led("a/.hidden"));
        assert_eq!(root(".norn/notes.md"), dot_led(".norn"));
        assert_eq!(root("notes/v1.2/note.md"), None);
        assert_eq!(root("notes/note.md"), None);
        assert_eq!(
            excluded.reason(&path(".obsidian/plugins/x/styles.css")),
            Some(Excluded::DotLed)
        );
        assert!(!excluded.excludes(&path("notes/note.md")));
    }

    /// **The bar on precedence.** A path under a supplied or built-in root
    /// answers with that root, so the fallback subtree stays the mechanism
    /// root a reach of it is refused by, though `.norn` is itself dot-led.
    #[test]
    fn a_supplied_or_mechanism_root_answers_before_a_dot_led_segment() {
        let (paths, excluded) = exclusions(CaseSensitivity::Sensitive, &["staging"]);
        let root = |spelling| {
            excluded
                .covering_root(&paths.normalize(Path::new(spelling)).unwrap())
                .map(|(root, reason)| (root.as_path().to_path_buf(), reason))
        };
        assert_eq!(
            root("staging/.trash/note.md"),
            Some((PathBuf::from("staging"), Excluded::Host))
        );
        assert_eq!(
            root(".norn/tmp/staged"),
            Some((PathBuf::from(FALLBACK), Excluded::Mechanism))
        );
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
                member(".NORN/TMP/staged"),
                Some(if folded {
                    Excluded::Mechanism
                } else {
                    Excluded::DotLed
                }),
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
