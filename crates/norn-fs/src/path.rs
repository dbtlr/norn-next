#![forbid(unsafe_code)]
//! Vault-relative path identity, normalized once at the filesystem boundary.
//!
//! A [`PathNormalizer`] belongs to one vault root. It keeps the spelling found
//! in the directory tree for display and access, while producing an opaque key
//! for comparisons. The key folds ASCII case only when an existing directory
//! entry proves that the root treats alternate case spellings as the same
//! entry. No platform-name or mount-type guess is made.
//!
//! Case folding is deliberately ASCII-only. Unix path names are byte strings,
//! not necessarily UTF-8; preserving non-ASCII bytes avoids inventing a lossy
//! Unicode policy while still covering the case behavior Norn currently
//! promises.

use std::ffi::{OsStr, OsString};
use std::fmt;
use std::fs;
use std::hash::{Hash, Hasher};
use std::io;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};

/// The case behavior established for a vault root.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CaseSensitivity {
    /// Alternate ASCII case names may identify distinct directory entries.
    Sensitive,
    /// Alternate ASCII case names resolve to the same directory entry.
    Insensitive,
}

/// Why a root-scoped normalizer could not be constructed.
#[derive(Debug)]
pub enum NormalizerError {
    /// The supplied root could not be enumerated.
    ReadRoot { root: PathBuf, source: io::Error },
    /// Existing entries provided no safe, case-bearing probe.
    Indeterminate { root: PathBuf },
}

impl fmt::Display for NormalizerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ReadRoot { root, source } => {
                write!(
                    formatter,
                    "cannot inspect vault root {}: {source}",
                    root.display()
                )
            }
            Self::Indeterminate { root } => write!(
                formatter,
                "cannot determine case sensitivity from entries in vault root {}",
                root.display()
            ),
        }
    }
}

impl std::error::Error for NormalizerError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::ReadRoot { source, .. } => Some(source),
            Self::Indeterminate { .. } => None,
        }
    }
}

/// Why a path has no vault-relative identity.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PathError {
    /// The vault root itself is not a document identity.
    Empty,
    /// Absolute paths and platform prefixes are outside the vault-relative API.
    Absolute,
    /// A parent component could escape the vault root.
    ParentTraversal,
}

impl fmt::Display for PathError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Empty => "a vault-relative path cannot be empty",
            Self::Absolute => "an absolute path has no vault-relative identity",
            Self::ParentTraversal => "a vault-relative path cannot contain `..`",
        })
    }
}

impl std::error::Error for PathError {}

/// The one root-scoped producer of normalized path identities.
#[derive(Clone, Debug)]
pub struct PathNormalizer {
    sensitivity: CaseSensitivity,
}

impl PathNormalizer {
    /// Builds a normalizer after positively determining the root's case behavior.
    ///
    /// Detection is read-only. The root's own entry in its parent is tried
    /// first, so an empty vault can still carry evidence. Existing children are
    /// fallback probes. A same-identity alternate lookup proves insensitivity
    /// only when the alternate spelling is not itself present as a directory
    /// entry; this prevents hardlink aliases from masquerading as insensitive
    /// lookup. A missing alternate spelling or a different identity proves
    /// sensitivity. If no entry can supply safe evidence, construction refuses.
    #[allow(clippy::disallowed_methods)] // The vault filesystem seam: this crate owns path identity detection.
    pub fn detect(root: &Path) -> Result<Self, NormalizerError> {
        if let (Some(parent), Some(name)) = (root.parent(), root.file_name())
            && same_device(root, parent)
            && let Some(sensitivity) = probe_case_behavior(parent, name)
        {
            return Ok(Self { sensitivity });
        }

        let entries = fs::read_dir(root).map_err(|source| NormalizerError::ReadRoot {
            root: root.to_owned(),
            source,
        })?;

        for entry in entries {
            let entry = entry.map_err(|source| NormalizerError::ReadRoot {
                root: root.to_owned(),
                source,
            })?;
            let name = entry.file_name();
            if alternate_ascii_case(&name).is_none() {
                continue;
            }
            if let Some(sensitivity) = probe_case_behavior(root, &name) {
                return Ok(Self { sensitivity });
            }
        }

        Err(NormalizerError::Indeterminate {
            root: root.to_owned(),
        })
    }

    /// Returns the case behavior proven when this normalizer was constructed.
    pub fn case_sensitivity(&self) -> CaseSensitivity {
        self.sensitivity
    }

    /// Normalizes a path while retaining its actual access/display spelling.
    pub fn normalize(&self, path: &Path) -> Result<NormalizedPath, PathError> {
        let mut access = PathBuf::new();
        for component in path.components() {
            match component {
                Component::Normal(part) => access.push(part),
                Component::CurDir => {}
                Component::ParentDir => return Err(PathError::ParentTraversal),
                Component::RootDir | Component::Prefix(_) => return Err(PathError::Absolute),
            }
        }
        if access.as_os_str().is_empty() {
            return Err(PathError::Empty);
        }

        let key = match self.sensitivity {
            CaseSensitivity::Sensitive => access.as_os_str().to_owned(),
            CaseSensitivity::Insensitive => ascii_fold(access.as_os_str()),
        };
        Ok(NormalizedPath { access, key })
    }
}

/// A vault-relative path with separate access spelling and comparison identity.
#[derive(Clone, Debug)]
pub struct NormalizedPath {
    access: PathBuf,
    key: OsString,
}

impl NormalizedPath {
    /// The normalized relative spelling to use for display and filesystem access.
    pub fn as_path(&self) -> &Path {
        &self.access
    }

    /// The opaque root-scoped comparison and sort key.
    pub(crate) fn comparison_key(&self) -> &OsStr {
        &self.key
    }
}

impl PartialEq for NormalizedPath {
    fn eq(&self, other: &Self) -> bool {
        self.key == other.key
    }
}

impl Eq for NormalizedPath {}

impl PartialOrd for NormalizedPath {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for NormalizedPath {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.key.cmp(&other.key)
    }
}

impl Hash for NormalizedPath {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.key.hash(state);
    }
}

fn same_identity(left: &fs::Metadata, right: &fs::Metadata) -> bool {
    left.dev() == right.dev() && left.ino() == right.ino()
}

#[allow(clippy::disallowed_methods)] // The vault filesystem seam: read-only mount detection.
fn same_device(left: &Path, right: &Path) -> bool {
    match (fs::symlink_metadata(left), fs::symlink_metadata(right)) {
        (Ok(left), Ok(right)) => left.dev() == right.dev(),
        _ => false,
    }
}

#[allow(clippy::disallowed_methods)] // The vault filesystem seam: read-only case detection.
fn probe_case_behavior(parent: &Path, name: &OsStr) -> Option<CaseSensitivity> {
    let alternate = alternate_ascii_case(name)?;
    let actual_metadata = fs::symlink_metadata(parent.join(name)).ok()?;

    match fs::symlink_metadata(parent.join(&alternate)) {
        Ok(other) if !same_identity(&actual_metadata, &other) => Some(CaseSensitivity::Sensitive),
        Ok(_) => {
            // If both spellings are actual entries, they may merely be hardlink
            // aliases. Only lookup of a spelling absent from the directory can
            // positively demonstrate case-insensitive name resolution.
            let alternate_is_entry = fs::read_dir(parent)
                .ok()?
                .filter_map(Result::ok)
                .any(|entry| entry.file_name() == alternate);
            (!alternate_is_entry).then_some(CaseSensitivity::Insensitive)
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Some(CaseSensitivity::Sensitive),
        Err(_) => None,
    }
}

fn alternate_ascii_case(name: &OsStr) -> Option<OsString> {
    let mut bytes = name.as_bytes().to_vec();
    let byte = bytes.iter_mut().find(|byte| byte.is_ascii_alphabetic())?;
    *byte = if byte.is_ascii_lowercase() {
        byte.to_ascii_uppercase()
    } else {
        byte.to_ascii_lowercase()
    };
    Some(OsString::from_vec(bytes))
}

fn ascii_fold(path: &OsStr) -> OsString {
    OsString::from_vec(path.as_bytes().iter().map(u8::to_ascii_lowercase).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::ffi::OsStrExt;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn normalizer(sensitivity: CaseSensitivity) -> PathNormalizer {
        PathNormalizer { sensitivity }
    }

    #[allow(clippy::disallowed_methods)] // Harness scaffolding: creating the root under inspection.
    fn scratch() -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time after epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!("norn-path-{}-{nonce}", std::process::id()));
        fs::create_dir(&path).expect("scratch directory");
        path
    }

    #[test]
    fn removes_current_directory_components_and_redundant_separators() {
        let path = normalizer(CaseSensitivity::Sensitive)
            .normalize(Path::new("./notes//today.md"))
            .expect("relative identity");
        assert_eq!(path.as_path(), Path::new("notes/today.md"));
        assert_eq!(path.comparison_key(), OsStr::new("notes/today.md"));
    }

    #[test]
    fn refuses_empty_absolute_and_parent_paths() {
        let paths = normalizer(CaseSensitivity::Sensitive);
        assert_eq!(
            paths.normalize(Path::new(".")).unwrap_err(),
            PathError::Empty
        );
        assert_eq!(
            paths.normalize(Path::new("/note.md")).unwrap_err(),
            PathError::Absolute
        );
        assert_eq!(
            paths.normalize(Path::new("notes/../note.md")).unwrap_err(),
            PathError::ParentTraversal
        );
    }

    #[test]
    fn insensitive_identity_folds_ascii_but_preserves_access_and_non_utf8() {
        let raw = OsStr::from_bytes(b"Notes/\xffTODAY.md");
        let path = normalizer(CaseSensitivity::Insensitive)
            .normalize(Path::new(raw))
            .expect("ordinary unix path");
        assert_eq!(path.as_path().as_os_str().as_bytes(), b"Notes/\xffTODAY.md");
        assert_eq!(path.comparison_key().as_bytes(), b"notes/\xfftoday.md");
    }

    #[test]
    fn equality_uses_the_root_scoped_key() {
        let paths = normalizer(CaseSensitivity::Insensitive);
        let upper = paths.normalize(Path::new("Notes/A.md")).unwrap();
        let lower = paths.normalize(Path::new("notes/a.md")).unwrap();
        assert_eq!(upper, lower);
        assert_ne!(upper.as_path(), lower.as_path());
    }

    #[test]
    fn ordering_uses_the_same_key_as_equality() {
        let paths = normalizer(CaseSensitivity::Insensitive);
        let alpha = paths.normalize(Path::new("Z/alpha.md")).unwrap();
        let beta = paths.normalize(Path::new("a/BETA.md")).unwrap();
        assert!(beta < alpha);
    }

    #[test]
    #[allow(clippy::disallowed_methods)] // Harness scaffolding: observing the host filesystem's case behavior.
    fn detection_uses_an_existing_entry_and_matches_lookup_behavior() {
        let parent = scratch();
        let root = parent.join("123");
        fs::create_dir(&root).expect("uncased root");
        fs::write(root.join("CaseProbe"), b"").expect("probe entry");
        let same = fs::symlink_metadata(root.join("caseProbe"))
            .ok()
            .zip(fs::symlink_metadata(root.join("CaseProbe")).ok())
            .is_some_and(|(left, right)| same_identity(&left, &right));
        let detected = PathNormalizer::detect(&root).expect("detectable root");
        assert_eq!(
            detected.case_sensitivity(),
            if same {
                CaseSensitivity::Insensitive
            } else {
                CaseSensitivity::Sensitive
            }
        );
        fs::remove_dir_all(parent).expect("remove scratch");
    }

    #[test]
    #[allow(clippy::disallowed_methods)] // Harness scaffolding: observing the host filesystem's case behavior.
    fn detection_uses_the_empty_root_entry_as_case_evidence() {
        let root = scratch();
        let parent = root.parent().expect("scratch parent");
        let name = root.file_name().expect("scratch name");
        let expected = probe_case_behavior(parent, name).expect("case-bearing root entry");

        let detected = PathNormalizer::detect(&root).expect("detectable empty root");
        assert_eq!(detected.case_sensitivity(), expected);
        fs::remove_dir_all(root).expect("remove scratch");
    }

    #[test]
    #[allow(clippy::disallowed_methods)] // Harness scaffolding: arranging hardlink aliases.
    fn hardlinked_alternate_spellings_do_not_prove_insensitivity() {
        let parent = scratch();
        let root = parent.join("123");
        fs::create_dir(&root).expect("uncased root");
        fs::write(root.join("CaseProbe"), b"").expect("probe entry");
        if fs::hard_link(root.join("CaseProbe"), root.join("caseProbe")).is_err() {
            // A case-insensitive host cannot contain the distinct spellings
            // needed to exercise the sensitive-filesystem hardlink case.
            fs::remove_dir_all(parent).expect("remove scratch");
            return;
        }

        assert!(matches!(
            PathNormalizer::detect(&root),
            Err(NormalizerError::Indeterminate { .. })
        ));
        fs::remove_dir_all(parent).expect("remove scratch");
    }

    #[test]
    #[allow(clippy::disallowed_methods)] // Harness scaffolding: arranging a root without case evidence.
    fn detection_refuses_when_the_root_has_no_case_evidence() {
        let parent = scratch();
        let root = parent.join("123");
        fs::create_dir(&root).expect("uncased root");
        fs::write(root.join("123"), b"").expect("uncased entry");
        assert!(matches!(
            PathNormalizer::detect(&root),
            Err(NormalizerError::Indeterminate { .. })
        ));
        fs::remove_dir_all(parent).expect("remove scratch");
    }

    #[test]
    #[allow(clippy::disallowed_methods)] // Harness scaffolding: cleaning up the probe root.
    fn mount_probe_guard_compares_device_identity() {
        let root = scratch();
        assert!(same_device(&root, &root));
        assert!(!same_device(&root.join("missing"), &root));
        fs::remove_dir(root).expect("remove scratch directory");
    }
}
