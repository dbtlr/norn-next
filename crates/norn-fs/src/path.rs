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

use std::cmp::Ordering;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::fs;
use std::hash::{Hash, Hasher};
use std::io;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};

use crate::identity::identity_of;

/// The case behavior established for a vault root.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CaseSensitivity {
    /// Alternate ASCII case names may identify distinct directory entries.
    Sensitive,
    /// Alternate ASCII case names resolve to the same directory entry.
    Insensitive,
}

impl CaseSensitivity {
    /// Compare two UTF-8 vault-relative spellings with this root's proven
    /// lookup semantics.
    pub fn compare(self, left: &str, right: &str) -> Ordering {
        match self {
            Self::Sensitive => left.as_bytes().cmp(right.as_bytes()),
            Self::Insensitive => left
                .bytes()
                .map(|byte| byte.to_ascii_lowercase())
                .cmp(right.bytes().map(|byte| byte.to_ascii_lowercase()))
                .then_with(|| left.as_bytes().cmp(right.as_bytes())),
        }
    }
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

    /// A normalizer for a root whose case behavior a case states outright,
    /// so identity rules are judged on both behaviors from any host.
    #[cfg(test)]
    pub(crate) fn for_sensitivity(sensitivity: CaseSensitivity) -> Self {
        Self { sensitivity }
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

        let mut key = Vec::new();
        fold_onto(self.sensitivity, &mut key, access.as_os_str().as_bytes());
        Ok(NormalizedPath {
            access,
            key: OsString::from_vec(key),
        })
    }
}

/// The comparison keys of one directory's children, taken a name at a time.
///
/// The key this hands back for `name` is the key [`PathNormalizer::normalize`]
/// produces for the same child path, and
/// `a_child_key_is_the_key_normalization_produces` holds the two together. It
/// exists because a caller enumerating a directory asks for far more keys than
/// it keeps paths: the parent's own bytes are folded once here and only the
/// child's are rewritten after them, so a name the caller discards costs one
/// fold into a buffer it already owns rather than a [`NormalizedPath`].
pub(crate) struct ChildKeys {
    sensitivity: CaseSensitivity,
    /// The parent's key, a separator where the parent is not the root, and
    /// whichever child name was asked for last.
    key: Vec<u8>,
    /// How much of `key` is the parent's and stays across names.
    parent: usize,
}

impl ChildKeys {
    /// Keys under `parent`, a normalized vault-relative directory spelling —
    /// empty for the vault root, whose children are named by one component.
    pub(crate) fn under(normalizer: &PathNormalizer, parent: &Path) -> ChildKeys {
        let sensitivity = normalizer.sensitivity;
        let mut key = Vec::new();
        fold_onto(sensitivity, &mut key, parent.as_os_str().as_bytes());
        if !key.is_empty() {
            key.push(b'/');
        }
        ChildKeys {
            sensitivity,
            parent: key.len(),
            key,
        }
    }

    /// The comparison key of this directory's child `name`.
    ///
    /// The returned slice is the buffer this holds, so it stands until the next
    /// name is asked for.
    pub(crate) fn of(&mut self, name: &[u8]) -> &[u8] {
        self.key.truncate(self.parent);
        fold_onto(self.sensitivity, &mut self.key, name);
        &self.key
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

    /// Whether this path is `root` itself or lies beneath it.
    ///
    /// Containment is answered on the comparison key, so a root's case behavior
    /// decides it exactly as it decides equality, and only whole components
    /// match: `notes-archive` lies beneath `notes` no more than a sibling does.
    ///
    /// In-crate: [`Exclusions`](crate::Exclusions) is the exported way to ask
    /// what a root contains, so a host cannot spell a second answer out of this
    /// primitive.
    ///
    /// A test asking whether a *reported* root reaches a path is asking a
    /// different question — both sides are values one harness run collected,
    /// with no volume behind them — and it is answered outside this crate, by
    /// `norn_testkit::invalidation::at_or_above`. Neither answer is the other's
    /// substitute: this one carries the vault's proven case behavior, that one
    /// carries none.
    pub(crate) fn starts_with(&self, root: &Self) -> bool {
        let (path, prefix) = (self.key.as_bytes(), root.key.as_bytes());
        path.starts_with(prefix) && matches!(path.get(prefix.len()), None | Some(b'/'))
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
    identity_of(left) == identity_of(right)
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

/// The one fold every comparison key in this module is built by, appended to
/// `key`. A whole path and a child name reach it by different routes and get
/// the same bytes, which is what lets a caller compare one against the other.
fn fold_onto(sensitivity: CaseSensitivity, key: &mut Vec<u8>, bytes: &[u8]) {
    match sensitivity {
        CaseSensitivity::Sensitive => key.extend_from_slice(bytes),
        CaseSensitivity::Insensitive => key.extend(bytes.iter().map(u8::to_ascii_lowercase)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::ffi::OsStrExt;
    use std::sync::atomic::AtomicU64;

    fn normalizer(sensitivity: CaseSensitivity) -> PathNormalizer {
        PathNormalizer::for_sensitivity(sensitivity)
    }

    /// Distinguishes two scratch roots taken in the same process. A clock
    /// reading does not: two cases running on two threads read the same
    /// nanosecond often enough to collide, and the loser meets a directory that
    /// already exists. A run that reuses a process id meets whatever the
    /// previous one left behind, which is why the name is cleared first.
    static SERIAL: AtomicU64 = AtomicU64::new(0);

    #[allow(clippy::disallowed_methods)] // Harness scaffolding: creating the root under inspection.
    fn scratch() -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "norn-path-{}-{}",
            std::process::id(),
            SERIAL.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&path);
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

    /// **A child key is the key normalization produces, or it is a second
    /// identity.** The two are separate code because one of them is asked for
    /// far more often than a whole path is kept, and this is what holds them to
    /// the same answer — over both case behaviors, a root parent and a nested
    /// one, and names that are not UTF-8.
    #[test]
    fn a_child_key_is_the_key_normalization_produces() {
        let parents: [&Path; 3] = [
            Path::new(""),
            Path::new("Notes"),
            Path::new("Notes/Daily Log"),
        ];
        let names: [&OsStr; 4] = [
            OsStr::new("TODAY.md"),
            OsStr::new("today.md"),
            OsStr::new(".hidden"),
            OsStr::from_bytes(b"\xffRaw Bytes.md"),
        ];
        for sensitivity in [CaseSensitivity::Sensitive, CaseSensitivity::Insensitive] {
            let paths = normalizer(sensitivity);
            for parent in parents {
                let mut keys = ChildKeys::under(&paths, parent);
                for name in names {
                    let whole = paths
                        .normalize(&parent.join(name))
                        .expect("a vault-relative child path");
                    assert_eq!(
                        keys.of(name.as_bytes()),
                        whole.comparison_key().as_bytes(),
                        "{sensitivity:?} key of {name:?} under {parent:?}"
                    );
                }
            }
        }
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
    fn containment_matches_whole_components_under_the_root_scoped_key() {
        let paths = normalizer(CaseSensitivity::Sensitive);
        let root = paths.normalize(Path::new("notes")).unwrap();
        assert!(
            paths
                .normalize(Path::new("notes"))
                .unwrap()
                .starts_with(&root)
        );
        assert!(
            paths
                .normalize(Path::new("notes/today.md"))
                .unwrap()
                .starts_with(&root)
        );
        assert!(
            !paths
                .normalize(Path::new("notes-archive/today.md"))
                .unwrap()
                .starts_with(&root)
        );
        assert!(!root.starts_with(&paths.normalize(Path::new("notes/today.md")).unwrap()));
    }

    #[test]
    fn containment_folds_case_exactly_where_equality_does() {
        let sensitive = normalizer(CaseSensitivity::Sensitive);
        let insensitive = normalizer(CaseSensitivity::Insensitive);
        for paths in [&sensitive, &insensitive] {
            let root = paths.normalize(Path::new("Notes")).unwrap();
            let under = paths.normalize(Path::new("notes/today.md")).unwrap();
            assert_eq!(
                under.starts_with(&root),
                paths.case_sensitivity() == CaseSensitivity::Insensitive
            );
        }
    }

    #[test]
    fn ordering_uses_the_same_key_as_equality() {
        let paths = normalizer(CaseSensitivity::Insensitive);
        let alpha = paths.normalize(Path::new("Z/alpha.md")).unwrap();
        let beta = paths.normalize(Path::new("a/BETA.md")).unwrap();
        assert!(beta < alpha);
    }

    #[test]
    fn exposed_utf8_order_folds_ascii_only_and_keeps_a_stable_tiebreaker() {
        assert!(
            CaseSensitivity::Insensitive
                .compare("a/Z.md", "A/b.md")
                .is_gt()
        );
        assert!(CaseSensitivity::Insensitive.compare("A.md", "a.md").is_lt());
        assert!(CaseSensitivity::Sensitive.compare("A.md", "a.md").is_lt());
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

    /// A root the process cannot enumerate is not an indeterminate root: the
    /// two refusals name different situations, and only this one carries the
    /// error the filesystem gave.
    #[test]
    #[allow(clippy::disallowed_methods)] // Harness scaffolding: naming a root that is not there.
    fn detection_refuses_a_root_it_cannot_enumerate() {
        let parent = scratch();
        let root = parent.join("123");
        let refusal = PathNormalizer::detect(&root).expect_err("an absent root");
        assert!(
            matches!(&refusal, NormalizerError::ReadRoot { root: named, source }
                if named == &root && source.kind() == io::ErrorKind::NotFound),
            "{refusal:?}"
        );
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
